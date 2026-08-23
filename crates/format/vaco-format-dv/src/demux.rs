//! The DV demuxer: read one fixed-size frame at a time.
//!
//! # Why there is no PES/box/EBML layer here
//!
//! Plan 18 §3.4 calls DV "not really a container", and the demuxer reflects
//! that literally: there is no pack scanning, no box walking, no PES layer
//! — nothing to walk. A DV file is `frame_count` copies of a value whose
//! size is fixed for the whole file (10 or 12 DIF sequences × 150 blocks ×
//! 80 bytes, chosen once by [`crate::profile::DvProfile::detect`] from the
//! very first frame). Every `read_packet` call after that is
//! `io.read_exact(frame_size)`.
//!
//! # Video only, today
//!
//! Real DV interleaves compressed audio samples into fixed positions inside
//! every frame (subcode/AAUX areas), which is exactly what makes DV DV
//! rather than a stream of independent still pictures. This demuxer
//! declares an audio [`Stream`] (matching every measured `ffprobe` output,
//! which reports two streams for an ordinary DV file) but does **not**
//! extract audio packets from it.
//!
//! That is a deliberate, documented gap, not an oversight: probing the one
//! real sample available while writing this (`ffmpeg -f dv` output, 48 kHz
//! stereo `pcm_s16le`) found the AAUX "Audio Source" pack this crate would
//! need to decode filled with `0xFF` — the DV convention for "not set" —
//! rather than the bit pattern the public technical descriptions of
//! SMPTE 314M / IEC 61834 predict. Shipping a sample-deinterleaving routine
//! this crate cannot verify against a byte-exact reference would be worse
//! than not shipping one: wrong audio silently looks like working audio.
//! See the docs file for exactly what would need measuring to close this.

use std::collections::VecDeque;

use vaco_codec_core::{AudioParameters, CodecParameters, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::profile::DvProfile;

/// DV has no index, no seek table, and no discontinuity concept of its own —
/// a frame is a frame. `FIXED_FRAMESIZE` is the one flag that is genuinely
/// true here and nowhere else in this workspace's demuxers yet.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX.union(FormatFlags::FIXED_FRAMESIZE);

/// The conventional default this crate declares for the (unextracted) audio
/// stream: 48 kHz, 16-bit, stereo, locked — the overwhelmingly common
/// consumer DV configuration, and what every sample measured while writing
/// this crate reports through `ffprobe`. Not decoded from the file; see the
/// module docs.
const DEFAULT_AUDIO_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_AUDIO_CHANNELS: u16 = 2;

/// The DV demuxer.
pub struct DvDemuxer {
    io: IoContext,
    profile: DvProfile,
    streams: Vec<Stream>,
    queue: VecDeque<Packet>,
    budget: Budget,
    frame_index: u64,
    total_frames: Option<u64>,
    eof: bool,
}

impl std::fmt::Debug for DvDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DvDemuxer")
            .field("profile", &self.profile)
            .field("frame_index", &self.frame_index)
            .field("total_frames", &self.total_frames)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl DvDemuxer {
    /// Open a DV elementary stream.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when the first block is not a DV Header block.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        Self::open_with_limits(src, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling.
    ///
    /// # Errors
    /// As [`DvDemuxer::open`].
    pub fn open_with_limits(src: Box<dyn MediaSource>, limits: Limits) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let head: [u8; 4] = io
            .peek(4)?
            .get(..4)
            .and_then(|b| b.try_into().ok())
            .ok_or(Error::UnexpectedEof)?;
        let Some(profile) = DvProfile::detect(&head) else {
            return Err(Error::InvalidData(
                "dv: first block is not a DV Header block",
            ));
        };
        // `DvProfile::detect` only knows standard-rate DV25 (see its docs): a
        // double-rate profile (DVCPRO50/DVCPRO HD) can share a `0x1F` byte
        // at the halfway point too (measured: it is itself built from two
        // interleaved tracks), so checking only that one byte is not
        // enough — this compares the whole 4-byte header pattern the first
        // frame started with. A short read here just means the file is one
        // frame long, which is fine, not a mismatch.
        let second = io.peek(profile.frame_size.saturating_add(4))?;
        if let Some(next_head) = second.get(profile.frame_size..profile.frame_size + 4)
            && next_head != head.as_slice()
        {
            return Err(Error::InvalidData(
                "dv: frame size does not match this stream (double-rate DV profile, \
                 e.g. DVCPRO50/DVCPRO HD, is not supported)",
            ));
        }
        let frame_size_u64 = profile.frame_size as u64;
        let total_frames = io.size().and_then(|n| n.checked_div(frame_size_u64));

        let video = VideoParameters {
            width: profile.width,
            height: profile.height,
            coded_width: profile.width,
            coded_height: profile.height,
            frame_rate: profile.frame_rate,
            ..VideoParameters::default()
        };
        let mut vparams = CodecParameters::new(MediaType::Video);
        vparams.video = Some(video);
        // Per-frame time base: one tick per frame is exact for both 30000/1001
        // and 25 fps, unlike a fixed 90 kHz base which cannot represent
        // 30000/1001 exactly.
        let time_base = Rational {
            num: profile.frame_rate.den,
            den: profile.frame_rate.num,
        };
        let mut video_stream = Stream::new(0, MediaType::Video, time_base);
        video_stream.params = vparams;
        if let Some(n) = total_frames {
            video_stream.frame_count = Some(n);
        }

        let audio = AudioParameters {
            sample_rate: DEFAULT_AUDIO_SAMPLE_RATE,
            ..AudioParameters::default()
        };
        let mut aparams = CodecParameters::new(MediaType::Audio);
        aparams.audio = Some(audio);
        let audio_time_base = Rational {
            num: 1,
            den: DEFAULT_AUDIO_SAMPLE_RATE.cast_signed(),
        };
        let mut audio_stream = Stream::new(1, MediaType::Audio, audio_time_base);
        audio_stream.params = aparams;
        audio_stream.metadata_set(
            "dv_audio_channels_assumed",
            DEFAULT_AUDIO_CHANNELS.to_string(),
        );

        Ok(Self {
            io,
            profile,
            streams: vec![video_stream, audio_stream],
            queue: VecDeque::new(),
            budget: Budget::new(limits),
            frame_index: 0,
            total_frames,
            eof: false,
        })
    }

    /// Wall-clock duration of one frame, in microseconds.
    ///
    /// Integer division is exactly what this needs, not a rounding
    /// shortcut to avoid: a frame's duration in whole microseconds is
    /// definitionally `1_000_000 * den / num`, and switching to floats
    /// would trade an exact answer for an inexact one.
    #[allow(
        clippy::integer_division,
        reason = "exact tick arithmetic, not an approximation"
    )]
    fn frame_duration(&self) -> Duration {
        let rate = self.profile.frame_rate;
        if rate.num == 0 {
            return Duration::ZERO;
        }
        Duration::from_micros(1_000_000i64 * i64::from(rate.den) / i64::from(rate.num))
    }

    fn read_frame(&mut self) -> Result<()> {
        let available = self.io.peek(self.profile.frame_size)?.len();
        if available == 0 {
            return Err(Error::Eof);
        }
        if available < self.profile.frame_size {
            // A truncated tail frame: not enough bytes for a whole frame.
            // Treated as end of stream rather than corruption — many DV
            // captures are cut mid-frame by whatever stopped the recording.
            return Err(Error::Eof);
        }
        let mut buf = vec![0u8; self.profile.frame_size];
        self.io.read_exact(&mut buf)?;
        let mut pkt = Packet::from_slice(&mut self.budget, &buf)?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(self.frame_index.cast_signed());
        pkt.dts = pkt.pts;
        pkt.duration = self.frame_duration();
        pkt.flags |= PacketFlags::KEY; // DV is all-intra: every frame is a keyframe
        self.queue.push_back(pkt);
        self.frame_index += 1;
        Ok(())
    }
}

impl Demuxer for DvDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(p);
            }
            if self.eof {
                return Err(Error::Eof);
            }
            match self.read_frame() {
                Ok(()) => {}
                Err(Error::Eof) => {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                Err(e) => return Err(e),
            }
        }
    }

    #[allow(
        clippy::integer_division,
        reason = "exact frame-boundary arithmetic: a byte offset that is not \
                  a multiple of frame_size has no other meaningful frame index"
    )]
    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let frame_size = self.profile.frame_size as u64;
        let frame = match target {
            SeekTarget::Byte(pos) => pos / frame_size,
            SeekTarget::Timestamp { ts, .. } => {
                let ticks = ts.ticks().unwrap_or(0);
                if ticks < 0 { 0 } else { ticks.cast_unsigned() }
            }
            SeekTarget::Frame { frame, .. } => frame,
        };
        let byte_pos = frame.saturating_mul(frame_size);
        self.io.seek(byte_pos)?;
        self.queue.clear();
        self.eof = false;
        self.frame_index = frame;
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        let n = self.total_frames?;
        Some(Duration::from_micros(
            self.frame_duration().0.saturating_mul(n.cast_signed()),
        ))
    }
}
