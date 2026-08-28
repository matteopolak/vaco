//! JPEG-LS lossless still-image decode and encode (ITU-T T.87 / ISO/IEC
//! 14495-1 baseline).
//!
//! `Vaco-Spec-Ref: locoi-hpl98-193` — the algorithm's own paper, not the
//! paywalled ISO/ITU text: see [`context`]'s module doc for what came from
//! the paper directly and what was instead measured against the reference
//! `ffmpeg -c:v jpegls` binary (D17), because the paper documents the
//! regular-mode model in full but defers the run-mode adaptation table and
//! the run-interruption sample formulas to the standard text itself.
//!
//! # What is here
//!
//! | Module | Contents |
//! |---|---|
//! | [`bits`] | the bit-level (not byte-level) stuffing JPEG-LS's entropy segment uses |
//! | [`golomb`] | Golomb-power-of-2 codes with the limited-length escape |
//! | [`context`] | gradient quantization, the 365-context table, bias cancellation, run mode |
//! | [`marker`] | `SOI`/`SOF55`/`LSE`/`SOS`/`EOI` marker segments |
//! | [`codec`] | the per-image decode/encode loop tying the above together |
//!
//! Only the lossless case (`NEAR = 0`) is implemented; a scan with a nonzero
//! `NEAR` is rejected with [`vaco_core::Error::Unsupported`] rather than
//! decoded wrong, per this crate's own shipping bar. 8-bit-per-sample,
//! single-component (grayscale, non-interleaved) and three-component
//! (line-interleaved RGB) scans are covered — the two shapes
//! `ffmpeg -c:v jpegls` itself ever produces; a sample-interleaved scan is
//! rejected the same way (its run-mode state would need to be tracked
//! per-component *within* one row, which this crate's row-at-a-time loop
//! does not do).
//!
//! # A known gap, honestly
//!
//! Round-tripped bit-exact against `ffmpeg -c:v jpegls`'s own decode on a
//! wide range of synthetic content: solid fields, sharp two-tone
//! transitions (both `a == b` and `a != b` run-interruption cases, every
//! Golomb parameter from 0 up through several escape codes), vertical and
//! diagonal gradients, uniform noise, and three-component RGB. Against
//! `ffmpeg`'s own `testsrc`/`gradients` patterns — busier, multi-directional
//! photographic-like content — small (1-2 count), non-cascading pixel
//! differences remain in rare spots, and one longer run eventually desyncs
//! outright partway through the last row. Each fix so far has come from
//! measuring a real disagreement against `ffmpeg`, not from guessing; the
//! remaining gap is real and not yet isolated, most likely one more
//! rarely-hit formula detail in the same family (context reset, run
//! interruption's sign convention, or the escape length limit) that this
//! crate's synthetic corpus does not exercise. Filed rather than hidden:
//! this is a real, open defect, not a rounding tolerance — on the one
//! `ffmpeg`-encoded fixture it was measured on, most divergences ran out
//! the input entirely into a clean [`vaco_core::Error`] rather than a
//! panic, but a handful of individual pixels differed by 1-2 with no error
//! at all, which is the "confidently wrong" failure this crate's own
//! shipping bar warns against — do not treat a clean decode as proof of a
//! byte-exact one without comparing against an independent decode.
//!
//! # How to change it
//!
//! [`context`] holds every piece of per-context state and the formulas that
//! update it; a new interleave mode or component count starts in
//! [`codec`]'s sample-order loops, which are the only place that knows how
//! `(x, y, component)` maps onto scan order.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds the decoded frame the same way every other
//! image decoder in this tree does — width/height come from the
//! attacker-controlled `SOF55` marker, validated by
//! [`vaco_frame::Frame::alloc_video`] before a sample is decoded.
//!
//! # Dependencies
//!
//! `vaco-codec-core` (the decode/encode protocol), `vaco-frame`/`vaco-pixfmt`/
//! `vaco-pool` (the decoded picture), `vaco-packet` (the encoded bytes),
//! `vaco-limits` (allocation bounds).

#![forbid(unsafe_code)]

mod bits;
mod codec;
mod context;
mod golomb;
mod marker;

pub use codec::{decode, encode};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`], one JPEG-LS image per
/// packet.
#[derive(Debug)]
pub struct JpeglsDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl JpeglsDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Default for JpeglsDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for JpeglsDecoder {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else {
                    return Ok(());
                };
                let mut budget = Budget::new(self.limits.clone());
                let mut frame = codec::decode(pkt.payload(), &mut budget)?;
                frame.pts = pkt.pts;
                self.machine.emit(frame);
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }
}

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`], one JPEG-LS image per
/// frame.
#[derive(Debug)]
pub struct JpeglsEncoder {
    machine: Machine<Packet>,
    limits: Limits,
}

impl JpeglsEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Default for JpeglsEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for JpeglsEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        &[vaco_pixfmt::PixFmt::Gray8, vaco_pixfmt::PixFmt::Rgb24]
    }

    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else {
                    return Ok(());
                };
                let bytes = codec::encode(frame)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &bytes)?;
                packet.pts = frame.pts;
                self.machine.emit(packet);
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        JpeglsDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        JpeglsEncoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static JPEGLS_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "jpegls",
    long_name: "JPEG-LS (ITU-T T.87 / ISO/IEC 14495-1)",
    id: vaco_codec_core::CodecId::JpegLs,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static JPEGLS_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "jpegls",
    long_name: "JPEG-LS (ITU-T T.87 / ISO/IEC 14495-1)",
    id: vaco_codec_core::CodecId::JpegLs,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_encoder,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_answer_to_their_own_name() {
        assert_eq!(JPEGLS_DECODER.name, "jpegls");
        assert_eq!(JPEGLS_ENCODER.name, "jpegls");
        assert_eq!(JPEGLS_DECODER.id, vaco_codec_core::CodecId::JpegLs);
    }
}
