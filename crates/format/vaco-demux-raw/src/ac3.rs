//! The `ac3` and `eac3` raw elementary-stream demuxers.
//!
//! Both formats share one syncword (`0x0B77`) and one accident of bit layout:
//! `bsid` — the field that tells the two formats apart — sits at the same
//! byte offset (top five bits of byte 5) in both, because the fields before it
//! happen to sum to exactly 40 bits in each (`16+16+2+6` for classic AC-3;
//! `16+2+3+11+2+2+3+1` for E-AC-3). [`parse`] reads it first and dispatches.
//!
//! # Frame size, measured rather than tabulated
//!
//! Classic AC-3 states a `frmsizecod` that indexes a bit-rate table; the frame
//! size in 16-bit words is `bit_rate_kbps * 1536 * 1000 / (sample_rate * 16)`,
//! floored, with one extra word added at 44.1 kHz when `frmsizecod` is odd —
//! the padding bit `ffmpeg -c:a ac3` implements. Verified against real encodes
//! rather than trusted from a transcribed table:
//!
//! | fixture | fscod | frmsizecod | `bit_rate` | frame bytes (measured) | formula |
//! |---|---|---|---|---|---|
//! | `ac3.ac3` | 0 (48k) | 20 | 192k | 768 | 192*1536*1000/(48000*16) = 768 |
//! | `ac3_384.ac3` | 0 (48k) | 28 | 384k | 1536 | 384*1536*1000/(48000*16) = 1536 |
//! | `ac3_44.ac3` frame 0 | 1 (44.1k) | 20 (even) | 192k | 834 | floor(192*1536*1000/(44100*16)) = 834 |
//! | `ac3_44.ac3` frame 1+ | 1 (44.1k) | 21 (odd) | 192k | 836 | 834 + 1 (odd code) |
//!
//! E-AC-3 states its frame size directly (`frmsiz`, frame bytes
//! `(frmsiz+1)*2`) — no table at all, confirmed against `eac3.eac3`'s
//! `frmsiz=895 -> 1792` bytes, which also matches `ffprobe`'s reported size.
//!
//! # Time base
//!
//! Fixed `1/90000` regardless of sample rate — measured identically on a
//! 48 kHz and a 44.1 kHz fixture (`ac3.ac3`, `ac3_44.ac3`), both via
//! `ffprobe -show_streams`. Per-packet duration is
//! `(samples_per_frame * 90000) / sample_rate`, floored: `ac3_44.ac3` reports
//! a constant `duration=3134`, and `1536*90000/44100` floors to exactly that
//! (not 3135, the rounded value) — confirmed with exact integer arithmetic
//! after an initial slip that computed 3135.

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// `Table 5.18`-equivalent bit-rate list, indexed by `frmsizecod >> 1`. Stated
/// directly by the specification (19 standard rates), not `FFmpeg`'s expression
/// of it — this is the input to the frame-size *formula* above, not a
/// transcribed byte-count table.
const BITRATES_KBPS: [u16; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

/// `fscod` -> sample rate, both formats.
const SAMPLE_RATES: [u32; 3] = [48000, 44100, 32000];

/// `numblkscod` -> blocks per E-AC-3 syncframe (classic AC-3 has none of this
/// syntax and is always 6 blocks / 1536 samples).
const NUMBLKS: [u32; 4] = [1, 2, 3, 6];

const SAMPLES_PER_BLOCK: u32 = 256;
const TIME_BASE: Rational = Rational {
    num: 1,
    den: 90_000,
};
const SYNCWORD: [u8; 2] = [0x0B, 0x77];

/// Consecutive bytes tolerated while resynchronising before giving up.
const MAX_RESYNC: u32 = 64 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct SyncFrame {
    pub frame_size: usize,
    pub sample_rate: u32,
    pub samples: u32,
    pub channels: ChannelLayout,
    pub bit_rate_kbps: Option<u32>,
    pub is_eac3: bool,
}

/// The acmod -> speaker-position table, ATSC A/52 §5.3.2.4 (Table 5.8 /
/// Table 5.9's channel array assignment). `acmod` 0 is dual mono (two
/// independent programme channels rather than a stereo pair), approximated
/// here as [`ChannelLayout::STEREO`] since the reference reports it as
/// `stereo` too; every other entry is a positional layout.
fn acmod_layout(acmod: u32, lfeon: bool) -> ChannelLayout {
    use vaco_chlayout::Channel;
    let base: &[Channel] = match acmod {
        0 | 2 => &[Channel::FrontLeft, Channel::FrontRight],
        1 => &[Channel::FrontCenter],
        3 => &[
            Channel::FrontLeft,
            Channel::FrontCenter,
            Channel::FrontRight,
        ],
        4 => &[Channel::FrontLeft, Channel::FrontRight, Channel::BackCenter],
        5 => &[
            Channel::FrontLeft,
            Channel::FrontCenter,
            Channel::FrontRight,
            Channel::BackCenter,
        ],
        6 => &[
            Channel::FrontLeft,
            Channel::FrontRight,
            Channel::SideLeft,
            Channel::SideRight,
        ],
        _ => &[
            Channel::FrontLeft,
            Channel::FrontCenter,
            Channel::FrontRight,
            Channel::SideLeft,
            Channel::SideRight,
        ],
    };
    let mut mask = 0u64;
    for ch in base {
        if let Some(bit) = ch.bit() {
            mask |= 1u64 << bit;
        }
    }
    if lfeon && let Some(bit) = Channel::LowFrequency.bit() {
        mask |= 1u64 << bit;
    }
    ChannelLayout::from_mask(mask).unwrap_or_else(|| {
        let channels = base.len() as u32 + u32::from(lfeon);
        ChannelLayout::unspecified(channels)
    })
}

/// Parse one syncframe header. Reads at most the first 8 bytes; the caller is
/// responsible for having at least that many available (`None` otherwise).
///
/// `bsid` sits at the same offset in both formats (see module docs) and is
/// what dispatches: `<= 10` is classic AC-3 (8 is standard; a handful of
/// legacy encoders used 6/9/10 for backward-compatible variants that share
/// this exact syncinfo layout), `11..=16` is E-AC-3 (16 is the only value the
/// specification defines; the rest are accepted leniently since the syncinfo
/// layout does not change under them).
pub(crate) fn parse(buf: &[u8]) -> Option<SyncFrame> {
    let head: &[u8; 8] = buf.first_chunk()?;
    if head[0] != SYNCWORD[0] || head[1] != SYNCWORD[1] {
        return None;
    }
    let bsid = u32::from(head[5]) >> 3;
    if bsid <= 10 {
        parse_ac3(buf)
    } else if bsid <= 16 {
        parse_eac3(buf)
    } else {
        None
    }
}

/// `bit_rate_kbps * 1536 * 1000 / (sample_rate * 16)`, floored. Exact for
/// every standard rate at 48 kHz and 32 kHz (verified: `sample_rate * 16`
/// divides `bit_rate_kbps * 1536 * 1000` for all 19 rates at both); at
/// 44.1 kHz it floors and [`parse_ac3`] adds the one-word pad the odd
/// `frmsizecod` codes state. See the module docs for the measured fixture
/// values this was checked against.
#[allow(
    clippy::integer_division,
    reason = "the AC-3 frame-size formula is an intentional floor, not a precision loss"
)]
const fn frame_words(bit_rate_kbps: u16, sample_rate: u32) -> u64 {
    (bit_rate_kbps as u64 * 1536 * 1000) / (sample_rate as u64 * 16)
}

/// `samples * ticks_per_sample_num / sample_rate`, floored — the same
/// intentional floor `ffprobe` uses for a raw AC-3/E-AC-3 packet's
/// `pts`/`duration` (measured: `ac3_44.ac3`'s constant `duration=3134` is
/// `floor(1536*90000/44100)`, not the rounded 3135).
#[allow(
    clippy::integer_division,
    reason = "matches the reference's own floor when converting a sample count to 1/90000 ticks"
)]
const fn ticks(samples: u64, ticks_per_sample_num: u64, sample_rate: u64) -> u64 {
    samples.saturating_mul(ticks_per_sample_num) / sample_rate
}

fn parse_ac3(buf: &[u8]) -> Option<SyncFrame> {
    let mut r = BitReader::new(buf);
    r.skip(16); // syncword
    r.skip(16); // crc1
    let fscod = r.get(2);
    let frmsizecod = r.get(6);
    if fscod == 3 || frmsizecod > 37 {
        return None;
    }
    let sample_rate = *SAMPLE_RATES.get(fscod as usize)?;
    let bitrate_kbps = *BITRATES_KBPS.get((frmsizecod >> 1) as usize)?;
    let base = frame_words(bitrate_kbps, sample_rate);
    let extra = u64::from(fscod == 1 && frmsizecod & 1 == 1);
    let words = base.checked_add(extra)?;
    let frame_size = usize::try_from(words.checked_mul(2)?).ok()?;

    r.skip(5); // bsid, already read by `parse` for dispatch
    let _bsmod = r.get(3);
    let acmod = r.get(3);
    if acmod & 0x1 != 0 && acmod != 0x1 {
        r.skip(2); // cmixlev
    }
    if acmod & 0x4 != 0 {
        r.skip(2); // surmixlev
    }
    if acmod == 0x2 {
        r.skip(2); // dsurmod
    }
    let lfeon = r.get_bit() != 0;
    if r.check().is_err() {
        return None;
    }
    Some(SyncFrame {
        frame_size,
        sample_rate,
        samples: 1536,
        channels: acmod_layout(acmod, lfeon),
        bit_rate_kbps: Some(u32::from(bitrate_kbps)),
        is_eac3: false,
    })
}

fn parse_eac3(buf: &[u8]) -> Option<SyncFrame> {
    let mut r = BitReader::new(buf);
    r.skip(16); // syncword
    let _strmtyp = r.get(2);
    let _substreamid = r.get(3);
    let frmsiz = r.get(11);
    let frame_size = usize::try_from(frmsiz.checked_add(1)?)
        .ok()?
        .checked_mul(2)?;
    let fscod = r.get(2);
    let (sample_rate, samples) = if fscod == 3 {
        let fscod2 = r.get(2);
        // The reduced sample rate is exactly half; every entry in
        // `SAMPLE_RATES` is even, so a right shift is exact.
        let sr = *SAMPLE_RATES.get(fscod2 as usize)? >> 1;
        (sr, 1536)
    } else {
        let numblkscod = r.get(2);
        let sr = *SAMPLE_RATES.get(fscod as usize)?;
        let blocks = *NUMBLKS.get(numblkscod as usize)?;
        (sr, blocks * SAMPLES_PER_BLOCK)
    };
    let acmod = r.get(3);
    let lfeon = r.get_bit() != 0;
    if r.check().is_err() {
        return None;
    }
    // No stated bit rate field; derived from what the frame actually spends,
    // matching `eac3.eac3`'s measured `bit_rate=448000` for a 1792-byte,
    // 1536-sample, 48 kHz frame: 1792*8*48000/1536 = 448000.
    let bit_rate_kbps = (frame_size as u64)
        .checked_mul(8)
        .and_then(|v| v.checked_mul(u64::from(sample_rate)))
        .and_then(|v| v.checked_div(u64::from(samples.max(1))))
        .and_then(|bps| bps.checked_div(1000))
        .and_then(|kbps| u32::try_from(kbps).ok());
    Some(SyncFrame {
        frame_size,
        sample_rate,
        samples,
        channels: acmod_layout(acmod, lfeon),
        bit_rate_kbps,
        is_eac3: true,
    })
}

// ------------------------------------------------------------------- probing

/// Consecutive chained frames required before this claims the reference's
/// measured score. Same shape and same constant as `vaco-demux-mpegaudio`'s
/// probe, and for the same reason: `0x0B77` is sixteen bits, not eleven, but
/// a raw stream is exactly the kind of file an unrelated format's payload can
/// coincidentally contain two bytes of.
const STRONG_RUN: u32 = 4;
const MIN_RUN: u32 = 2;
const SCORE_STRONG: ProbeScore = ProbeScore(51);
const SCORE_WEAK: ProbeScore = ProbeScore(24);

fn chained_run(data: &ProbeData<'_>, want_eac3: bool) -> u32 {
    let mut pos = 0usize;
    let mut run = 0u32;
    while let Some(chunk) = data.buf.get(pos..) {
        let Some(frame) = parse(chunk) else { break };
        if frame.is_eac3 != want_eac3 || frame.frame_size == 0 {
            break;
        }
        run = run.saturating_add(1);
        if run >= STRONG_RUN {
            break;
        }
        pos = pos.saturating_add(frame.frame_size);
    }
    run
}

pub(crate) fn probe_ac3(data: &ProbeData<'_>) -> ProbeScore {
    score(chained_run(data, false))
}

pub(crate) fn probe_eac3(data: &ProbeData<'_>) -> ProbeScore {
    score(chained_run(data, true))
}

fn score(run: u32) -> ProbeScore {
    match run {
        r if r >= STRONG_RUN => SCORE_STRONG,
        r if r >= MIN_RUN => SCORE_WEAK,
        _ => ProbeScore::NONE,
    }
}

// ------------------------------------------------------------------ demuxer

#[derive(Debug)]
pub struct Ac3Demuxer {
    io: IoContext,
    budget: Budget,
    stream: Stream,
    next: Option<Packet>,
    sample_pos: u64,
    is_eac3: bool,
}

impl Ac3Demuxer {
    /// # Errors
    /// [`Error::InvalidData`] if no frame sync is found within the resync
    /// window, or whatever the transport reports.
    pub fn open(src: Box<dyn MediaSource>, is_eac3: bool) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let budget = Budget::new(Limits::permissive());
        let first = find_next_frame(&mut io, is_eac3)?;

        let mut audio = AudioParameters {
            sample_rate: first.sample_rate,
            layout: Some(first.channels),
            ..Default::default()
        };
        audio.bits_per_coded_sample = Some(0);
        let mut params = CodecParameters::new(MediaType::Audio);
        params.codec_id = Some(if is_eac3 { CodecId::Eac3 } else { CodecId::Ac3 });
        params.bit_rate = first.bit_rate_kbps.map(|k| u64::from(k) * 1000);
        params.audio = Some(audio);

        let mut stream = Stream::new(0, MediaType::Audio, TIME_BASE);
        stream.params = params;

        let mut demuxer = Self {
            io,
            budget,
            stream,
            next: None,
            sample_pos: 0,
            is_eac3,
        };
        demuxer.next = demuxer.read_one_frame()?;
        Ok(demuxer)
    }

    fn read_one_frame(&mut self) -> Result<Option<Packet>> {
        let Some(frame) = (match find_next_frame(&mut self.io, self.is_eac3) {
            Ok(f) => Some(f),
            Err(Error::Eof) => None,
            Err(e) => return Err(e),
        }) else {
            return Ok(None);
        };
        let pos = self.io.pos();
        let mut buf = self.budget.alloc::<u8>(frame.frame_size)?;
        self.io.read_exact(&mut buf)?;

        let ticks_per_sample_num = u64::from(TIME_BASE.den.unsigned_abs());
        let rate = u64::from(frame.sample_rate.max(1));
        let pts_ticks = ticks(self.sample_pos, ticks_per_sample_num, rate);
        let duration_ticks = ticks(u64::from(frame.samples), ticks_per_sample_num, rate);
        self.sample_pos = self.sample_pos.saturating_add(u64::from(frame.samples));

        let mut packet = Packet::from_slice(&mut self.budget, &buf)?;
        packet.stream_index = 0;
        packet.pts = Timestamp::new(pts_ticks.min(i64::MAX as u64).cast_signed());
        packet.dts = packet.pts;
        packet.duration = Timestamp::new(duration_ticks.min(i64::MAX as u64).cast_signed())
            .to_duration(TIME_BASE)
            .unwrap_or(Duration::ZERO);
        packet.pos = Some(pos);
        packet.flags |= PacketFlags::KEY;
        Ok(Some(packet))
    }
}

impl Demuxer for Ac3Demuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let out = self.next.take().ok_or(Error::Eof)?;
        self.next = self.read_one_frame()?;
        Ok(out)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::Unsupported(
            "ac3/eac3: byte-accurate seek needs a frame index this demuxer does not keep",
        ))
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

/// Advance byte by byte until a syncframe of the right kind (`is_eac3`)
/// parses at the current position.
fn find_next_frame(io: &mut IoContext, is_eac3: bool) -> Result<SyncFrame> {
    let mut skipped = 0u32;
    loop {
        let peek = io.peek(8)?;
        // `peek` returns fewer than requested only at true EOF (never an
        // error) — an empty result here is the clean end of stream, not
        // garbage to resynchronise past; `io.skip` past it reports
        // `UnexpectedEof`, which is the wrong shape for "no more frames".
        if peek.is_empty() {
            return Err(Error::Eof);
        }
        if let Some(frame) = parse(peek)
            && frame.is_eac3 == is_eac3
        {
            return Ok(frame);
        }
        io.skip(1)?;
        skipped = skipped.saturating_add(1);
        if skipped > MAX_RESYNC {
            return Err(Error::InvalidData(
                "ac3: no frame sync found within the resync window",
            ));
        }
    }
}

pub const DEMUXER_AC3: DemuxerDesc = DemuxerDesc {
    name: "ac3",
    long_name: "raw AC-3",
    extensions: &["ac3"],
    mime_types: &["audio/x-ac3"],
    flags: FormatFlags::GENERIC_INDEX,
    probe: probe_ac3,
    open: |src, _parsers: &dyn ParserProvider| {
        Ok(Box::new(Ac3Demuxer::open(src, false)?) as Box<dyn Demuxer>)
    },
};

pub const DEMUXER_EAC3: DemuxerDesc = DemuxerDesc {
    name: "eac3",
    long_name: "raw E-AC-3",
    extensions: &["eac3", "ec3"],
    mime_types: &["audio/x-eac3"],
    flags: FormatFlags::GENERIC_INDEX,
    probe: probe_eac3,
    open: |src, _parsers: &dyn ParserProvider| {
        Ok(Box::new(Ac3Demuxer::open(src, true)?) as Box<dyn Demuxer>)
    },
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    /// A minimal synthetic classic-AC-3 frame: fscod=0 (48k), frmsizecod=20
    /// (192kbps -> 768-byte frames), acmod=2 (stereo), no lfe.
    fn ac3_frame() -> Vec<u8> {
        let mut f = vec![0u8; 768];
        f[0] = 0x0B;
        f[1] = 0x77;
        f[4] = 20; // fscod=0, frmsizecod=20
        f[5] = 8 << 3; // bsid=8, bsmod=0
        f[6] = 2 << 5; // acmod=2 (stereo), rest zero (no lfe)
        f
    }

    /// A minimal synthetic E-AC-3 frame matching the bytes measured from
    /// `eac3.eac3`'s header (see module docs): strmtyp=0, frmsiz=895 (1792
    /// bytes), fscod=0, numblkscod=3 (6 blocks), acmod=7, lfeon=1, bsid=16.
    fn eac3_frame() -> Vec<u8> {
        let mut f = vec![0u8; 1792];
        f[0] = 0x0B;
        f[1] = 0x77;
        f[2] = 0x03;
        f[3] = 0x7f;
        f[4] = 0x3f;
        f[5] = 0x87;
        f
    }

    #[test]
    fn ac3_header_parses_the_synthetic_frame() {
        let frame = parse(&ac3_frame()).unwrap();
        assert!(!frame.is_eac3);
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.frame_size, 768);
        assert_eq!(frame.samples, 1536);
        assert_eq!(frame.bit_rate_kbps, Some(192));
        assert_eq!(frame.channels.channels, 2);
    }

    #[test]
    fn eac3_header_matches_the_measured_fixture_bytes() {
        let frame = parse(&eac3_frame()).unwrap();
        assert!(frame.is_eac3);
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.frame_size, 1792);
        assert_eq!(frame.samples, 1536);
        assert_eq!(frame.channels.channels, 6);
        assert_eq!(frame.bit_rate_kbps, Some(448));
    }

    #[test]
    fn a_run_of_ac3_frames_scores_the_reference_value() {
        let mut data = Vec::new();
        for _ in 0..6 {
            data.extend_from_slice(&ac3_frame());
        }
        assert_eq!(probe_ac3(&ProbeData::new(&data)), SCORE_STRONG);
        // An eac3-only probe must not also claim a classic-AC-3 stream.
        assert!(probe_eac3(&ProbeData::new(&data)) < SCORE_STRONG);
    }

    #[test]
    fn a_run_of_eac3_frames_scores_the_reference_value() {
        let mut data = Vec::new();
        for _ in 0..6 {
            data.extend_from_slice(&eac3_frame());
        }
        assert_eq!(probe_eac3(&ProbeData::new(&data)), SCORE_STRONG);
        assert!(probe_ac3(&ProbeData::new(&data)) < SCORE_STRONG);
    }

    #[test]
    fn one_incidental_syncword_scores_nothing() {
        let mut data = vec![0u8; 512];
        data[100] = 0x0B;
        data[101] = 0x77;
        assert_eq!(probe_ac3(&ProbeData::new(&data)), ProbeScore::NONE);
    }

    #[test]
    fn prose_scores_nothing() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(200);
        assert_eq!(
            probe_ac3(&ProbeData::new(text.as_bytes())),
            ProbeScore::NONE
        );
        assert_eq!(
            probe_eac3(&ProbeData::new(text.as_bytes())),
            ProbeScore::NONE
        );
    }

    #[test]
    fn empty_input_never_panics() {
        assert_eq!(probe_ac3(&ProbeData::new(&[])), ProbeScore::NONE);
        assert_eq!(probe_eac3(&ProbeData::new(&[])), ProbeScore::NONE);
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn demuxer_reads_three_stereo_ac3_frames() {
        let mut data = Vec::new();
        for _ in 0..3 {
            data.extend_from_slice(&ac3_frame());
        }
        let src = Box::new(MemorySource::new(data));
        let mut d = Ac3Demuxer::open(src, false).unwrap();
        assert_eq!(d.streams()[0].params.codec_id, Some(CodecId::Ac3));
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 768);
        assert_eq!(p0.pts, Timestamp::new(0));
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.pts, Timestamp::new(2880));
        let _p2 = d.read_packet().unwrap();
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn demuxer_reads_eac3_frames_with_5_1_layout() {
        let mut data = Vec::new();
        for _ in 0..2 {
            data.extend_from_slice(&eac3_frame());
        }
        let src = Box::new(MemorySource::new(data));
        let mut d = Ac3Demuxer::open(src, true).unwrap();
        assert_eq!(d.streams()[0].params.codec_id, Some(CodecId::Eac3));
        assert_eq!(
            d.streams()[0]
                .params
                .audio
                .as_ref()
                .and_then(|a| a.layout.as_ref())
                .map(|l| l.channels),
            Some(6)
        );
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.len, 1792);
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.pts, Timestamp::new(2880));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn an_ac3_demuxer_refuses_an_eac3_only_stream() {
        let data = eac3_frame();
        let src = Box::new(MemorySource::new(data));
        assert!(Ac3Demuxer::open(src, false).is_err());
    }
}

