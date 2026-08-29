//! [`WhipMuxer`] — the registered `whip` muxer, and the extension point it
//! is built on.
//!
//! # The problem this solves (#619)
//!
//! Every `Muxer` in this tree is opened as `MuxerDesc::open: fn(Box<dyn
//! MediaSink>) -> Result<Box<dyn Muxer>>` — a pre-connected byte sink,
//! ready to be written to. WHIP cannot fit that shape: before a single byte
//! of media can go anywhere, a publisher has to `POST` an SDP offer over
//! HTTP, receive an SDP answer, run an ICE connectivity check, and complete
//! a DTLS handshake — four round trips of network negotiation with **no**
//! byte-oriented sink at any point in the middle of them. `MediaSink` (one
//! blocking `write(&[u8])`) has nowhere to represent "I am still
//! negotiating."
//!
//! # The extension point: `NOFILE` + `bind_url` + `init()`, not a new one
//!
//! `vaco-format-core` already has every piece this needs, just never
//! connected for this purpose. Measured directly against `ffmpeg 9.0.1`
//! (D17: `ffmpeg -f whip /this/is/not/a/url` never attempts to open that
//! string as a file at all — it reaches the muxer's own protocol dispatch
//! and rejects `file` by name, meaning the generic layer never touched it):
//! **WHIP is `AVFMT_NOFILE`**, the same declaration `vaco-format-core`
//! already models as [`vaco_format_core::FormatFlags::NOFILE`] and every
//! `null`/device-style muxer already uses.
//!
//! `Muxer::bind_url` (existing, added for `image2`'s `NEEDNUMBER` case) is
//! documented as "the real destination, once known" — a muxer whose true
//! unit of output cannot be expressed as one `Box<dyn MediaSink>` gets it
//! there instead. Nothing about that method requires the *file* framing
//! `image2` uses; it is exactly as good a channel for "the WHIP endpoint
//! URL, once known." And `Muxer::init` — called once, after every stream is
//! declared, before the header — is exactly the point a WHIP publisher
//! needs: the SDP offer needs every stream's codec, which is not known at
//! `bind_url` time (called before `add_stream`) but *is* known by `init`.
//!
//! So [`WhipMuxer`] does three small things, each already legal:
//!
//! 1. `open()` **ignores its `Box<dyn MediaSink>` entirely** — same as
//!    every `NOFILE` muxer already does; nothing reads it.
//! 2. `bind_url` stores the endpoint URL. No network I/O yet.
//! 3. `init()` performs the whole negotiation — SDP, ICE, DTLS, SRTP key
//!    derivation — and leaves the muxer holding a live, encrypted UDP
//!    socket by the time it returns. `write_header` is then a no-op, and
//!    `write_packet` just packetises and sends.
//!
//! **What changed in `vaco-format-core` to make this legal: nothing.**
//! `Muxer`'s trait signature, `MuxerDesc.open`'s fn-pointer type, and every
//! existing muxer are untouched — verified by `cargo check --workspace`
//! passing with this crate added, and every other muxer's own test suite
//! unmodified. The one change proposed alongside this crate is a doc-only
//! clarification on [`vaco_format_core::Muxer::bind_url`] naming this
//! pattern explicitly, so the next negotiating protocol (WHEP, an RTMP
//! variant that authenticates before streaming) does not have to
//! rediscover it by reading this file.
//!
//! The one place that *does* need a two-line change is `vaco-cli`'s
//! `open_output`: today it calls `bind_url` only for `NEEDNUMBER`, and
//! returns a `NOFILE` muxer without ever telling it its own destination
//! URL. Generalising that one `if` to also try `bind_url` for `NOFILE` (an
//! error other than "unsupported" is real, everything else is silently
//! fine — see that function) is what lets the CLI reach this muxer with no
//! WHIP-specific branch at all: every future `NOFILE` muxer that wants its
//! URL gets it the same way.
//!
//! # Real interop
//!
//! Verified end to end against `mediamtx` 1.20.1 (a real, independent WebRTC
//! media server, not a mock — see `docs/format/vaco-mux-whip.md` for the
//! transcript): a real DTLS handshake, a real ICE `MESSAGE-INTEGRITY`-
//! verified connectivity check, and real SRTP-protected H.264 RTP packets
//! accepted and recorded by the server.

use std::net::UdpSocket;
use std::time::Duration;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Result};
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_mux_rtp::{Packetizer, packetizer_for};
use vaco_packet::Packet;
use vaco_protocol_core::{ProtocolEnv, ProtocolRegistry};
use vaco_protocol_dtls::DtlsOptions;
use vaco_protocol_ice::IceCredentials;
use vaco_protocol_srtp::{SessionKeys, SrtpContext, derive_session_keys_aes128};

use crate::candidate;
use crate::sdp::{self, MediaOffer};

/// `ffmpeg -h muxer=whip`'s own `-pkt_size` default.
const DEFAULT_MTU: usize = 1200;

/// A host-candidate priority (RFC 8445 §5.1.2.1: `type_pref=126`,
/// `local_pref=65535`, `component=1`) — the same value `mediamtx` 1.20.1's
/// own candidates advertise, measured directly. This crate is not choosing
/// between several local candidates, so one fixed value is enough.
const ICE_PRIORITY: u32 = 2_130_706_431;

/// Bounds on the ICE connectivity check and the whole negotiation, so a
/// silent or malicious endpoint cannot hang a mux run forever.
const ICE_TIMEOUT: Duration = Duration::from_millis(800);
const ICE_RETRIES: u32 = 4;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Generous enough for a real handshake flight over a slow path, per-read
/// (not for the whole handshake) — see the call site.
const DTLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// A time-derived PRNG seed — this workspace declares no RNG crate (D10),
/// the same trick `vaco-mux-rtp::muxer::time_seed` uses.
fn time_seed() -> u64 {
    let now = vaco_time::Instant::now();
    let text = format!("{now:?}");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// One declared stream's RTP identity and packetiser.
struct StreamState {
    packetizer: Box<dyn Packetizer>,
    encoding_name: &'static str,
    clock_rate: u32,
    payload_type: u8,
    fmtp: String,
    ssrc: u32,
    sequence: u16,
    mid: u32,
    kind: &'static str,
    /// M6/B3: whether [`Muxer::check_bitstream`] has already answered for
    /// this stream. Without it, a track needing `h264_mp4toannexb` would
    /// answer `Insert` on every one of `decide_bitstream`'s re-asks — see
    /// [`Muxer::check_bitstream`]'s own doc and `vaco-mux-mp4`'s identical
    /// `bsf_decided` flag for the same reason.
    bsf_decided: bool,
}

/// What [`negotiate`] produces: a live, encrypted transport plus one
/// [`SrtpContext`] per declared stream, in the same order.
struct Session {
    socket: UdpSocket,
    contexts: Vec<SrtpContext>,
    /// The URL to `DELETE` on `write_trailer`, from the answer's `Location`
    /// header — `None` if the server did not send one (WHIP requires it,
    /// but this crate is lenient about tearing down: see `write_trailer`).
    delete_url: Option<String>,
}

/// The `whip` muxer. See the module docs for the design.
#[derive(Default)]
pub struct WhipMuxer {
    endpoint: Option<String>,
    streams: Vec<StreamState>,
    session: Option<Session>,
}

impl std::fmt::Debug for WhipMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhipMuxer")
            .field("endpoint", &self.endpoint)
            .field("streams", &self.streams.len())
            .field("negotiated", &self.session.is_some())
            .finish()
    }
}

/// RTP encoding name, clock rate (RFC 7587 §4.2 fixes Opus's at 48000
/// regardless of the stream's real sample rate — this is not read from
/// `params`) and `a=fmtp` value for one codec this muxer can carry.
///
/// # Errors
/// [`Error::Unsupported`] for any codec other than H.264 or Opus — the two
/// `ffmpeg -h muxer=whip` itself defaults to, and the two this crate has a
/// packetiser and a real-peer verification for.
fn codec_rtp_info(codec: CodecId, params: &CodecParameters) -> Result<(&'static str, u32, String)> {
    match codec {
        CodecId::H264 => Ok(("H264", 90_000, h264_fmtp(params))),
        CodecId::Opus => Ok((
            "opus",
            48_000,
            "minptime=10;useinbandfec=1".to_owned(),
        )),
        _ => Err(Error::Unsupported(
            "the whip muxer carries H.264 video and Opus audio only",
        )),
    }
}

/// `profile-level-id` (RFC 6184 §8.1): `profile_idc`, a zero constraint-flags
/// byte, `level_idc` — matches `ffmpeg 9.0.1`'s own real WHIP offers
/// byte-for-byte (measured: its `profile-level-id=64000c` is `[0x64, 0x00,
/// 0x0c]`, constraint byte zero), so this is not an approximation invented
/// for this crate, it is what the reference itself sends. `CodecParameters`
/// carries `profile.value`/`level.0` as plain codec-native integers (no raw
/// SPS bytes reach this crate — `vaco-mux-whip` is a `vaco-mux-*` crate and
/// D14.1 forbids it depending on `vaco-parse-h264` to get them any other
/// way), so the constraint-flags byte specifically is not recoverable here;
/// zero is what every encoder observed so far actually sends anyway.
fn h264_fmtp(params: &CodecParameters) -> String {
    let profile_idc = params.profile.map_or(0x42, |p| p.value.clamp(0, 0xFF) as u8);
    let level_idc = params.level.map_or(0x1f, |l| l.0.clamp(0, 0xFF) as u8);
    format!(
        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile_idc:02x}00{level_idc:02x}"
    )
}

impl Muxer for WhipMuxer {
    fn flags(&self) -> FormatFlags {
        // No byte-oriented sink at all (see the module docs), and RTP
        // timestamps do not run on the container's own monotonic clock the
        // way a file format's do.
        FormatFlags::NOFILE | FormatFlags::TS_DISCONT
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let Some(codec) = params.codec_id else {
            return Err(Error::Unsupported("the whip muxer needs a known codec id"));
        };
        let (encoding_name, clock_rate, fmtp) = codec_rtp_info(codec, params)?;
        let Some((_name, factory)) = packetizer_for(codec) else {
            return Err(Error::Unsupported(
                "no RTP packetiser is implemented for this codec",
            ));
        };
        let kind = match params.effective_media_type() {
            Some(MediaType::Video) => "video",
            Some(MediaType::Audio) => "audio",
            _ => return Err(Error::Unsupported("the whip muxer carries audio/video only")),
        };
        let index = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        let seed = time_seed() ^ u64::from(index).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let ssrc = u32::try_from(seed & 0xFFFF_FFFF).unwrap_or(1);
        let sequence = u16::try_from((seed >> 32) & 0xFFFF).unwrap_or(0);
        let payload_type = u8::try_from(96 + self.streams.len()).unwrap_or(127);
        self.streams.push(StreamState {
            packetizer: factory(),
            encoding_name,
            clock_rate,
            payload_type,
            fmtp,
            ssrc,
            sequence,
            mid: index,
            kind,
            bsf_decided: false,
        });
        Ok(index)
    }

    fn init(&mut self) -> Result<()> {
        let Some(endpoint) = self.endpoint.clone() else {
            return Err(Error::Unsupported(
                "the whip muxer was opened without a destination URL (bind_url was never called)",
            ));
        };
        if self.streams.is_empty() {
            return Err(Error::InvalidData("the whip muxer needs at least one stream"));
        }
        let session = negotiate(&endpoint, &self.streams)?;
        self.session = Some(session);
        Ok(())
    }

    fn write_header(&mut self) -> Result<()> {
        if self.session.is_none() {
            return Err(Error::Unsupported(
                "write_header called before init negotiated a session",
            ));
        }
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let Some(session) = self.session.as_mut() else {
            return Err(Error::Unsupported("write_packet called before init"));
        };
        let idx = usize::try_from(packet.stream_index).unwrap_or(usize::MAX);
        let Some(stream) = self.streams.get_mut(idx) else {
            return Err(Error::InvalidData("packet names an undeclared stream"));
        };
        let Some(ctx) = session.contexts.get_mut(idx) else {
            return Err(Error::InvalidData("packet names an undeclared stream"));
        };
        let Some(pts) = packet.pts.ticks() else {
            return Err(Error::InvalidData(
                "RTP packets need a pts to derive an RTP timestamp",
            ));
        };
        let rtp_timestamp = u32::try_from(pts & 0xFFFF_FFFF).unwrap_or(0);
        let payloads = stream.packetizer.packetize(packet.payload(), DEFAULT_MTU);
        let last = payloads.len().saturating_sub(1);
        for (i, payload) in payloads.iter().enumerate() {
            let header = vaco_format_rtp::RtpHeader {
                version: vaco_format_rtp::RTP_VERSION,
                padding: false,
                extension: false,
                marker: i == last,
                payload_type: stream.payload_type,
                sequence_number: stream.sequence,
                timestamp: rtp_timestamp,
                ssrc: stream.ssrc,
                csrc_count: 0,
            };
            let seq = stream.sequence;
            stream.sequence = stream.sequence.wrapping_add(1);
            let plaintext = vaco_format_rtp::rtp::build_basic(&header, payload);
            let protected = ctx.protect(seq, 12, &plaintext);
            session.socket.send(&protected).map_err(Error::Io)?;
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        // WHIP teardown (`DELETE` on the session `Location`) is best-effort:
        // the media was already fully sent by the time this runs, and a
        // server that has gone away, or a network blip on the way out,
        // should not turn a successful publish into a failed mux run —
        // matching `ffmpeg`'s own observed tolerance (a failed teardown
        // DELETE logs a warning, not a hard error).
        if let Some(session) = self.session.take()
            && let Some(url) = session.delete_url
        {
            let _ = crate::http::request("DELETE", &url, None, None, HTTP_TIMEOUT);
        }
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<vaco_core::Rational> {
        let idx = usize::try_from(stream_index).ok()?;
        let clock_rate = self.streams.get(idx)?.clock_rate;
        Some(vaco_core::Rational::new(1, i32::try_from(clock_rate).unwrap_or(90_000)))
    }

    fn bind_url(&mut self, url: &str) -> Result<()> {
        self.endpoint = Some(url.to_owned());
        Ok(())
    }

    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        packet: &Packet,
    ) -> Result<vaco_format_core::mux::BitstreamAction> {
        let idx = usize::try_from(packet.stream_index).ok();
        if idx
            .and_then(|i| self.streams.get(i))
            .is_some_and(|s| s.bsf_decided)
        {
            return Ok(vaco_format_core::mux::BitstreamAction::Keep);
        }
        if let Some(s) = idx.and_then(|i| self.streams.get_mut(i)) {
            s.bsf_decided = true;
        }
        // RFC 6184's RTP packetisation (and this crate's own `H264Packetizer`,
        // `vaco-mux-rtp::h264`) needs Annex-B NAL units with start codes, not
        // the length-prefixed (`AVCC`) form a container like MP4 stores.
        // Measured the hard way: a stream-copied `.mp4` H264 track produced
        // real, successfully-encrypted SRTP packets that `mediamtx` silently
        // dropped, because every "packet" the packetiser saw was a 4-byte
        // length prefix it could not find a start code in — no NAL units,
        // no RTP payloads, ever, while `MuxReport` still counted the input
        // bytes as written. `vaco-mux-mp4`'s own `check_bitstream` is the
        // precedent for the `bsf_decided` guard above, needed for the same
        // reason: nothing about `params`/`packet` changes between
        // `decide_bitstream`'s re-asks, so a stateless answer would request
        // the same filter forever.
        if params.codec_id == Some(CodecId::H264) {
            let already_annexb = params
                .video
                .as_ref()
                .and_then(|v| v.nal_length_size)
                .map_or_else(|| looks_like_annexb(packet.payload()), |n| n == 0);
            if !already_annexb {
                return Ok(vaco_format_core::mux::BitstreamAction::Insert {
                    name: "h264_mp4toannexb",
                });
            }
        }
        Ok(vaco_format_core::mux::BitstreamAction::Keep)
    }
}

/// Whether `payload` starts with an Annex B start code (`00 00 01` or
/// `00 00 00 01`) — the fallback sniff for a stream whose
/// `CodecParameters::video::nal_length_size` was never populated (a raw
/// `.264` elementary-stream demuxer, for instance, which has no
/// container-level answer to that question but is Annex-B by construction).
fn looks_like_annexb(payload: &[u8]) -> bool {
    matches!(payload.get(..3), Some([0, 0, 1])) || matches!(payload.get(..4), Some([0, 0, 0, 1]))
}

/// A [`std::io::Read`]/[`std::io::Write`] transport over one UDP socket that
/// answers a peer's own STUN Binding Requests instead of handing them to
/// whatever is driving a DTLS handshake through it.
///
/// # Why this exists
///
/// Measured against a real peer (`mediamtx` 1.20.1, D17), not assumed: it
/// runs a full ICE agent rather than ICE-lite, and keeps sending Binding
/// Requests to the publisher throughout the DTLS handshake window — signed
/// with *our* local password, `USERNAME <our-ufrag>:<its-ufrag>`, RFC
/// 8445's ordinary shape for a peer's own connectivity check. A plain
/// transport (`vaco_protocol_dtls::transport::UdpTransport`) hands every
/// datagram straight to OpenSSL, which cannot tell those apart from DTLS
/// records; without an answer, `mediamtx`'s own ICE state never confirms,
/// and the handshake this crate is driving over the *same* socket never
/// receives anything back — silence, not a rejection, which read as "the
/// handshake failed" with no clue why until this was traced with a
/// byte-logging instrumented client against the real server (see
/// `docs/format/vaco-mux-whip.md`).
///
/// `read` demultiplexes with [`vaco_protocol_ice::looks_like_stun`] (RFC
/// 7983 §7): a STUN-shaped datagram is answered in place via
/// [`vaco_protocol_ice::respond_to_binding_request`] (which itself refuses
/// to answer anything not authenticated with `local_pwd` — see that
/// function's own security note) and the loop continues; anything else is
/// handed to the caller as the bytes of one DTLS record.
struct DemuxTransport {
    socket: UdpSocket,
    local_pwd: String,
}

impl DemuxTransport {
    /// Hand back the underlying socket once the handshake using this
    /// transport is done — the same shape `UdpTransport::socket` offers,
    /// needed for the same reason: SRTP traffic afterward is sent as plain
    /// UDP datagrams, not through OpenSSL, on this same 5-tuple.
    const fn socket(&self) -> &UdpSocket {
        &self.socket
    }
}

impl std::io::Read for DemuxTransport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = self.socket.recv(buf)?;
            let Some(datagram) = buf.get(..n) else {
                return Ok(n);
            };
            if vaco_protocol_ice::looks_like_stun(datagram) {
                if let Some(response) =
                    vaco_protocol_ice::respond_to_binding_request(datagram, &self.local_pwd)
                {
                    // Best-effort: a failed reply here just means the peer
                    // retries, the same as if this datagram had been lost
                    // on the wire.
                    let _ = self.socket.send(&response);
                }
                continue;
            }
            return Ok(n);
        }
    }
}

impl std::io::Write for DemuxTransport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.socket.send(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The whole negotiation: SDP offer/answer, ICE, DTLS, SRTP key derivation.
/// Blocking — every step is a network round trip or a handshake, and there
/// is nothing useful to do concurrently with any of them for a single
/// publish.
///
/// # Errors
/// Whatever HTTP, SDP parsing, ICE or DTLS refuses with; every failure is a
/// real one (a malformed answer, a failed connectivity check, a fingerprint
/// mismatch), never silently downgraded.
fn negotiate(endpoint: &str, streams: &[StreamState]) -> Result<Session> {
    let seed = time_seed();
    let local_ufrag = vaco_protocol_ice::ice_credential(seed, 8);
    let local_pwd = vaco_protocol_ice::ice_credential(seed.wrapping_add(1), 24);

    let (cert, pkey) = vaco_protocol_dtls::cert::generate_self_signed()?;
    let cert_der = cert
        .to_der()
        .map_err(|_| Error::InvalidData("could not encode the local DTLS certificate"))?;
    let local_fingerprint = colonify(
        &vaco_hash::HashAlgo::Sha256
            .digest_hex(&cert_der)
            .unwrap_or_default(),
    );
    let cert_pem = String::from_utf8(
        cert.to_pem()
            .map_err(|_| Error::InvalidData("could not PEM-encode the local DTLS certificate"))?,
    )
    .map_err(|_| Error::InvalidData("local DTLS certificate PEM was not UTF-8"))?;
    let key_pem = String::from_utf8(
        pkey.private_key_to_pem_pkcs8()
            .map_err(|_| Error::InvalidData("could not PEM-encode the local DTLS key"))?,
    )
    .map_err(|_| Error::InvalidData("local DTLS key PEM was not UTF-8"))?;

    let media: Vec<MediaOffer> = streams
        .iter()
        .map(|s| MediaOffer {
            kind: s.kind,
            payload_type: s.payload_type,
            encoding_name: s.encoding_name,
            clock_rate: s.clock_rate,
            fmtp: s.fmtp.clone(),
            ssrc: s.ssrc,
            mid: s.mid,
        })
        .collect();
    let offer = sdp::build_offer(&local_ufrag, &local_pwd, &local_fingerprint, &media);

    let response = crate::http::request(
        "POST",
        endpoint,
        Some("application/sdp"),
        Some(offer.as_bytes()),
        HTTP_TIMEOUT,
    )?;
    if response.status != 201 {
        return Err(Error::InvalidData(
            "WHIP endpoint did not answer 201 Created",
        ));
    }
    let delete_url = response
        .header("location")
        .map(|loc| vaco_protocol_http::url::resolve_location(endpoint, loc));
    let answer_body = String::from_utf8(response.body)
        .map_err(|_| Error::InvalidData("WHIP answer body was not UTF-8 SDP"))?;
    let answered = sdp::parse_answer(&answer_body)?;
    let Some(first) = answered.first() else {
        return Err(Error::InvalidData("WHIP answer named no media"));
    };
    if !first.setup.eq_ignore_ascii_case("passive") {
        return Err(Error::Unsupported(
            "WHIP answer did not select DTLS setup:passive for our setup:active offer",
        ));
    }

    let ice_creds = IceCredentials {
        local_ufrag: local_ufrag.clone(),
        remote_ufrag: first.ice_ufrag.clone(),
        remote_pwd: first.ice_pwd.clone(),
    };
    let candidates = candidate::usable_candidates(first.candidates.iter().map(String::as_str));
    if candidates.is_empty() {
        return Err(Error::Unsupported(
            "WHIP answer offered no usable (UDP, component 1) ICE candidate",
        ));
    }

    let registry = ProtocolRegistry::new();
    let cancel = vaco_io::CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel);

    let mut socket = None;
    for c in &candidates {
        let hp = vaco_protocol_socket::url::HostPort {
            host: c.address.clone(),
            port: c.port,
        };
        let Ok(sock) = vaco_protocol_dtls::connect::connect_udp(&hp, Some(ICE_TIMEOUT), &env)
        else {
            continue;
        };
        if vaco_protocol_ice::connectivity_check(
            &sock,
            &ice_creds,
            ICE_PRIORITY,
            ICE_TIMEOUT,
            ICE_RETRIES,
        )
        .is_ok()
        {
            socket = Some(sock);
            break;
        }
    }
    let Some(socket) = socket else {
        return Err(Error::InvalidData(
            "ICE connectivity check failed against every candidate the WHIP answer offered",
        ));
    };

    // `connectivity_check` left a short read timeout on this socket for its
    // own retry loop; `vaco-protocol-dtls`'s handshake transport is a plain
    // blocking `Read`/`Write` with no retry of its own (see that crate's
    // `transport` module docs), so a timeout shorter than one real
    // handshake flight turns into a hard `WouldBlock` failure rather than a
    // wait. A generous fixed bound (not `None`/forever) still protects
    // against a peer that never answers at all.
    socket
        .set_read_timeout(Some(DTLS_HANDSHAKE_TIMEOUT))
        .map_err(Error::Io)?;
    let dtls_opts = DtlsOptions {
        use_srtp: true,
        cert_pem,
        key_pem,
        ..DtlsOptions::default()
    };
    // Not a plain `UdpTransport`: measured against `mediamtx` (D17), it runs
    // a full ICE agent and keeps sending us its own STUN Binding Requests
    // throughout the handshake window (see `DemuxTransport`'s own doc for
    // the full story). A transport that just handed those to OpenSSL as if
    // they were DTLS records would starve the handshake of ever seeing a
    // real response — silence, not an error, which is exactly what a first
    // attempt without this measured and fixed.
    let demux = DemuxTransport {
        socket,
        local_pwd: local_pwd.clone(),
    };
    let stream =
        vaco_protocol_dtls::connect::handshake_over(demux, &dtls_opts, None, None, None)?;

    let peer_cert = stream
        .ssl()
        .peer_certificate()
        .ok_or(Error::InvalidData("DTLS peer presented no certificate"))?;
    let peer_der = peer_cert
        .to_der()
        .map_err(|_| Error::InvalidData("could not encode the peer's DTLS certificate"))?;
    let peer_fingerprint = vaco_hash::HashAlgo::Sha256
        .digest_hex(&peer_der)
        .unwrap_or_default();
    // The real security check: the certificate presented over the wire must
    // be the one the SDP answer promised. Never weakened — a mismatch is a
    // hard failure, exactly like a rejected TLS certificate elsewhere in
    // this workspace.
    if peer_fingerprint != first.fingerprint {
        return Err(Error::InvalidData(
            "DTLS peer certificate fingerprint does not match the WHIP answer's a=fingerprint",
        ));
    }

    let mut keying_material = [0u8; 60];
    vaco_protocol_dtls::connect::export_srtp_keying_material_from(&stream, &mut keying_material)?;
    let client_key: [u8; 16] = keying_material
        .get(0..16)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::InvalidData("SRTP keying material was too short"))?;
    let client_salt: [u8; 14] = keying_material
        .get(32..46)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::InvalidData("SRTP keying material was too short"))?;
    let keys: SessionKeys = derive_session_keys_aes128(&client_key, &client_salt);

    let raw_socket = stream
        .get_ref()
        .socket()
        .try_clone()
        .map_err(Error::Io)?;
    drop(stream);

    let contexts = streams
        .iter()
        .map(|s| SrtpContext::new(keys.clone(), s.ssrc))
        .collect();

    Ok(Session {
        socket: raw_socket,
        contexts,
        delete_url,
    })
}

/// `abcdef...` -> `AB:CD:EF:...`, the `a=fingerprint` wire spelling.
fn colonify(hex: &str) -> String {
    let upper = hex.to_ascii_uppercase();
    let mut out = String::new();
    for (i, ch) in upper.chars().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push(':');
        }
        out.push(ch);
    }
    out
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match MuxerDesc::open's fn-pointer signature exactly"
)]
fn open_whip_muxer(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    // The sink is deliberately unused: see the module docs. Every existing
    // `NOFILE` muxer already does this (`vaco-mux-utility`'s `null`, for
    // instance) — this is not a new pattern, only a new reason to use it.
    Ok(Box::new(WhipMuxer::default()))
}

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "whip",
    long_name: "WHIP (WebRTC-HTTP Ingestion Protocol) output",
    extensions: &[],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Opus),
    open: open_whip_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn colonify_matches_the_sdp_spelling() {
        assert_eq!(colonify("abcd"), "AB:CD");
        assert_eq!(colonify(""), "");
    }

    #[test]
    fn flags_declare_nofile() {
        let m = WhipMuxer::default();
        assert!(m.flags().contains(FormatFlags::NOFILE));
    }

    #[test]
    fn bind_url_stores_the_endpoint() {
        let mut m = WhipMuxer::default();
        m.bind_url("http://example.com/whip").unwrap();
        assert_eq!(m.endpoint.as_deref(), Some("http://example.com/whip"));
    }

    #[test]
    fn init_without_bind_url_is_a_clean_error() {
        let mut m = WhipMuxer::default();
        m.add_stream(&CodecParameters::video().with_codec(CodecId::H264))
            .unwrap();
        assert!(m.init().is_err());
    }

    #[test]
    fn add_stream_rejects_an_unsupported_codec() {
        let mut m = WhipMuxer::default();
        let params = CodecParameters::video().with_codec(CodecId::Vp9);
        assert!(m.add_stream(&params).is_err());
    }

    #[test]
    fn h264_fmtp_matches_the_real_ffmpeg_shape() {
        let mut params = CodecParameters::video().with_codec(CodecId::H264);
        params.profile = Some(vaco_codec_core::Profile {
            value: 0x64,
            name: "High",
        });
        params.level = Some(vaco_codec_core::Level(12));
        let fmtp = h264_fmtp(&params);
        assert_eq!(
            fmtp,
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=64000c"
        );
    }

    #[test]
    fn write_packet_before_init_is_a_clean_error() {
        let mut m = WhipMuxer::default();
        m.add_stream(&CodecParameters::video().with_codec(CodecId::H264))
            .unwrap();
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, b"nal").unwrap();
        pkt.pts = vaco_core::Timestamp::new(0);
        assert!(m.write_packet(&pkt).is_err());
    }
}
