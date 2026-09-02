//! `yuv4mpegpipe` — the one member of the raw-video family that is
//! self-describing.
//!
//! # The format
//!
//! A de facto standard from the `mjpegtools` project. One header line:
//!
//! ```text
//! YUV4MPEG2 W<width> H<height> F<num>:<den> I<p|t|b|m> A<num>:<den> C<space>\n
//! ```
//!
//! every tag but `W`/`H` optional, in any order, space-separated, terminated
//! by a single `\n`. Then one `FRAME<params>\n` line per picture, each
//! immediately followed by that many raw planar bytes.
//!
//! # Measured against ffprobe 8.1
//!
//! ```text
//! $ ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=0.4 -pix_fmt yuv420p \
//!          -f yuv4mpegpipe t.y4m
//! $ ffprobe -show_streams -show_format t.y4m
//! ```
//!
//! * `probe_score` is **100** for a file starting with the exact 9-byte magic
//!   `YUV4MPEG2` — stronger than the generic "magic, nothing further checked"
//!   convention row (90), so this crate treats the whole header line as the
//!   self-consistency check the convention table asks for.
//! * `time_base` is exactly `1/F` from the header (`1/5` for the command
//!   above) — unlike the bitstream family (`crate::bitstream`), which fixes
//!   `1/1_200_000` regardless of the declared rate. Y4M is self-describing,
//!   so there is nothing to guess.
//! * `pts` is the frame index, `duration = 1`, every packet flagged `KEY`.
//!
//! Only the `C420jpeg`/`C420mpeg2`/`C420paldv`/`C420`/`C422`/`C444`/`Cmono`/
//! `Cgray` colorspace tags are mapped to a [`PixFmt`]; an unrecognised or
//! absent `C` tag falls back to the spec's own default, 4:2:0.

use vaco_codec_core::{CodecParameters, FieldOrder, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::time::duration_from_rate;
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, SeekFlags, SeekTarget, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;

pub const MAGIC: &[u8] = b"YUV4MPEG2";
const FRAME_TAG: &[u8] = b"FRAME";
/// Header/frame-marker lines are a handful of bytes in every real file; this
/// bounds the read against a hostile stream that never sends `\n`.
const MAX_LINE: usize = 4096;

fn read_line(io: &mut IoContext) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        if out.len() >= MAX_LINE {
            return Err(Error::InvalidData("yuv4mpegpipe line too long"));
        }
        let b = io.r8()?;
        if b == b'\n' {
            return Ok(out);
        }
        out.push(b);
    }
}

#[derive(Debug, Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    framerate: Rational,
    format: PixFmt,
    field_order: FieldOrder,
}

/// The `I` tag's one-letter value, measured against real `ffprobe` (a
/// `-vf setfield=prog|tff|bff` round-trip through `-f yuv4mpegpipe`, each
/// re-probed): `Ip` -> `progressive`, `It` -> `tt` (top field first), `Ib`
/// -> `bb` (bottom field first). Y4M carries whole deinterlaced frames, not
/// separately-coded fields, so this crate's own `TopFirst`/`BottomFirst`
/// map onto it directly -- `TopCodedFirst`/`BottomCodedFirst` are an H.264/
/// HEVC-style coded-vs-displayed distinction Y4M has no room to state.
/// `Im` (mixed) and an absent tag both become `Unknown`: mixed cannot be
/// expressed as one `FieldOrder` value, and the spec states the tag is
/// optional, though every sample this crate can produce (`ffmpeg` always
/// writes one) leaves that branch unmeasured against a real reference file.
fn interlacing(tag: &[u8]) -> FieldOrder {
    match tag {
        b"p" => FieldOrder::Progressive,
        b"t" => FieldOrder::TopFirst,
        b"b" => FieldOrder::BottomFirst,
        _ => FieldOrder::Unknown,
    }
}

fn colorspace(tag: &[u8]) -> PixFmt {
    match tag {
        b"422" => PixFmt::Yuv422p,
        b"444" => PixFmt::Yuv444p,
        b"mono" | b"gray" => PixFmt::Gray8,
        // `420jpeg`/`420mpeg2`/`420paldv`/`420`, and anything unrecognised,
        // all fall back to the spec's own default: plain 4:2:0.
        _ => PixFmt::Yuv420p,
    }
}

fn parse_ratio(s: &[u8]) -> Option<Rational> {
    let text = core::str::from_utf8(s).ok()?;
    let (n, d) = text.split_once(':')?;
    Some(Rational::new(n.parse().ok()?, d.parse().ok()?))
}

fn parse_header(line: &[u8]) -> Result<Header> {
    let rest = line
        .strip_prefix(MAGIC)
        .ok_or(Error::InvalidData("not a yuv4mpegpipe stream"))?;
    let mut width = None;
    let mut height = None;
    let mut framerate = Rational::new(25, 1);
    let mut format = PixFmt::Yuv420p;
    let mut field_order = FieldOrder::Unknown;
    for field in rest.split(|&b| b == b' ').filter(|f| !f.is_empty()) {
        let Some((&tag, value)) = field.split_first() else {
            continue;
        };
        match tag {
            b'W' => {
                width = core::str::from_utf8(value)
                    .ok()
                    .and_then(|s| s.parse().ok());
            }
            b'H' => {
                height = core::str::from_utf8(value)
                    .ok()
                    .and_then(|s| s.parse().ok());
            }
            b'F' => {
                if let Some(r) = parse_ratio(value) {
                    framerate = r;
                }
            }
            b'C' => format = colorspace(value),
            b'I' => field_order = interlacing(value),
            // `A` (aspect) and `X` (extension) tags are read past but not
            // otherwise interpreted.
            _ => {}
        }
    }
    let (Some(width), Some(height)) = (width, height) else {
        return Err(Error::InvalidData("yuv4mpegpipe header missing W or H"));
    };
    Ok(Header {
        width,
        height,
        framerate,
        format,
        field_order,
    })
}

/// The `yuv4mpegpipe` demuxer.
#[derive(Debug)]
pub struct Yuv4MpegDemuxer {
    io: IoContext,
    streams: [Stream; 1],
    budget: Budget,
    frame_size: usize,
    frames_read: u64,
    eof: bool,
}

impl Yuv4MpegDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] for a malformed or missing header; otherwise
    /// whatever the transport reports.
    pub fn open(src: Box<dyn MediaSource>, _parsers: &dyn ParserProvider) -> Result<Self> {
        Self::open_with_limits(src, Limits::permissive())
    }

    /// # Errors
    /// As [`Yuv4MpegDemuxer::open`].
    pub fn open_with_limits(src: Box<dyn MediaSource>, limits: Limits) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let line = read_line(&mut io)?;
        let header = parse_header(&line)?;
        let layout = header
            .format
            .plane_layout(header.width, header.height, 1)
            .map_err(|_| Error::InvalidData("yuv4mpegpipe geometry overflowed"))?;
        if layout.total == 0 {
            return Err(Error::InvalidData("yuv4mpegpipe frame size is zero"));
        }
        let time_base = header.framerate.inverse();
        let video = VideoParameters {
            width: header.width,
            height: header.height,
            coded_width: header.width,
            coded_height: header.height,
            frame_rate: header.framerate,
            format: Some(header.format),
            field_order: header.field_order,
            ..VideoParameters::default()
        };
        let mut params = CodecParameters::new(MediaType::Video);
        params.video = Some(video);
        let mut stream = Stream::new(0, MediaType::Video, time_base);
        stream.params = params;
        stream.metadata_set("raw_codec_name", "rawvideo");
        Ok(Self {
            io,
            streams: [stream],
            budget: Budget::new(limits),
            frame_size: layout.total,
            frames_read: 0,
            eof: false,
        })
    }
}

impl Demuxer for Yuv4MpegDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let line = match read_line(&mut self.io) {
            Ok(l) => l,
            Err(Error::Eof | Error::UnexpectedEof) => {
                self.eof = true;
                return Err(Error::Eof);
            }
            Err(e) => return Err(e),
        };
        if !line.starts_with(FRAME_TAG) {
            return Err(Error::InvalidData("expected a yuv4mpegpipe FRAME marker"));
        }
        let pos = self.io.pos();
        let mut pkt = Packet::alloc(&mut self.budget, self.frame_size)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::try_from(self.frames_read).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.duration = duration_from_rate(self.frame_rate()).unwrap_or(Duration::ZERO);
        pkt.pos = Some(pos);
        pkt.flags = PacketFlags::KEY;
        self.frames_read = self.frames_read.saturating_add(1);
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // The `FRAME\n` marker means frames are not fixed-stride in the
        // file, so a byte-offset seek cannot be computed without an index.
        // Not attempted: report the honest answer rather than guess.
        Err(Error::Unsupported(
            "yuv4mpegpipe seeking is not implemented",
        ))
    }
}

impl Yuv4MpegDemuxer {
    fn frame_rate(&self) -> Rational {
        self.streams[0]
            .params
            .video
            .as_ref()
            .map_or(Rational::new(25, 1), |v| v.frame_rate)
    }
}

fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(MAGIC) {
        ProbeScore::MAX
    } else {
        ProbeScore::from_extension(data, &["y4m"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(Yuv4MpegDemuxer::open(src, parsers)?))
}

pub const DEMUXER_YUV4MPEGPIPE: DemuxerDesc = DemuxerDesc {
    name: "yuv4mpegpipe",
    long_name: "YUV4MPEG pipe",
    extensions: &["y4m"],
    mime_types: &[],
    // A raw format carries no index of its own — the file *is* the
    // elementary stream — so the generic byte/timestamp index is what
    // seeks it. `GENERIC_INDEX` says that; `empty()` said nothing, and
    // `empty()` is not neutral: it silently opts into the monotonic-DTS
    // repair decision rather than expressing one, which is why
    // `every_registered_demuxer_declares_flags` refuses it.
    flags: vaco_format_core::FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    fn sample(width: u32, height: u32, frames: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(
            format!("YUV4MPEG2 W{width} H{height} F25:1 Ip A1:1 C420jpeg\n").as_bytes(),
        );
        let frame_size = (width as usize * height as usize * 3) / 2; // 4:2:0
        for i in 0..frames {
            v.extend_from_slice(b"FRAME\n");
            v.extend(std::iter::repeat_n(i as u8, frame_size));
        }
        v
    }

    #[test]
    fn header_and_frames_round_trip() {
        let bytes = sample(4, 4, 3);
        let src = Box::new(MemorySource::new(bytes));
        let mut d = Yuv4MpegDemuxer::open(src, &NoParsers).unwrap();
        assert_eq!(d.streams()[0].params.video.as_ref().unwrap().width, 4);
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 24); // 4*4*1.5
        assert_eq!(p0.pts.ticks(), Some(0));
        assert!(p0.is_key());
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.pts.ticks(), Some(1));
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.pts.ticks(), Some(2));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn the_magic_scores_max() {
        let bytes = sample(2, 2, 1);
        assert_eq!(probe(&ProbeData::new(&bytes)), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b"not it")), ProbeScore::NONE);
    }

    #[test]
    fn a_header_missing_width_is_rejected() {
        let bytes = b"YUV4MPEG2 H4 F25:1\nFRAME\n".to_vec();
        let src = Box::new(MemorySource::new(bytes));
        assert!(Yuv4MpegDemuxer::open(src, &NoParsers).is_err());
    }

    /// Measured against real `ffprobe` (see `interlacing`'s own doc
    /// comment): `Ip` -> `progressive`, `It` -> `tt`, `Ib` -> `bb`. An
    /// absent `I` tag reports `unknown`, the dedicated "not stated"
    /// sentinel -- distinct from a real `Ip` assertion, which is the
    /// distinction finding 63/64 exist for.
    #[test]
    fn the_interlace_tag_maps_to_the_measured_field_order() {
        let header = |line: &[u8]| parse_header(line).unwrap().field_order;
        assert_eq!(header(b"YUV4MPEG2 W4 H4 F25:1 Ip"), FieldOrder::Progressive);
        assert_eq!(header(b"YUV4MPEG2 W4 H4 F25:1 It"), FieldOrder::TopFirst);
        assert_eq!(header(b"YUV4MPEG2 W4 H4 F25:1 Ib"), FieldOrder::BottomFirst);
        // No `I` tag at all: unmeasured (every real encoder writes one),
        // but the spec states it is optional, and this crate must not
        // invent a `Progressive` where nothing was said.
        assert_eq!(header(b"YUV4MPEG2 W4 H4 F25:1"), FieldOrder::Unknown);
        // `Im` (mixed) cannot be expressed as one `FieldOrder` value.
        assert_eq!(header(b"YUV4MPEG2 W4 H4 F25:1 Im"), FieldOrder::Unknown);
    }
}
