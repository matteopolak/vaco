//! JPEG XL, wrapping the `jxl-oxide` crate (D11). Decode-only.
//!
//! # What it is
//!
//! [`codec::decode`] translates JPEG XL bytes (still or the animated
//! `jpegxl_anim` form) to `f32` [`vaco_frame::Frame`]s. [`JxlDecoder`] wraps
//! it in the `vaco_codec_core::SendReceive` protocol every codec in this
//! tree shares. There is no encoder: `jxl-oxide` is a decoder only, and no
//! other pure-Rust JPEG XL encoder is in scope for this crate.
//!
//! # How it works
//!
//! A packet is the whole file; the animated form can yield several
//! keyframes from one packet, so decode declares [`Caps::SUBFRAMES`] and
//! queues every rendered keyframe with one `Machine::emit_all` call.
//!
//! # How to change it
//!
//! [`codec`] is the only module that knows the `jxl_oxide::` types. A
//! colour-space coverage gap (CMYK) belongs in
//! [`codec::output_pixfmt`]'s match.
//!
//! # Dependencies
//!
//! `jxl-oxide` (the wrapped decoder, default features: pure-Rust `rayon`
//! threading only — the optional `lcms2`/`moxcms` colour-management
//! backends and the `image` integration are not enabled), `vaco-codec-core`,
//! `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;

pub use codec::decode;

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`]: one JPEG XL packet
/// in, one or more `f32` frames out.
#[derive(Debug)]
pub struct JxlDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl JxlDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES),
            limits,
        }
    }
}

impl Default for JxlDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for JxlDecoder {
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
                let mut frames = codec::decode(pkt.payload(), &mut budget)?;
                for frame in &mut frames {
                    frame.pts = pkt.pts;
                }
                self.machine.emit_all(frames);
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

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        JxlDecoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4). There is no
/// matching `encoder` fragment: `jxl-oxide` provides no encoder.
pub static JPEGXL_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "jpegxl",
    long_name: "JPEG XL",
    id: vaco_codec_core::CodecId::JpegXl,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make: make_decoder,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_core::Error;

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"not a jpeg xl file at all, padded a bit more", &mut budget)
            .unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidData(_) | Error::Unsupported(_) | Error::UnexpectedEof
        ));
    }

    #[test]
    fn send_receive_protocol_shape_on_bad_input() {
        // No pure-Rust JPEG XL encoder is available to build a golden fixture
        // in-process (this crate is decode-only, see the module docs), so
        // the protocol shape is exercised through the error path: `send`
        // must still surface the decode error rather than panicking or
        // hanging the state machine.
        let mut budget = Budget::new(Limits::permissive());
        let mut packet = vaco_packet::Packet::from_slice(&mut budget, b"not jxl").expect("packet");
        packet.pts = vaco_core::Timestamp::new(0);

        let mut dec = JxlDecoder::new(Limits::permissive());
        let err = dec.send(Some(&packet)).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidData(_) | Error::Unsupported(_) | Error::UnexpectedEof
        ));
    }
}
