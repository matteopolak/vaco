//! [`RtspDemuxer`] (RFC 2326/7826 client) and the registered `rtsp`/`rtp`/
//! `sdp` [`vaco_format_core::DemuxerDesc`]s.
//!
//! # The gap this module reports rather than works around
//!
//! [`vaco_format_core::DemuxerDesc::open`] takes exactly one already-opened
//! [`vaco_io::MediaSource`] — there is no sensible bytes-already-fetched
//! value to hand it for `rtsp://`, unlike an HLS playlist. [`RTSP_DEMUXER`]'s
//! registered `open` therefore always returns
//! [`vaco_core::Error::Unsupported`], and the real entry point is
//! [`RtspDemuxer::open`], which takes the URL and a
//! [`vaco_protocol_core::ProtocolEnv`] directly — a caller (the CLI, an
//! embedder) that recognises the `rtsp://` scheme must call it instead of
//! going through the generic protocol-then-demux pipeline, exactly the way
//! the reference's own `avformat_open_input` special-cases RTSP's URL
//! scheme before any generic protocol resolution happens (measured: `ffmpeg
//! -v debug -i rtsp://...` never logs a `[tcp @ ...]` open the way `-i
//! http://...` does — the RTSP demuxer opens its own transport internally).
//!
//! [`SDP_DEMUXER`]'s registered path is less broken: a standalone `.sdp`
//! file's bytes genuinely can be handed to it, but resolving the media
//! still needs UDP sockets this path has no [`vaco_protocol_core::ProtocolEnv`]
//! for — so, mirroring `vaco-demux-hls`'s `access: None` case exactly, the
//! registered path parses the SDP and reports streams with `Unsupported`
//! errors from `read_packet` rather than failing to open at all.
//! [`SdpDemuxer::open`] is the real entry point with network access.

use std::collections::HashMap;
use std::time::Duration;

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::ParserProvider;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, Stream};
use vaco_io::MediaSource;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_protocol_core::{ProtocolEnv, ProtocolRegistry};

use crate::options::RtspOptions;
use crate::session::RtspSession;
use crate::transport::udp::UdpReceivePair;
use crate::transport::{TransportMode, TransportSpec};

const RTP_CLOCK_DEFAULT: u32 = 90_000;

/// Per-track state: the depacketiser and wherever its RTP bytes come from.
struct Track {
    depack: Box<dyn vaco_format_rtp::Depacketizer>,
    stream_index: u32,
    /// `Some` for TCP-interleaved/HTTP-tunnelled tracks — the RFC 2326
    /// §10.12 channel number RTP data arrives on for this track (RTCP is
    /// always `rtp_channel + 1`, the convention every SETUP in this crate
    /// offers).
    rtp_channel: Option<u8>,
    udp: Option<UdpReceivePair>,
}

/// An RTSP client session driving zero or more depacketised media streams.
///
/// See the crate's top-level docs for the transport-security posture this
/// type's `setup`/`open` logic enforces, and this module's docs for why
/// [`RtspDemuxer::open`] (not the registered [`RTSP_DEMUXER`]) is the real
/// entry point.
pub struct RtspDemuxer {
    session: RtspSession,
    streams: Vec<Stream>,
    tracks: Vec<Track>,
    mode: TransportMode,
    budget: Budget,
}

impl std::fmt::Debug for RtspDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtspDemuxer")
            .field("streams", &self.streams.len())
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

fn resolve_control(base: &str, control: &str) -> String {
    if control == "*" {
        base.to_owned()
    } else if control.contains("://") {
        control.to_owned()
    } else if let Some(stripped) = base.strip_suffix('/') {
        format!("{stripped}/{control}")
    } else {
        format!("{base}/{control}")
    }
}

fn depacketizer_for(
    media: &vaco_format_rtp::MediaDescription,
) -> Option<(
    vaco_codec_core::CodecId,
    Box<dyn vaco_format_rtp::Depacketizer>,
    u32,
)> {
    let pt: u8 = media.formats.first()?.parse().ok()?;
    let rtpmap = media
        .attrs("rtpmap")
        .find_map(vaco_format_rtp::sdp::parse_rtpmap);
    if let Some(map) = &rtpmap
        && let Some((codec, factory)) = vaco_format_rtp::for_encoding(&map.encoding_name)
    {
        return Some((codec, factory(), map.clock_rate));
    }
    let row = vaco_format_rtp::static_payload(pt)?;
    let codec = row.codec?;
    let (_, factory) = vaco_format_rtp::for_encoding(row.name)?;
    Some((codec, factory(), row.clock_rate))
}

impl RtspDemuxer {
    /// Open an RTSP session end to end: connect, `DESCRIBE`, `SETUP` every
    /// track whose media type is allowed
    /// ([`RtspOptions::allowed_media_types`]), `PLAY`.
    ///
    /// `mode` picks the transport this crate offers in `SETUP` — see
    /// [`crate::transport`]'s module docs for the preference order a caller
    /// that does not care can use.
    ///
    /// # Errors
    /// Whatever [`RtspSession::connect`]/`describe`/`setup`/`play` report,
    /// including [`vaco_protocol_core::ProtocolError`]-derived
    /// [`Error::Unsupported`] when the whitelist refuses a needed scheme.
    pub fn open(
        url: &str,
        mode: TransportMode,
        opts: &RtspOptions,
        registry: &ProtocolRegistry,
        env: &ProtocolEnv<'_>,
        _parsers: &dyn ParserProvider,
    ) -> Result<Self> {
        opts.validate()?;
        let timeout = if opts.timeout > 0 {
            Some(Duration::from_micros(
                u64::try_from(opts.timeout).unwrap_or(0),
            ))
        } else {
            None
        };
        let mut session = RtspSession::connect(url, timeout, env)?;
        session.set_user_agent(opts.user_agent.clone());
        let sdp_text = session.describe()?;
        let sdp = vaco_format_rtp::sdp::parse(&sdp_text)?;

        let mut streams = Vec::new();
        let mut tracks = Vec::new();
        let mut next_channel: u8 = 0;

        for media in &sdp.media {
            let media_type = match media.media.as_str() {
                "video"
                    if opts
                        .allowed_media_types
                        .contains(crate::options::AllowedMediaTypes::VIDEO) =>
                {
                    MediaType::Video
                }
                "audio"
                    if opts
                        .allowed_media_types
                        .contains(crate::options::AllowedMediaTypes::AUDIO) =>
                {
                    MediaType::Audio
                }
                _ => continue,
            };
            let Some((codec, depack, clock_rate)) = depacketizer_for(media) else {
                continue;
            };
            let Some(control) = media.control().or_else(|| {
                sdp.attributes
                    .iter()
                    .find(|a| a.is("control"))
                    .and_then(|a| a.value.as_deref())
            }) else {
                continue;
            };
            let track_uri = resolve_control(session.base_uri(), control);

            let channels = (next_channel, next_channel.saturating_add(1));
            let offer_ports = (opts.min_port.try_into().unwrap_or(5000u16), 0u16);
            let offer = TransportSpec::offer(mode, offer_ports, channels);
            let setup = session.setup(&track_uri, &offer)?;

            let stream_index = u32::try_from(streams.len()).unwrap_or(0);
            let mut params = match media_type {
                MediaType::Video => CodecParameters::video(),
                _ => CodecParameters::audio(),
            };
            params = params.with_codec(codec);
            let mut stream = Stream::new(
                stream_index,
                media_type,
                Rational::new(1, i32::try_from(clock_rate).unwrap_or(90_000)),
            );
            stream.params = params;
            streams.push(stream);

            let udp = match setup.transport.mode() {
                TransportMode::UdpUnicast => {
                    let min = u16::try_from(opts.min_port).unwrap_or(5000);
                    let max = u16::try_from(opts.max_port).unwrap_or(65000);
                    let pair = crate::transport::udp::bind_local_pair(registry, env, min, max)?;
                    Some(pair)
                }
                TransportMode::UdpMulticast => {
                    let group = setup
                        .transport
                        .destination
                        .clone()
                        .ok_or(Error::InvalidData(
                            "multicast SETUP response named no destination",
                        ))?;
                    let (rtp_port, rtcp_port) = setup
                        .transport
                        .server_port
                        .ok_or(Error::InvalidData("multicast SETUP response named no port"))?;
                    Some(crate::transport::udp::join_multicast(
                        registry, env, &group, rtp_port, rtcp_port,
                    )?)
                }
                TransportMode::TcpInterleaved | TransportMode::Http => None,
            };

            let rtp_channel = setup.transport.interleaved.map(|(a, _)| a);
            next_channel = next_channel.saturating_add(2);

            tracks.push(Track {
                depack,
                stream_index,
                rtp_channel,
                udp,
            });
        }

        if !opts.initial_pause {
            session.play(None)?;
        }

        Ok(Self {
            session,
            streams,
            tracks,
            mode,
            budget: Budget::new(Limits::permissive()),
        })
    }

    /// Live pause/resume — see the crate's top-level docs for why this is
    /// an inherent method rather than on [`Demuxer`], which has no `pause`.
    ///
    /// # Errors
    /// As [`RtspSession::pause`]/`play`.
    pub fn pause(&mut self) -> Result<()> {
        self.session.pause()?;
        Ok(())
    }

    /// # Errors
    /// As [`RtspSession::play`].
    pub fn play(&mut self) -> Result<()> {
        self.session.play(None)?;
        Ok(())
    }

    fn read_interleaved(&mut self) -> Result<Packet> {
        loop {
            let (channel, data) = self.session.read_frame()?;
            let Some(track) = self
                .tracks
                .iter_mut()
                .find(|t| t.rtp_channel == Some(channel))
            else {
                continue; // RTCP channel, or a track we did not set up
            };
            let Ok(rtp) = vaco_format_rtp::RtpPacket::parse(&data) else {
                continue;
            };
            if let Some(bytes) =
                track
                    .depack
                    .push(rtp.header.marker, rtp.header.timestamp, rtp.payload)?
            {
                let mut pkt = Packet::from_slice(&mut self.budget, &bytes)?;
                pkt.stream_index = track.stream_index;
                return Ok(pkt);
            }
        }
    }

    fn read_udp(&mut self) -> Result<Packet> {
        let mut buf = vec![0u8; 65_535];
        loop {
            let mut any_ready = false;
            for track in &mut self.tracks {
                let Some(udp) = &mut track.udp else { continue };
                // A zero-length read or an error (typically a timed-out
                // socket, from the short per-open `timeout` this crate
                // sets) both mean "nothing from this track this round" —
                // move on to the next one.
                let Ok(n) = udp.rtp.read(&mut buf) else {
                    continue;
                };
                if n == 0 {
                    continue;
                }
                any_ready = true;
                let Ok(rtp) = vaco_format_rtp::RtpPacket::parse(buf.get(..n).unwrap_or(&[])) else {
                    continue;
                };
                if let Some(bytes) =
                    track
                        .depack
                        .push(rtp.header.marker, rtp.header.timestamp, rtp.payload)?
                {
                    let mut pkt = Packet::from_slice(&mut self.budget, &bytes)?;
                    pkt.stream_index = track.stream_index;
                    return Ok(pkt);
                }
            }
            let _ = any_ready;
        }
    }
}

impl Demuxer for RtspDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        match self.mode {
            TransportMode::TcpInterleaved | TransportMode::Http => self.read_interleaved(),
            TransportMode::UdpUnicast | TransportMode::UdpMulticast => self.read_udp(),
        }
    }

    fn seek(
        &mut self,
        _target: vaco_format_core::SeekTarget,
        _flags: vaco_format_core::SeekFlags,
    ) -> Result<()> {
        // A real implementation would reissue `PLAY` with a `Range:`
        // header (RFC 2326 §12.29) — deferred; see the crate docs.
        Err(Error::NotSeekable)
    }
}

/// Always [`Error::Unsupported`] — see this module's docs for why the
/// registered path cannot function for RTSP at all.
fn open_rtsp_desc(
    _source: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Err(Error::Unsupported(
        "the rtsp demuxer needs network access DemuxerDesc::open cannot carry; use RtspDemuxer::open",
    ))
}

fn probe_rtsp(_data: &ProbeData<'_>) -> ProbeScore {
    // RTSP is never probed from bytes — it is dispatched by URL scheme,
    // exactly like the reference (see this module's docs).
    ProbeScore::NONE
}

/// The registered descriptor. See this module's docs for
/// [`RtspDemuxer::open`] being the real entry point.
pub const RTSP_DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "rtsp",
    long_name: "RTSP input",
    extensions: &[],
    mime_types: &[],
    flags: FormatFlags::NOFILE.union(FormatFlags::TS_DISCONT),
    probe: probe_rtsp,
    open: open_rtsp_desc,
};

// ---------------------------------------------------------------- rtp

/// The bare `rtp` container format (`-f rtp`): raw RTP packets read one per
/// `MediaSource::read` call (matching `udp:`'s own one-read-one-datagram
/// framing, which is what this format is normally layered over), with no
/// SDP to resolve dynamic payload types from — only RFC 3551 static types
/// are named, exactly the shape `crate::payload`'s table supports.
///
/// A dynamic payload type (`96..=127`) produces a stream with `codec: None`
/// rather than failing outright — the packets are still delivered as opaque
/// data, which is what a caller piping raw RTP through `-c copy` needs, and
/// matches the "detection is strict, demuxing is lenient" rule (a
/// depacketiser cannot run without knowing the codec, so this format simply
/// does not depacketise in that case — [`RtpDemuxer::read_packet`] returns
/// the RTP payload verbatim, header stripped, RFC 3550 padding removed).
pub struct RtpDemuxer {
    source: Box<dyn MediaSource>,
    streams: Vec<Stream>,
    budget: Budget,
    seen_pt: HashMap<u8, u32>,
}

impl std::fmt::Debug for RtpDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtpDemuxer")
            .field("streams", &self.streams.len())
            .finish_non_exhaustive()
    }
}

impl RtpDemuxer {
    #[must_use]
    pub fn new(source: Box<dyn MediaSource>) -> Self {
        Self {
            source,
            streams: Vec::new(),
            budget: Budget::new(Limits::permissive()),
            seen_pt: HashMap::new(),
        }
    }
}

impl Demuxer for RtpDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let mut buf = vec![0u8; 65_535];
        let n = self.source.read(&mut buf)?;
        if n == 0 {
            return Err(Error::Eof);
        }
        let rtp = vaco_format_rtp::RtpPacket::parse(buf.get(..n).unwrap_or(&[]))?;
        let stream_index = *self
            .seen_pt
            .entry(rtp.header.payload_type)
            .or_insert_with(|| {
                let idx = u32::try_from(self.streams.len()).unwrap_or(0);
                let row = vaco_format_rtp::static_payload(rtp.header.payload_type);
                let media_type = MediaType::Data;
                let mut stream = Stream::new(
                    idx,
                    media_type,
                    Rational::new(
                        1,
                        i32::try_from(row.map_or(RTP_CLOCK_DEFAULT, |r| r.clock_rate))
                            .unwrap_or(90_000),
                    ),
                );
                if let Some(codec) = row.and_then(|r| r.codec) {
                    stream.params = CodecParameters::new(media_type).with_codec(codec);
                }
                self.streams.push(stream);
                idx
            });
        let mut pkt = Packet::from_slice(&mut self.budget, rtp.payload)?;
        pkt.stream_index = stream_index;
        Ok(pkt)
    }

    fn seek(
        &mut self,
        _target: vaco_format_core::SeekTarget,
        _flags: vaco_format_core::SeekFlags,
    ) -> Result<()> {
        Err(Error::NotSeekable)
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match DemuxerDesc::open's fn-pointer signature exactly"
)]
fn open_rtp_desc(
    source: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(RtpDemuxer::new(source)))
}

fn probe_rtp(data: &ProbeData<'_>) -> ProbeScore {
    // An RTP packet's first two bits are the version (always 2) — a weak
    // signal on its own, so this only ever contributes a low score,
    // exactly as `-f rtp` requires being named explicitly in practice.
    if data.get(0).is_some_and(|b| b >> 6 == 2) {
        ProbeScore::weak(1)
    } else {
        ProbeScore::NONE
    }
}

pub const RTP_DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "rtp",
    long_name: "RTP input",
    extensions: &[],
    mime_types: &["audio/rtp", "video/rtp"],
    flags: FormatFlags::NOFILE.union(FormatFlags::TS_DISCONT),
    probe: probe_rtp,
    open: open_rtp_desc,
};

// ---------------------------------------------------------------- sdp

/// The `sdp` container format (`-f sdp`): parses a standalone SDP
/// description and, given real network access, opens the UDP transports it
/// names directly — no RTSP negotiation at all, since the SDP already
/// states the ports.
pub struct SdpDemuxer {
    streams: Vec<Stream>,
    tracks: Vec<Track>,
    budget: Budget,
}

impl std::fmt::Debug for SdpDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdpDemuxer")
            .field("streams", &self.streams.len())
            .finish_non_exhaustive()
    }
}

impl SdpDemuxer {
    /// Parse `source`'s bytes as SDP and, if `registry`/`env` are supplied,
    /// open every `video`/`audio` `m=` line's UDP receive pair immediately
    /// (SDP names ports up front; there is no `SETUP` step to defer it to).
    ///
    /// `registry`/`env` are `None` for the registered [`SDP_DEMUXER`] path
    /// (no network access to give it — see this module's top-level docs);
    /// in that case streams are still reported, but
    /// [`Demuxer::read_packet`] returns [`Error::Unsupported`].
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a body that is not valid UTF-8 or not
    /// parseable SDP; otherwise whatever opening a named UDP transport
    /// reports.
    pub fn open(
        mut source: Box<dyn MediaSource>,
        registry: Option<(&ProtocolRegistry, &ProtocolEnv<'_>)>,
    ) -> Result<Self> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = source.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
            if bytes.len() > 8 * 1024 * 1024 {
                return Err(Error::LimitExceeded {
                    limit: "sdp body",
                    requested: bytes.len() as u64,
                    cap: 8 * 1024 * 1024,
                });
            }
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidData("SDP body is not valid UTF-8"))?;
        let sdp = vaco_format_rtp::sdp::parse(text)?;

        let default_conn = sdp.connection.as_ref().map(|c| c.address.clone());
        let mut streams = Vec::new();
        let mut tracks = Vec::new();
        for media in &sdp.media {
            let media_type = match media.media.as_str() {
                "video" => MediaType::Video,
                "audio" => MediaType::Audio,
                _ => continue,
            };
            let Some((codec, depack, clock_rate)) = depacketizer_for(media) else {
                continue;
            };
            let stream_index = u32::try_from(streams.len()).unwrap_or(0);
            let mut stream = Stream::new(
                stream_index,
                media_type,
                Rational::new(1, i32::try_from(clock_rate).unwrap_or(90_000)),
            );
            stream.params = match media_type {
                MediaType::Video => CodecParameters::video(),
                _ => CodecParameters::audio(),
            }
            .with_codec(codec);
            streams.push(stream);

            let udp = if let Some((registry, env)) = registry {
                let host = media
                    .connection
                    .as_ref()
                    .map(|c| c.address.clone())
                    .or_else(|| default_conn.clone())
                    .ok_or(Error::InvalidData("SDP media has no connection address"))?;
                Some(crate::transport::udp::join_multicast(
                    registry,
                    env,
                    &host,
                    media.port,
                    media.port.saturating_add(1),
                )?)
            } else {
                None
            };

            tracks.push(Track {
                depack,
                stream_index,
                rtp_channel: None,
                udp,
            });
        }

        Ok(Self {
            streams,
            tracks,
            budget: Budget::new(Limits::permissive()),
        })
    }
}

impl Demuxer for SdpDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let mut buf = vec![0u8; 65_535];
        loop {
            let mut had_socket = false;
            for track in &mut self.tracks {
                let Some(udp) = &mut track.udp else { continue };
                had_socket = true;
                let Ok(n) = udp.rtp.read(&mut buf) else {
                    continue;
                };
                if n == 0 {
                    continue;
                }
                let Ok(rtp) = vaco_format_rtp::RtpPacket::parse(buf.get(..n).unwrap_or(&[])) else {
                    continue;
                };
                if let Some(bytes) =
                    track
                        .depack
                        .push(rtp.header.marker, rtp.header.timestamp, rtp.payload)?
                {
                    let mut pkt = Packet::from_slice(&mut self.budget, &bytes)?;
                    pkt.stream_index = track.stream_index;
                    return Ok(pkt);
                }
            }
            if !had_socket {
                return Err(Error::Unsupported(
                    "this SdpDemuxer was opened without network access (SDP_DEMUXER's registered path); use SdpDemuxer::open with a ProtocolRegistry",
                ));
            }
        }
    }

    fn seek(
        &mut self,
        _target: vaco_format_core::SeekTarget,
        _flags: vaco_format_core::SeekFlags,
    ) -> Result<()> {
        Err(Error::NotSeekable)
    }
}

fn open_sdp_desc(
    source: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(SdpDemuxer::open(source, None)?))
}

fn probe_sdp(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(b"v=0") {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

pub const SDP_DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "sdp",
    long_name: "SDP",
    extensions: &["sdp"],
    mime_types: &["application/sdp"],
    flags: FormatFlags::NOFILE.union(FormatFlags::TS_DISCONT),
    probe: probe_sdp,
    open: open_sdp_desc,
};
