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
//! Each format registers under its own `vaco_codec_core::CodecId` variant;
//! `vaco-component.toml` is what makes each reachable as `-c:v <name>`.

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
                let mut frame = (self.decode)(pkt.payload(), &mut budget)?;
                // The per-format `decode` functions are pure over bytes
                // alone and have no packet to read a timestamp off;
                // stamping the frame with the packet's own `pts` here is
                // what lets a muxer downstream of a decode-then-encode leg
                // place this image in time at all.
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
                let mut packet = Packet::from_slice(&mut budget, &bytes)?;
                // Mirrors the decoder's own `pts` stamp: the per-format
                // `encode` functions are pure over the frame's pixels alone,
                // so the packet they hand back has no timing of its own
                // until this copies the source frame's `pts` onto it.
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

pub static BMP_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "bmp",
    long_name: "BMP (Windows and OS/2 bitmap)",
    id: vaco_codec_core::CodecId::Bmp,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| {
        Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
            ImageDecoder::bmp(limits),
        )))
    },
};

pub static BMP_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "bmp",
    long_name: "BMP (Windows and OS/2 bitmap)",
    id: vaco_codec_core::CodecId::Bmp,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| {
        Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
            ImageEncoder::bmp(limits),
        )))
    },
};

/// PCX/TGA/SGI/XWD/XBM's registration: the `DecoderDesc`/`EncoderDesc` pair
/// that makes each reachable as `-c:v <name>`, against the
/// a `vaco_codec_core::CodecId` variant of its own. `$ctor` is this
/// crate's constructor suffix (`ImageDecoder::$ctor`); `$name` is the
/// registered CLI name, which is not always the same spelling — TGA
/// registers as `"targa"`, matching the reference's own codec name for it
/// (`ffmpeg -codecs`: `targa`, "Truevision Targa image"), while this crate's
/// own module and functions stay `tga` to match the format's usual
/// abbreviation.
macro_rules! image_simple_codec {
    ($dec:ident, $enc:ident, $id:ident, $ctor:ident, $name:literal, $long_name:literal) => {
        #[doc = concat!("Registered as this crate's `", $name, "` `decoder` fragment.")]
        pub static $dec: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
            name: $name,
            long_name: $long_name,
            id: vaco_codec_core::CodecId::$id,
            media_type: vaco_core::MediaType::Video,
            caps: Caps::empty(),
            supported_rates: &[],
            make: |limits| {
                Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
                    ImageDecoder::$ctor(limits),
                )))
            },
        };

        #[doc = concat!("Registered as this crate's `", $name, "` `encoder` fragment.")]
        pub static $enc: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
            name: $name,
            long_name: $long_name,
            id: vaco_codec_core::CodecId::$id,
            media_type: vaco_core::MediaType::Video,
            caps: Caps::empty(),
            supported_rates: &[],
            make: |limits| {
                Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
                    ImageEncoder::$ctor(limits),
                )))
            },
        };
    };
}

image_simple_codec!(
    PCX_DECODER,
    PCX_ENCODER,
    Pcx,
    pcx,
    "pcx",
    "PC Paintbrush PCX image"
);
image_simple_codec!(
    TGA_DECODER,
    TGA_ENCODER,
    Targa,
    tga,
    "targa",
    "Truevision Targa image"
);
image_simple_codec!(SGI_DECODER, SGI_ENCODER, Sgi, sgi, "sgi", "SGI image");
image_simple_codec!(
    XWD_DECODER,
    XWD_ENCODER,
    Xwd,
    xwd,
    "xwd",
    "XWD (X Window Dump) image"
);
image_simple_codec!(
    XBM_DECODER,
    XBM_ENCODER,
    Xbm,
    xbm,
    "xbm",
    "XBM (X BitMap) image"
);

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
