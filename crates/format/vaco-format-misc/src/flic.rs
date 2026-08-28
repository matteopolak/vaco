//! Autodesk Animator FLIC (`.fli`/`.flc`), the palette-animation format that
//! predates every game-video codec in this crate by close to a decade.
//!
//! `Vaco-Spec-Ref compuphase-flic-doc` gives the header, frame and chunk
//! layout below (the format's canonical public writeup, since Autodesk never
//! published one itself).
//!
//! # Layout
//!
//! A 128-byte file header, then a flat sequence of 16-byte-headed frame
//! chunks, each containing its own sub-chunks:
//!
//! ```text
//! file header (128 bytes)
//!    4   2   type        0xAF11 FLI, 0xAF12 FLC, 0xAF44 (non-8-bit)
//!    6   2   frames
//!    8   2   width
//!   10   2   height
//!   12   2   depth
//!   16   4   speed       FLI: ticks of 1/70 s; FLC/0xAF44: milliseconds
//!
//! frame, repeated
//!    0   4   size        this frame's bytes, header included
//!    4   2   type        0xF1FA
//!    6   2   chunk_count
//!    8   2   delay
//!   16   …   `chunk_count` sub-chunks (size:u32, type:u16, payload — not
//!            interpreted by this module)
//! ```
//!
//! # Measured against the reference (`ffprobe` 8.1)
//!
//! No encoder exists for this format either, so every fixture here is
//! hand-built from the layout above. Findings, from comparing hand-built
//! files against what `ffprobe` reports:
//!
//! * `probe_score` is **99**, magic alone, for all three `type` values
//!   above; `0xAF30`/`0xAF31` (documented compression variants) and anything
//!   else are rejected outright rather than scored lower.
//! * A packet is one whole frame chunk — header and every sub-chunk,
//!   unmodified — never the sub-chunks split apart. `size` is trusted for
//!   framing; the file header's own `frames` count is not — a header
//!   claiming 100 frames over a file that physically holds three still
//!   yields exactly three packets, and `nb_frames` is not printed at all.
//! * Only **frame index 0** is a keyframe, regardless of which chunk type
//!   it uses. A `BLACK` frame (id 13, no image data at all) at index 0
//!   still reports `flags=K`, and a `BYTE_RUN` frame (id 15, a full
//!   from-scratch image) at index 1 does not. Keyframe-ness is therefore
//!   purely positional here, not a property of the chunk type — confirmed by
//!   swapping which chunk type sits at index 0 and watching the flag follow
//!   the position.
//! * `extradata` is the **entire 128-byte file header**, verbatim.
//! * `r_frame_rate` is `70/speed` for `type == 0xAF11` and `1000/speed`
//!   otherwise, **reduced** — `speed=66` measures as `500/33`, not the
//!   unreduced `1000/66`; both formulas were checked independently
//!   (`speed=7` on FLI gives `10/1`), not assumed from one and applied to
//!   the other. `avg_frame_rate` stays `0/0`, the same `ivf` quirk this
//!   crate's other constant-frame-rate format shows (and `roq`, in the same
//!   crate, does not — see its own module doc).
//!
//! # What is not implemented
//!
//! * `FLIC` has no [`CodecId`] variant in `vaco-codec-core`; `codec_id` is
//!   `None` here (see the crate-level report for the full list of missing
//!   game-video codec ids).
//! * The `PREFIX_TYPE` (`0xF100`) top-level chunk, used only by the
//!   still-image "CEL" sibling format, is not handled — this module expects
//!   every top-level chunk after the file header to be a `0xF1FA` frame, and
//!   returns [`vaco_core::Error::InvalidData`] on anything else.

use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const HEADER_LEN: u64 = 128;
const FRAME_MAGIC: u16 = 0xF1FA;
const MAGIC_FLI: u16 = 0xAF11;
const MAGIC_FLC: u16 = 0xAF12;
const MAGIC_FLC_HIGH_COLOR: u16 = 0xAF44;

/// Measured against `ffprobe` 8.1: the three accepted `type` values all
/// score 99, magic alone, independent of extension.
const FLIC_SCORE: ProbeScore = ProbeScore(99);

const MAX_FRAME: u32 = 128 << 20;

const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

fn le16(buf: &[u8], at: usize) -> u16 {
    buf.get(at..at.saturating_add(2))
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map_or(0, u16::from_le_bytes)
}

fn le32(buf: &[u8], at: usize) -> u32 {
    buf.get(at..at.saturating_add(4))
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, u32::from_le_bytes)
}

fn is_flic_magic(magic: u16) -> bool {
    matches!(magic, MAGIC_FLI | MAGIC_FLC | MAGIC_FLC_HIGH_COLOR)
}

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    match data.rl16(4) {
        Some(m) if is_flic_magic(m) => FLIC_SCORE,
        _ => ProbeScore::NONE,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "flic",
    long_name: "FLI/FLC/FLX animation",
    extensions: &["fli", "flc", "flx"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(FlicDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct FlicDemuxer {
    io: IoContext,
    stream: Stream,
    budget: Budget,
    frame_index: i64,
    eof: bool,
}

impl FlicDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the 128-byte header does not parse.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut header = [0u8; HEADER_LEN as usize];
        io.read_exact(&mut header)?;
        let magic = le16(&header, 4);
        if !is_flic_magic(magic) {
            return Err(Error::InvalidData("flic: unrecognised file type field"));
        }
        let width = u32::from(le16(&header, 8));
        let height = u32::from(le16(&header, 10));
        let speed = le32(&header, 16).max(1);

        let time_base = if magic == MAGIC_FLI {
            Rational::new(speed.cast_signed(), 70)
        } else {
            Rational::new(speed.cast_signed(), 1000)
        }
        .reduced();
        let mut stream = Stream::new(0, MediaType::Video, time_base);
        stream.r_frame_rate = time_base.inverse();
        let mut params = vaco_codec_core::CodecParameters::video();
        if let Some(v) = params.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.field_order = vaco_codec_core::FieldOrder::Unknown;
        }
        params.extradata = Some(header.to_vec());
        stream.params = params;

        Ok(Self {
            io,
            stream,
            budget: Budget::new(vaco_limits::Limits::permissive()),
            frame_index: 0,
            eof: false,
        })
    }
}

impl Demuxer for FlicDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
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
        let frame_type = self.io.rl16()?;
        if frame_type != FRAME_MAGIC {
            return Err(Error::InvalidData("flic: expected a frame chunk (0xF1FA)"));
        }
        if !(16..=MAX_FRAME).contains(&size) {
            return Err(Error::LimitExceeded {
                limit: "flic_frame",
                requested: u64::from(size),
                cap: u64::from(MAX_FRAME),
            });
        }
        let n = usize::try_from(size).unwrap_or(usize::MAX);
        let mut pkt = Packet::alloc(&mut self.budget, n)?;
        {
            let buf = pkt.payload_mut();
            let split = 6.min(buf.len());
            let (head, tail) = buf.split_at_mut(split);
            if let Some(dst) = head.get_mut(0..4) {
                dst.copy_from_slice(&size.to_le_bytes());
            }
            if let Some(dst) = head.get_mut(4..6) {
                dst.copy_from_slice(&frame_type.to_le_bytes());
            }
            self.io.read_exact(tail)?;
        }
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(self.frame_index);
        pkt.dts = pkt.pts;
        pkt.pos = Some(pos);
        if self.frame_index == 0 {
            pkt.flags = PacketFlags::KEY;
        }
        self.frame_index = self.frame_index.saturating_add(1);
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::Unsupported("flic: seeking is not implemented"))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn header(magic: u16, width: u16, height: u16, speed: u32) -> Vec<u8> {
        let mut h = vec![0u8; 128];
        h[4..6].copy_from_slice(&magic.to_le_bytes());
        h[8..10].copy_from_slice(&width.to_le_bytes());
        h[10..12].copy_from_slice(&height.to_le_bytes());
        h[16..20].copy_from_slice(&speed.to_le_bytes());
        h
    }

    fn subchunk(ctype: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(6 + payload.len() as u32).to_le_bytes());
        v.extend_from_slice(&ctype.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn frame(subchunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = subchunks.iter().flatten().copied().collect();
        let mut v = Vec::new();
        v.extend_from_slice(&(16 + body.len() as u32).to_le_bytes());
        v.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        v.extend_from_slice(&(subchunks.len() as u16).to_le_bytes());
        v.extend_from_slice(&66u16.to_le_bytes());
        v.extend_from_slice(&[0u8; 6]);
        v.extend_from_slice(&body);
        v
    }

    #[test]
    fn probe_accepts_the_three_documented_magics_only() {
        for m in [MAGIC_FLI, MAGIC_FLC, MAGIC_FLC_HIGH_COLOR] {
            let mut buf = vec![0u8; 8];
            buf[4..6].copy_from_slice(&m.to_le_bytes());
            assert_eq!(probe(&ProbeData::new(&buf)), FLIC_SCORE);
        }
        let mut buf = vec![0u8; 8];
        buf[4..6].copy_from_slice(&0xAF30u16.to_le_bytes());
        assert_eq!(probe(&ProbeData::new(&buf)), ProbeScore::NONE);
    }

    #[test]
    fn only_the_first_frame_is_a_keyframe() {
        let mut data = header(MAGIC_FLC, 64, 48, 66);
        data.extend(frame(&[subchunk(13, &[])]));
        data.extend(frame(&[subchunk(15, &[0; 8])]));
        let mut d = FlicDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let p0 = d.read_packet().unwrap();
        assert!(p0.is_key());
        assert_eq!(p0.payload().len(), 22);
        let p1 = d.read_packet().unwrap();
        assert!(!p1.is_key());
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn frame_count_in_the_header_is_not_trusted() {
        let mut data = header(MAGIC_FLC, 64, 48, 66);
        data[6..8].copy_from_slice(&100u16.to_le_bytes());
        data.extend(frame(&[subchunk(13, &[])]));
        let mut d = FlicDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert!(d.read_packet().is_ok());
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn frame_rate_formula_differs_between_fli_and_flc() {
        let mut fli = header(MAGIC_FLI, 64, 48, 7);
        fli.extend(frame(&[subchunk(13, &[])]));
        let d = FlicDemuxer::open(Box::new(MemorySource::new(fli))).unwrap();
        assert_eq!(d.streams().first().unwrap().r_frame_rate, Rational::new(10, 1));

        let mut flc = header(MAGIC_FLC, 64, 48, 66);
        flc.extend(frame(&[subchunk(13, &[])]));
        let d = FlicDemuxer::open(Box::new(MemorySource::new(flc))).unwrap();
        assert_eq!(
            d.streams().first().unwrap().r_frame_rate,
            Rational::new(1000, 66)
        );
    }

    #[test]
    fn extradata_is_the_whole_file_header() {
        let mut data = header(MAGIC_FLC, 64, 48, 66);
        data.extend(frame(&[subchunk(13, &[])]));
        let d = FlicDemuxer::open(Box::new(MemorySource::new(data.clone()))).unwrap();
        assert_eq!(
            d.streams()
                .first()
                .unwrap()
                .params
                .extradata
                .as_deref(),
            Some(&data[..128])
        );
    }

    #[test]
    fn rejects_unrecognised_magic() {
        let data = header(0x1234, 64, 48, 66);
        assert!(FlicDemuxer::open(Box::new(MemorySource::new(data))).is_err());
    }
}
