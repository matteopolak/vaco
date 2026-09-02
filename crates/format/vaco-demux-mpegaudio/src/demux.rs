//! The `mp3` demuxer.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::{Demuxer, FormatOptions, SeekFlags, SeekTarget, Stream};
use vaco_format_id3::{Id3v1Tag, Id3v2Header, Id3v2Tag};
use vaco_format_mpegaudio::{Layer, MpegAudioHeader, VbriHeader, XingHeader, vbri};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData};
use vaco_sampfmt::SampleFmt;

/// The reference's own fixed stream time base for every MPEG audio file,
/// regardless of sample rate: the least common multiple of the nine valid
/// sample rates, confirmed by inspecting `ffprobe -show_streams` output at
/// 32000/44100/48000 Hz and finding the same `time_base=1/14112000` in each.
const TIME_BASE: Rational = Rational {
    num: 1,
    den: 14_112_000,
};

/// The reference decoder's own extra filterbank delay, added to (and
/// subtracted from) the LAME tag's encoder delay/padding to get the sample
/// counts actually trimmed at decode time. Confirmed against a real
/// `ffmpeg -c:a libmp3lame` file: `skip_samples` on the first packet was the
/// LAME delay plus this constant, and `discard_padding` on the last packet
/// was the LAME padding minus it.
const DECODER_DELAY: u32 = 529;

/// Consecutive garbage bytes tolerated while resynchronising before giving up
/// and reporting the stream as unreadable.
const MAX_RESYNC: u32 = 64 * 1024;

/// [`TIME_BASE`]'s denominator divides evenly by every one of the nine valid
/// MPEG sample rates by construction (it is their least common multiple), so
/// one sample always converts to a whole number of ticks.
#[allow(
    clippy::integer_division,
    reason = "TIME_BASE.den is a multiple of every valid sample rate; this division is always exact"
)]
fn ticks_per_sample(sample_rate: u32) -> u64 {
    u64::from(TIME_BASE.den.unsigned_abs()) / u64::from(sample_rate.max(1))
}

/// Saturate a tick count into `Timestamp`'s signed range instead of wrapping.
fn ticks_i64(ticks: u64) -> i64 {
    ticks.min(i64::MAX as u64).cast_signed()
}

#[derive(Debug)]
pub struct MpegAudioDemuxer {
    io: IoContext,
    stream: Stream,
    budget: Budget,
    next: Option<Packet>,
    frame_index: u64,
    first_emitted: bool,
    skip_start: u32,
    skip_end: u32,
    /// Byte offset of the first real (non-tag) audio frame, for seeking.
    audio_start: u64,
    /// Average bytes/second for a byte-offset seek estimate; `None` when the
    /// stream is free-format and no VBR header stated a byte count.
    avg_byte_rate: Option<u64>,
    /// A free-format stream's frame length, once derived and validated
    /// against the frame that follows it (`measure_free_format_len`'s own
    /// docs) — held constant for the rest of the stream rather than
    /// re-scanned (and re-risking a false sync) every single frame. This is
    /// the *unpadded* length; `padding_bit` still adds one byte per frame
    /// as usual. `None` until the first free-format frame is seen, and for
    /// every non-free-format stream.
    free_format_len: Option<u32>,
}

impl MpegAudioDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if no frame sync can be found at all.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut budget = Budget::new(Limits::permissive());
        let (id3v2_len, id3v2_entries) = read_leading_id3v2(&mut io, &mut budget);
        let _ = id3v2_len;

        let first = find_next_header(&mut io)?;
        let tag = read_first_frame_tag(&mut io, first, &mut budget)?;

        let mut stream = Stream::new(0, MediaType::Audio, TIME_BASE);
        stream.metadata = id3v2_entries;
        configure_stream(&mut stream, first, tag.as_ref());

        let (skip_start, skip_end) = tag.as_ref().and_then(|t| t.lame).map_or((0, 0), |lame| {
            (
                u32::from(lame.encoder_delay).saturating_add(DECODER_DELAY),
                u32::from(lame.encoder_padding).saturating_sub(DECODER_DELAY),
            )
        });

        if let Some(t) = &tag {
            // A dedicated VBR/CBR header frame carries no audio; only the
            // frame after it is real.
            io.skip(u64::from(t.consumed))?;
        }
        let audio_start = io.pos();

        merge_trailing_id3v1(&mut io, &mut stream, audio_start);

        let avg_byte_rate = tag
            .as_ref()
            .and_then(|t| t.total_bytes)
            .zip(stream.duration())
            .and_then(|(bytes, dur)| {
                let secs = dur.as_secs_f64();
                (secs > 0.0).then(|| (f64::from(bytes) / secs) as u64)
            })
            .or_else(|| first.bitrate_kbps().map(|k| (u64::from(k) * 1000) >> 3));

        let mut demuxer = Self {
            io,
            stream,
            budget,
            next: None,
            frame_index: 0,
            first_emitted: false,
            skip_start,
            skip_end,
            audio_start,
            avg_byte_rate,
            free_format_len: None,
        };
        demuxer.next = demuxer.read_one_frame()?;
        Ok(demuxer)
    }

    fn read_one_frame(&mut self) -> Result<Option<Packet>> {
        let Some(header) = (match find_next_header(&mut self.io) {
            Ok(h) => Some(h),
            Err(Error::Eof) => None,
            Err(e) => return Err(e),
        }) else {
            return Ok(None);
        };
        let len = match header.frame_len() {
            Some(l) => l as usize,
            None => self.free_format_frame_len(header)?,
        };
        let mut buf = self.budget.alloc::<u8>(len)?;
        let pos = self.io.pos();
        self.io.read_exact(&mut buf)?;

        let ticks_per_sample = ticks_per_sample(header.sample_rate_hz());
        let ticks = self
            .frame_index
            .saturating_mul(u64::from(header.samples_per_frame()))
            .saturating_mul(ticks_per_sample);
        let pts = Timestamp::new(ticks_i64(ticks));
        let duration_ticks = u64::from(header.samples_per_frame()).saturating_mul(ticks_per_sample);
        let duration = Timestamp::new(ticks_i64(duration_ticks))
            .to_duration(TIME_BASE)
            .unwrap_or(Duration::ZERO);

        let mut packet = Packet::from_slice(&mut self.budget, &buf)?;
        packet.stream_index = 0;
        packet.pts = pts;
        packet.dts = pts;
        packet.duration = duration;
        packet.pos = Some(pos);
        packet.flags |= vaco_packet::PacketFlags::KEY;
        self.frame_index = self.frame_index.saturating_add(1);
        Ok(Some(packet))
    }

    /// A free-format frame's total length (header included), deriving it
    /// once per stream and holding it constant afterward.
    ///
    /// `bitrate_index == 0` means the frame length is in no table; the only
    /// way to know it is to find where the next sync falls. A real
    /// free-format encoder keeps one fixed length for the whole stream
    /// (only `padding_bit` toggles it by a byte), so deriving it fresh on
    /// every frame is both needless and risky: an 11-bit sync pattern
    /// (`0x7FF`) is not rare inside Huffman-coded payload bytes, and a false
    /// sync there gives a *plausible* wrong length that then poisons every
    /// frame read after it. Deriving it once, validated against the frame
    /// that actually follows the candidate length, and reusing it (with
    /// only the padding bit varying) is what the same trap looks like from
    /// the other side: an unvalidated one-shot scan and an unvalidated
    /// scan-every-frame fail exactly the same way, just at different rates.
    fn free_format_frame_len(&mut self, header: MpegAudioHeader) -> Result<usize> {
        if let Some(base) = self.free_format_len {
            return Ok((base.saturating_add(u32::from(header.padding))) as usize);
        }
        let total = measure_free_format_len(&mut self.io, header)?;
        let base = total.saturating_sub(usize::from(header.padding));
        self.free_format_len = Some(base as u32);
        Ok(total)
    }
}

impl Demuxer for MpegAudioDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let mut out = self.next.take().ok_or(Error::Eof)?;
        match self.read_one_frame()? {
            Some(p) => self.next = Some(p),
            None => attach_skip(&mut out, 0, self.skip_end),
        }
        if !self.first_emitted {
            attach_skip(&mut out, self.skip_start, 0);
            self.first_emitted = true;
        }
        Ok(out)
    }

    /// A byte-offset estimate from the average byte rate, then resync on the
    /// next valid frame sync. Not sample-accurate: real VBR files have no
    /// exact way to convert a timestamp to a byte offset without the Xing
    /// table of contents, which this does not yet consult.
    #[allow(
        clippy::integer_division,
        reason = "converting an estimated sample count to a frame index is a floor by definition"
    )]
    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        let byte_offset = match target {
            SeekTarget::Byte(b) => b,
            SeekTarget::Timestamp { ts, .. } => {
                let rate = self.avg_byte_rate.ok_or(Error::NotSeekable)?;
                let Some(ticks) = ts.ticks() else {
                    return Err(Error::InvalidData(
                        "mpegaudio: seek target has no timestamp",
                    ));
                };
                let secs = ticks.max(0) as u64 as f64 / f64::from(TIME_BASE.den);
                self.audio_start.saturating_add((secs * rate as f64) as u64)
            }
            SeekTarget::Frame { .. } => {
                return Err(Error::Unsupported("mpegaudio: frame-indexed seek"));
            }
        };
        self.io.seek(byte_offset.max(self.audio_start))?;
        let header = find_next_header(&mut self.io)?;
        let rate = self.avg_byte_rate.unwrap_or(1).max(1);
        let elapsed_secs = (byte_offset.saturating_sub(self.audio_start)) as f64 / rate as f64;
        let samples = (elapsed_secs * f64::from(header.sample_rate_hz())) as u64;
        self.frame_index = samples / u64::from(header.samples_per_frame().max(1));
        self.next = self.read_one_frame()?;
        self.first_emitted = true;
        Ok(())
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        self.stream.duration()
    }
}

fn attach_skip(packet: &mut Packet, start: u32, end: u32) {
    if start == 0 && end == 0 {
        return;
    }
    for sd in &mut packet.side_data {
        if let PacketSideData::SkipSamples {
            start: s, end: e, ..
        } = sd
        {
            *s = s.saturating_add(start);
            *e = e.saturating_add(end);
            return;
        }
    }
    packet.side_data.push(PacketSideData::SkipSamples {
        start,
        end,
        skip_reason: 0,
        discard_reason: 0,
    });
}

/// A parsed VBR/CBR header frame: how many bytes of the stream it occupies
/// (the whole frame, since it carries no audio of its own) and whatever
/// duration/gapless fields it stated.
struct FirstFrameTag {
    consumed: u32,
    total_frames: Option<u32>,
    total_bytes: Option<u32>,
    lame: Option<vaco_format_mpegaudio::LameTag>,
}

fn read_first_frame_tag(
    io: &mut IoContext,
    header: MpegAudioHeader,
    _budget: &mut Budget,
) -> Result<Option<FirstFrameTag>> {
    let Some(len) = header.frame_len() else {
        return Ok(None);
    };
    let peek = io.peek(len as usize)?;
    if let Some(side) = header.side_info_len() {
        let off = MpegAudioHeader::LEN + header.crc_len() + side;
        if let Some(region) = peek.get(off..)
            && let Some(xing) = XingHeader::parse(region)
        {
            return Ok(Some(FirstFrameTag {
                consumed: len,
                total_frames: xing.num_frames,
                total_bytes: xing.num_bytes,
                lame: xing.lame,
            }));
        }
    }
    if let Some(region) = peek.get(vbri::FRAME_OFFSET..)
        && let Some(v) = VbriHeader::parse(region)
    {
        return Ok(Some(FirstFrameTag {
            consumed: len,
            total_frames: Some(v.num_frames),
            total_bytes: Some(v.num_bytes),
            lame: None,
        }));
    }
    Ok(None)
}

fn configure_stream(stream: &mut Stream, header: MpegAudioHeader, tag: Option<&FirstFrameTag>) {
    let mut audio = AudioParameters {
        sample_rate: header.sample_rate_hz(),
        format: Some(SampleFmt::F32P),
        layout: Some(if header.channels() == 1 {
            ChannelLayout::MONO
        } else {
            ChannelLayout::STEREO
        }),
        bits_per_coded_sample: Some(0),
        bits_per_raw_sample: None,
        initial_padding: 0,
    };
    let codec_id = match header.layer {
        Layer::I => CodecId::Mp1,
        Layer::II => CodecId::Mp2,
        Layer::III => CodecId::Mp3,
    };
    let bit_rate = header.bitrate_kbps().map(|k| u64::from(k) * 1000);

    if let Some(t) = tag
        && let Some(frames) = t.total_frames
    {
        let samples_per_frame = u64::from(header.samples_per_frame());
        let delay = t.lame.map_or(0, |l| u64::from(l.encoder_delay));
        let padding = t.lame.map_or(0, |l| u64::from(l.encoder_padding));
        let total_samples = u64::from(frames)
            .saturating_mul(samples_per_frame)
            .saturating_sub(delay)
            .saturating_sub(padding);
        let tps = ticks_per_sample(audio.sample_rate);
        stream.set_duration_ts(ticks_i64(total_samples.saturating_mul(tps)));
        if delay > 0 {
            let skip = delay.saturating_add(u64::from(DECODER_DELAY));
            stream.start_time = Timestamp::new(ticks_i64(skip.saturating_mul(tps)));
        }
    }

    audio.bits_per_raw_sample = None;
    let mut params = CodecParameters::new(MediaType::Audio);
    params.codec_id = Some(codec_id);
    params.bit_rate = bit_rate;
    params.audio = Some(audio);
    stream.params = params;
}

/// Attempt to read and merge a trailing `ID3v1` tag, restoring `io` to
/// `resume_at` (the first real frame) afterward regardless of outcome — this
/// is best-effort metadata, not load-bearing for demuxing.
fn merge_trailing_id3v1(io: &mut IoContext, stream: &mut Stream, resume_at: u64) {
    let Some(size) = io.size() else { return };
    let tag_len = vaco_format_id3::id3v1::LEN as u64;
    if size < tag_len || io.seek(size - tag_len).is_err() {
        return;
    }
    let mut buf = [0u8; 128];
    if io.read_exact(&mut buf).is_ok()
        && let Some(tag) = Id3v1Tag::parse(&buf)
    {
        for (k, v) in tag.entries() {
            if !stream
                .metadata
                .iter()
                .any(|(ek, _)| ek.eq_ignore_ascii_case(&k))
            {
                stream.metadata.push((k, v));
            }
        }
    }
    let _ = io.seek(resume_at);
}

fn read_leading_id3v2(io: &mut IoContext, budget: &mut Budget) -> (u64, Vec<(String, String)>) {
    let Ok(peek) = io.peek(vaco_format_id3::header::LEN) else {
        return (0, Vec::new());
    };
    let Ok(header) = Id3v2Header::parse(peek) else {
        return (0, Vec::new());
    };
    let total = header.total_len();
    let Ok(total_usize) = usize::try_from(total) else {
        return (0, Vec::new());
    };
    let Ok(mut buf) = budget.alloc::<u8>(total_usize) else {
        return (0, Vec::new());
    };
    if io.read_exact(&mut buf).is_err() {
        return (0, Vec::new());
    }
    let entries = Id3v2Tag::parse(&buf, budget)
        .map(|t| t.entries)
        .unwrap_or_default();
    (total_usize as u64, entries)
}

/// Advance `io` byte by byte until a syntactically valid header is found.
fn find_next_header(io: &mut IoContext) -> Result<MpegAudioHeader> {
    let mut skipped = 0u32;
    loop {
        let peek = io.peek(4)?;
        let Some(chunk) = peek.first_chunk::<4>() else {
            return Err(Error::Eof);
        };
        if let Some(h) = MpegAudioHeader::parse(u32::from_be_bytes(*chunk)) {
            return Ok(h);
        }
        io.skip(1)?;
        skipped = skipped.saturating_add(1);
        if skipped > MAX_RESYNC {
            return Err(Error::InvalidData(
                "mpegaudio: no frame sync found within the resync window",
            ));
        }
    }
}

/// A free-format frame's length is not stated anywhere in its header; the
/// only way to know it is to find where the next sync falls — but a bare
/// sync match is not enough; see below.
///
/// `this_header` is the header of the frame whose length is being measured,
/// so a candidate length can be checked for more than "some header starts
/// here": the header there must be the same version, layer and sample rate
/// (a real stream never changes those frame-to-frame; a false sync inside
/// Huffman-coded payload bytes has no reason to preserve them), and, for a
/// genuinely free-format stream, `bitrate_index == 0` again (a free-format
/// encoder is free-format for the whole stream, not just its first frame).
/// A candidate that fails this is not a frame boundary; keep scanning from
/// one byte later rather than accepting the first raw sync match.
fn measure_free_format_len(io: &mut IoContext, this_header: MpegAudioHeader) -> Result<usize> {
    let mut len = MpegAudioHeader::LEN;
    loop {
        let peek = io.peek(len.saturating_add(4))?;
        let Some(candidate) = peek.get(len..) else {
            return Err(Error::Eof);
        };
        let Some(chunk) = candidate.first_chunk::<4>() else {
            return Err(Error::InvalidData(
                "mpegaudio: free-format frame runs past the end of input",
            ));
        };
        if let Some(next) = MpegAudioHeader::parse(u32::from_be_bytes(*chunk))
            && next.version == this_header.version
            && next.layer == this_header.layer
            && next.sample_rate_index == this_header.sample_rate_index
            && next.bitrate_index == this_header.bitrate_index
        {
            return Ok(len);
        }
        len = len.saturating_add(1);
        if len > MAX_RESYNC as usize {
            return Err(Error::InvalidData(
                "mpegaudio: free-format frame did not resynchronise",
            ));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod free_format_tests {
    use super::*;
    use vaco_format_mpegaudio::{ChannelMode, Emphasis, Layer, Version};
    use vaco_io::{IoContext, IoOptions, MemorySource};

    fn free_format_header() -> MpegAudioHeader {
        MpegAudioHeader {
            version: Version::Mpeg1,
            layer: Layer::III,
            has_crc: false,
            bitrate_index: 0, // free-format
            sample_rate_index: 0,
            padding: false,
            private_bit: false,
            channel_mode: ChannelMode::Mono,
            mode_extension: 0,
            copyright: false,
            original: false,
            emphasis: Emphasis::None,
        }
    }

    /// A false sync (`0x7FF` inside otherwise-arbitrary payload bytes) at an
    /// earlier offset than the real next frame must not be accepted just
    /// because a header parses there — it has to look like a continuation
    /// of the same stream (matching version/layer/sample rate/bitrate
    /// index), which a byte coincidence has no reason to do.
    #[test]
    fn a_false_sync_with_the_wrong_version_is_not_accepted() {
        let this_header = free_format_header();
        let mut data = this_header.to_bytes().to_vec();
        // A "sync" 20 bytes in whose version bits differ from `this_header`
        // — must be skipped even though `0x7FF` still matches.
        let mut false_header = this_header;
        false_header.version = Version::Mpeg2;
        data.resize(20, 0);
        data.extend_from_slice(&false_header.to_bytes());
        // The real next frame, another 10 bytes later, genuinely matches.
        data.resize(30, 0);
        data.extend_from_slice(&this_header.to_bytes());

        let mut io =
            IoContext::new(Box::new(MemorySource::new(data)), &IoOptions::default()).unwrap();
        let len = measure_free_format_len(&mut io, this_header).unwrap();
        assert_eq!(
            len, 30,
            "must skip the false sync and land on the real next frame"
        );
    }

    /// A genuine same-parameters header at the very next 4 bytes is
    /// accepted immediately.
    #[test]
    fn a_genuine_continuation_is_accepted_at_the_first_candidate() {
        let this_header = free_format_header();
        let mut data = this_header.to_bytes().to_vec();
        data.resize(21, 0);
        data.extend_from_slice(&this_header.to_bytes());

        let mut io =
            IoContext::new(Box::new(MemorySource::new(data)), &IoOptions::default()).unwrap();
        let len = measure_free_format_len(&mut io, this_header).unwrap();
        assert_eq!(len, 21);
    }
}
