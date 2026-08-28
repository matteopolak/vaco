//! `Decoder` implementation: per-packet frame-header dispatch to Layer I, II
//! or III, and the shared per-channel synthesis filterbank history.

use std::collections::VecDeque;

use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_format_mpegaudio::{Layer, MpegAudioHeader};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

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
        self.pending.push_back(frame);
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
