//! The bitstream family: 22 registrations sharing one timestamp convention
//! and one of five framing strategies.
//!
//! # Measured against ffprobe 8.1
//!
//! ```text
//! $ ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 -c:v libx264 -f h264 t.h264
//! $ ffprobe -show_streams -show_packets t.h264
//! ```
//!
//! * `time_base` is **always `1/1_200_000`**, independent of the declared
//!   `-framerate` — measured identically on `h264` and, with `-f mjpeg`
//!   forced, on `mjpeg` too. `duration_ts = round(1_200_000 * framerate.den /
//!   framerate.num)` (240000 was measured directly for a 5 fps declaration,
//!   i.e. `1_200_000 / 5`). Since a packet's *absolute* duration is
//!   `duration_ts * time_base`, this collapses to plain `1 / framerate`
//!   seconds regardless of which internal tick base is used, so
//!   [`vaco_format_core::time::duration_from_rate`] is what this module
//!   actually stamps on each [`vaco_packet::Packet`].
//! * **Every packet has `pts = N/A` and `dts = N/A`** — a raw bitstream
//!   carries no timestamps and none are synthesised, only a per-packet
//!   `duration`. This is true even on the reference's own h264 demuxer with
//!   its real parser attached, so [`BitstreamDemuxer`] never invents one
//!   either.
//! * `data` (and, by extension, every format with no `-framerate` option:
//!   `bit`, `loas`, `s337m`) has **no duration at all** — measured directly:
//!   `duration=N/A` on every packet, packets are flat 1024-byte reads.
//!
//! # Framing, and what is measured versus assumed
//!
//! | Framing | Formats | Status |
//! |---|---|---|
//! | [`Framing::StartCode3`] with a real parser | `h264`, `hevc` | The parser path (via `ParserProvider`) is not exercised by this crate's own tests, which use `NoParsers` per D14.1 — only the fallback scan below is. |
//! | [`Framing::Obu`] with a real parser | `av1`, `obu` | Same as above; [`crate::obu`]'s own framing is spec-derived and unit-tested directly, with or without a parser. |
//! | [`Framing::StartCode3`], no parser exists in this workspace | `vvc`, `m4v`, `mpegvideo`, `cavsvideo`, `avs2`, `avs3`, `vc1`, `evc` | Structurally present: splits at every start code, which is coarser than the reference's per-access-unit grouping (a parameter set and its picture become two packets, not one) and always reports `KEY`. Not verified against a real encoder's packet count. |
//! | [`Framing::Marker`] | `mjpeg`, `mjpeg_2000` | JPEG SOI/EOI and JP2 SOC/EOC scanning, spec-derived. Lightly tested; the reference detects a mislabelled MJPEG dump as `jpeg_pipe` before it reaches this demuxer at all (measured), so probing this family without a matching extension is inherently unreliable on both sides. |
//! | [`Framing::Dirac`] | `dirac` | The 13-byte parse-info header's `next_parse_offset` field, per SMPTE 2042. Not exercised against a real Dirac stream. |
//! | [`Framing::FixedBlock`] | `h261`, `h263`, `dnxhd`, `bit`, `data`, `s337m`, `loas` | No structural framing at all: fixed 1024-byte reads, matching the reference's *own* fallback for `data` (measured directly) but almost certainly not what the reference does for `h261`/`h263`/`dnxhd`, which have real parsers upstream that this crate does not. Registered, and said so, per the brief. |
//!
//! Every registration in this module loads the whole remaining input at
//! `open` and computes its packet table once, bounded by the caller's
//! [`vaco_limits::Limits`] the same way every allocation in this crate is —
//! see the crate docs for why streaming was not worth the complexity here.

use vaco_codec_core::{CodecId, CodecParameters, ParserDriver, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::time::duration_from_rate;
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, SeekFlags, SeekTarget, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::obu;
use crate::startcode;

/// Reference default for `-framerate` on every member that has one.
pub const DEFAULT_FRAMERATE: Rational = Rational::new(25, 1);
/// Reference default for `-raw_packet_size`, and the fixed chunk size used
/// by [`Framing::FixedBlock`].
pub const RAW_PACKET_SIZE: usize = 1024;
/// The fixed time base every "generic raw video demuxer" family member uses,
/// independent of the declared frame rate. Measured; see the module docs.
pub const TIME_BASE_DEN: i32 = 1_200_000;
/// Largest input this demuxer will buffer whole, on top of the caller's
/// [`Limits`] (which are checked first and usually bind first). A backstop,
/// not the primary control.
const MAX_BUFFERED: u64 = 512 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    StartCode3,
    Obu,
    Marker { start: [u8; 2], end: [u8; 2] },
    Dirac,
    FixedBlock,
}

#[derive(Debug, Clone, Copy)]
pub struct BitstreamSpec {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub(crate) framing: Framing,
    /// The `CodecId` to ask the `ParserProvider` for, when the framing wants
    /// one (`StartCode3`/`Obu` only). `None` for families with no parser in
    /// this workspace yet.
    pub parser_codec: Option<CodecId>,
    /// Whether `-framerate`/duration apply at all (false for `data`-like
    /// formats: `bit`, `data`, `loas`, `s337m`).
    pub has_framerate: bool,
}

macro_rules! spec {
    ($name:literal, $long:literal, $exts:expr, $framing:expr, $parser:expr, $fr:expr) => {
        BitstreamSpec {
            name: $name,
            long_name: $long,
            extensions: $exts,
            framing: $framing,
            parser_codec: $parser,
            has_framerate: $fr,
        }
    };
}

pub const AV1: BitstreamSpec = spec!(
    "av1",
    "AV1 Annex B",
    &["obu"],
    Framing::Obu,
    Some(CodecId::Av1),
    true
);
pub const OBU: BitstreamSpec = spec!(
    "obu",
    "AV1 low overhead OBU",
    &["obu"],
    Framing::Obu,
    Some(CodecId::Av1),
    true
);
pub const AVS2: BitstreamSpec = spec!(
    "avs2",
    "raw AVS2-P2/IEEE1857.4",
    &["avs", "avs2"],
    Framing::StartCode3,
    None,
    true
);
pub const AVS3: BitstreamSpec = spec!(
    "avs3",
    "raw AVS3-P2/IEEE1857.10",
    &["avs3"],
    Framing::StartCode3,
    None,
    true
);
pub const BIT: BitstreamSpec = spec!(
    "bit",
    "G.729 BIT file format",
    &["bit"],
    Framing::FixedBlock,
    None,
    false
);
pub const CAVSVIDEO: BitstreamSpec = spec!(
    "cavsvideo",
    "raw Chinese AVS (Audio Video Standard)",
    &["avs"],
    Framing::StartCode3,
    None,
    true
);
pub const DATA: BitstreamSpec = spec!("data", "raw data", &[], Framing::FixedBlock, None, false);
pub const DIRAC: BitstreamSpec = spec!("dirac", "raw Dirac", &[], Framing::Dirac, None, true);
pub const DNXHD: BitstreamSpec = spec!(
    "dnxhd",
    "raw DNxHD (SMPTE VC-3)",
    &[],
    Framing::FixedBlock,
    None,
    true
);
pub const EVC: BitstreamSpec = spec!(
    "evc",
    "EVC Annex B",
    &["evc"],
    Framing::StartCode3,
    None,
    true
);
pub const H261: BitstreamSpec = spec!(
    "h261",
    "raw H.261",
    &["h261"],
    Framing::FixedBlock,
    None,
    true
);
pub const H263: BitstreamSpec = spec!("h263", "raw H.263", &[], Framing::FixedBlock, None, true);
pub const H264: BitstreamSpec = spec!(
    "h264",
    "raw H.264 video",
    &["h26l", "h264", "264", "avc"],
    Framing::StartCode3,
    Some(CodecId::H264),
    true
);
pub const HEVC: BitstreamSpec = spec!(
    "hevc",
    "raw HEVC video",
    &["hevc", "h265", "265"],
    Framing::StartCode3,
    Some(CodecId::Hevc),
    true
);
pub const LOAS: BitstreamSpec = spec!(
    "loas",
    "LOAS AudioSyncStream",
    &[],
    Framing::FixedBlock,
    None,
    false
);
pub const M4V: BitstreamSpec = spec!(
    "m4v",
    "raw MPEG-4 video",
    &["m4v"],
    Framing::StartCode3,
    // MPEG-4 part 2 raw streams have no MPEG-1/2-style start-code ambiguity
    // (see `MPEGVIDEO` below), so this reaches `vaco-parse-mpegvideo`'s
    // `Mpeg4Parser` unconditionally.
    Some(CodecId::Mpeg4),
    true
);
pub const MJPEG: BitstreamSpec = spec!(
    "mjpeg",
    "raw MJPEG video",
    &["mjpg", "mjpeg", "mpo"],
    Framing::Marker {
        start: [0xFF, 0xD8],
        end: [0xFF, 0xD9],
    },
    None,
    true
);
pub const MJPEG_2000: BitstreamSpec = spec!(
    "mjpeg_2000",
    "raw MJPEG 2000 video",
    &["j2k"],
    Framing::Marker {
        start: [0xFF, 0x4F],
        end: [0xFF, 0xD9],
    },
    None,
    true
);
pub const MPEGVIDEO: BitstreamSpec = spec!(
    "mpegvideo",
    "raw MPEG video",
    &[],
    Framing::StartCode3,
    // `parser_codec` is a single static `CodecId` chosen once here, so it
    // cannot defer to "MPEG-1 or MPEG-2, decided after the first sequence
    // header is seen" — the reference's own `mpegvideo` raw demuxer covers
    // both off the identical `00 00 01 xx` start-code space.
    // `PARSER_MPEG1`/`PARSER_MPEG2` both construct the same `Mpeg12Parser`
    // and differ only in which `CodecId` reaches them through
    // `ParserProvider::parser_for`, so either answer reaches the right
    // parser; `Mpeg2video` is chosen because plain MPEG-1 elementary streams
    // are rare in the wild, matching every practical `.m2v` file. A bare
    // MPEG-1 stream opened this way states the wrong `codec_name`, which is
    // the known, narrower limitation this accepts rather than solves.
    Some(CodecId::Mpeg2video),
    true
);
/// A generic `Framing::FixedBlock` stand-in with no real SMPTE 337M framing
/// (no `Pa`/`Pb` sync words, no `Pc`/`Pd` data-type/length fields) — added by
/// this crate's mechanical sweep of ffmpeg's raw-demuxer names before
/// `vaco-format-spdif` existed. No longer registered under `s337m`: that
/// name now resolves to `vaco_format_spdif::S337M_DEMUXER`, a real burst
/// parser measured byte-identical against `ffmpeg -f spdif` (see
/// `planning/TECH-DEBT.md`'s now-resolved "`s337m` is registered twice"
/// entry). Left defined and unregistered rather than deleted, in case a
/// genuinely distinct bare-bitstream use ever turns up.
pub const S337M: BitstreamSpec =
    spec!("s337m", "SMPTE 337M", &[], Framing::FixedBlock, None, false);
pub const VC1: BitstreamSpec = spec!("vc1", "raw VC-1", &["vc1"], Framing::StartCode3, None, true);
pub const VVC: BitstreamSpec = spec!(
    "vvc",
    "raw H.266/VVC video",
    &["h266", "266", "vvc"],
    Framing::StartCode3,
    None,
    true
);

/// All 21 registered bitstream-family specs, in `ffmpeg -demuxers` order.
///
/// `S337M` is deliberately not in this list — see its own doc comment.
pub const BITSTREAM_FORMATS: &[BitstreamSpec] = &[
    AV1, AVS2, AVS3, BIT, CAVSVIDEO, DATA, DIRAC, DNXHD, EVC, H261, H263, H264, HEVC, LOAS, M4V,
    MJPEG, MJPEG_2000, MPEGVIDEO, OBU, VC1, VVC,
];

/// Options private to this family: `-framerate` (ignored where
/// `has_framerate` is false).
#[derive(Debug, Clone, Copy)]
pub struct BitstreamOptions {
    pub framerate: Rational,
}

impl Default for BitstreamOptions {
    fn default() -> Self {
        Self {
            framerate: DEFAULT_FRAMERATE,
        }
    }
}

fn fixed_duration(spec: &BitstreamSpec, framerate: Rational) -> Duration {
    if !spec.has_framerate {
        return Duration::ZERO;
    }
    duration_from_rate(framerate).unwrap_or(Duration::ZERO)
}

/// One byte-range packet, computed ahead of time from the whole buffer.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

fn compute_spans(framing: Framing, data: &[u8]) -> Vec<Span> {
    match framing {
        Framing::StartCode3 => startcode::spans(data)
            .into_iter()
            .map(|(start, end)| Span { start, end })
            .collect(),
        Framing::Obu => obu::temporal_units(data)
            .into_iter()
            .map(|(start, end)| Span { start, end })
            .collect(),
        Framing::Marker { start, end } => marker_spans(data, start, end),
        Framing::Dirac => dirac_spans(data),
        Framing::FixedBlock => {
            let mut out = Vec::new();
            let mut pos = 0usize;
            while pos < data.len() {
                let end = (pos + RAW_PACKET_SIZE).min(data.len());
                out.push(Span { start: pos, end });
                pos = end;
            }
            out
        }
    }
}

/// Scan for `start`, then the next `end` at or after it; repeat from there.
/// Bytes before the first `start` marker are dropped, matching the
/// start-code family's convention of ignoring un-delimited leading bytes.
fn marker_spans(data: &[u8], start: [u8; 2], end: [u8; 2]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(s) = find2(data, start, pos) {
        let search_from = s.saturating_add(2);
        let e = find2(data, end, search_from).map_or(data.len(), |e| e.saturating_add(2));
        out.push(Span { start: s, end: e });
        if e <= s {
            break;
        }
        pos = e;
    }
    out
}

fn find2(data: &[u8], needle: [u8; 2], from: usize) -> Option<usize> {
    let slice = data.get(from..)?;
    if slice.len() < 2 {
        return None;
    }
    slice.windows(2).position(|w| w == needle).map(|i| i + from)
}

/// SMPTE 2042 Dirac parse-unit framing: `'BBCD'` + `parse_code`(1) +
/// `next_parse_offset`(u32 BE) + `previous_parse_offset`(u32 BE).
/// `next_parse_offset` counts bytes from the start of *this* parse unit to
/// the start of the next, header included; `0` marks the final unit.
fn dirac_spans(data: &[u8]) -> Vec<Span> {
    const MAGIC: [u8; 4] = *b"BBCD";
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(slice) = data.get(pos..) {
        let Some(rel) = slice.windows(4).position(|w| w == MAGIC) else {
            break;
        };
        let start = pos + rel;
        let Some(next_off) = data
            .get(start.saturating_add(5)..start.saturating_add(9))
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_be_bytes)
        else {
            break;
        };
        let end = if next_off >= 13 {
            start.saturating_add(next_off as usize).min(data.len())
        } else {
            data.len()
        };
        out.push(Span { start, end });
        if end <= start {
            break;
        }
        pos = end;
    }
    out
}

/// Charge-and-collect the whole remaining input, bounded by `budget`.
fn read_all(io: &mut IoContext, budget: &mut Budget) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = budget.alloc::<u8>(64 * 1024)?;
    loop {
        let n = io.read_partial(&mut chunk)?;
        if n == 0 {
            break;
        }
        let Some(taken) = chunk.get(..n) else {
            return Err(Error::InvalidData(
                "short read reported more bytes than taken",
            ));
        };
        budget.charge(taken.len() as u64)?;
        if out.len() as u64 + taken.len() as u64 > MAX_BUFFERED {
            return Err(Error::LimitExceeded {
                limit: "raw_bitstream_buffer",
                requested: out.len() as u64 + taken.len() as u64,
                cap: MAX_BUFFERED,
            });
        }
        out.extend_from_slice(taken);
    }
    Ok(out)
}

/// Drive a real parser over the whole buffer, collecting every unit it
/// produces. Used only when the caller's `ParserProvider` has one.
fn drive_parser(
    parser: Box<dyn vaco_codec_core::Parser>,
    data: &[u8],
) -> Result<std::collections::VecDeque<Packet>> {
    let mut driver = ParserDriver::new(parser, Limits::permissive());
    driver.push(data)?;
    driver.finish();
    let mut out = std::collections::VecDeque::new();
    loop {
        match driver.next_unit() {
            Ok(pkt) => out.push_back(pkt),
            Err(Error::Eof | Error::NeedMoreInput) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

enum Frames {
    Spans(Vec<Span>),
    Ready(std::collections::VecDeque<Packet>),
}

/// The bitstream-family demuxer, parameterised at construction by
/// [`BitstreamSpec`].
pub struct BitstreamDemuxer {
    spec: &'static BitstreamSpec,
    data: Vec<u8>,
    frames: Frames,
    cursor: usize,
    streams: [Stream; 1],
    duration: Duration,
    budget: Budget,
    eof: bool,
}

impl core::fmt::Debug for BitstreamDemuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BitstreamDemuxer")
            .field("spec", &self.spec.name)
            .field("bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}

impl BitstreamDemuxer {
    /// # Errors
    /// [`Error::LimitExceeded`] if the input is larger than the caller's
    /// [`Limits`] allow; otherwise whatever the transport reports.
    pub fn open(
        spec: &'static BitstreamSpec,
        src: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
        opts: &BitstreamOptions,
    ) -> Result<Self> {
        Self::open_with_limits(spec, src, parsers, opts, Limits::permissive())
    }

    /// # Errors
    /// As [`BitstreamDemuxer::open`].
    pub fn open_with_limits(
        spec: &'static BitstreamSpec,
        src: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
        opts: &BitstreamOptions,
        limits: Limits,
    ) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut budget = Budget::new(limits);
        let data = read_all(&mut io, &mut budget)?;
        let duration = fixed_duration(spec, opts.framerate);

        let frames = match spec.parser_codec.and_then(|c| parsers.parser_for(c)) {
            Some(parser) => Frames::Ready(drive_parser(parser, &data)?),
            None => Frames::Spans(compute_spans(spec.framing, &data)),
        };

        let mut params = CodecParameters::new(MediaType::Video);
        if let Some(codec) = spec.parser_codec {
            params.codec_id = Some(codec);
        } else {
            let mut video = VideoParameters::default();
            if spec.has_framerate {
                video.frame_rate = opts.framerate;
            }
            params.video = Some(video);
        }
        let time_base = Rational::new(1, TIME_BASE_DEN);
        let mut stream = Stream::new(0, MediaType::Video, time_base);
        stream.params = params;
        if spec.parser_codec.is_none() {
            stream.metadata_set("raw_codec_name", spec.name);
        }

        Ok(Self {
            spec,
            data,
            frames,
            cursor: 0,
            streams: [stream],
            duration,
            budget,
            eof: false,
        })
    }

    #[must_use]
    pub const fn spec(&self) -> &'static BitstreamSpec {
        self.spec
    }
}

impl Demuxer for BitstreamDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        match &mut self.frames {
            Frames::Ready(q) => {
                let Some(mut pkt) = q.pop_front() else {
                    self.eof = true;
                    return Err(Error::Eof);
                };
                pkt.stream_index = 0;
                pkt.pts = Timestamp::NONE;
                pkt.dts = Timestamp::NONE;
                pkt.duration = self.duration;
                Ok(pkt)
            }
            Frames::Spans(spans) => {
                let Some(span) = spans.get(self.cursor).copied() else {
                    self.eof = true;
                    return Err(Error::Eof);
                };
                self.cursor = self.cursor.saturating_add(1);
                let Some(bytes) = self.data.get(span.start..span.end) else {
                    return Err(Error::InvalidData("packet span outside the buffer"));
                };
                let pos = span.start as u64;
                let mut pkt = Packet::from_slice(&mut self.budget, bytes)?;
                pkt.stream_index = 0;
                pkt.pts = Timestamp::NONE;
                pkt.dts = Timestamp::NONE;
                pkt.duration = self.duration;
                pkt.pos = Some(pos);
                pkt.flags = PacketFlags::KEY;
                Ok(pkt)
            }
        }
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::Unsupported(
            "the raw bitstream family carries no timestamps to seek by",
        ))
    }
}

macro_rules! bitstream_reg {
    ($ident:ident, $spec:expr) => {
        pub const $ident: DemuxerDesc = DemuxerDesc {
            name: $spec.name,
            long_name: $spec.long_name,
            extensions: $spec.extensions,
            mime_types: &[],
            // A raw format carries no index of its own — the file *is* the
            // elementary stream — so the generic byte/timestamp index is what
            // seeks it. `GENERIC_INDEX` says that; `empty()` said nothing, and
            // `empty()` is not neutral: it silently opts into the monotonic-DTS
            // repair decision rather than expressing one, which is why
            // `every_registered_demuxer_declares_flags` refuses it.
            flags: vaco_format_core::FormatFlags::GENERIC_INDEX,
            probe: |data: &ProbeData<'_>| probe_for(&$spec, data),
            open: |src: Box<dyn MediaSource>, parsers: &dyn ParserProvider| {
                Ok(Box::new(BitstreamDemuxer::open(
                    &$spec,
                    src,
                    parsers,
                    &BitstreamOptions::default(),
                )?) as Box<dyn Demuxer>)
            },
        };
    };
}

/// Score `data` for `spec`. Structural evidence (a start code, a JPEG/JP2
/// marker, a Dirac magic) scores 51 — the one value directly measured, on
/// `h264` and `obu`, both regardless of extension. Everything else falls
/// back to [`ProbeScore::from_extension`]. `FixedBlock`-framed formats have
/// no structural evidence at all and score by extension only, which matches
/// `data`'s measured behaviour of never auto-detecting.
fn probe_for(spec: &BitstreamSpec, data: &ProbeData<'_>) -> ProbeScore {
    let structural = match spec.framing {
        // The **first** start code must be at the very beginning, not merely
        // present somewhere.
        //
        // "Somewhere" is the whole file, and start codes are three bytes that
        // occur inside every MPEG-family payload — so an MPEG-TS file, whose
        // PES payloads are full of real elementary-stream start codes, matched
        // every `StartCode3` format. Since a structural hit scores 51 and a
        // confidently-detected transport stream scores 50 (both measured
        // against the reference), the raw formats won by one point and
        // `ffprobe file.ts` reported `format_name=avs2`. Found by the
        // differential harness on its first run.
        //
        // A raw elementary stream **opens** with a start code, optionally after
        // one leading zero byte, so requiring offset 0 or 1 costs nothing real
        // and rejects every container — but it does not stop the ten
        // `StartCode3` formats agreeing with *each other*: every one of them
        // opens with a start code, so all ten scored 51 on any of them and
        // ties broke alphabetically (`avs2` beat `h264` on an actual H.264
        // elementary stream). The second half of the fix is
        // [`start_code_identifier`]: the byte or bytes immediately after the
        // start code, checked against what that specific format is required
        // to open with.
        Framing::StartCode3 => startcode::start_codes(data.buf)
            .first()
            .is_some_and(|&i| i <= 1 && start_code_identifier(spec.name, data, i + 3)),
        // The strict test, not `temporal_units`: that one falls back to
        // reporting the whole buffer when nothing parses, which is right for
        // demuxing and makes every non-empty input look like AV1 when probing.
        Framing::Obu => obu::looks_like_obu_stream(data.buf),
        Framing::Marker { start, .. } => data.starts_with(&start),
        Framing::Dirac => data.starts_with(b"BBCD"),
        Framing::FixedBlock => false,
    };
    if structural {
        ProbeScore(51)
    } else {
        ProbeScore::from_extension(data, spec.extensions)
    }
}

/// Whether the byte(s) at `at` (immediately after a `00 00 01` start code)
/// are what `name`'s format is required to open with.
///
/// # Measured, per finding 3 of `planning/CONFORMANCE-FINDINGS.md`
///
/// Generated with `ffmpeg -f lavfi -i testsrc=d=0.5 -c:v <codec> -f
/// <rawformat> out.bin` for every `StartCode3` member that this build's
/// `ffmpeg -codecs` lists an encoder for, then read back with `xxd` and
/// cross-checked with `ffprobe -show_entries format=format_name` on the
/// unforced file:
///
/// | format | encoder used | first byte(s) after `00 00 01` | reference detects |
/// |---|---|---|---|
/// | `h264` | `libx264` | `0x67` (SPS, `nal_ref_idc` 3) | `h264` |
/// | `h264` | `libx264 -x264-params aud=1` | `0x09` (AUD, `nal_ref_idc` 0) | `h264` |
/// | `hevc` | `libx265` | `0x40 0x01` (VPS, type 32) | `hevc` |
/// | `hevc` | `libx265 -x265-params aud=1` | `0x46 0x01` (AUD, type 35) | `hevc` |
/// | `mpegvideo` | `mpeg1video` | `0xB3` (`sequence_header_code`) | `mpegvideo` |
/// | `mpegvideo` | `mpeg2video` | `0xB3` (same code, both MPEG-1 and -2) | `mpegvideo` |
/// | `m4v` | `mpeg4` | `0xB0` (`visual_object_sequence_start_code`) | `m4v` |
///
/// A PPS observed mid-stream (not first, so not load-bearing for detection)
/// was `0x68`, added to `h264`'s accepted set on the strength of the H.264
/// NAL-type enumeration it shares with the two identifiers actually measured
/// at offset 0: parameter sets and the access-unit delimiter are the only
/// units Annex B allows to open a stream with.
///
/// `avs2`, `avs3`, `cavsvideo`, `evc`, `vc1` and `vvc` have **no encoder in
/// this `ffmpeg` 8.1 build** — `ffmpeg -codecs` shows `avs2`/`avs3`/`evc`
/// with neither the `D` nor the `E` flag set (known to the codec table,
/// nothing compiled in for either direction), `cavs`/`vc1`/`vvc` with `D`
/// but no `E` (decode-only). There is no reference sample to read an
/// identifier back from, so per the brief they make no structural claim
/// here and fall back to [`ProbeScore::from_extension`]. Recording a value
/// recalled rather than measured is the exact mistake this finding exists
/// to prevent; the probe-matrix test below asserts these six lose to
/// extension-scored siblings rather than pretending to detect them
/// structurally.
fn start_code_identifier(name: &str, data: &ProbeData<'_>, at: usize) -> bool {
    match name {
        "h264" => data
            .get(at)
            .is_some_and(|b0| b0 & 0x80 == 0 && matches!(b0 & 0x1F, 7..=9)),
        "hevc" => data
            .get(at)
            .is_some_and(|b0| b0 & 0x80 == 0 && matches!((b0 >> 1) & 0x3F, 32..=35)),
        "mpegvideo" => data.get(at) == Some(0xB3),
        "m4v" => data.get(at) == Some(0xB0),
        _ => false,
    }
}

bitstream_reg!(DEMUXER_AV1, AV1);
bitstream_reg!(DEMUXER_AVS2, AVS2);
bitstream_reg!(DEMUXER_AVS3, AVS3);
bitstream_reg!(DEMUXER_BIT, BIT);
bitstream_reg!(DEMUXER_CAVSVIDEO, CAVSVIDEO);
bitstream_reg!(DEMUXER_DATA, DATA);
bitstream_reg!(DEMUXER_DIRAC, DIRAC);
bitstream_reg!(DEMUXER_DNXHD, DNXHD);
bitstream_reg!(DEMUXER_EVC, EVC);
bitstream_reg!(DEMUXER_H261, H261);
bitstream_reg!(DEMUXER_H263, H263);
bitstream_reg!(DEMUXER_H264, H264);
bitstream_reg!(DEMUXER_HEVC, HEVC);
bitstream_reg!(DEMUXER_LOAS, LOAS);
bitstream_reg!(DEMUXER_M4V, M4V);
bitstream_reg!(DEMUXER_MJPEG, MJPEG);
bitstream_reg!(DEMUXER_MJPEG_2000, MJPEG_2000);
bitstream_reg!(DEMUXER_MPEGVIDEO, MPEGVIDEO);
bitstream_reg!(DEMUXER_OBU, OBU);
// Defined for parity with `S337M` but deliberately absent from
// `BITSTREAM_DEMUXERS` — see that const's doc comment.
bitstream_reg!(DEMUXER_S337M, S337M);
bitstream_reg!(DEMUXER_VC1, VC1);
bitstream_reg!(DEMUXER_VVC, VVC);

/// All 21 registered bitstream-family descriptors, in [`BITSTREAM_FORMATS`]
/// order. `DEMUXER_S337M` is deliberately not in this list: `s337m` is
/// registered from `vaco-format-spdif` instead (see `S337M`'s doc comment).
pub const BITSTREAM_DEMUXERS: &[&DemuxerDesc] = &[
    &DEMUXER_AV1,
    &DEMUXER_AVS2,
    &DEMUXER_AVS3,
    &DEMUXER_BIT,
    &DEMUXER_CAVSVIDEO,
    &DEMUXER_DATA,
    &DEMUXER_DIRAC,
    &DEMUXER_DNXHD,
    &DEMUXER_EVC,
    &DEMUXER_H261,
    &DEMUXER_H263,
    &DEMUXER_H264,
    &DEMUXER_HEVC,
    &DEMUXER_LOAS,
    &DEMUXER_M4V,
    &DEMUXER_MJPEG,
    &DEMUXER_MJPEG_2000,
    &DEMUXER_MPEGVIDEO,
    &DEMUXER_OBU,
    &DEMUXER_VC1,
    &DEMUXER_VVC,
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    #[test]
    fn there_are_twenty_one_registrations() {
        // `s337m` moved to `vaco-format-spdif::S337M_DEMUXER`; `S337M`/
        // `DEMUXER_S337M` stay defined here but are no longer registered
        // (see `S337M`'s own doc comment).
        assert_eq!(BITSTREAM_FORMATS.len(), 21);
        assert_eq!(BITSTREAM_DEMUXERS.len(), 21);
    }

    #[test]
    fn every_descriptor_matches_its_spec() {
        for (desc, spec) in BITSTREAM_DEMUXERS.iter().zip(BITSTREAM_FORMATS.iter()) {
            assert_eq!(desc.name, spec.name);
            assert_eq!(desc.long_name, spec.long_name);
            assert_eq!(desc.extensions, spec.extensions);
        }
    }

    #[test]
    fn m4v_and_mpegvideo_each_name_a_parser_codec() {
        assert_eq!(M4V.parser_codec, Some(CodecId::Mpeg4));
        assert_eq!(MPEGVIDEO.parser_codec, Some(CodecId::Mpeg2video));
    }

    #[test]
    fn mpegvideo_reports_its_codec_id_even_with_no_parser_available() {
        // `NoParsers` (D14.1: this crate depends on no `vaco-parse-*` crate,
        // so its own tests can never construct a real one) means
        // `parser_for` always answers `None` here, exercising the
        // `Frames::Spans` fallback — but `parser_codec` being `Some` still
        // means `CodecParameters.codec_id` is set from it, and the
        // `raw_codec_name` fallback (for a spec with no parser codec at all)
        // is not invented alongside it.
        let src = Box::new(MemorySource::new(vec![0, 0, 1, 0xB3, 1, 2, 3]));
        let d = BitstreamDemuxer::open(&MPEGVIDEO, src, &NoParsers, &BitstreamOptions::default())
            .unwrap();
        assert_eq!(d.streams()[0].params.codec_id, Some(CodecId::Mpeg2video));
        assert_eq!(d.streams()[0].metadata_get("raw_codec_name"), None);
    }

    #[test]
    fn h264_falls_back_to_start_code_splitting_with_no_parser() {
        let mut data = Vec::new();
        data.extend_from_slice(&[0, 0, 1, 0x67, 0xAA]); // SPS-ish
        data.extend_from_slice(&[0, 0, 1, 0x65, 0xBB, 0xCC]); // IDR-ish
        let src = Box::new(MemorySource::new(data));
        let mut d =
            BitstreamDemuxer::open(&H264, src, &NoParsers, &BitstreamOptions::default()).unwrap();
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 5);
        assert!(p0.pts.is_none());
        assert!(p0.is_key());
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.len, 6);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn every_packet_gets_the_same_fixed_duration() {
        let mut data = Vec::new();
        data.extend_from_slice(&[0, 0, 1, 1]);
        data.extend_from_slice(&[0, 0, 1, 2, 3]);
        data.extend_from_slice(&[0, 0, 1, 4, 5, 6]);
        let src = Box::new(MemorySource::new(data));
        let opts = BitstreamOptions {
            framerate: Rational::new(5, 1),
        };
        let mut d = BitstreamDemuxer::open(&M4V, src, &NoParsers, &opts).unwrap();
        let want = duration_from_rate(Rational::new(5, 1)).unwrap();
        assert_eq!(d.read_packet().unwrap().duration, want);
        assert_eq!(d.read_packet().unwrap().duration, want);
    }

    #[test]
    fn data_has_no_duration_and_fixed_1024_byte_chunks() {
        let bytes = vec![0u8; 2500];
        let src = Box::new(MemorySource::new(bytes));
        let mut d =
            BitstreamDemuxer::open(&DATA, src, &NoParsers, &BitstreamOptions::default()).unwrap();
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 1024);
        assert_eq!(p0.duration, Duration::ZERO);
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.len, 1024);
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.len, 452);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn mjpeg_marker_framing_finds_two_pictures() {
        let mut data = Vec::new();
        data.extend_from_slice(&[0xFF, 0xD8]);
        data.extend_from_slice(&[1, 2, 3]);
        data.extend_from_slice(&[0xFF, 0xD9]);
        data.extend_from_slice(&[0xFF, 0xD8]);
        data.extend_from_slice(&[4, 5]);
        data.extend_from_slice(&[0xFF, 0xD9]);
        let src = Box::new(MemorySource::new(data));
        let mut d =
            BitstreamDemuxer::open(&MJPEG, src, &NoParsers, &BitstreamOptions::default()).unwrap();
        assert_eq!(d.read_packet().unwrap().len, 7);
        assert_eq!(d.read_packet().unwrap().len, 6);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn av1_obu_framing_splits_on_temporal_delimiters() {
        // header byte with has_size_field set: (type << 3) | 0x02
        let td = [(2 << 3) | 0x02, 0x00]; // type 2 (TD), size 0
        let mut data = Vec::new();
        data.extend_from_slice(&td);
        data.extend_from_slice(&[(1 << 3) | 0x02, 2, 0xAA, 0xBB]); // seq header
        data.extend_from_slice(&td);
        data.extend_from_slice(&[(6 << 3) | 0x02, 1, 0xCC]); // frame
        let src = Box::new(MemorySource::new(data));
        let mut d =
            BitstreamDemuxer::open(&AV1, src, &NoParsers, &BitstreamOptions::default()).unwrap();
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 6);
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.len, 5);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
}
