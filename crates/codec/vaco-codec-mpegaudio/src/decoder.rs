//! `Decoder` implementation: per-packet frame-header dispatch to Layer I, II
//! or III, and the shared per-channel synthesis filterbank history.

use std::collections::VecDeque;

use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_format_mpegaudio::{Layer, MpegAudioHeader};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData, PacketSideDataKind};

use crate::layer3::Layer3State;
use crate::synthesis::Synthesis;

#[derive(Debug)]
pub struct MpegAudioDecoder {
    limits: Limits,
    pending: VecDeque<Frame>,
    /// One filterbank per channel, shared by all three layers. Re-created if
    /// a later packet's channel count differs from what it was built for —
    /// a channel-count change mid-stream is not something any layer's
    /// filterbank history can straddle anyway.
    synth: Vec<Synthesis>,
    /// Layer III's bit reservoir and overlap-add history. Lazily created the
    /// first time a Layer III packet arrives.
    layer3: Option<Layer3State>,
}

impl MpegAudioDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: VecDeque::new(),
            synth: Vec::new(),
            layer3: None,
        }
    }

    fn synth_for(&mut self, channels: usize) -> &mut [Synthesis] {
        if self.synth.len() != channels {
            self.synth = (0..channels).map(|_| Synthesis::new()).collect();
        }
        &mut self.synth
    }
}

impl Decoder for MpegAudioDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            return Ok(());
        };
        let payload = packet.payload();
        let header = MpegAudioHeader::parse_bytes(payload)
            .ok_or(Error::InvalidData("mpegaudio: packet does not start with a valid frame header"))?;
        let crc_len = header.crc_len();
        let body_start = MpegAudioHeader::LEN + crc_len;
        let body = payload
            .get(body_start..)
            .ok_or(Error::InvalidData("mpegaudio: packet shorter than its own header"))?;

        let mut budget = Budget::new(self.limits.clone());
        let channels = usize::from(header.channels());
        let frame = match header.layer {
            Layer::I => {
                let synth = self.synth_for(channels);
                crate::layer1::decode(header, body, synth, &mut budget)?
            }
            Layer::II => {
                let synth = self.synth_for(channels);
                crate::layer2::decode(header, body, synth, &mut budget)?
            }
            Layer::III => {
                self.synth_for(channels);
                let state = self.layer3.get_or_insert_with(|| Layer3State::new(channels));
                crate::layer3::decode(header, body, state, &mut self.synth, &mut budget)?
            }
        };
        let (skip_front, skip_back) = match packet.side_data(PacketSideDataKind::SkipSamples) {
            Some(PacketSideData::SkipSamples { start, end, .. }) => (*start, *end),
            _ => (0, 0),
        };
        let frame = trim_gapless(frame, skip_front, skip_back, &mut budget)?;
        if frame_sample_count(&frame) > 0 {
            self.pending.push_back(frame);
        }
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(Error::NeedMoreInput)
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.synth.clear();
        self.layer3 = None;
    }
}

/// Total sample count of an audio frame; `0` for anything else (this
/// decoder never produces anything else).
fn frame_sample_count(frame: &Frame) -> u32 {
    match &frame.data {
        FrameData::Audio { samples, .. } => *samples,
        _ => 0,
    }
}

/// Drop `front` samples from the start and `back` from the end of a decoded
/// audio frame — the LAME/Xing gapless trim (`PacketSideData::SkipSamples`),
/// applied here because this is the one place that owns both a packet's
/// side data and the frame decoded from it. A no-op (returns `frame`
/// unchanged, no allocation) when both are zero, which is the overwhelming
/// majority of frames: only the very first and very last packets of a
/// stream carry non-zero `start`/`end`.
///
/// Builds a new, correctly-sized frame rather than shrinking the existing
/// one in place: a `Plane`'s byte length is fixed at allocation (`stride`
/// is set once, from the sample count `Frame::alloc_audio` was called
/// with), so there is no in-place "make this buffer report fewer bytes"
/// operation to reach for — allocating at the true final size is what every
/// other frame in this crate already does.
fn trim_gapless(frame: Frame, front: u32, back: u32, budget: &mut Budget) -> Result<Frame> {
    if front == 0 && back == 0 {
        return Ok(frame);
    }
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
    let front = front.min(samples);
    let kept_after_front = samples.saturating_sub(front);
    let back = back.min(kept_after_front);
    let new_samples = kept_after_front.saturating_sub(back);

    let mut out = Frame::alloc_audio(budget, format, layout, new_samples, sample_rate)?;
    let bytes_per_sample = format.bytes_per_sample();
    let start_byte = (front as usize).saturating_mul(bytes_per_sample);
    let keep_bytes = (new_samples as usize).saturating_mul(bytes_per_sample);
    for ch in 0..planes.len() {
        let Some(src) = frame.plane(ch) else { continue };
        let Some(src_row) = src.row(0) else { continue };
        let Some(src_slice) = src_row.get(start_byte..start_byte.saturating_add(keep_bytes)) else {
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
    out.time_base = frame.time_base;
    out.flags = frame.flags;
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod gapless_tests {
    use super::*;
    use vaco_chlayout::ChannelLayout;
    use vaco_sampfmt::SampleFmt;

    fn ramp_frame(samples: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            SampleFmt::F32P,
            ChannelLayout::MONO,
            samples,
            44100,
        )
        .expect("alloc");
        let mut plane = frame.plane_mut(0).expect("plane 0");
        let row = plane.row_mut(0).expect("row 0");
        for (i, chunk) in row.chunks_exact_mut(4).enumerate() {
            chunk.copy_from_slice(&(i as f32).to_le_bytes());
        }
        frame
    }

    fn samples_of(frame: &Frame) -> Vec<f32> {
        let plane = frame.plane(0).expect("plane 0");
        let row = plane.row(0).expect("row 0");
        row.chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn zero_trim_returns_the_frame_unchanged() {
        let frame = ramp_frame(10);
        let mut budget = Budget::new(Limits::permissive());
        let out = trim_gapless(frame, 0, 0, &mut budget).expect("trim");
        assert_eq!(frame_sample_count(&out), 10);
        assert_eq!(samples_of(&out), (0..10).map(|i| i as f32).collect::<Vec<_>>());
    }

    #[test]
    fn front_and_back_trim_keep_the_middle() {
        let frame = ramp_frame(10);
        let mut budget = Budget::new(Limits::permissive());
        let out = trim_gapless(frame, 3, 2, &mut budget).expect("trim");
        assert_eq!(frame_sample_count(&out), 5);
        assert_eq!(samples_of(&out), vec![3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn trim_exceeding_the_frame_empties_it_rather_than_panicking() {
        let frame = ramp_frame(4);
        let mut budget = Budget::new(Limits::permissive());
        let out = trim_gapless(frame, 100, 100, &mut budget).expect("trim");
        assert_eq!(frame_sample_count(&out), 0);
    }
}
