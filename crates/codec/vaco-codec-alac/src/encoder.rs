//! [`vaco_codec_core::Encoder`] wrapper over [`frame_codec::encode`].

use std::collections::VecDeque;

use vaco_codec_core::Encoder;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::frame_codec;

#[derive(Debug)]
pub struct AlacEncoder {
    limits: Limits,
    pending: VecDeque<Packet>,
}

impl AlacEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: VecDeque::new(),
        }
    }
}

impl Encoder for AlacEncoder {
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        let Some(frame) = frame else {
            return Ok(());
        };
        let mut budget = Budget::new(self.limits.clone());
        let bytes = frame_codec::encode(frame, &mut budget)?;
        let mut packet = Packet::from_slice(&mut budget, &bytes)?;
        packet.pts = frame.pts;
        // Every packet is independently decodable: the adaptive Golomb-Rice
        // state (`rice.rs`) and the predictor's transmitted coefficients
        // both reset per packet — there is no cross-packet state — matching
        // `CodecId::Alac`'s registered `INTRA_ONLY` property.
        packet.flags = PacketFlags::KEY;
        self.pending.push_back(packet);
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        self.pending.pop_front().ok_or(Error::NeedMoreInput)
    }

    fn flush(&mut self) {
        self.pending.clear();
    }

    /// Mirrors `frame_codec::bytes_per_sample`'s own accepted set. Same
    /// reasoning as `vaco-codec-flac`/`vaco-codec-pcm`'s overrides: without
    /// this, a caller only discovers the s16p/s32p requirement from a
    /// failed `send_frame`, too late to insert a conversion first
    /// (E2E-GAPS #3).
    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        &[vaco_sampfmt::SampleFmt::S16P, vaco_sampfmt::SampleFmt::S32P]
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_chlayout::ChannelLayout;
    use vaco_codec_core::Decoder as _;
    use vaco_core::Timestamp;
    use vaco_sampfmt::SampleFmt;

    fn mono_frame(samples: &[i32]) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            SampleFmt::S16P,
            ChannelLayout::MONO,
            samples.len() as u32,
            44100,
        )
        .unwrap();
        {
            let mut plane = frame.plane_mut(0).unwrap();
            let row = plane.row_mut(0).unwrap();
            for (i, &s) in samples.iter().enumerate() {
                if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                    dst.copy_from_slice(&(s as i16).to_le_bytes());
                }
            }
        }
        frame.pts = Timestamp::new(7);
        frame
    }

    #[test]
    fn send_receive_protocol_shape() {
        let samples: Vec<i32> = (0..512).map(|i| (i % 200) - 100).collect();
        let frame = mono_frame(&samples);
        let mut enc = AlacEncoder::new(Limits::permissive());
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));
        enc.send_frame(Some(&frame)).unwrap();
        let packet = enc.receive_packet().unwrap();
        assert_eq!(packet.pts, Timestamp::new(7));
        assert!(packet.flags.contains(PacketFlags::KEY));
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));

        enc.flush();
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn encoded_packet_decodes_back_through_the_decoder() {
        let samples: Vec<i32> = (0..1024).map(|i| ((i * 3) % 400) - 200).collect();
        let frame = mono_frame(&samples);
        let mut enc = AlacEncoder::new(Limits::permissive());
        enc.send_frame(Some(&frame)).unwrap();
        let packet = enc.receive_packet().unwrap();

        let mut dec = crate::AlacDecoder::new(Limits::permissive());
        dec.send_packet(Some(&packet)).unwrap();
        let decoded = dec.receive_frame().unwrap();
        let vaco_frame::FrameData::Audio { planes, .. } = &decoded.data else {
            panic!("audio frame");
        };
        let row = planes.first().unwrap().data.as_slice();
        let got: Vec<i32> = row
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(got, samples);
    }
}
