//! PNM family (pbm/pgm/ppm/pam/pfm/phm) decode and encode.
//!
//! Six sibling formats, one shape: a text header naming dimensions and a
//! sample layout, then one raw (or, for pbm/pgm/ppm, optionally ASCII) raster.
//! Each format's module — [`netpbm`], [`pam`], [`floatmap`] — owns its header
//! and pixel-format mapping; [`ImageDecoder`]/[`ImageEncoder`] wrap any of
//! them in the `vaco_codec_core::SendReceive` protocol via a function pointer,
//! since every one of these formats is a single packet in, single frame out.
//!
//! `Limits` bounds every decode the way it bounds every other decoder in this
//! tree: dimensions come from the header, so `vaco_frame::Frame::alloc_video`
//! validates them before a pixel is touched.

#![forbid(unsafe_code)]

mod bits;
mod floatmap;
mod netpbm;
mod pam;
mod reader;

pub use floatmap::{decode_pfm, decode_phm, encode_pfm, encode_phm};
pub use netpbm::{decode_pbm, decode_pgm, decode_ppm, encode_pbm, encode_pgm, encode_ppm};
pub use pam::{decode as decode_pam, encode as encode_pam};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

type DecodeFn = fn(&[u8], &mut Budget) -> Result<Frame>;
type EncodeFn = fn(&Frame) -> Result<Vec<u8>>;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`] for any PNM-family
/// member, one image per packet.
#[derive(Debug)]
pub struct ImageDecoder {
    machine: Machine<Frame>,
    limits: Limits,
    decode: DecodeFn,
}

impl ImageDecoder {
    /// A decoder that calls `decode` and bounds its allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits, decode: DecodeFn) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            decode,
        }
    }

    /// A decoder for pbm (`P1`/`P4`).
    #[must_use]
    pub fn pbm(limits: Limits) -> Self {
        Self::new(limits, decode_pbm)
    }

    /// A decoder for pgm (`P2`/`P5`).
    #[must_use]
    pub fn pgm(limits: Limits) -> Self {
        Self::new(limits, decode_pgm)
    }

    /// A decoder for ppm (`P3`/`P6`).
    #[must_use]
    pub fn ppm(limits: Limits) -> Self {
        Self::new(limits, decode_ppm)
    }

    /// A decoder for pam (`P7`).
    #[must_use]
    pub fn pam(limits: Limits) -> Self {
        Self::new(limits, decode_pam)
    }

    /// A decoder for pfm (`Pf`/`PF`).
    #[must_use]
    pub fn pfm(limits: Limits) -> Self {
        Self::new(limits, decode_pfm)
    }

    /// A decoder for phm (`Ph`/`PH`).
    #[must_use]
    pub fn phm(limits: Limits) -> Self {
        Self::new(limits, decode_phm)
    }
}

impl SendReceive for ImageDecoder {
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
                let frame = (self.decode)(pkt.payload(), &mut budget)?;
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`] for any PNM-family
/// member, one image per frame.
#[derive(Debug)]
pub struct ImageEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    encode: EncodeFn,
}

impl ImageEncoder {
    /// An encoder that calls `encode` and bounds the packet it allocates by
    /// `limits`.
    #[must_use]
    pub fn new(limits: Limits, encode: EncodeFn) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            encode,
        }
    }

    /// An encoder for pbm (`P4`).
    #[must_use]
    pub fn pbm(limits: Limits) -> Self {
        Self::new(limits, encode_pbm)
    }

    /// An encoder for pgm (`P5`).
    #[must_use]
    pub fn pgm(limits: Limits) -> Self {
        Self::new(limits, encode_pgm)
    }

    /// An encoder for ppm (`P6`).
    #[must_use]
    pub fn ppm(limits: Limits) -> Self {
        Self::new(limits, encode_ppm)
    }

    /// An encoder for pam (`P7`).
    #[must_use]
    pub fn pam(limits: Limits) -> Self {
        Self::new(limits, encode_pam)
    }

    /// An encoder for pfm.
    #[must_use]
    pub fn pfm(limits: Limits) -> Self {
        Self::new(limits, encode_pfm)
    }

    /// An encoder for phm.
    #[must_use]
    pub fn phm(limits: Limits) -> Self {
        Self::new(limits, encode_phm)
    }
}

impl SendReceive for ImageEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
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
                let bytes = (self.encode)(frame)?;
                let mut budget = Budget::new(self.limits.clone());
                let packet = Packet::from_slice(&mut budget, &bytes)?;
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code exercising the wrappers, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_core::Error;

    #[test]
    fn send_receive_protocol_shape() {
        let raw = b"P4\n8 2\n\xAA\x55".to_vec();
        let mut dec = ImageDecoder::pbm(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &raw).expect("packet");
        dec.send(Some(&packet)).expect("send");
        let frame = dec.receive().expect("frame");
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));

        let mut enc = ImageEncoder::pbm(Limits::permissive());
        enc.send(Some(&frame)).expect("send");
        let out = enc.receive().expect("packet");
        assert_eq!(out.payload(), raw.as_slice());
    }
}
