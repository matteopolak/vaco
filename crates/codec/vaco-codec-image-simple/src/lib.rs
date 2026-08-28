//! BMP, PCX, TGA, SGI, XWD, XBM decode and encode.
//!
//! Six unrelated-on-the-wire formats that nonetheless share one shape: a
//! fixed or near-fixed binary header naming dimensions and a pixel layout,
//! then one image's worth of pixels — raw, RLE, or (XBM) a text array. Each
//! format's module owns its header and pixel-format mapping;
//! [`ImageDecoder`]/[`ImageEncoder`] wrap any of them in the
//! `vaco_codec_core::SendReceive` protocol via a function pointer, since
//! every one is a single packet in, single frame out.
//!
//! None of these six have a `vaco_codec_core::CodecId` yet, so they cannot be
//! registered as `vaco-registry` decoders/encoders in this pass — see
//! `docs/codec/vaco-codec-image-simple.md`.

#![forbid(unsafe_code)]

mod bmp;
mod pcx;
mod reader;
mod sgi;
mod tga;
mod xbm;
mod xwd;

pub use bmp::{decode as decode_bmp, encode as encode_bmp};
pub use pcx::{decode as decode_pcx, encode as encode_pcx};
pub use sgi::{decode as decode_sgi, encode as encode_sgi};
pub use tga::{decode as decode_tga, encode as encode_tga};
pub use xbm::{decode as decode_xbm, encode as encode_xbm};
pub use xwd::{decode as decode_xwd, encode as encode_xwd};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

type DecodeFn = fn(&[u8], &mut Budget) -> Result<Frame>;
type EncodeFn = fn(&Frame) -> Result<Vec<u8>>;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`] for any format in this
/// crate, one image per packet.
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

    /// A decoder for BMP.
    #[must_use]
    pub fn bmp(limits: Limits) -> Self {
        Self::new(limits, bmp::decode)
    }

    /// A decoder for PCX.
    #[must_use]
    pub fn pcx(limits: Limits) -> Self {
        Self::new(limits, pcx::decode)
    }

    /// A decoder for TGA.
    #[must_use]
    pub fn tga(limits: Limits) -> Self {
        Self::new(limits, tga::decode)
    }

    /// A decoder for SGI.
    #[must_use]
    pub fn sgi(limits: Limits) -> Self {
        Self::new(limits, sgi::decode)
    }

    /// A decoder for XWD.
    #[must_use]
    pub fn xwd(limits: Limits) -> Self {
        Self::new(limits, xwd::decode)
    }

    /// A decoder for XBM.
    #[must_use]
    pub fn xbm(limits: Limits) -> Self {
        Self::new(limits, xbm::decode)
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`] for any format in this
/// crate, one image per frame.
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

    /// An encoder for BMP.
    #[must_use]
    pub fn bmp(limits: Limits) -> Self {
        Self::new(limits, bmp::encode)
    }

    /// An encoder for PCX.
    #[must_use]
    pub fn pcx(limits: Limits) -> Self {
        Self::new(limits, pcx::encode)
    }

    /// An encoder for TGA.
    #[must_use]
    pub fn tga(limits: Limits) -> Self {
        Self::new(limits, tga::encode)
    }

    /// An encoder for SGI.
    #[must_use]
    pub fn sgi(limits: Limits) -> Self {
        Self::new(limits, sgi::encode)
    }

    /// An encoder for XWD.
    #[must_use]
    pub fn xwd(limits: Limits) -> Self {
        Self::new(limits, xwd::encode)
    }

    /// An encoder for XBM.
    #[must_use]
    pub fn xbm(limits: Limits) -> Self {
        Self::new(limits, xbm::encode)
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
        let raw = b"#define image_width 8\n#define image_height 1\nstatic unsigned char image_bits[] = {\n 0xFF\n };\n".to_vec();
        let mut dec = ImageDecoder::xbm(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &raw).expect("packet");
        dec.send(Some(&packet)).expect("send");
        let frame = dec.receive().expect("frame");
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));

        let mut enc = ImageEncoder::xbm(Limits::permissive());
        enc.send(Some(&frame)).expect("send");
        let out = enc.receive().expect("packet");
        assert_eq!(out.payload(), raw.as_slice());
    }
}
