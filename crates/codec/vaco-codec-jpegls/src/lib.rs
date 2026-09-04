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
//! One real bug in this family has been found and fixed: `decode_ri_sample`/
//! `encode_ri_sample` updated the run-interruption context's `A`
//! accumulator with the entropy mapping's gap-compressed `shifted` value
//! instead of the real reconstructed error magnitude `eps`. The two agree
//! whenever `eps <= 0`, so it stayed invisible until enough positive-`eps`
//! same-context interruptions built up to move the derived Golomb parameter
//! `k` across a power-of-two boundary, at which point the *next*
//! same-context sample was read with the wrong `k` — a wrong pixel with no
//! error raised at all. `tests/regression.rs`'s `ramp_17x17` fixture
//! reproduces exactly this: five repeated `a == b`, `eps == 1`
//! interruptions, byte-exact against `ffmpeg -c:v jpegls` before and after.
//!
//! A size sweep of `ffmpeg`-encoded ramps (`tests/regression.rs`'s doc
//! comment has the fixture; the sweep itself was run by hand, not
//! committed) narrowed a **second**, still-open bug in the same
//! accumulator: on a busier ramp large enough to run the same
//! run-interruption context through many interruptions (measured on a
//! 255x255 `ffmpeg`-encoded fixture), the accumulated `A` this crate
//! computes is provably 2 too high by the tenth same-context event — each
//! of the nine preceding events was individually confirmed correct (right
//! reconstructed pixel *and* right bit-stream position, checked directly
//! against the file's own bytes), yet the aggregate produces `k = 3` where
//! the file's literal bits require `k = 2`. Every accumulation rule tried
//! (`eps`, `shifted`, and a speculative early reset) either reproduces this
//! exact contradiction or breaks one of the smaller fixtures that already
//! passes, so the true rule is evidently not a uniform per-event scalar.
//! Filed rather than hidden: this is a real, open defect, not a rounding
//! tolerance. Affected sizes decode either to a clean
//! [`vaco_core::Error::UnexpectedEof`] once the drift is large enough to run
//! off the end of the entropy segment, or — more dangerously — to a handful
//! of individual wrong pixels with no error at all, which is the
//! "confidently wrong" failure this crate's own shipping bar warns against.
//! Do not treat a clean decode as proof of a byte-exact one without
//! comparing against an independent decode.
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

pub use codec::{decode, encode, parameters};

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
                // Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`/
                // `vaco-codec-webp`'s encoders: never set `Packet::duration`.
                // Propagated from the input `Frame` for consistency with
                // every other video/image encoder in this tree.
                packet.duration = frame.duration;
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code exercising the encoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_answer_to_their_own_name() {
        assert_eq!(JPEGLS_DECODER.name, "jpegls");
        assert_eq!(JPEGLS_ENCODER.name, "jpegls");
        assert_eq!(JPEGLS_DECODER.id, vaco_codec_core::CodecId::JpegLs);
    }

    /// Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`/
    /// `vaco-codec-webp`'s encoders: `send` used to never set
    /// `Packet::duration`.
    #[test]
    fn send_propagates_the_input_frames_real_duration() {
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, vaco_pixfmt::PixFmt::Gray8, 4, 4)
            .expect("alloc video frame");
        frame.duration = vaco_core::Duration::from_micros(40_000);
        let mut enc = JpeglsEncoder::new(vaco_limits::Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("receive packet");
        assert_eq!(packet.duration, vaco_core::Duration::from_micros(40_000));
    }
}
