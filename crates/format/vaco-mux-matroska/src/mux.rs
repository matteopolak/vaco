//! [`MatroskaMuxer`]: the shared implementation behind the `matroska` and
//! `webm` registrations.
//!
//! # What is deliberately not here
//!
//! [`vaco_format_core::Muxer`] gives a muxer no channel for file-level
//! metadata or chapters — `add_stream` takes only [`CodecParameters`], and
//! nothing else in the trait carries a title, a tag list, or a chapter table.
//! `Tags`, `Chapters` and `Attachments` are therefore not written: there is
//! nothing for this crate to write from. `Cues` needs no such channel — every
//! field it carries comes from the packets themselves — so it is implemented
//! in full.
//!
//! `SeekHead` is left out too, on purpose rather than by trait limitation: it
//! is RFC 9559's optional fast-locate index, and every reader has to fall
//! back to a linear scan for `Info`/`Tracks` when it is absent or wrong —
//! this workspace's own `vaco-demux-matroska` does. Building it correctly
//! needs either a second seek-patch pass or fixed-width placeholder
//! arithmetic for no behavioural gain over the `Cues`-only index this crate
//! already writes, so it is deferred rather than added for its own sake.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_demux_matroska::ebml::schema as el;
use vaco_format_core::options::{FFlags, FormatOptions};
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_format_ebml::{
    id_bytes, patch_known_size, vint_unknown, write_element, write_float, write_int, write_string,
    write_uint,
};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

use crate::block;
use crate::codec;

/// How long, in milliseconds, a `Cluster` is allowed to span before this
/// muxer starts a new one regardless of keyframes. `ffmpeg`'s own
/// `cluster_time_limit` defaults to "no limit", but an audio-only file still
/// needs *some* cap or the whole file becomes one `Cluster` — five seconds is
/// a conservative, commonly used value and is not claimed to match the
/// reference's own unbounded default byte-for-byte.
const MAX_CLUSTER_MS: i64 = 5000;

/// A container profile: what differs between `matroska` and `webm` beyond
/// the element tree, which both share in full.
#[derive(Debug, Clone, Copy)]
pub struct Variant {
    doc_type: &'static str,
    is_webm: bool,
}

/// The `matroska` `DocType`.
pub const MATROSKA: Variant = Variant {
    doc_type: "matroska",
    is_webm: false,
};

/// The `webm` `DocType`.
pub const WEBM: Variant = Variant {
    doc_type: "webm",
    is_webm: true,
};

/// The registry descriptor for `matroska`.
pub const MUXER_MATROSKA: MuxerDesc = MuxerDesc {
    name: "matroska",
    long_name: "Matroska",
    extensions: &["mkv"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_matroska,
};

/// The registry descriptor for `webm`.
pub const MUXER_WEBM: MuxerDesc = MuxerDesc {
    name: "webm",
    long_name: "WebM",
    extensions: &["webm"],
    default_video: Some(CodecId::Vp9),
    default_audio: Some(CodecId::Opus),
    open: open_webm,
};

fn open_matroska(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MatroskaMuxer::new(
        MATROSKA,
        sink,
        &FormatOptions::default(),
    )?))
}

fn open_webm(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MatroskaMuxer::new(
        WEBM,
        sink,
        &FormatOptions::default(),
    )?))
}

/// One declared track's write-side state.
#[derive(Debug, Clone)]
struct TrackOut {
    number: u64,
    is_video: bool,
    codec_id: &'static str,
    default_duration_ns: Option<u64>,
    width: u32,
    height: u32,
    sample_rate: f64,
    channels: u64,
    bit_depth: Option<u64>,
    extradata: Option<Vec<u8>>,
    /// Whether the stream may reorder frames (RFC 9559's `Block` needs a
    /// `ReferenceBlock` for such a frame; `SimpleBlock` cannot carry one).
    reorders: bool,
    /// The most recent block's timestamp on this track, in output ticks —
    /// what a reordered frame's `ReferenceBlock` delta is computed against.
    prev_ts: Option<i64>,
}

/// One buffered `Cluster`, built fully in memory before it is written.
///
/// Measured against `ffmpeg 8.1` (see the crate's module docs): a `Cluster`'s
/// size field is always the shortest VINT that holds it, on both a seekable
/// and a non-seekable sink, which is only possible if the whole cluster is
/// assembled before its header is written. This mirrors that.
#[derive(Debug)]
struct Cluster {
    start_ticks: i64,
    body: Vec<u8>,
    /// Absolute byte offset, in the sink, of this cluster's own element ID —
    /// recorded when the cluster is flushed, for `Cues`.
    byte_pos: u64,
    /// Whether a video keyframe opened this cluster, which is what earns it
    /// a `CuePoint`.
    keyframe_opened: bool,
}

/// One `Cues` entry.
#[derive(Debug)]
struct CueEntry {
    time_ticks: u64,
    track: u64,
    /// Byte offset of the cluster's ID, relative to the first byte of the
    /// `Segment`'s data (RFC 9559 section 11.8's `CueClusterPosition`).
    cluster_pos_rel: u64,
}

/// The Matroska/`WebM` muxer.
#[derive(Debug)]
pub struct MatroskaMuxer {
    variant: Variant,
    out: IoWriter,
    tracks: Vec<TrackOut>,
    header_written: bool,
    trailer_written: bool,
    /// Absolute byte offset of `Segment`'s eight-octet size field.
    segment_size_at: u64,
    /// Absolute byte offset of the first octet of `Segment`'s data — what
    /// every `Cues` position is relative to.
    segment_data_start: u64,
    cluster: Option<Cluster>,
    cues: Vec<CueEntry>,
    /// `DateUTC`, nanoseconds since the Matroska epoch (2001-01-01), or
    /// `None` to omit the element — the `+bitexact` default, and the only
    /// path that does not touch a clock (see the crate's module docs).
    date_utc_ns: Option<i64>,
    /// `webm` starts at `DocTypeVersion` 2 and is bumped to 4 the moment a
    /// track needs a version-4 feature; `matroska` is always 4 (both
    /// measured against `ffmpeg 8.1`, see `docs/format/vaco-mux-matroska.md`).
    needs_doctype_v4: bool,
    max_end_ticks: u64,
    /// How long, in ticks, a `Cluster` may span before a new one starts
    /// regardless of keyframes. Configurable so [`crate::webm_chunk`] can
    /// make a `Cluster` boundary and a chunk boundary the same thing.
    max_cluster_ms: i64,
    /// Absolute byte offset of every `Cluster`'s own element ID, in the order
    /// they were opened — [`crate::webm_chunk::WebmChunkMuxer`] reads this to
    /// know where each chunk begins in the single stream this trait can
    /// write to (see that module's docs for why it needs to).
    cluster_starts: Vec<u64>,
}

/// Matroska's epoch (2001-01-01T00:00:00 UTC) as Unix nanoseconds.
const MATROSKA_EPOCH_UNIX_NS: i64 = 978_307_200_000_000_000;

impl MatroskaMuxer {
    /// A muxer over `sink` for the given container `variant`.
    ///
    /// # Errors
    ///
    /// Propagates buffer allocation failure from [`IoWriter::new`].
    pub fn new(variant: Variant, sink: Box<dyn MediaSink>, opts: &FormatOptions) -> Result<Self> {
        let bitexact = opts.fflags.contains(FFlags::BITEXACT);
        let date_utc_ns = if bitexact || opts.start_time_realtime == i64::MIN {
            None
        } else {
            // `start_time_realtime` is Unix microseconds (see
            // `vaco-format-core`'s own use of it); Matroska wants nanoseconds
            // since 2001-01-01.
            opts.start_time_realtime
                .checked_mul(1000)
                .and_then(|unix_ns| unix_ns.checked_sub(MATROSKA_EPOCH_UNIX_NS))
        };
        Ok(Self {
            variant,
            out: IoWriter::new(sink, &IoOptions::default())?,
            tracks: Vec::new(),
            header_written: false,
            trailer_written: false,
            segment_size_at: 0,
            segment_data_start: 0,
            cluster: None,
            cues: Vec::new(),
            date_utc_ns,
            needs_doctype_v4: !variant.is_webm,
            max_end_ticks: 0,
            max_cluster_ms: MAX_CLUSTER_MS,
            cluster_starts: Vec::new(),
        })
    }

    /// Override the cluster time span cap (see [`MatroskaMuxer::max_cluster_ms`]'s
    /// field docs). Must be called before [`MatroskaMuxer::write_header`].
    pub const fn set_max_cluster_ms(&mut self, ms: i64) {
        self.max_cluster_ms = ms;
    }

    /// Absolute byte offset of every `Cluster` opened so far, in order.
    #[must_use]
    pub fn cluster_starts(&self) -> &[u64] {
        &self.cluster_starts
    }

    /// A muxer for the `matroska` `DocType`.
    ///
    /// # Errors
    /// As [`MatroskaMuxer::new`].
    pub fn new_matroska(sink: Box<dyn MediaSink>, opts: &FormatOptions) -> Result<Self> {
        Self::new(MATROSKA, sink, opts)
    }

    /// A muxer for the `webm` `DocType`.
    ///
    /// # Errors
    /// As [`MatroskaMuxer::new`].
    pub fn new_webm(sink: Box<dyn MediaSink>, opts: &FormatOptions) -> Result<Self> {
        Self::new(WEBM, sink, opts)
    }

    /// Bytes written to the sink so far.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.out.pos()
    }

    fn ebml_header_bytes(&self) -> Vec<u8> {
        let doc_version: u64 = if self.needs_doctype_v4 { 4 } else { 2 };
        let mut body = write_uint(el::EBMLVERSION, 1);
        body.extend_from_slice(&write_uint(el::EBMLREADVERSION, 1));
        body.extend_from_slice(&write_uint(el::EBMLMAXIDLENGTH, 4));
        body.extend_from_slice(&write_uint(el::EBMLMAXSIZELENGTH, 8));
        body.extend_from_slice(&write_string(el::DOCTYPE, self.variant.doc_type));
        body.extend_from_slice(&write_uint(el::DOCTYPEVERSION, doc_version));
        body.extend_from_slice(&write_uint(el::DOCTYPEREADVERSION, 2));
        write_element(el::EBML, &body)
    }

    fn info_bytes(&self) -> Vec<u8> {
        let mut body = write_uint(el::TIMESTAMPSCALE, 1_000_000);
        body.extend_from_slice(&write_string(el::MUXINGAPP, "vaco-mux-matroska"));
        body.extend_from_slice(&write_string(el::WRITINGAPP, "vaco-mux-matroska"));
        if let Some(ns) = self.date_utc_ns {
            body.extend_from_slice(&write_int(el::DATEUTC, ns));
        }
        if self.max_end_ticks > 0 {
            // Duration is in `TimestampScale` units (milliseconds, at the
            // 1_000_000 ns/tick scale fixed above).
            body.extend_from_slice(&write_float(el::DURATION, self.max_end_ticks as f64));
        }
        write_element(el::INFO, &body)
    }

    fn track_entry_bytes(t: &TrackOut) -> Vec<u8> {
        let mut body = write_uint(el::TRACKNUMBER, t.number);
        body.extend_from_slice(&write_uint(el::TRACKUID, t.number));
        body.extend_from_slice(&write_uint(el::TRACKTYPE, if t.is_video { 1 } else { 2 }));
        // Measured against `ffmpeg 8.1`: `FlagLacing` is written explicitly,
        // and is always 0 — this crate never emits a laced block by default
        // (see `crate::block`'s module docs).
        body.extend_from_slice(&write_uint(el::FLAGLACING, 0));
        body.extend_from_slice(&write_string(el::LANGUAGE, "und"));
        body.extend_from_slice(&write_string(el::CODECID, t.codec_id));
        if let Some(dur) = t.default_duration_ns {
            body.extend_from_slice(&write_uint(el::DEFAULTDURATION, dur));
        }
        if let Some(bytes) = t.extradata.as_ref().filter(|d| !d.is_empty()) {
            body.extend_from_slice(&vaco_format_ebml::binary(el::CODECPRIVATE, bytes));
        }
        if t.is_video {
            let mut video = write_uint(el::PIXELWIDTH, u64::from(t.width));
            video.extend_from_slice(&write_uint(el::PIXELHEIGHT, u64::from(t.height)));
            body.extend_from_slice(&write_element(el::VIDEO, &video));
        } else {
            let mut audio = write_float(el::SAMPLINGFREQUENCY, t.sample_rate);
            audio.extend_from_slice(&write_uint(el::CHANNELS, t.channels.max(1)));
            if let Some(bits) = t.bit_depth {
                audio.extend_from_slice(&write_uint(el::BITDEPTH, bits));
            }
            body.extend_from_slice(&write_element(el::AUDIO, &audio));
        }
        write_element(el::TRACKENTRY, &body)
    }

    fn tracks_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for t in &self.tracks {
            body.extend_from_slice(&Self::track_entry_bytes(t));
        }
        write_element(el::TRACKS, &body)
    }

    /// Flush the in-progress `Cluster`, if any, writing it as one complete
    /// element and recording its byte position for any keyframe it opened.
    fn flush_cluster(&mut self) -> Result<()> {
        let Some(cluster) = self.cluster.take() else {
            return Ok(());
        };
        let mut body = write_uint(
            el::TIMESTAMP,
            u64::try_from(cluster.start_ticks).unwrap_or(0),
        );
        body.extend_from_slice(&cluster.body);
        let bytes = write_element(el::CLUSTER, &body);
        // `byte_pos` was recorded before any of this cluster's bytes were
        // written, so it is exactly where `out.pos()` is now, before this
        // write — nothing to recompute.
        self.out.write(&bytes)?;
        if cluster.keyframe_opened {
            // The keyframe that opened the cluster is always its first
            // block, at the cluster's own start timestamp.
            for cue_track in self.tracks.iter().filter(|t| t.is_video) {
                self.cues.push(CueEntry {
                    time_ticks: u64::try_from(cluster.start_ticks).unwrap_or(0),
                    track: cue_track.number,
                    cluster_pos_rel: cluster.byte_pos.saturating_sub(self.segment_data_start),
                });
            }
        }
        Ok(())
    }

    fn cues_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for c in &self.cues {
            let mut positions = write_uint(el::CUETRACK, c.track);
            positions.extend_from_slice(&write_uint(el::CUECLUSTERPOSITION, c.cluster_pos_rel));
            let mut point = write_uint(el::CUETIME, c.time_ticks);
            point.extend_from_slice(&write_element(el::CUETRACKPOSITIONS, &positions));
            body.extend_from_slice(&write_element(el::CUEPOINT, &point));
        }
        write_element(el::CUES, &body)
    }
}

impl Muxer for MatroskaMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::empty()
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "matroska: streams must be added before the header is written",
            ));
        }
        let media = params
            .effective_media_type()
            .ok_or(Error::Unsupported("matroska: stream has no media type"))?;
        let is_video = match media {
            MediaType::Video => true,
            MediaType::Audio => false,
            _ => return Err(Error::Unsupported("matroska: only video and audio streams")),
        };
        let codec_id = params
            .codec_id
            .ok_or(Error::Unsupported("matroska: stream has no codec id"))?;

        if self.variant.is_webm {
            let allowed = if is_video {
                codec::webm_allows_video(codec_id)
            } else {
                codec::webm_allows_audio(codec_id)
            };
            if !allowed {
                return Err(Error::Unsupported(codec::WEBM_REJECTION));
            }
        }
        let codec_str = codec::codec_id_str(codec_id)
            .ok_or(Error::Unsupported("matroska: codec has no CodecID mapping"))?;

        // Measured against `ffmpeg 8.1`: a `webm` output needs `DocTypeVersion`
        // 4 once Opus is present (`CodecDelay`/`SeekPreRoll`); `matroska` is
        // always 4 regardless of codec.
        if codec_id == CodecId::Opus {
            self.needs_doctype_v4 = true;
        }

        let mut t = TrackOut {
            number: self.tracks.len() as u64 + 1,
            is_video,
            codec_id: codec_str,
            default_duration_ns: None,
            width: 0,
            height: 0,
            sample_rate: 0.0,
            channels: 1,
            bit_depth: None,
            extradata: params.extradata.clone(),
            reorders: false,
            prev_ts: None,
        };
        if is_video {
            let v = params.video.as_ref().ok_or(Error::Unsupported(
                "matroska: video stream has no VideoParameters",
            ))?;
            t.width = v.width;
            t.height = v.height;
            t.reorders = v.has_b_frames > 0;
            if v.frame_rate.is_defined() && !v.frame_rate.is_zero() && !v.frame_rate.is_infinite() {
                let per_frame = v.frame_rate.inverse(); // seconds per frame, as num/den
                let secs = f64::from(per_frame.num) / f64::from(per_frame.den);
                if secs.is_finite() && secs > 0.0 {
                    t.default_duration_ns = Some((secs * 1_000_000_000.0).round() as u64);
                }
            }
        } else {
            let a = params.audio.as_ref().ok_or(Error::Unsupported(
                "matroska: audio stream has no AudioParameters",
            ))?;
            if a.sample_rate == 0 {
                return Err(Error::Unsupported(
                    "matroska: audio stream has no sample rate",
                ));
            }
            t.sample_rate = f64::from(a.sample_rate);
            t.channels = a.layout.as_ref().map_or(1, |l| u64::from(l.channels));
            if codec_str.starts_with("A_PCM") {
                t.bit_depth = a.bits_per_coded_sample.map(u64::from).or(Some(16));
            }
        }

        let idx = u32::try_from(self.tracks.len())
            .map_err(|_| Error::Unsupported("matroska: too many tracks"))?;
        self.tracks.push(t);
        Ok(idx)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("matroska: header written twice"));
        }
        if self.tracks.is_empty() {
            return Err(Error::Unsupported("matroska: no streams to mux"));
        }
        self.header_written = true;

        self.out.write(&self.ebml_header_bytes())?;

        self.out.write(&id_bytes(el::SEGMENT))?;
        self.segment_size_at = self.out.pos();
        self.out.write(&vint_unknown(8))?;
        self.segment_data_start = self.out.pos();

        let info = self.info_bytes();
        self.out.write(&info)?;
        let tracks = self.tracks_bytes();
        self.out.write(&tracks)?;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData(
                "matroska: packet written before the header",
            ));
        }
        let idx = usize::try_from(packet.stream_index)
            .ok()
            .filter(|&i| i < self.tracks.len())
            .ok_or(Error::InvalidData(
                "matroska: packet names an unknown stream",
            ))?;

        let ts = packet.pts.ticks().unwrap_or(0);
        let dts = packet.dts.ticks().unwrap_or(ts);
        let is_key = packet.is_key();

        // Decide whether the current cluster can still hold this block:
        // reset when there is none yet, when a video keyframe should start a
        // fresh one, when the elapsed time is past the cap, or when the
        // relative timestamp would not fit the signed 16-bit field.
        let track_is_video = self.tracks.get(idx).is_some_and(|t| t.is_video);
        let needs_new_cluster = match &self.cluster {
            None => true,
            Some(c) => {
                (track_is_video && is_key)
                    || ts.saturating_sub(c.start_ticks) > self.max_cluster_ms
                    || i16::try_from(ts.saturating_sub(c.start_ticks)).is_err()
            }
        };
        if needs_new_cluster {
            self.flush_cluster()?;
            self.cluster_starts.push(self.out.pos());
            self.cluster = Some(Cluster {
                start_ticks: ts,
                body: Vec::new(),
                byte_pos: self.out.pos(),
                keyframe_opened: track_is_video && is_key,
            });
        }

        let Some(cluster) = self.cluster.as_mut() else {
            return Err(Error::InvalidData("matroska: no open cluster"));
        };
        let rel_ts = ts.saturating_sub(cluster.start_ticks);

        // `Packet::duration` is always microseconds (see `vaco_core::Duration`),
        // independent of the stream's time base, so it is converted to
        // `TimestampScale` ticks (1 tick == 1 ms, fixed in `info_bytes`)
        // directly rather than through the packet-timestamp rescale chain.
        // `ZERO` is also the field's default for "not stated", so it is
        // treated as absent rather than as a real zero-length block.
        let duration_ticks: Option<i64> = if packet.duration == vaco_core::Duration::ZERO {
            None
        } else {
            packet.duration.to_ticks(Rational::new(1, 1000))
        };
        let track = self.tracks.get_mut(idx).ok_or(Error::InvalidData(
            "matroska: packet names an unknown stream",
        ))?;
        #[allow(
            clippy::integer_division,
            reason = "converting a nanosecond count to whole TimestampScale ticks is an exact \
                      unit change, not a ratio computation"
        )]
        let default_duration_ticks = track
            .default_duration_ns
            .map(|ns| i64::try_from(ns / 1_000_000).unwrap_or(i64::MAX));
        let needs_duration = duration_ticks.is_some() && duration_ticks != default_duration_ticks;
        let needs_reference = track.reorders && ts != dts;

        let block_bytes = if needs_duration || needs_reference {
            let reference_ticks = needs_reference.then(|| {
                let prev = track.prev_ts.unwrap_or(dts);
                prev - dts
            });
            block::block_group(
                track.number,
                rel_ts,
                packet.payload(),
                duration_ticks.map(|d| u64::try_from(d).unwrap_or(0)),
                reference_ticks,
            )?
        } else {
            block::simple_block(track.number, rel_ts, is_key, packet.payload())?
        };
        track.prev_ts = Some(dts);
        cluster.body.extend_from_slice(&block_bytes);

        let end_ticks = ts.saturating_add(duration_ticks.unwrap_or(0)).max(0);
        self.max_end_ticks = self
            .max_end_ticks
            .max(u64::try_from(end_ticks).unwrap_or(0));
        Ok(())
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        // `TimestampScale` is fixed at 1_000_000 ns/tick (see `info_bytes`),
        // which is one millisecond per tick — shared by every track, per
        // RFC 9559 (unlike MP4's per-track time base).
        Some(Rational::new(1, 1000))
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData(
                "matroska: trailer written before the header",
            ));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("matroska: trailer written twice"));
        }
        self.trailer_written = true;

        self.flush_cluster()?;

        if !self.cues.is_empty() {
            let cues = self.cues_bytes();
            self.out.write(&cues)?;
        }

        if self.out.is_seekable() {
            let end = self.out.pos();
            let size = end.saturating_sub(self.segment_data_start);
            self.out.seek(self.segment_size_at)?;
            patch_known_size(&mut self.out, size)?;
            self.out.seek(end)?;
        }
        // Non-seekable: the Segment keeps the unknown-size marker written at
        // `write_header`, matching the reference measured on a pipe.

        self.out.flush()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_codec_core::{AudioParameters, VideoParameters};
    use vaco_core::Timestamp;
    use vaco_format_core::Demuxer;
    use vaco_format_core::discovery::NoParsers;
    use vaco_format_core::vacoraw::{ForwardOnlySink, MemorySink, SharedBytes};
    use vaco_io::MemorySource;
    use vaco_packet::PacketFlags;

    fn h264_params() -> CodecParameters {
        let mut p = CodecParameters::video().with_codec(CodecId::H264);
        p.video = Some(VideoParameters {
            width: 64,
            height: 48,
            frame_rate: Rational::new(25, 1),
            ..VideoParameters::default()
        });
        p.extradata = Some(vec![1, 2, 3, 4]);
        p
    }

    fn opus_params() -> CodecParameters {
        let mut p = CodecParameters::audio().with_codec(CodecId::Opus);
        p.audio = Some(AudioParameters {
            sample_rate: 48000,
            ..AudioParameters::default()
        });
        p
    }

    fn pkt(stream: u32, pts: i64, key: bool) -> Packet {
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut p = Packet::from_slice(&mut budget, b"payload").unwrap();
        p.stream_index = stream;
        p.pts = Timestamp::new(pts);
        p.dts = Timestamp::new(pts);
        if key {
            p.flags = PacketFlags::KEY;
        }
        p
    }

    #[test]
    fn a_seekable_sink_gets_a_patched_known_segment_size() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        // Locate the Segment element and confirm its size is not the
        // all-ones unknown marker.
        let seg_id = vaco_format_ebml::id_bytes(el::SEGMENT);
        let at = bytes
            .windows(seg_id.len())
            .position(|w| w == seg_id.as_slice())
            .unwrap();
        let (size, _) = vaco_format_ebml::read_size(&bytes[at + seg_id.len()..], 8).unwrap();
        assert_ne!(size, vaco_format_ebml::Size::Unknown);
        assert_eq!(size.known(), Some(bytes.len() as u64 - (at as u64 + 12)));
    }

    #[test]
    fn a_non_seekable_sink_keeps_the_unknown_size_marker() {
        let s = ForwardOnlySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        // A muxer that tried to seek this sink would already have failed by
        // now; `write_trailer` succeeding at all is half the property.
        mux.write_trailer().unwrap();

        let bytes = buf.snapshot();
        let seg_id = vaco_format_ebml::id_bytes(el::SEGMENT);
        let at = bytes
            .windows(seg_id.len())
            .position(|w| w == seg_id.as_slice())
            .unwrap();
        let (size, _) = vaco_format_ebml::read_size(&bytes[at + seg_id.len()..], 8).unwrap();
        assert_eq!(size, vaco_format_ebml::Size::Unknown);
    }

    #[test]
    fn webm_rejects_a_codec_outside_the_allow_list() {
        let mut mux =
            MatroskaMuxer::new_webm(Box::new(MemorySink::new()), &FormatOptions::default())
                .unwrap();
        assert!(mux.add_stream(&h264_params()).is_err());
    }

    #[test]
    fn webm_accepts_opus_and_bumps_doctype_version_to_four() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_webm(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&opus_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();
        // DocTypeVersion is the fifth uint element in the EBML header, value 4.
        assert!(bytes.windows(3).any(|w| w == [0x42, 0x87, 0x81]));
    }

    #[test]
    fn a_track_entry_carries_codec_private_verbatim() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();
        assert!(bytes.windows(4).any(|w| w == [1, 2, 3, 4]));
    }

    #[test]
    fn a_video_keyframe_produces_a_cue_point() {
        let s = MemorySink::new();
        let buf = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();
        let cues_id = vaco_format_ebml::id_bytes(el::CUES);
        assert!(
            bytes
                .windows(cues_id.len())
                .any(|w| w == cues_id.as_slice())
        );
    }

    #[test]
    fn a_second_header_or_trailer_is_refused() {
        let mut mux =
            MatroskaMuxer::new_matroska(Box::new(MemorySink::new()), &FormatOptions::default())
                .unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        assert!(mux.write_header().is_err());
        mux.write_packet(&pkt(idx, 0, true)).unwrap();
        mux.write_trailer().unwrap();
        assert!(mux.write_trailer().is_err());
    }

    #[test]
    fn the_whole_file_reads_back_through_the_demuxer() {
        let s = MemorySink::new();
        let buf: SharedBytes = s.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(s), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.write_header().unwrap();
        for i in 0..5i64 {
            mux.write_packet(&pkt(idx, i * 40, i == 0)).unwrap();
        }
        mux.write_trailer().unwrap();
        let bytes = buf.snapshot();

        let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(bytes));
        let mut demux =
            vaco_demux_matroska::MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
                .unwrap();
        assert_eq!(demux.streams().len(), 1);
        let mut count = 0;
        while let Ok(p) = demux.read_packet() {
            assert_eq!(p.payload(), b"payload");
            count += 1;
        }
        assert_eq!(count, 5);
    }
}
