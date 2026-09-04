//! The `aac` raw elementary-stream demuxer: bare ADTS framing with no file
//! header at all — one syncframe after another, same shape as [`crate::ac3`].
//!
//! # Why this exists
//!
//! Before this module, nothing in the registry recognised a bare `.aac`
//! (ADTS) file at all: there was no demuxer named `aac`, so the highest-
//! scoring registered demuxer won by default regardless of how weak its
//! score was. On a real ADTS file that was `cdgraphics` — its probe counts
//! 24-byte-aligned chunks whose command byte's low six bits happen to equal
//! `0x09`, which happens on compressed audio data by chance roughly one
//! byte in 64, often enough over a multi-kilobyte prefix to clear
//! `cdgraphics`'s own threshold while every other candidate scored zero.
//! That is category (b) from the brief: the correct format was never tried,
//! not that it scored too low.
//!
//! # Header parsing is not duplicated here
//!
//! [`vaco_parse_aac::adts::AdtsHeader`] already parses `adts_fixed_header()`
//! plus `adts_variable_header()` (ISO/IEC 14496-3 subpart 4 §4.4.1.1) and
//! derives sample rate, channel layout and codec parameters from it. That
//! parser is a plain, stateless function of a byte slice, so both this
//! demuxer's probe (which gets no [`vaco_format_core::ParserProvider`] — see
//! `probe`'s signature) and its streaming reader call it directly rather
//! than re-deriving the ISO/IEC 13818-7 sampling-frequency and
//! channel-configuration tables a second time. `vaco-demux-raw` and
//! `vaco-parse-aac` are both layer 4 (`layers.toml`), so this is a same-layer
//! edge, not a violation of the crate's downward-dependency rule — the
//! `ParserProvider` seam `h264`/`hevc`/`av1`/`obu` use exists to let a
//! *stateful* per-container parser vary by caller, which a raw file's own
//! demuxer does not need.
//!
//! # Framing
//!
//! Each ADTS frame states its own total length (`aac_frame_length`, header
//! included) in its 13-bit field, so — like [`crate::ac3`]'s classic-AC-3 and
//! E-AC-3 syncframes, and unlike the whole-buffer `Framing::StartCode3`
//! family in [`crate::bitstream`] — this is a genuine streaming demuxer: read
//! a header, trust its declared length, read that many bytes, repeat.
//!
//! # Time base — measured, not chosen
//!
//! `ffprobe -show_streams` on `ffmpeg -c:a aac -f adts` output reports
//! `time_base=1/28224000` for every sample rate ADTS can carry (measured at
//! 8000, 11025 was not directly encoded, 16000, 22050, 32000, 44100, 48000
//! and 96000 Hz — same denominator every time, `ffmpeg` 9.0.1). `28224000`
//! is exactly divisible by all thirteen valid ADTS sample rates (`2^9 · 3^2 ·
//! 5^3 · 7^2`), so `28224000 / sample_rate` is always an exact integer and
//! one 1024-sample frame is always a whole number of ticks — confirmed
//! directly: a 44100 Hz frame measured `duration=655360`, and
//! `1024 * (28224000 / 44100) == 655360` exactly.
//!
//! One divergence, disclosed rather than chased: `packet=pts`, `packet=dts`
//! and `packet=size` are byte-identical to `ffprobe` on every one of a
//! 44.1kHz fixture's 88 packets (verified through the built `vaco-probe`
//! binary, not just this crate's own tests), but `packet=duration` reads
//! **655361** from us against the reference's **655360** on every single
//! packet. The tick math above is exact — `1024 * 640 == 655360` — the loss
//! happens one layer up: [`vaco_packet::Packet::duration`] is a
//! [`vaco_core::Duration`], which is fixed at whole-microsecond resolution,
//! and 655360 ticks of a `1/28224000` base is `23219.9546…` µs — not an
//! integer, so no microsecond value round-trips back to exactly 655360
//! (the nearest, 23220 µs, rescales back to 655361). `ac3`'s own
//! already-inexact duration (`floor(1536 × 90000 / 44100)`) happens to
//! survive the same round-trip by coincidence of rounding direction, which
//! is why this was not caught there first. Fixing it means changing what
//! `Packet::duration` *is* — an exact-ticks representation, or a
//! finer-grained one — which is a workspace-wide change, not a local one,
//! so it is disclosed here rather than attempted in this module.

use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_parse_aac::adts::{AdtsHeader, HEADER_LEN};

/// Measured; see the module docs.
const TIME_BASE_DEN: u64 = 28_224_000;
const TIME_BASE: Rational = Rational {
    num: 1,
    den: 28_224_000,
};

/// Samples per ADTS raw data block. ISO/IEC 13818-7: always 1024 — ADTS has
/// no `frameLengthFlag`, so the 960-sample variant cannot be signalled here.
const SAMPLES_PER_BLOCK: u32 = 1024;

/// Consecutive bytes tolerated while resynchronising before giving up.
const MAX_RESYNC: u32 = 64 * 1024;

// ------------------------------------------------------------------- probing

/// Consecutive chained frames required before this claims the reference's
/// measured score. Same shape and same constant as [`crate::ac3`] and
/// `vaco-demux-mpegaudio`'s probes, and for the same reason: the twelve-bit
/// ADTS sync word occurs by chance often enough in an unrelated payload that
/// one match proves nothing.
const STRONG_RUN: u32 = 4;
const MIN_RUN: u32 = 2;
const SCORE_STRONG: ProbeScore = ProbeScore(51);
const SCORE_WEAK: ProbeScore = ProbeScore(24);

fn chained_run(data: &ProbeData<'_>) -> u32 {
    let mut pos = 0usize;
    let mut run = 0u32;
    while let Some(chunk) = data.buf.get(pos..) {
        let Ok(header) = AdtsHeader::parse(chunk) else {
            break;
        };
        if header.frame_length == 0 {
            break;
        }
        run = run.saturating_add(1);
        if run >= STRONG_RUN {
            break;
        }
        pos = pos.saturating_add(usize::from(header.frame_length));
    }
    run
}

pub(crate) fn probe(data: &ProbeData<'_>) -> ProbeScore {
    match chained_run(data) {
        r if r >= STRONG_RUN => SCORE_STRONG,
        r if r >= MIN_RUN => SCORE_WEAK,
        _ => ProbeScore::NONE,
    }
}

// ------------------------------------------------------------------ demuxer

#[derive(Debug)]
pub struct AacDemuxer {
    io: IoContext,
    budget: Budget,
    stream: Stream,
    next: Option<Packet>,
    sample_pos: u64,
}

impl AacDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if no frame sync is found within the resync
    /// window, or whatever the transport reports.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let budget = Budget::new(Limits::permissive());
        let first = find_next_frame(&mut io)?;

        let mut stream = Stream::new(0, MediaType::Audio, TIME_BASE);
        stream.params = first.to_codec_parameters();

        let mut demuxer = Self {
            io,
            budget,
            stream,
            next: None,
            sample_pos: 0,
        };
        demuxer.next = demuxer.read_one_frame()?;
        Ok(demuxer)
    }

    fn read_one_frame(&mut self) -> Result<Option<Packet>> {
        let header = match find_next_frame(&mut self.io) {
            Ok(h) => h,
            Err(Error::Eof) => return Ok(None),
            Err(e) => return Err(e),
        };
        let pos = self.io.pos();
        let frame_len = usize::from(header.frame_length);
        let mut buf = self.budget.alloc::<u8>(frame_len)?;
        self.io.read_exact(&mut buf)?;

        let samples = u32::from(header.raw_data_blocks).saturating_mul(SAMPLES_PER_BLOCK);
        // Exact for every valid ADTS sampling_frequency_index — see the
        // module docs' divisibility note.
        #[allow(
            clippy::integer_division,
            reason = "TIME_BASE_DEN divides every ADTS sampling frequency exactly (module docs' divisibility note); the divisor is forced non-zero by max(1)"
        )]
        let ticks_per_sample = TIME_BASE_DEN / u64::from(header.sampling_frequency.max(1));
        let pts_ticks = self.sample_pos.saturating_mul(ticks_per_sample);
        let duration_ticks = u64::from(samples).saturating_mul(ticks_per_sample);
        self.sample_pos = self.sample_pos.saturating_add(u64::from(samples));

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

impl Demuxer for AacDemuxer {
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
            "aac: byte-accurate seek needs a frame index this demuxer does not keep",
        ))
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

/// Advance byte by byte until an ADTS header parses at the current position.
fn find_next_frame(io: &mut IoContext) -> Result<AdtsHeader> {
    let mut skipped = 0u32;
    loop {
        let peek = io.peek(HEADER_LEN)?;
        // `peek` returns fewer than requested only at true EOF (never an
        // error) — an empty result here is the clean end of stream, not
        // garbage to resynchronise past.
        if peek.is_empty() {
            return Err(Error::Eof);
        }
        if let Ok(header) = AdtsHeader::parse(peek) {
            return Ok(header);
        }
        io.skip(1)?;
        skipped = skipped.saturating_add(1);
        if skipped > MAX_RESYNC {
            return Err(Error::InvalidData(
                "aac: no ADTS frame sync found within the resync window",
            ));
        }
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "aac",
    long_name: "raw ADTS AAC (Advanced Audio Coding)",
    extensions: &["aac"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: |src, _parsers: &dyn ParserProvider| {
        Ok(Box::new(AacDemuxer::open(src)?) as Box<dyn Demuxer>)
    },
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    /// A minimal synthetic ADTS frame: MPEG-4, no CRC, AAC-LC, 44100 Hz,
    /// stereo, one raw data block, frame length `len` (header + payload).
    fn adts_frame(len: u16) -> Vec<u8> {
        let mut f = vec![0u8; usize::from(len)];
        f[0] = 0xff;
        f[1] = 0xf1; // syncword low nibble=1111, ID=0, layer=00, protection_absent=1
        f[2] = 0x50; // profile=01 (LC), sampling_frequency_index=0100 (44100), private=0, channel_config hi bit=0
        f[3] = 0x80 | ((len >> 11) as u8 & 0x03); // channel_config lo 2 bits = 010 -> stereo; original/home/copyright bits 0; frame_length bits 12-11
        f[4] = ((len >> 3) & 0xff) as u8;
        f[5] = (((len & 0x07) as u8) << 5) | 0x1f; // frame_length low 3 bits, buffer_fullness hi 5 bits (VBR marker)
        f[6] = 0xfc; // buffer_fullness lo 6 bits (all 1: VBR) + raw_data_blocks-1 = 00
        f
    }

    #[test]
    fn a_lone_frame_round_trips_through_the_real_header_parser() {
        let frame = adts_frame(200);
        let header = AdtsHeader::parse(&frame).unwrap();
        assert_eq!(header.frame_length, 200);
        assert_eq!(header.sampling_frequency, 44100);
        assert_eq!(header.channels(), Some(2));
    }

    #[test]
    fn probe_needs_at_least_two_chained_frames() {
        let mut data = Vec::new();
        data.extend_from_slice(&adts_frame(100));
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::NONE);

        data.extend_from_slice(&adts_frame(100));
        assert_eq!(probe(&ProbeData::new(&data)), SCORE_WEAK);

        for _ in 0..4 {
            data.extend_from_slice(&adts_frame(100));
        }
        assert_eq!(probe(&ProbeData::new(&data)), SCORE_STRONG);
    }

    #[test]
    fn probe_rejects_plain_prose() {
        let text = "The quick brown fox jumps over the lazy dog, repeatedly, at length.";
        assert_eq!(probe(&ProbeData::new(text.as_bytes())), ProbeScore::NONE);
    }

    #[test]
    fn probe_is_total_on_empty_and_short_input() {
        assert_eq!(probe(&ProbeData::new(&[])), ProbeScore::NONE);
        assert_eq!(probe(&ProbeData::new(&[0xff, 0xf1])), ProbeScore::NONE);
    }

    #[test]
    fn a_cdg_style_payload_never_outscores_a_real_adts_stream() {
        // The regression this module exists to fix: four chained ADTS frames
        // must clear `cdgraphics`'s own cap (measured at 85, but only ever
        // reached with 90+ 24-byte chunks whose command byte matches by
        // chance — a handful of coincidental hits scores far lower).
        let mut data = Vec::new();
        for _ in 0..4 {
            data.extend_from_slice(&adts_frame(100));
        }
        let score = probe(&ProbeData::new(&data));
        let cdg_ceiling_on_a_weak_coincidence = ProbeScore(20);
        assert!(score > cdg_ceiling_on_a_weak_coincidence);
    }

    #[test]
    fn demuxer_reads_every_frame_and_reports_audio() {
        let mut data = Vec::new();
        for _ in 0..5 {
            data.extend_from_slice(&adts_frame(100));
        }
        let mut d = AacDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams()[0].media_type(), Some(MediaType::Audio));
        let mut count = 0;
        loop {
            let result = d.read_packet();
            if matches!(result, Err(Error::Eof)) {
                break;
            }
            let pkt = result.unwrap();
            assert_eq!(pkt.payload().len(), 100);
            assert!(pkt.is_key());
            count += 1;
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn packet_durations_accumulate_at_the_measured_time_base() {
        let mut data = Vec::new();
        for _ in 0..3 {
            data.extend_from_slice(&adts_frame(100));
        }
        let mut d = AacDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        // 28224000 / 44100 = 640; one 1024-sample frame is 1024*640 = 655360
        // ticks — the exact value measured against `ffprobe` (see module docs).
        let expected_step = 655_360i64;
        let mut expected_pts = 0i64;
        for _ in 0..3 {
            let pkt = d.read_packet().unwrap();
            assert_eq!(pkt.pts.ticks(), Some(expected_pts));
            expected_pts += expected_step;
        }
    }
}
