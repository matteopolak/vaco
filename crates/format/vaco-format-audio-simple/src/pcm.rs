//! Shared machinery every format in this crate reduces to once its header is
//! parsed: a single stream of raw, interleaved audio samples, read or
//! written in fixed-size blocks with a timestamp derived from a running
//! sample count.
//!
//! Per the brief for this crate (plan `18-formats.md` §3.4.6): "several of
//! the nine are a header struct and a data pointer" — this is that shared
//! data-pointer half, so each format module is only its header.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Result, Timestamp};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Disposition, Stream};
use vaco_io::{IoContext, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

/// Whole frames in `bytes` at `bytes_per_frame` each, discarding a partial
/// trailing frame (a byte count that is not a whole number of frames is
/// exactly the "final short packet" case every demuxer in this crate hits at
/// EOF, not a value that needs finer-than-integer precision). This is the
/// one place `clippy::integer_division` is deliberately overridden rather
/// than routed around with floating point, which would be the wrong tool
/// for a count — every call site below shares this justification instead of
/// repeating it.
#[allow(
    clippy::integer_division,
    reason = "exact frame count from a byte count; truncating a partial trailing frame is intentional"
)]
#[must_use]
pub fn frames_in(bytes: u64, bytes_per_frame: u32) -> u64 {
    let bpf = if bytes_per_frame == 0 {
        1
    } else {
        bytes_per_frame
    };
    bytes / u64::from(bpf)
}

/// Target bytes per packet, before rounding down to a whole number of
/// frames. In the same ballpark as the reference's own raw-PCM demuxers
/// (a few milliseconds of audio per packet — not the whole file in one
/// packet, and not one frame per packet).
pub const TARGET_PACKET_BYTES: usize = 4096;

/// What a format module knows about its own samples once the header is
/// parsed: enough to build [`vaco_codec_core::AudioParameters`] and to frame
/// the raw data that follows.
#[derive(Debug, Clone, Copy)]
pub struct PcmLayout {
    pub sample_rate: u32,
    pub channels: u16,
    /// Bytes per interleaved frame (all channels), never zero.
    pub bytes_per_frame: u32,
}

impl PcmLayout {
    #[must_use]
    pub const fn new(sample_rate: u32, channels: u16, bytes_per_frame: u32) -> Self {
        Self {
            sample_rate,
            channels,
            bytes_per_frame: if bytes_per_frame == 0 {
                1
            } else {
                bytes_per_frame
            },
        }
    }
}

/// `(sample_fmt, bits_per_raw_sample)` for an integer or float PCM stream
/// whose container states `bits_per_coded_sample` bits per sample.
///
/// **Measured against `ffmpeg` 8.1**, not derived from `vaco-sampfmt`'s
/// layout by assumption (plan 13 §1b): `ffprobe -show_streams` on `au`/`aiff`
/// files built with `ffmpeg -c:a pcm_s24be -f aiff` reports `sample_fmt=s32`
/// and `bits_per_raw_sample=24` — the reference's *working* sample format for
/// 24-bit PCM is 32-bit (sign-extended), not a packed 24-bit type
/// `vaco-sampfmt` does not have, so there is no gap to route around. Every
/// other width reported `bits_per_raw_sample=N/A`, matching
/// `vaco-format-riff::wave::WaveFormatEx`'s own documented finding that a
/// "natural" depth (16-in-16, 32-in-32) states nothing extra:
///
/// ```text
///           bits_per_coded_sample   sample_fmt   bits_per_raw_sample
/// int            8                     u8              N/A
/// int           16                     s16             N/A
/// int           24                     s32             24
/// int           32                     s32             N/A
/// int           64                     s64             N/A
/// float         32                     flt             N/A
/// float         64                     dbl             N/A
/// ```
#[must_use]
pub const fn sample_fmt_for(
    bits_per_coded_sample: u8,
    is_float: bool,
) -> (Option<SampleFmt>, Option<u8>) {
    if is_float {
        return match bits_per_coded_sample {
            32 => (Some(SampleFmt::F32), None),
            64 => (Some(SampleFmt::F64), None),
            _ => (None, None),
        };
    }
    match bits_per_coded_sample {
        8 => (Some(SampleFmt::U8), None),
        16 => (Some(SampleFmt::S16), None),
        24 => (Some(SampleFmt::S32), Some(24)),
        32 => (Some(SampleFmt::S32), None),
        64 => (Some(SampleFmt::S64), None),
        _ => (None, None),
    }
}

/// The width/signedness/endianness-specific `CodecId` for a PCM stream.
///
/// `CodecId::Pcm` is the generic fallback and it is **not** what the reference
/// reports. Measured on AIFF, AU and CAF, all three agreeing:
///
/// ```text
/// encoder      codec_name   sample_fmt   bits_per_raw_sample
/// pcm_s16be    pcm_s16be    s16          N/A
/// pcm_s16le    pcm_s16le    s16          N/A
/// pcm_s24be    pcm_s24be    s32          24
/// pcm_s8       pcm_s8       u8           N/A
/// pcm_u8       pcm_u8       u8           N/A
/// pcm_f32be    pcm_f32be    flt          N/A
/// pcm_f64be    pcm_f64be    dbl          N/A
/// pcm_alaw     pcm_alaw     s16          N/A
/// pcm_mulaw    pcm_mulaw    s16          N/A
/// ```
///
/// Two of those rows are the reason this is measured rather than derived:
/// `pcm_s8` decodes to `u8`, not to some `s8` (there is no such sample
/// format), and A-law/µ-law decode to `s16` while being neither.
///
/// Returns the generic [`CodecId::Pcm`] only for a width the shared enum has
/// no variant for — 64-bit integer PCM is the one such case today.
#[must_use]
pub const fn codec_id_for(
    bits_per_coded_sample: u8,
    is_float: bool,
    big_endian: bool,
    signed: bool,
) -> CodecId {
    if is_float {
        return match (bits_per_coded_sample, big_endian) {
            (32, true) => CodecId::PcmF32be,
            (32, false) => CodecId::PcmF32le,
            (64, true) => CodecId::PcmF64be,
            (64, false) => CodecId::PcmF64le,
            _ => CodecId::Pcm,
        };
    }
    // Endianness is meaningless at one byte per sample, so 8-bit splits on
    // signedness alone.
    match (bits_per_coded_sample, signed, big_endian) {
        (8, true, _) => CodecId::PcmS8,
        (8, false, _) => CodecId::PcmU8,
        (16, _, true) => CodecId::PcmS16be,
        (16, _, false) => CodecId::PcmS16le,
        (24, _, true) => CodecId::PcmS24be,
        (24, _, false) => CodecId::PcmS24le,
        (32, _, true) => CodecId::PcmS32be,
        (32, _, false) => CodecId::PcmS32le,
        _ => CodecId::Pcm,
    }
}

/// The decoded sample format for a PCM-shaped `CodecId`, keyed on the codec
/// rather than on the coded width.
///
/// The width is not enough on its own, which is what makes this a separate
/// function rather than a call to [`sample_fmt_for`]: A-law and µ-law are
/// eight bits coded and decode to `s16`, and `pcm_s8` decodes to `u8` because
/// there is no signed 8-bit sample format. Both rows are in
/// [`codec_id_for`]'s measured table.
///
/// Returns `None` for anything that is not PCM-shaped, so a caller can use it
/// as the family test as well as the lookup.
#[must_use]
pub const fn sample_fmt_of(codec_id: CodecId) -> Option<(SampleFmt, Option<u8>)> {
    match codec_id {
        CodecId::PcmU8 | CodecId::PcmS8 => Some((SampleFmt::U8, None)),
        CodecId::PcmS16le | CodecId::PcmS16be | CodecId::PcmAlaw | CodecId::PcmMulaw => {
            Some((SampleFmt::S16, None))
        }
        CodecId::PcmS24le | CodecId::PcmS24be => Some((SampleFmt::S32, Some(24))),
        CodecId::PcmS32le | CodecId::PcmS32be => Some((SampleFmt::S32, None)),
        CodecId::PcmF32le | CodecId::PcmF32be => Some((SampleFmt::F32, None)),
        CodecId::PcmF64le | CodecId::PcmF64be => Some((SampleFmt::F64, None)),
        _ => None,
    }
}

/// Bits actually **stored** per sample, which is not always the decoded
/// format's width.
///
/// A-law and µ-law store eight and decode to `s16`; 24-bit PCM stores 24 and
/// decodes to `s32`. A muxer writing a container header needs the stored
/// width, and [`sample_fmt_of`] deliberately answers the other question.
#[must_use]
pub const fn coded_bits(codec_id: CodecId) -> Option<u8> {
    match codec_id {
        CodecId::PcmU8 | CodecId::PcmS8 | CodecId::PcmAlaw | CodecId::PcmMulaw => Some(8),
        CodecId::PcmS16le | CodecId::PcmS16be => Some(16),
        CodecId::PcmS24le | CodecId::PcmS24be => Some(24),
        CodecId::PcmS32le | CodecId::PcmS32be | CodecId::PcmF32le | CodecId::PcmF32be => Some(32),
        CodecId::PcmF64le | CodecId::PcmF64be => Some(64),
        _ => None,
    }
}

/// Whether a PCM-shaped codec stores its samples little-endian.
///
/// `Some(false)` is big-endian; `None` means endianness does not apply — one
/// byte per sample, so there is no order to state. Containers that carry a
/// byte-order flag need the distinction, and a container that assumes
/// big-endian silently corrupts a little-endian copy.
#[must_use]
pub const fn is_little_endian(codec_id: CodecId) -> Option<bool> {
    match codec_id {
        CodecId::PcmS16le
        | CodecId::PcmS24le
        | CodecId::PcmS32le
        | CodecId::PcmF32le
        | CodecId::PcmF64le => Some(true),
        CodecId::PcmS16be
        | CodecId::PcmS24be
        | CodecId::PcmS32be
        | CodecId::PcmF32be
        | CodecId::PcmF64be => Some(false),
        CodecId::PcmU8 | CodecId::PcmS8 | CodecId::PcmAlaw | CodecId::PcmMulaw => None,
        _ => None,
    }
}

/// Build [`CodecParameters`] for one PCM (or PCM-shaped, e.g. A-law/µ-law)
/// stream.
///
/// `codec_id` is `Some(CodecId::Pcm)` for every integer/float/A-law/µ-law
/// variant and `None` for anything `vaco-codec-core`'s small hand-maintained
/// enum has no variant for — the same policy `vaco-format-riff::wave_tags`
/// documents and this crate follows for consistency (D19: one convention,
/// not a second one invented here).
#[must_use]
pub fn params(
    layout: PcmLayout,
    codec_id: Option<CodecId>,
    format: Option<SampleFmt>,
    bits_per_coded_sample: Option<u8>,
    bits_per_raw_sample: Option<u8>,
) -> CodecParameters {
    let mut p = CodecParameters::audio();
    p.codec_id = codec_id;
    if let Some(audio) = p.audio.as_mut() {
        audio.sample_rate = layout.sample_rate;
        audio.format = format;
        audio.layout = ChannelLayout::default_for(u32::from(layout.channels));
        audio.bits_per_coded_sample = bits_per_coded_sample;
        audio.bits_per_raw_sample = bits_per_raw_sample;
    }
    p
}

/// A single-stream demuxer for "header already consumed; the rest of the
/// source is raw interleaved audio, either to a declared byte length or to
/// EOF".
#[derive(Debug)]
pub struct RawPcmDemuxer {
    io: IoContext,
    stream: Stream,
    data_start: u64,
    /// Bytes of audio data, clamped to what the source actually holds when
    /// its size is known. `None` means "read until EOF" (a streaming write,
    /// or a format that states no length at all).
    data_len: Option<u64>,
    bytes_per_frame: u32,
    frames_emitted: u64,
    eof: bool,
}

impl RawPcmDemuxer {
    /// `declared_len` is the format's own statement of the data size in
    /// bytes, if it makes one; it is clamped against [`IoContext::size`] here
    /// so no format module has to repeat that policy.
    #[must_use]
    pub fn new(
        io: IoContext,
        stream: Stream,
        data_start: u64,
        declared_len: Option<u64>,
        bytes_per_frame: u32,
    ) -> Self {
        let data_len = declared_len.map(|n| match io.size() {
            Some(size) => n.min(size.saturating_sub(data_start)),
            None => n,
        });
        Self {
            io,
            stream,
            data_start,
            data_len,
            bytes_per_frame: bytes_per_frame.max(1),
            frames_emitted: 0,
            eof: false,
        }
    }

    #[must_use]
    pub fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    /// Total data bytes, once known — from the declared length, or, for an
    /// unbounded stream, from the source's own size if it has one.
    #[must_use]
    fn total_data_bytes(&self) -> Option<u64> {
        self.data_len
            .or_else(|| self.io.size().map(|s| s.saturating_sub(self.data_start)))
    }

    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        let bytes = self.total_data_bytes()?;
        let frames = frames_in(bytes, self.bytes_per_frame);
        let micros = frames.checked_mul(1_000_000)?.checked_div(u64::from(
            self.stream.params.audio.as_ref()?.sample_rate.max(1),
        ))?;
        Some(Duration::from_micros(
            i64::try_from(micros).unwrap_or(i64::MAX),
        ))
    }

    /// Read one packet.
    ///
    /// # Errors
    /// [`vaco_core::Error::Eof`] at the end of the data; propagates transport
    /// failure and [`vaco_core::Error::LimitExceeded`] from `budget`.
    pub fn read_packet(&mut self, budget: &mut Budget) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let bpf = u64::from(self.bytes_per_frame);
        let pos_bytes = self.frames_emitted.saturating_mul(bpf);
        if let Some(len) = self.data_len
            && pos_bytes >= len
        {
            self.eof = true;
            return Err(Error::Eof);
        }

        let mut want = TARGET_PACKET_BYTES;
        want -= want % self.bytes_per_frame as usize;
        if want == 0 {
            want = self.bytes_per_frame as usize;
        }
        if let Some(len) = self.data_len {
            let remaining = usize::try_from(len.saturating_sub(pos_bytes)).unwrap_or(usize::MAX);
            want = want.min(remaining.max(1));
        }

        let mut pkt = Packet::alloc(budget, want)?;
        let n = self.io.read_partial(pkt.payload_mut())?;
        if n == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        pkt.len = n;
        pkt.stream_index = 0;
        let frame_index = frames_in(pos_bytes, self.bytes_per_frame);
        pkt.pts = Timestamp::new(i64::try_from(frame_index).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        pkt.pos = Some(self.data_start.saturating_add(pos_bytes));

        // A short final read that does not land on a frame boundary still
        // ends the stream (this is EOF, not corruption): count only whole
        // frames so `frames_emitted` never runs ahead of what was actually
        // delivered, and stop rather than spin re-reading a fractional tail
        // forever.
        let whole_frames = frames_in(u64::try_from(n).unwrap_or(0), self.bytes_per_frame);
        if whole_frames == 0 {
            self.eof = true;
        }
        self.frames_emitted = self.frames_emitted.saturating_add(whole_frames.max(1));
        Ok(pkt)
    }

    /// Byte-accurate seek: converts a timestamp or frame target into a byte
    /// offset into the data and seeks the source there directly. Audio PCM
    /// has no keyframe distinction, so [`SeekFlags`] otherwise has nothing to
    /// say here.
    ///
    /// # Errors
    /// [`vaco_core::Error::NotSeekable`] if the source cannot seek.
    pub fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let frame = match target {
            SeekTarget::Byte(b) => {
                frames_in(b.saturating_sub(self.data_start), self.bytes_per_frame)
            }
            SeekTarget::Frame { frame, .. } => frame,
            SeekTarget::Timestamp { ts, .. } => {
                let ticks = ts.ticks().unwrap_or(0);
                u64::try_from(ticks.max(0)).unwrap_or(0)
            }
        };
        let byte_pos = self
            .data_start
            .saturating_add(frame.saturating_mul(u64::from(self.bytes_per_frame)));
        self.io.seek(byte_pos)?;
        self.frames_emitted = frame;
        self.eof = false;
        Ok(())
    }
}

/// The media-type/disposition boilerplate every format's `Stream::new` needs;
/// pulled out only so the format modules do not each repeat the
/// `Disposition::empty()` import.
#[must_use]
pub fn new_stream(time_base: vaco_core::Rational) -> Stream {
    let mut s = Stream::new(0, MediaType::Audio, time_base);
    s.disposition = Disposition::empty();
    s
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_core::Rational;
    use vaco_io::{IoOptions, MemorySource};
    use vaco_limits::Limits;

    fn demux_of(data: Vec<u8>, bpf: u32, declared_len: Option<u64>) -> RawPcmDemuxer {
        let src = Box::new(MemorySource::new(data));
        let io = IoContext::new(src, &IoOptions::default()).unwrap();
        let mut stream = new_stream(Rational::new(1, 44_100));
        stream.params = params(
            PcmLayout::new(44_100, 2, bpf),
            Some(CodecId::Pcm),
            Some(SampleFmt::S16),
            Some(16),
            None,
        );
        RawPcmDemuxer::new(io, stream, 0, declared_len, bpf)
    }

    #[test]
    fn packets_cover_the_whole_stream_with_increasing_pts() {
        let data = vec![0xABu8; TARGET_PACKET_BYTES * 2 + 40];
        let mut d = demux_of(data.clone(), 4, Some(data.len() as u64));
        let mut budget = Budget::new(Limits::permissive());
        let mut total = 0usize;
        let mut last_pts = -1i64;
        loop {
            match d.read_packet(&mut budget) {
                Ok(pkt) => {
                    assert!(pkt.pts.ticks().unwrap() > last_pts);
                    last_pts = pkt.pts.ticks().unwrap();
                    total += pkt.len;
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(total, data.len());
    }

    #[test]
    fn an_unbounded_stream_reads_to_true_eof() {
        let data = vec![0x11u8; 100];
        let mut d = demux_of(data.clone(), 2, None);
        let mut budget = Budget::new(Limits::permissive());
        let mut total = 0usize;
        loop {
            match d.read_packet(&mut budget) {
                Ok(pkt) => total += pkt.len,
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(total, 100);
    }

    #[test]
    fn a_declared_length_longer_than_the_source_is_clamped() {
        let data = vec![0x22u8; 10];
        let mut d = demux_of(data, 2, Some(10_000));
        let mut budget = Budget::new(Limits::permissive());
        let mut total = 0usize;
        loop {
            match d.read_packet(&mut budget) {
                Ok(pkt) => total += pkt.len,
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(total, 10);
    }

    #[test]
    fn seeking_lands_on_a_frame_boundary() {
        let data = (0u8..=200).collect::<Vec<_>>();
        let mut d = demux_of(data.clone(), 4, Some(data.len() as u64));
        d.seek(
            SeekTarget::Frame {
                stream_index: 0,
                frame: 3,
            },
            SeekFlags::empty(),
        )
        .unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let pkt = d.read_packet(&mut budget).unwrap();
        assert_eq!(pkt.pts.ticks(), Some(3));
        assert_eq!(pkt.payload()[0], data[12]);
    }

    #[test]
    fn duration_matches_frame_count_over_rate() {
        let d = demux_of(vec![0u8; 44_100 * 4], 4, Some(44_100 * 4));
        let dur = d.duration().unwrap();
        assert_eq!(dur.as_micros(), 1_000_000);
    }

    #[test]
    fn empty_data_is_immediate_eof() {
        let mut d = demux_of(Vec::new(), 4, Some(0));
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(d.read_packet(&mut budget), Err(Error::Eof)));
    }

    #[test]
    fn zero_bytes_per_frame_is_treated_as_one() {
        // A malformed header claiming zero channels/width must not divide by
        // zero anywhere in this path.
        let data = vec![1u8; 8];
        let mut d = demux_of(data, 0, Some(8));
        let mut budget = Budget::new(Limits::permissive());
        assert!(d.read_packet(&mut budget).is_ok());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod sample_fmt_of_tests {
    use super::*;

    /// The whole measured table, and the two rows that make it a codec lookup
    /// rather than a width lookup.
    #[test]
    fn every_pcm_codec_maps_to_the_format_the_reference_reports() {
        for (id, want, raw) in [
            (CodecId::PcmU8, SampleFmt::U8, None),
            (CodecId::PcmS8, SampleFmt::U8, None),
            (CodecId::PcmS16le, SampleFmt::S16, None),
            (CodecId::PcmS16be, SampleFmt::S16, None),
            (CodecId::PcmS24le, SampleFmt::S32, Some(24)),
            (CodecId::PcmS32le, SampleFmt::S32, None),
            (CodecId::PcmF32le, SampleFmt::F32, None),
            (CodecId::PcmF64le, SampleFmt::F64, None),
            // Eight bits coded, sixteen decoded. A width lookup gets these
            // wrong, which is the whole reason this function exists.
            (CodecId::PcmAlaw, SampleFmt::S16, None),
            (CodecId::PcmMulaw, SampleFmt::S16, None),
        ] {
            assert_eq!(sample_fmt_of(id), Some((want, raw)), "{id:?}");
        }
    }

    #[test]
    fn a_non_pcm_codec_answers_none_so_it_doubles_as_the_family_test() {
        assert_eq!(sample_fmt_of(CodecId::Mp3), None);
        assert_eq!(sample_fmt_of(CodecId::H264), None);
        // Including the generic placeholder: it states no width, so it cannot
        // state a sample format either.
        assert_eq!(sample_fmt_of(CodecId::Pcm), None);
    }
}

#[cfg(test)]
mod codec_id_tests {
    use super::codec_id_for;
    use vaco_codec_core::CodecId;

    /// The width/endianness table, pinned to what the reference reports.
    ///
    /// ```sh
    /// for c in pcm_s16be pcm_s16le pcm_s24be pcm_s8 pcm_u8 pcm_f32be pcm_f64be; do
    ///   ffmpeg -y -v quiet -f lavfi -i sine=d=0.1 -c:a $c x.aiff
    ///   ffprobe -v quiet -of csv=p=0 -show_entries stream=codec_name x.aiff
    /// done
    /// ```
    #[test]
    fn integer_widths_split_on_endianness_above_one_byte() {
        assert_eq!(codec_id_for(16, false, true, true), CodecId::PcmS16be);
        assert_eq!(codec_id_for(16, false, false, true), CodecId::PcmS16le);
        assert_eq!(codec_id_for(24, false, true, true), CodecId::PcmS24be);
        assert_eq!(codec_id_for(24, false, false, true), CodecId::PcmS24le);
        assert_eq!(codec_id_for(32, false, true, true), CodecId::PcmS32be);
        assert_eq!(codec_id_for(32, false, false, true), CodecId::PcmS32le);
    }

    /// One byte per sample has no endianness, so 8-bit splits on signedness.
    #[test]
    fn eight_bit_ignores_endianness_and_splits_on_sign() {
        for big in [true, false] {
            assert_eq!(codec_id_for(8, false, big, true), CodecId::PcmS8);
            assert_eq!(codec_id_for(8, false, big, false), CodecId::PcmU8);
        }
    }

    #[test]
    fn floats_split_on_width_and_endianness() {
        assert_eq!(codec_id_for(32, true, true, true), CodecId::PcmF32be);
        assert_eq!(codec_id_for(32, true, false, true), CodecId::PcmF32le);
        assert_eq!(codec_id_for(64, true, true, true), CodecId::PcmF64be);
        assert_eq!(codec_id_for(64, true, false, true), CodecId::PcmF64le);
    }

    /// The generic `Pcm` survives only where the shared enum cannot follow.
    ///
    /// It is not a codec the reference ever names, so reaching it is a gap,
    /// not an answer — and the assertion says which gap.
    #[test]
    fn the_generic_id_is_reached_only_by_sixty_four_bit_integers() {
        assert_eq!(codec_id_for(64, false, true, true), CodecId::Pcm);
        assert_eq!(codec_id_for(12, false, true, true), CodecId::Pcm);
        for bits in [8_u8, 16, 24, 32] {
            for big in [true, false] {
                assert_ne!(codec_id_for(bits, false, big, true), CodecId::Pcm);
            }
        }
    }
}
