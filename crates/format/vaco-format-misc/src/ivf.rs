//! IVF, the raw-frame container `libvpx`/`libaom` test tools use for VP8, VP9
//! and AV1 bitstreams. Little-endian throughout.
//!
//! ```text
//! header (32 bytes)
//!   0 "DKIF" | 4 version(0) | 6 header_len | 8 fourcc "VP80"/"VP90"/"AV01"
//!  12 width  | 14 height    | 16 frame_rate(rate) | 20 time_scale(scale)
//!  24 frame_count | 28 unused                       -- fps = rate/scale
//!
//! frame, repeated
//!   0 payload size | 4 pts, in scale/rate ticks (8 bytes) | 12 payload
//! ```
//!
//! Fixtures: `ffmpeg -c:v libvpx -f ivf`, `-c:v libvpx-vp9 -f ivf`,
//! `-c:v libsvtav1 -f ivf`.
//!
//! # Measured against the reference (`ffmpeg`/`ffprobe` 8.1)
//!
//! * `probe_score` is **98**, not 100 — identical whether the extension is
//!   `.ivf`, absent or wrong, and identical piped, so it is the magic alone.
//! * `r_frame_rate` is exactly `rate/scale`; `avg_frame_rate` stays `0/0`
//!   even though `frame_count` and a constant rate would allow computing it.
//! * `duration_ts`/`nb_frames` come from the header's `frame_count` field,
//!   not from counting the frames actually present.
//! * Only the codec's own keyframe bit decides `flags=K`: a 25-frame
//!   all-intra clip still shows `K` on frame 0 alone once real inter
//!   prediction is in use, so this module reads that bit rather than
//!   assuming every IVF frame is a keyframe. Per codec:
//!   `Vaco-Spec-Ref rfc-6386` §9.1, a VP8 frame's first byte's low bit is
//!   `frame_type` (0 = key); `Vaco-Spec-Ref vp9-bitstream-spec-v0.6` §6.2, a
//!   VP9 `uncompressed_header` is a 2-bit `frame_marker`, two profile bits
//!   (plus one more when profile is 3), a `show_existing_frame` bit, then —
//!   only when that bit is 0 — a `frame_type` bit; `Vaco-Spec-Ref
//!   aom-av1-spec` §5.3.1, an AV1 temporal unit is scanned for an OBU of
//!   type 1 (`OBU_SEQUENCE_HEADER`), a presence test rather than a full
//!   header parse, and a heuristic for that reason.
//!
//! `video.format` is left unset: the pixel format lives in the codec's own
//! sequence header, so `vaco-probe` prints `pix_fmt=unknown` where the
//! reference names one.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, ExactDuration, MediaType, Rational, Result, Timestamp};
use vaco_format_core::Stream;
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const MAGIC: [u8; 4] = *b"DKIF";
const HEADER_LEN_MIN: u16 = 32;

/// Measured against `ffprobe` 8.1: the magic alone scores 98, both with and
/// without a matching extension or a MIME type.
pub const IVF_SCORE: ProbeScore = ProbeScore(98);

/// Largest single frame this demuxer accepts before consulting the budget —
/// a structural bound, since `size` is a raw 32-bit field an attacker
/// controls completely.
const MAX_FRAME: u32 = 256 << 20;

const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.tag(0) == Some(MAGIC) {
        IVF_SCORE
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "ivf",
    long_name: "On2 IVF",
    extensions: &["ivf"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "ivf",
    long_name: "On2 IVF",
    extensions: &["ivf"],
    default_video: Some(CodecId::Vp8),
    default_audio: None,
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(IvfDemuxer::open(src)?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(IvfMuxer::new(sink)?))
}

fn codec_for_fourcc(fourcc: [u8; 4]) -> Option<CodecId> {
    match &fourcc {
        b"VP80" => Some(CodecId::Vp8),
        b"VP90" => Some(CodecId::Vp9),
        b"AV01" => Some(CodecId::Av1),
        _ => None,
    }
}

fn fourcc_for_codec(codec: CodecId) -> Option<[u8; 4]> {
    match codec {
        CodecId::Vp8 => Some(*b"VP80"),
        CodecId::Vp9 => Some(*b"VP90"),
        CodecId::Av1 => Some(*b"AV01"),
        _ => None,
    }
}

/// A big-endian-numbered, MSB-first bit cursor over a byte slice — what the
/// VP9 uncompressed header's bit-packed fields need and nothing more.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos >> 3)?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Some(b)
    }
}

/// `Vaco-Spec-Ref vp9-bitstream-spec-v0.6` §6.2 `uncompressed_header`.
fn vp9_is_keyframe(payload: &[u8]) -> bool {
    let mut r = BitReader::new(payload);
    let (Some(m1), Some(m0)) = (r.bit(), r.bit()) else {
        return false;
    };
    if (m1, m0) != (1, 0) {
        return false;
    }
    let (Some(profile_low), Some(profile_high)) = (r.bit(), r.bit()) else {
        return false;
    };
    let profile = (profile_high << 1) | profile_low;
    if profile == 3 && r.bit().is_none() {
        return false;
    }
    let Some(show_existing_frame) = r.bit() else {
        return false;
    };
    if show_existing_frame == 1 {
        // Points at an already-decoded frame; not itself a coded keyframe.
        return false;
    }
    r.bit() == Some(0)
}

const OBU_SEQUENCE_HEADER: u8 = 1;

/// `leb128`, `Vaco-Spec-Ref aom-av1-spec` §4.10.5: little-endian base-128,
/// at most 8 bytes for the sizes this format needs.
fn read_leb128(data: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    for i in 0..8 {
        let byte = *data.get(at.checked_add(i)?)?;
        value |= u64::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Whether any OBU in this temporal unit is a sequence header —
/// `Vaco-Spec-Ref aom-av1-spec` §5.3.1 OBU syntax. A presence test, not a
/// parse of `frame_type`: encoders emit a sequence header once per keyframe
/// access unit and this is cheap to check without decoding the frame header
/// itself, which needs the sequence header's own state to parse at all.
fn av1_has_sequence_header(payload: &[u8]) -> bool {
    let mut pos = 0usize;
    while let Some(&header) = payload.get(pos) {
        let obu_type = (header >> 3) & 0x0f;
        let extension_flag = (header >> 2) & 1;
        let has_size_field = (header >> 1) & 1;
        let mut cursor = pos.saturating_add(1);
        if extension_flag == 1 {
            cursor = cursor.saturating_add(1);
        }
        let obu_len = if has_size_field == 1 {
            let Some((len, used)) = read_leb128(payload, cursor) else {
                break;
            };
            cursor = cursor.saturating_add(used);
            usize::try_from(len).unwrap_or(usize::MAX)
        } else {
            payload.len().saturating_sub(cursor)
        };
        if obu_type == OBU_SEQUENCE_HEADER {
            return true;
        }
        pos = cursor.saturating_add(obu_len);
    }
    false
}

fn is_keyframe(codec: Option<CodecId>, payload: &[u8]) -> bool {
    match codec {
        Some(CodecId::Vp8) => payload.first().is_some_and(|&b| b & 0x01 == 0),
        Some(CodecId::Vp9) => vp9_is_keyframe(payload),
        Some(CodecId::Av1) => av1_has_sequence_header(payload),
        _ => false,
    }
}

#[derive(Debug)]
pub struct IvfDemuxer {
    io: IoContext,
    stream: Stream,
    codec: Option<CodecId>,
    data_start: u64,
    eof: bool,
}

impl IvfDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the header does not parse.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.tag()? != MAGIC {
            return Err(Error::InvalidData("ivf: missing DKIF signature"));
        }
        let _version = io.rl16()?;
        let header_len = io.rl16()?;
        if header_len < HEADER_LEN_MIN {
            return Err(Error::InvalidData(
                "ivf: header shorter than the fixed part",
            ));
        }
        let fourcc = io.tag()?;
        let width = u32::from(io.rl16()?);
        let height = u32::from(io.rl16()?);
        let rate = io.rl32()?.max(1);
        let scale = io.rl32()?.max(1);
        let frame_count = io.rl32()?;
        let _unused = io.rl32()?;
        io.seek(u64::from(header_len))?;

        let codec = codec_for_fourcc(fourcc);
        let time_base = Rational::new(scale.cast_signed(), rate.cast_signed());
        let mut params = CodecParameters::video();
        params.codec_id = codec;
        params.codec_tag = Some(fourcc);
        if let Some(v) = params.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.frame_rate = Rational::new(rate.cast_signed(), scale.cast_signed());
        }

        let mut stream = Stream::new(0, MediaType::Video, time_base);
        stream.params = params;
        stream.r_frame_rate = Rational::new(rate.cast_signed(), scale.cast_signed());
        if frame_count > 0 {
            stream.duration_ts = Some(i64::from(frame_count));
            stream.frame_count = Some(u64::from(frame_count));
        }

        Ok(Self {
            data_start: io.pos(),
            io,
            stream,
            codec,
            eof: false,
        })
    }

    fn read_one(&mut self, budget: &mut Budget) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let pos = self.io.pos();
        let size = match self.io.rl32() {
            Ok(v) => v,
            Err(Error::UnexpectedEof) => {
                self.eof = true;
                return Err(Error::Eof);
            }
            Err(e) => return Err(e),
        };
        if size > MAX_FRAME {
            return Err(Error::LimitExceeded {
                limit: "ivf_frame",
                requested: u64::from(size),
                cap: u64::from(MAX_FRAME),
            });
        }
        if let Some(total) = self.io.size()
            && u64::from(size) > total.saturating_sub(self.io.pos()).saturating_add(8)
        {
            return Err(Error::InvalidData(
                "ivf: frame claims more bytes than remain",
            ));
        }
        let ts = self.io.rl64()?.cast_signed();
        let n = usize::try_from(size).unwrap_or(usize::MAX);
        let mut pkt = Packet::alloc(budget, n)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(ts);
        pkt.dts = Timestamp::new(ts);
        pkt.pos = Some(pos);
        if is_keyframe(self.codec, pkt.payload()) {
            pkt.flags = PacketFlags::KEY;
        }
        Ok(pkt)
    }
}

impl Demuxer for IvfDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        self.read_one(&mut budget)
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        if flags.contains(SeekFlags::BYTE) {
            let SeekTarget::Byte(pos) = target else {
                return Err(Error::Unsupported("ivf: BYTE flag needs a byte target"));
            };
            self.io.seek(pos.max(self.data_start))?;
            self.eof = false;
            return Ok(());
        }
        let target = target.resolve_frames(self.stream.r_frame_rate, self.stream.time_base)?;
        let SeekTarget::Timestamp { stream_index, ts } = target else {
            return Err(Error::Unsupported("ivf: unsupported seek target"));
        };
        if stream_index != 0 {
            return Err(Error::InvalidData("ivf: no such stream"));
        }
        let want = ts.ticks().unwrap_or(0);

        self.io.seek(self.data_start)?;
        self.eof = false;
        let mut landing = self.data_start;
        loop {
            let pos = self.io.pos();
            let size = match self.io.rl32() {
                Ok(v) => v,
                Err(Error::UnexpectedEof) => {
                    self.io.seek(landing)?;
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            let frame_ts = self.io.rl64()?.cast_signed();
            if frame_ts >= want {
                if flags.contains(SeekFlags::BACKWARD) && frame_ts > want && pos != self.data_start
                {
                    self.io.seek(landing)?;
                } else {
                    self.io.seek(pos)?;
                }
                return Ok(());
            }
            landing = pos;
            self.io.skip(u64::from(size))?;
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.stream.duration()
    }

    fn duration_exact(&self) -> Option<ExactDuration> {
        self.stream.duration_exact()
    }
}

#[derive(Debug)]
pub struct IvfMuxer {
    out: IoWriter,
    codec: Option<CodecId>,
    width: u32,
    height: u32,
    time_base: Rational,
    frame_count: u32,
    header_written: bool,
}

impl IvfMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            codec: None,
            width: 0,
            height: 0,
            time_base: Rational::new(1, 30),
            frame_count: 0,
            header_written: false,
        })
    }
}

impl Muxer for IvfMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.codec.is_some() {
            return Err(Error::Unsupported(
                "ivf: only one video stream is supported",
            ));
        }
        let video = params
            .video
            .as_ref()
            .ok_or(Error::InvalidData("ivf: not a video stream"))?;
        let codec_id = params
            .codec_id
            .ok_or(Error::Unsupported("ivf: stream has no codec_id"))?;
        fourcc_for_codec(codec_id).ok_or(Error::Unsupported("ivf: codec has no IVF fourcc"))?;
        self.codec = Some(codec_id);
        self.width = video.width;
        self.height = video.height;
        if video.frame_rate.is_defined() && !video.frame_rate.is_zero() {
            self.time_base = video.frame_rate.inverse();
        }
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let codec = self
            .codec
            .ok_or(Error::InvalidData("ivf: no stream added"))?;
        let fourcc = fourcc_for_codec(codec).unwrap_or(*b"\0\0\0\0");
        self.out.write(&MAGIC)?;
        self.out.wl16(0)?;
        self.out.wl16(32)?;
        self.out.write(&fourcc)?;
        self.out
            .wl16(u16::try_from(self.width.min(u32::from(u16::MAX))).unwrap_or(u16::MAX))?;
        self.out
            .wl16(u16::try_from(self.height.min(u32::from(u16::MAX))).unwrap_or(u16::MAX))?;
        // `time_base` is `scale/rate`, so `rate` is the denominator and
        // `scale` the numerator of the stored fraction (measured: a 25 fps
        // stream carries `rate=25` at offset 16, `scale=1` at offset 20).
        self.out.wl32(self.time_base.den.max(1).cast_unsigned())?;
        self.out.wl32(self.time_base.num.max(1).cast_unsigned())?;
        self.out.wl32(0)?; // frame_count, patched in write_trailer
        self.out.wl32(0)?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("ivf: packet written before the header"));
        }
        let ts = packet.pts.ticks().or(packet.dts.ticks()).unwrap_or(0);
        self.out.wl32(
            u32::try_from(packet.payload().len()).map_err(|_| {
                Error::InvalidData("ivf: frame too large for the 32-bit size field")
            })?,
        )?;
        self.out.wl64(ts.cast_unsigned())?;
        self.out.write(packet.payload())?;
        self.frame_count = self.frame_count.saturating_add(1);
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        (stream_index == 0 && self.codec.is_some()).then_some(self.time_base)
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("ivf: trailer written before the header"));
        }
        if !self.out.is_seekable() {
            return self.out.flush();
        }
        let end = self.out.pos();
        self.out.seek(24)?;
        self.out.wl32(self.frame_count)?;
        self.out.seek(end)?;
        self.out.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::probe::ProbeData;
    use vaco_io::MemorySource;

    fn vp8_frame(key: bool) -> Vec<u8> {
        let mut v = vec![if key { 0x30 } else { 0x31 }, 0, 0];
        v.extend_from_slice(&[0u8; 5]);
        v
    }

    #[test]
    fn probe_requires_dkif() {
        assert_eq!(probe(&ProbeData::new(b"DKIF")), IVF_SCORE);
        assert_eq!(probe(&ProbeData::new(b"not an ivf file")), ProbeScore::NONE);
    }

    fn header(fourcc: [u8; 4], frames: &[(bool, &[u8])]) -> Vec<u8> {
        header_at_rate(fourcc, 25, 1, frames)
    }

    fn header_at_rate(fourcc: [u8; 4], rate: u32, scale: u32, frames: &[(bool, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"DKIF");
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&32u16.to_le_bytes());
        buf.extend_from_slice(&fourcc);
        buf.extend_from_slice(&64u16.to_le_bytes());
        buf.extend_from_slice(&48u16.to_le_bytes());
        buf.extend_from_slice(&rate.to_le_bytes());
        buf.extend_from_slice(&scale.to_le_bytes());
        buf.extend_from_slice(&(frames.len() as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        for (i, (_key, payload)) in frames.iter().enumerate() {
            buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(i as u64).to_le_bytes());
            buf.extend_from_slice(payload);
        }
        buf
    }

    #[test]
    fn aggregate_duration_keeps_native_frame_ticks_exact() {
        let key = vp8_frame(true);
        let data = header_at_rate(*b"VP80", 30_000, 1_001, &[(true, &key)]);
        let mut d = IvfDemuxer::open(Box::new(MemorySource::new(data))).unwrap();

        assert_eq!(d.streams().first().unwrap().duration_ts, Some(1));
        assert_eq!(
            d.duration_exact().map(vaco_core::ExactDuration::as_ratio),
            Some((1_001, 30_000))
        );
        assert_eq!(d.read_packet().unwrap().pts.ticks(), Some(0));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn reads_header_and_frames() {
        let key = vp8_frame(true);
        let inter = vp8_frame(false);
        let data = header(*b"VP80", &[(true, &key), (false, &inter)]);
        let mut d = IvfDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams().len(), 1);
        assert_eq!(
            d.streams().first().unwrap().params.codec_id,
            Some(CodecId::Vp8)
        );
        assert_eq!(d.streams().first().unwrap().frame_count, Some(2));

        let p0 = d.read_packet().unwrap();
        assert!(p0.is_key());
        assert_eq!(p0.pts.ticks(), Some(0));
        let p1 = d.read_packet().unwrap();
        assert!(!p1.is_key());
        assert_eq!(p1.pts.ticks(), Some(1));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn unknown_fourcc_has_no_codec_id_but_still_frames() {
        let data = header(*b"XYZW", &[(true, &[1, 2, 3])]);
        let mut d = IvfDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams().first().unwrap().params.codec_id, None);
        assert_eq!(
            d.streams().first().unwrap().params.codec_tag,
            Some(*b"XYZW")
        );
        let p0 = d.read_packet().unwrap();
        assert!(!p0.is_key());
    }

    #[test]
    fn rejects_short_header() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"DKIF");
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        assert!(IvfDemuxer::open(Box::new(MemorySource::new(buf))).is_err());
    }

    #[test]
    fn vp8_keyframe_bit_is_the_low_bit_of_byte_zero() {
        assert!(is_keyframe(Some(CodecId::Vp8), &[0b0000_0000]));
        assert!(!is_keyframe(Some(CodecId::Vp8), &[0b0000_0001]));
    }

    #[test]
    fn vp9_keyframe_needs_frame_marker_and_frame_type() {
        // frame_marker=10, profile=00, show_existing_frame=0, frame_type=0
        assert!(vp9_is_keyframe(&[0b1000_0000]));
        // frame_type=1 (non-key)
        assert!(!vp9_is_keyframe(&[0b1000_0100]));
        // show_existing_frame=1: never a keyframe regardless of what follows
        assert!(!vp9_is_keyframe(&[0b1000_1000, 0]));
        // wrong frame_marker
        assert!(!vp9_is_keyframe(&[0b0100_0000]));
    }

    #[test]
    fn av1_sequence_header_obu_marks_a_keyframe() {
        // OBU header: type=1 (sequence header), has_size_field=1; size=1; one payload byte.
        let obu = [(1 << 3) | 0x02, 0x01, 0x00];
        assert!(av1_has_sequence_header(&obu));
        // type=6 (frame), no sequence header anywhere.
        let obu = [(6 << 3) | 0x02, 0x01, 0x00];
        assert!(!av1_has_sequence_header(&obu));
    }

    #[test]
    fn muxer_round_trips_through_the_demuxer() {
        use vaco_format_core::vacoraw::MemorySink;

        let sink = MemorySink::new();
        let written = sink.shared();
        let mut mux = IvfMuxer::new(Box::new(sink)).unwrap();
        let mut params = CodecParameters::video().with_codec(CodecId::Vp9);
        if let Some(v) = params.video.as_mut() {
            v.width = 64;
            v.height = 48;
            v.frame_rate = Rational::new(25, 1);
        }
        mux.add_stream(&params).unwrap();
        mux.write_header().unwrap();
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        for i in 0..3u64 {
            let mut pkt = Packet::from_slice(&mut budget, &vp8_frame(i == 0)).unwrap();
            pkt.pts = Timestamp::new(i.cast_signed());
            pkt.dts = pkt.pts;
            mux.write_packet(&pkt).unwrap();
        }
        mux.write_trailer().unwrap();

        let bytes = written.snapshot();
        let mut demux = IvfDemuxer::open(Box::new(MemorySource::new(bytes))).unwrap();
        assert_eq!(demux.streams().first().unwrap().frame_count, Some(3));
        assert_eq!(
            demux.streams().first().unwrap().params.codec_id,
            Some(CodecId::Vp9)
        );
        for i in 0..3u64 {
            let pkt = demux.read_packet().unwrap();
            assert_eq!(pkt.pts.ticks(), Some(i.cast_signed()));
        }
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }
}
