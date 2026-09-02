//! `Decoder` trait implementation: adapts [`crate::decode::decode_frame`] to
//! the send/receive packet-to-frame machine, and builds a real
//! [`vaco_frame::Frame`] from the decoded samples.

use vaco_codec_core::Decoder;
use vaco_core::{Error, MediaType, Result};
use vaco_format_ac3::tables::acmod_layout;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

use crate::decode::{DecodeOptions, StreamState, decode_frame};

#[derive(Debug)]
pub struct Ac3Decoder {
    limits: Limits,
    state: StreamState,
    opts: DecodeOptions,
    pending: std::collections::VecDeque<Frame>,
    /// Set by `send_packet(None)`; makes `receive_frame` answer `Eof`
    /// once `pending` is empty instead of `NeedMoreInput` forever, which
    /// otherwise stalls `vaco-sched`'s drain loop until its `ProgressGuard`
    /// watchdog kills the run with `NoProgress` ("progress limit
    /// exceeded") -- the same contract violation measured, and fixed the
    /// same way, in `vaco-codec-alac`'s and `vaco-codec-vorbis`'s decoders.
    draining: bool,
}

impl Ac3Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            state: StreamState::new(),
            opts: DecodeOptions::default(),
            pending: std::collections::VecDeque::new(),
            draining: false,
        }
    }

    /// Same decoder, with dynamic-range compression applied per the
    /// transmitted `dynrng` gain word — the DRC-on/off comparison cell of
    /// the conformance matrix.
    #[must_use]
    pub fn with_drc(mut self, apply: bool) -> Self {
        self.opts.apply_drc = apply;
        self
    }
}

impl Decoder for Ac3Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            self.draining = true;
            return Ok(());
        };
        let decoded = decode_frame(packet.payload(), &mut self.state, &self.opts)
            .map_err(|_| Error::InvalidData("ac3: could not decode frame"))?;

        let layout = acmod_layout(decoded.acmod, decoded.lfeon);
        let samples = decoded.channels.first().map_or(0, Vec::len);
        let mut budget = Budget::new(self.limits.clone());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            SampleFmt::F32P,
            layout,
            u32::try_from(samples).unwrap_or(0),
            decoded.sample_rate,
        )?;

        let mut all_channels = decoded.channels;
        if let Some(lfe) = decoded.lfe {
            all_channels.push(lfe);
        }
        {
            let mut planes = frame.planes_mut();
            for (plane, data) in planes.iter_mut().zip(all_channels.iter()) {
                let Some(row) = plane.row_mut(0) else {
                    continue;
                };
                for (dst, &sample) in row.chunks_exact_mut(4).zip(data.iter()) {
                    dst.copy_from_slice(&sample.to_le_bytes());
                }
            }
        }
        self.pending.push_back(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(if self.draining { Error::Eof } else { Error::NeedMoreInput })
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.state = StreamState::new();
        self.draining = false;
    }
}

const _: fn() = || {
    // Compile-time reminder that this decoder only ever produces audio;
    // if `Frame` ever grows a required video field this stops compiling
    // here instead of failing a runtime assertion somewhere else.
    let _ = MediaType::Audio;
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_format_ac3::syncinfo;

    /// The registry-reachable path end to end: `DecoderDesc::make`'s exact
    /// signature, `send_packet`/`receive_frame` against a real committed
    /// fixture, and a real `Frame` back out — not just `decode_frame` in
    /// isolation.
    #[test]
    fn the_registry_path_decodes_a_real_fixture_to_a_frame() {
        let data = include_bytes!("../tests/fixtures/small_ac3.ac3");
        let info = syncinfo::parse(data).expect("a valid ac3 header");
        let first = &data[..info.frame_size];

        let mut dec = (crate::DECODER_AC3.make)(Limits::permissive());
        let packet = Packet::from_slice(&mut Budget::new(Limits::permissive()), first)
            .expect("packet alloc");
        dec.send_packet(Some(&packet)).expect("send_packet");
        let frame = dec.receive_frame().expect("receive_frame");
        assert!(frame.is_audio());
        dec.flush();
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    /// `send_packet(None)` must make `receive_frame` answer `Eof` once
    /// `pending` is drained, not `NeedMoreInput` forever -- see the
    /// `draining` field's own doc for why (measured against `vaco-sched`'s
    /// `ProgressGuard` livelock watchdog).
    #[test]
    fn draining_answers_eof_once_empty_not_need_more_input_forever() {
        let data = include_bytes!("../tests/fixtures/small_ac3.ac3");
        let info = syncinfo::parse(data).expect("a valid ac3 header");
        let first = &data[..info.frame_size];

        let mut dec = Ac3Decoder::new(Limits::permissive());
        let packet = Packet::from_slice(&mut Budget::new(Limits::permissive()), first).expect("packet alloc");
        dec.send_packet(Some(&packet)).expect("send_packet");
        dec.receive_frame().expect("one real frame before draining");

        dec.send_packet(None).expect("send_packet(None) starts draining");
        assert!(matches!(dec.receive_frame(), Err(Error::Eof)), "must answer Eof once drained and empty, not NeedMoreInput forever");
    }
}
