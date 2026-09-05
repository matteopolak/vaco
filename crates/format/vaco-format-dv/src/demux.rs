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

use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, VideoParameters};
use vaco_color::{ChromaLocation, ColorInfo};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

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

/// DV's own fixed tick rate for the video stream's `time_base`, measured
/// directly against real `ffprobe` on both NTSC and PAL DV -- see the
/// constructor's comment on `video_stream` for the measurement itself. Named
/// rather than inlined because [`DvDemuxer::ticks_per_frame`] and
/// [`DvDemuxer::seek`] both need the same value the constructor used.
const DV_TIME_BASE_DEN: i32 = 60_000;

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
            // Chroma follows the system, measured on both:
            //
            //   ffprobe -show_entries stream=pix_fmt ntsc.dv  # yuv411p
            //   ffprobe -show_entries stream=pix_fmt pal.dv   # yuv420p
            //
            // (IEC 61834 PAL is 4:2:0; the 25 Mbps NTSC variant is 4:1:1.
            // DVCPRO50's 4:2:2 is a different profile this crate does not
            // detect yet, so nothing here claims to cover it.)
            format: Some(if profile.is_pal {
                PixFmt::Yuv420p
            } else {
                PixFmt::Yuv411p
            }),
            // DV's pixels are never square: the format samples at a fixed
            // 720 luma columns regardless of the picture's true 4:3 shape,
            // so every 4:3 DV stream needs a non-square SAR to display
            // correctly, and it is a fixed, standard value, not something
            // this crate could compute from the frame it has already
            // parsed. Measured directly (`ffmpeg -c:v dvvideo`, real
            // `ffprobe`): 720x480 (NTSC) reports `sample_aspect_ratio=8:9`;
            // 720x576 (PAL) reports `16:15`. Only the 4:3 case is filled
            // in -- DV's widescreen flag lives in a VAUX subcode pack this
            // crate does not read yet (16:9 NTSC/PAL are `32:27`/`64:45`
            // per the DV standard, but that is recalled, not measured
            // against a real 16:9 fixture the way the two rows above are,
            // so it is named here rather than guessed into the table).
            sample_aspect_ratio: if profile.is_pal {
                Rational::new(16, 15)
            } else {
                Rational::new(8, 9)
            },
            // Measured directly (`ffmpeg -c:v dvvideo`, real `ffprobe`) on
            // both NTSC (yuv411p) and PAL (yuv420p) DV: `chroma_location=
            // topleft` in both cases. This is DV's own siting convention,
            // distinct from the MPEG-1/2/4 family's unconditional `left`
            // (see `vaco-parse-mpegvideo`'s `mpeg12.rs`/`mpeg4.rs`) -- two
            // different codecs, two different fixed conventions, not one
            // shared default.
            color: ColorInfo {
                chroma_location: ChromaLocation::TopLeft,
                ..ColorInfo::default()
            },
            // Measured directly (`ffmpeg -f lavfi ... -c:v dvvideo`, real
            // `ffprobe`, both NTSC and PAL): `field_order=unknown`. DV
            // carries no interlace-flag bit this crate reads (or that SMPTE
            // 314M states for the header blocks parsed here), and
            // `VideoParameters::field_order`'s own `#[default]` is
            // `Progressive`, which silently reported the wrong value here --
            // the same trap `vaco-parse-mpegvideo`'s `mpeg4.rs` and
            // `vaco-parse-image`'s `jpeg.rs` both independently hit and
            // named. DV has no separate container-level merge step to
            // interact with (unlike Matroska's own `FieldOrder`/
            // `FlagInterlaced` EBML elements), so this is unconditional.
            field_order: vaco_codec_core::FieldOrder::Unknown,
            ..VideoParameters::default()
        };
        let mut vparams = CodecParameters::new(MediaType::Video).with_codec(CodecId::Dvvideo);
        vparams.video = Some(video);
        // Measured directly (`ffmpeg -c:v dvvideo`, real `ffprobe`), on both
        // a 2-frame NTSC fixture and a 150-frame/5s one, and separately on a
        // 1s PAL clip: `time_base=1/60000` and `avg_frame_rate=60000/1` for
        // *both* systems, unconditionally -- not derived from the file's
        // real frame count or duration (the 150-frame NTSC clip still reports
        // exactly `60000/1`, not something closer to `30000/1001`), and not
        // `50/1` for 25 fps PAL either. This is DV's own fixed tick rate, the
        // same for every profile this crate detects, and it is why
        // `DV_TIME_BASE_DEN` divides evenly by both `30000/1001` (2002
        // ticks/frame) and `25/1` (2400 ticks/frame) -- that was the point of
        // choosing it, not a coincidence.
        //
        // `r_frame_rate` is unaffected: it keeps falling back to
        // `video.frame_rate` (the true `30000/1001`/`25/1` rate) exactly as
        // measured.
        let time_base = Rational::new(1, DV_TIME_BASE_DEN);
        let mut video_stream = Stream::new(0, MediaType::Video, time_base);
        video_stream.params = vparams;
        video_stream.avg_frame_rate = Rational::new(DV_TIME_BASE_DEN, 1);
        if let Some(n) = total_frames {
            video_stream.frame_count = Some(n);
        }

        let audio = AudioParameters {
            sample_rate: DEFAULT_AUDIO_SAMPLE_RATE,
            // DV audio is uncompressed 16-bit little-endian PCM. Measured:
            //
            //   ffprobe -show_entries stream=codec_name,sample_fmt t.dv
            //   # pcm_s16le, s16
            format: Some(SampleFmt::S16),
            ..AudioParameters::default()
        };
        let mut aparams = CodecParameters::new(MediaType::Audio).with_codec(CodecId::PcmS16le);
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

    /// Exact wall-clock duration of one frame at the profile's native rate.
    fn frame_duration(&self) -> Duration {
        let rate = self.profile.frame_rate;
        Duration::from_ticks(1, Rational::new(rate.den, rate.num)).unwrap_or(Duration::ZERO)
    }

    /// How many `1/DV_TIME_BASE_DEN` ticks one frame spans: `2002` for NTSC
    /// (`60000 * 1001 / 30000`), `2400` for PAL (`60000 * 1 / 25`). Both of
    /// the profiles this crate detects divide evenly -- see
    /// [`DV_TIME_BASE_DEN`]'s doc comment.
    #[allow(
        clippy::integer_division,
        reason = "exact tick arithmetic: both supported profiles divide evenly"
    )]
    fn ticks_per_frame(&self) -> u64 {
        let rate = self.profile.frame_rate;
        if rate.num == 0 {
            return 0;
        }
        (i64::from(DV_TIME_BASE_DEN) * i64::from(rate.den) / i64::from(rate.num)).cast_unsigned()
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
        pkt.pts = Timestamp::new(
            self.frame_index
                .saturating_mul(self.ticks_per_frame())
                .cast_signed(),
        );
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
        let ticks_per_frame = self.ticks_per_frame().max(1);
        let frame = match target {
            SeekTarget::Byte(pos) => pos / frame_size,
            SeekTarget::Timestamp { ts, .. } => {
                let ticks = ts.ticks().unwrap_or(0);
                // `ts` is in the stream's own `time_base`
                // (`1/DV_TIME_BASE_DEN`, not one-tick-per-frame any more --
                // see the constructor's comment), so this recovers the frame
                // index the same way `read_frame` derived the timestamp from
                // it in the first place.
                if ticks < 0 {
                    0
                } else {
                    ticks.cast_unsigned() / ticks_per_frame
                }
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
        self.duration_exact()
    }

    fn duration_exact(&self) -> Option<vaco_core::ExactDuration> {
        let ticks = self.total_frames?.checked_mul(self.ticks_per_frame())?;
        vaco_core::ExactDuration::from_ticks(
            i64::try_from(ticks).ok()?,
            Rational::new(1, DV_TIME_BASE_DEN),
        )
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod codec_identity_tests {
    use vaco_codec_core::CodecId;
    use vaco_format_core::Demuxer;
    use vaco_pixfmt::PixFmt;
    use vaco_sampfmt::SampleFmt;

    use super::DvDemuxer;
    use crate::profile::DvProfile;

    /// One 25 Mbps DV frame's worth of zeroes with a valid header, for each
    /// system. `DvProfile::detect` reads the header byte at offset 3.
    fn frame(pal: bool) -> Vec<u8> {
        let profile = DvProfile::detect(&header(pal)).expect("a known profile");
        let mut v = header(pal);
        v.resize(profile.frame_size, 0);
        v
    }

    fn header(pal: bool) -> Vec<u8> {
        let mut v = vec![0x1F, 0x07, 0x00, if pal { 0xBF } else { 0x3F }];
        v.resize(80, 0);
        v
    }

    /// The identity fields the reference reports for a DV file, which `vaco`
    /// left entirely empty until CONFORMANCE-FINDINGS 24:
    ///
    /// ```sh
    /// ffprobe -v quiet -of csv=p=0 \
    ///   -show_entries stream=codec_name,pix_fmt,sample_fmt ntsc.dv
    /// # dvvideo,yuv411p
    /// # pcm_s16le,s16
    /// ```
    #[test]
    fn both_streams_carry_a_codec_id_and_a_format() {
        for (pal, want_pix) in [(false, PixFmt::Yuv411p), (true, PixFmt::Yuv420p)] {
            let data = frame(pal);
            let src = Box::new(vaco_io::MemorySource::new(data));
            let d = DvDemuxer::open(src).expect("open");
            let streams = vaco_format_core::Demuxer::streams(&d);
            assert_eq!(streams[0].params.codec_id, Some(CodecId::Dvvideo));
            assert_eq!(
                streams[0].params.video.as_ref().unwrap().format,
                Some(want_pix)
            );
            assert_eq!(streams[1].params.codec_id, Some(CodecId::PcmS16le));
            assert_eq!(
                streams[1].params.audio.as_ref().unwrap().format,
                Some(SampleFmt::S16)
            );
        }
    }

    #[test]
    fn aggregate_duration_keeps_ntsc_frame_clock_exact() {
        let mut demux = DvDemuxer::open(Box::new(vaco_io::MemorySource::new(frame(false))))
            .expect("open NTSC frame");

        assert_eq!(demux.streams()[0].frame_count, Some(1));
        assert_eq!(
            demux.duration().map(vaco_core::Duration::as_ratio),
            Some((1001, 30_000))
        );
        assert_eq!(
            demux
                .duration_exact()
                .map(vaco_core::ExactDuration::as_ratio),
            Some((1_001, 30_000))
        );
        let packet = demux.read_packet().expect("one video packet");
        assert_eq!(packet.pts.ticks(), Some(0));
        assert_eq!(packet.duration.as_ratio(), (1001, 30_000));
        assert_eq!(packet.payload(), frame(false));
        assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
    }
}
