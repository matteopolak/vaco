//! The reference's `vnull`/`anull` null encoders.
//!
//! # What it is
//!
//! `vnull` ("Null video codec") and `anull` ("Null audio codec") are the
//! reference's dummy encoders: every frame sent in is discarded and no
//! packet is ever produced. They exist so a pipeline can be built and run
//! end to end — timing, muxer wiring, `-f null -` style throughput
//! measurement — without a real codec doing any work. Per the roadmap (plan
//! 20 §1.9, "C-47 merged in") this pair is encode-only: there is nothing to
//! decode, so this crate registers no [`vaco_codec_core::DecoderDesc`] at
//! all, matching the "0 dec / 2 enc" accounting issue #281 inherited from
//! C-47.
//!
//! # How it works
//!
//! [`NullEncoder`] is the one implementation both `-c:v vnull` and
//! `-c:a anull` share (the reference's own two dummy encoders differ only in
//! media type and name, never in behaviour). It follows the same
//! `SendReceive`-over-`Machine` shape every codec in this tree uses, but its
//! `send` never calls [`vaco_codec_core::machine::Machine::emit`] on the
//! `Accept::Input` branch — it just accepts and discards. That is not a
//! special case the protocol has to be told about: `vaco_codec_core::mock`'s
//! own `Step::Skip` ("produce nothing at all: a header-only packet") is the
//! identical shape already exercised by that module's property tests, so a
//! codec that *always* skips is on already-proven ground.
//! [`vaco_codec_core::Validated`] wraps it exactly as it wraps every other
//! encoder in this tree.
//!
//! # How to change it
//!
//! There is nothing to extend here: the whole behavioural contract is "eat
//! input, never emit, answer `NeedMoreInput` until drained and `Eof` after".
//! If a future need requires `vnull`/`anull` to report anything (byte counts,
//! say), it is still one [`NullEncoder`] with an added counter, not two.
//!
//! # Configuration
//!
//! None. `Limits` is accepted at construction, per every `DecoderDesc`/
//! `EncoderDesc::make` signature in this tree, but a codec that allocates
//! nothing has nothing to bound.
//!
//! # Dependencies
//!
//! `vaco-codec-core` (the protocol), `vaco-frame`/`vaco-packet` (the input/
//! output types, both used only as opaque values), `vaco-limits` (the
//! `make` signature).

#![forbid(unsafe_code)]

use vaco_codec_core::{
    Accept, AsEncoder, Caps, CodecId, Encoder, EncoderDesc, Machine, SendReceive, Validated,
};
use vaco_core::{MediaType, Result};
use vaco_frame::Frame;
use vaco_limits::Limits;
use vaco_packet::Packet;

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`] that discards every
/// frame it is sent and produces no packet, ever.
#[derive(Debug)]
pub struct NullEncoder {
    machine: Machine<Packet>,
}

impl NullEncoder {
    /// A fresh null encoder. There is no per-instance state worth carrying,
    /// so unlike every other codec in this tree there is no `Limits` to
    /// store — nothing here allocates.
    #[must_use]
    pub fn new() -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
        }
    }
}

impl Default for NullEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SendReceive for NullEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        // Step one, always, exactly like every other codec: let the machine
        // validate the transition and apply backpressure.
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            // The one line that makes this a null codec: the frame is
            // dropped here, not forwarded to `self.machine.emit`. Every
            // other codec's `Accept::Input` arm ends in an `emit` call; this
            // one deliberately does not, mirroring `vaco_codec_core::mock`'s
            // `Step::Skip`.
            Accept::Input => Ok(()),
        }
    }

    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }
}

fn make_vnull_encoder(_limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(NullEncoder::new())))
}

fn make_anull_encoder(_limits: Limits) -> Box<dyn Encoder> {
    Box::new(AsEncoder(Validated::new(NullEncoder::new())))
}

/// Registered as this crate's `encoder` fragment (plan 19 §3.4). Reference
/// `name`/`long_name` measured from `vaco_codec_core::CodecId::Vnull`'s own
/// registry entry ("Null video codec").
pub static VNULL_ENCODER: EncoderDesc = EncoderDesc {
    name: "vnull",
    long_name: "Null video codec",
    id: CodecId::Vnull,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_vnull_encoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4). Reference
/// `name`/`long_name` measured from `vaco_codec_core::CodecId::Anull`'s own
/// registry entry ("Null audio codec").
pub static ANULL_ENCODER: EncoderDesc = EncoderDesc {
    name: "anull",
    long_name: "Null audio codec",
    id: CodecId::Anull,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_anull_encoder,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the encoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use smallvec::SmallVec;
    use vaco_core::{Duration, Error, Rational, Timestamp};
    use vaco_frame::{FrameData, FrameFlags};
    use vaco_pixfmt::PixFmt;

    fn dummy_frame() -> Frame {
        Frame {
            data: FrameData::Video {
                format: PixFmt::Yuv420p,
                width: 16,
                height: 16,
                planes: SmallVec::default(),
            },
            pts: Timestamp::new(0),
            duration: Duration::ZERO,
            time_base: Rational::ONE,
            color: vaco_color::ColorInfo::default(),
            sample_aspect_ratio: Rational::ONE,
            flags: FrameFlags::KEY,
            side_data: SmallVec::default(),
        }
    }

    #[test]
    fn never_emits_a_packet() {
        let mut enc = NullEncoder::new();
        for i in 0..8 {
            let mut frame = dummy_frame();
            frame.pts = Timestamp::new(i);
            enc.send(Some(&frame)).expect("send is always accepted");
            assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        }
    }

    #[test]
    fn drains_to_eof_with_nothing_buffered() {
        let mut enc = NullEncoder::new();
        enc.send(Some(&dummy_frame())).expect("send");
        enc.send(None).expect("begin drain");
        assert!(matches!(enc.receive(), Err(Error::Eof)));
    }

    #[test]
    fn flush_resets_the_protocol_state() {
        let mut enc = NullEncoder::new();
        enc.send(Some(&dummy_frame())).expect("send");
        enc.send(None).expect("drain");
        assert!(matches!(enc.receive(), Err(Error::Eof)));
        enc.flush();
        // After a flush the machine is back at the start, so sending again
        // must not immediately answer `Eof`.
        enc.send(Some(&dummy_frame())).expect("send after flush");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn descriptors_report_the_reference_identity() {
        assert_eq!(VNULL_ENCODER.name, "vnull");
        assert_eq!(VNULL_ENCODER.long_name, "Null video codec");
        assert_eq!(VNULL_ENCODER.id, CodecId::Vnull);
        assert_eq!(VNULL_ENCODER.media_type, MediaType::Video);

        assert_eq!(ANULL_ENCODER.name, "anull");
        assert_eq!(ANULL_ENCODER.long_name, "Null audio codec");
        assert_eq!(ANULL_ENCODER.id, CodecId::Anull);
        assert_eq!(ANULL_ENCODER.media_type, MediaType::Audio);
    }

    #[test]
    fn descriptor_built_encoder_never_emits_either() {
        let mut enc = VNULL_ENCODER.build(Limits::permissive());
        enc.send_frame(Some(&dummy_frame())).expect("send");
        assert!(matches!(
            enc.receive_packet(),
            Err(Error::NeedMoreInput)
        ));
        enc.send_frame(None).expect("drain");
        assert!(matches!(enc.receive_packet(), Err(Error::Eof)));
    }
}
