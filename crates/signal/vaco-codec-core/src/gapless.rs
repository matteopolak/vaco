//! The one place `PacketSideData::SkipSamples` is honoured.
//!
//! Three demuxers state a gapless trim — `vaco-demux-mp4` from an `elst`
//! edit list, `vaco-demux-matroska` from `CodecDelay`, `vaco-demux-mpegaudio`
//! from a LAME/Xing tag — and every one of them says it the same way, as
//! [`PacketSideData::SkipSamples`] on the packet it applies to. Before this
//! module, exactly one of the workspace's audio decoders read that side data
//! (`vaco-codec-mpegaudio`), so AAC in MP4 emitted its 1024-sample encoder
//! priming and its trailing padding into the output, measured against
//! `ffmpeg 9.0.1` as 1912 extra samples on an 88-frame file.
//!
//! Per-decoder trimming is the "two lists that must agree" shape: the list of
//! decoders that could be handed a trim, and the list that remembers to apply
//! one. [`DecoderDesc::build`](crate::DecoderDesc::build) is the single place
//! every live [`Decoder`] in the workspace is constructed, so wrapping audio
//! decoders there removes the second list.
//!
//! **Divergence from the reference**: the reference also advances a trimmed
//! frame's presentation timestamp by the samples it dropped. Nothing here
//! does, because an audio decoder in this workspace leaves
//! `Frame::time_base` at `Rational::ONE` while `Frame::pts` is in the
//! *packet's* time base — there is no sound conversion available at this
//! layer. `vaco-codec-mpegaudio` already behaved this way before the trim
//! moved here, so this is the status quo, not a new gap.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData, PacketSideDataKind};

use crate::{Decoder, threading::Threading};

/// Wraps an audio decoder and applies each packet's gapless trim to the
/// frames decoded from it.
pub struct GaplessDecoder {
    inner: Box<dyn Decoder>,
    budget: Budget,
    /// Samples still to drop from the front. A `u64` because the count can
    /// exceed one frame and must then carry into the next: an MPEG-2 LSF
    /// Layer III frame is 576 samples while the LAME trim is 1105, which is
    /// how 16/22.05/24 kHz MP3 files kept exactly 529 samples the reference
    /// dropped.
    pending_start: u64,
    /// Samples to drop from the back of the frame decoded from the packet
    /// that stated it. Replaced, not accumulated, because each packet's
    /// `discard_padding` describes its own frame.
    pending_end: u32,
}

impl GaplessDecoder {
    /// Wrap `inner` if it could ever be handed a trim.
    ///
    /// Only audio carries one, and a decoder that is never sent
    /// [`PacketSideData::SkipSamples`] pays one `Option` check per packet and
    /// two integer comparisons per frame — no allocation and no copy.
    #[must_use]
    pub fn new(inner: Box<dyn Decoder>, limits: Limits) -> Self {
        Self {
            inner,
            budget: Budget::new(limits),
            pending_start: 0,
            pending_end: 0,
        }
    }
}

impl std::fmt::Debug for GaplessDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GaplessDecoder")
            .field("pending_start", &self.pending_start)
            .field("pending_end", &self.pending_end)
            .finish_non_exhaustive()
    }
}

impl Decoder for GaplessDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        // Accept first: `Error::OutputPending` means the packet was *not*
        // taken and the caller will send it again, so counting its trim
        // before the inner call would double it on the retry.
        self.inner.send_packet(packet)?;
        if let Some(packet) = packet
            && let Some(PacketSideData::SkipSamples { start, end, .. }) =
                packet.side_data(PacketSideDataKind::SkipSamples)
        {
            self.pending_start = self.pending_start.saturating_add(u64::from(*start));
            self.pending_end = *end;
        }
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        loop {
            let frame = self.inner.receive_frame()?;
            if self.pending_start == 0 && self.pending_end == 0 {
                return Ok(frame);
            }
            let FrameData::Audio { samples, .. } = frame.data else {
                return Ok(frame);
            };
            let front = u32::try_from(self.pending_start.min(u64::from(samples)))
                .unwrap_or(samples)
                .min(samples);
            self.pending_start -= u64::from(front);
            let after_front = samples - front;
            let back = self.pending_end.min(after_front);
            self.pending_end -= back;
            if front == 0 && back == 0 {
                return Ok(frame);
            }
            let trimmed = trim(frame, front, back, &mut self.budget)?;
            // A frame the trim consumed entirely is not an empty frame to
            // hand on; it is a frame that does not exist. The first packet of
            // an `ffmpeg -c:a aac` MP4 is exactly this — 1024 samples of
            // priming, trimmed by 1024.
            if sample_count(&trimmed) > 0 {
                return Ok(trimmed);
            }
        }
    }

    fn flush(&mut self) {
        // A seek re-arms the leading trim from the demuxer's own side data,
        // so nothing carried over from before the seek stays pending.
        self.pending_start = 0;
        self.pending_end = 0;
        self.inner.flush();
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.inner.set_extradata(extradata)
    }

    fn prime_video(&mut self, width: u32, height: u32) {
        self.inner.prime_video(width, height);
    }

    fn prime_audio(&mut self, sample_rate: u32, layout: vaco_chlayout::ChannelLayout) {
        self.inner.prime_audio(sample_rate, layout);
    }

    fn set_thread_count(&mut self, threads: usize) -> Threading {
        self.inner.set_thread_count(threads)
    }
}

fn sample_count(frame: &Frame) -> u32 {
    match frame.data {
        FrameData::Audio { samples, .. } => samples,
        _ => 0,
    }
}

/// Drop `front` samples from the start and `back` from the end.
///
/// Builds a correctly-sized frame rather than shrinking in place: a `Plane`'s
/// byte length is fixed at allocation, so there is no "report fewer bytes"
/// operation to reach for. The byte offsets come from
/// `SampleFmt::plane_size`, which is what makes this correct for an
/// interleaved format as well as a planar one — a plane of a packed format
/// holds every channel, so one sample is `bytes_per_sample * channels` wide,
/// not `bytes_per_sample`.
fn trim(frame: Frame, front: u32, back: u32, budget: &mut Budget) -> Result<Frame> {
    let FrameData::Audio {
        format,
        sample_rate,
        samples,
        ref layout,
        ref planes,
    } = frame.data
    else {
        return Ok(frame);
    };
    let layout = layout.clone();
    let channels = layout.channels;
    let new_samples = samples.saturating_sub(front).saturating_sub(back);
    let size = |n: u32| {
        format
            .plane_size(channels, n)
            .ok_or(Error::InvalidData("gapless: audio plane size overflows"))
    };
    let skip_bytes = size(front)?;
    let keep_bytes = size(new_samples)?;

    let mut out = Frame::alloc_audio(budget, format, layout, new_samples, sample_rate)?;
    for ch in 0..planes.len() {
        let Some(src) = frame.plane(ch) else { continue };
        let Some(src_row) = src.row(0) else { continue };
        let Some(src_slice) = src_row.get(skip_bytes..skip_bytes.saturating_add(keep_bytes)) else {
            continue;
        };
        if let Some(mut dst) = out.plane_mut(ch)
            && let Some(dst_row) = dst.row_mut(0)
        {
            let n = src_slice.len().min(dst_row.len());
            if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_slice.get(..n)) {
                d.copy_from_slice(s);
            }
        }
    }
    out.pts = frame.pts;
    // The sample count changed, so the frame's own duration is no longer the
    // one the decoder computed. `1/sample_rate` is the only time base a
    // sample count converts through unambiguously.
    out.duration = vaco_core::Timestamp::new(i64::from(new_samples))
        .to_duration(vaco_core::Rational::new(
            1,
            i32::try_from(sample_rate).unwrap_or(1).max(1),
        ))
        .unwrap_or(frame.duration);
    out.time_base = frame.time_base;
    out.flags = frame.flags;
    out.color = frame.color;
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test code; the ramp values are exact small integers in f32"
)]
mod tests {
    use super::*;
    use vaco_chlayout::ChannelLayout;
    use vaco_sampfmt::SampleFmt;

    /// `samples` samples of a ramp, one plane, so a trim's effect is visible
    /// as *which* values survive rather than merely how many.
    fn ramp_frame(format: SampleFmt, layout: ChannelLayout, samples: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame =
            Frame::alloc_audio(&mut budget, format, layout, samples, 44100).expect("alloc");
        let mut plane = frame.plane_mut(0).expect("plane 0");
        let row = plane.row_mut(0).expect("row 0");
        for (i, chunk) in row.chunks_exact_mut(4).enumerate() {
            chunk.copy_from_slice(&(i as f32).to_le_bytes());
        }
        frame
    }

    fn values(frame: &Frame) -> Vec<f32> {
        let plane = frame.plane(0).expect("plane 0");
        let row = plane.row(0).expect("row 0");
        row.chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn front_and_back_trim_keep_the_middle() {
        let frame = ramp_frame(SampleFmt::F32P, ChannelLayout::MONO, 10);
        let mut budget = Budget::new(Limits::permissive());
        let out = trim(frame, 3, 2, &mut budget).expect("trim");
        assert_eq!(sample_count(&out), 5);
        assert_eq!(values(&out), vec![3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    /// The bug an interleaved format would hide from a planar-only test: one
    /// sample of a packed frame is `bytes_per_sample * channels` wide, so a
    /// 2-sample front trim on stereo must skip four `f32`s, not two.
    #[test]
    fn front_trim_of_an_interleaved_frame_counts_every_channel() {
        let frame = ramp_frame(SampleFmt::F32, ChannelLayout::STEREO, 6);
        let mut budget = Budget::new(Limits::permissive());
        let out = trim(frame, 2, 1, &mut budget).expect("trim");
        assert_eq!(sample_count(&out), 3);
        assert_eq!(values(&out), vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn trim_exceeding_the_frame_empties_it_rather_than_panicking() {
        let frame = ramp_frame(SampleFmt::F32P, ChannelLayout::MONO, 4);
        let mut budget = Budget::new(Limits::permissive());
        let out = trim(frame, 100, 100, &mut budget).expect("trim");
        assert_eq!(sample_count(&out), 0);
    }

    /// A decoder emitting fixed-size frames, so a trim wider than one frame
    /// has somewhere to carry to. This is the shape that made 16/22.05/24 kHz
    /// MP3 keep 529 samples the reference dropped: the old per-packet trim
    /// clamped at the frame it was handed and discarded the remainder.
    #[derive(Debug)]
    struct FixedFrames {
        frames: Vec<Frame>,
    }

    impl FixedFrames {
        fn new(count: usize, samples: u32) -> Self {
            Self {
                frames: (0..count)
                    .map(|_| ramp_frame(SampleFmt::F32P, ChannelLayout::MONO, samples))
                    .rev()
                    .collect(),
            }
        }
    }

    impl Decoder for FixedFrames {
        fn send_packet(&mut self, _packet: Option<&Packet>) -> Result<()> {
            Ok(())
        }
        fn receive_frame(&mut self) -> Result<Frame> {
            self.frames.pop().ok_or(Error::Eof)
        }
        fn flush(&mut self) {}
    }

    fn packet_with_skip(start: u32, end: u32) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, &[0u8; 4]).expect("packet");
        pkt.set_side_data(PacketSideData::SkipSamples {
            start,
            end,
            skip_reason: 0,
            discard_reason: 0,
        });
        pkt
    }

    #[test]
    fn a_leading_trim_wider_than_one_frame_carries_into_the_next() {
        // 576-sample frames, a 1105-sample trim: the whole first frame plus
        // 529 samples of the second.
        let inner = Box::new(FixedFrames::new(3, 576));
        let mut dec = GaplessDecoder::new(inner, Limits::permissive());
        dec.send_packet(Some(&packet_with_skip(1105, 0)))
            .expect("send");
        let first = dec.receive_frame().expect("a frame survives the trim");
        assert_eq!(
            sample_count(&first),
            576 - 529,
            "the first frame the trim did not consume outright must lose the remainder, not zero"
        );
        assert_eq!(values(&first)[0], 529.0);
        let second = dec.receive_frame().expect("second");
        assert_eq!(sample_count(&second), 576, "the trim is spent by now");
    }

    #[test]
    fn a_trailing_trim_shortens_the_frame_of_the_packet_that_stated_it() {
        let inner = Box::new(FixedFrames::new(1, 1024));
        let mut dec = GaplessDecoder::new(inner, Limits::permissive());
        dec.send_packet(Some(&packet_with_skip(0, 888)))
            .expect("send");
        let frame = dec.receive_frame().expect("frame");
        assert_eq!(sample_count(&frame), 1024 - 888);
        assert_eq!(values(&frame)[0], 0.0);
    }

    #[test]
    fn a_packet_with_no_skip_side_data_passes_the_frame_through_untouched() {
        let inner = Box::new(FixedFrames::new(1, 32));
        let mut dec = GaplessDecoder::new(inner, Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[0u8; 4]).expect("packet");
        dec.send_packet(Some(&pkt)).expect("send");
        let frame = dec.receive_frame().expect("frame");
        assert_eq!(sample_count(&frame), 32);
        assert_eq!(values(&frame)[0], 0.0);
    }

    /// A retried packet must not have its trim counted twice — `send_packet`
    /// returning `OutputPending` means the packet was not taken.
    #[test]
    fn a_rejected_packet_does_not_bank_its_trim() {
        #[derive(Debug)]
        struct RejectsOnce {
            rejected: bool,
            inner: FixedFrames,
        }
        impl Decoder for RejectsOnce {
            fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
                if !self.rejected {
                    self.rejected = true;
                    return Err(Error::OutputPending);
                }
                self.inner.send_packet(packet)
            }
            fn receive_frame(&mut self) -> Result<Frame> {
                self.inner.receive_frame()
            }
            fn flush(&mut self) {}
        }
        let inner = Box::new(RejectsOnce {
            rejected: false,
            inner: FixedFrames::new(1, 100),
        });
        let mut dec = GaplessDecoder::new(inner, Limits::permissive());
        let pkt = packet_with_skip(10, 0);
        assert!(matches!(
            dec.send_packet(Some(&pkt)),
            Err(Error::OutputPending)
        ));
        dec.send_packet(Some(&pkt)).expect("the retry is accepted");
        let frame = dec.receive_frame().expect("frame");
        assert_eq!(
            sample_count(&frame),
            90,
            "10 samples trimmed once, not 20 for the rejected send plus the retry"
        );
    }
}
