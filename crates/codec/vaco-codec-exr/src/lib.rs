//! `OpenEXR`, wrapping the `exr` crate (D11).
//!
//! # What it is
//!
//! [`codec::decode`]/[`codec::encode`] translate bytes to and from
//! [`vaco_frame::Frame`], covering the RGB(A) `f32` channel shape.
//! [`ExrDecoder`]/[`ExrEncoder`] wrap those in the
//! `vaco_codec_core::SendReceive` protocol every codec in this tree shares.
//!
//! # How it works
//!
//! Every `OpenEXR` file this crate handles is one image, one frame — no
//! sub-image structure the way APNG or animated WebP has — so both wrappers
//! declare [`Caps::empty`] and lean on `vaco_codec_core::Machine` the same
//! way `vaco_codec_qoi`'s wrappers do. Compression (PIZ/ZIP/RLE/PXR24/B44/
//! DWA) is transparent: the `exr` crate decodes whichever method the file
//! declares and always encodes its own default.
//!
//! # How to change it
//!
//! [`codec`] is the only module that knows the `exr` crate's types. Coverage
//! gaps — deep data, multi-part files, non-RGB(A) channel layouts, tiled
//! images — surface as [`vaco_core::Error::Unsupported`] from
//! [`codec::decode`]; extending coverage means building a different `exr`
//! reader pipeline there, not changing this file.
//!
//! # Dependencies
//!
//! `exr` (the wrapped decoder/encoder), `vaco-codec-core`,
//! `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;

pub use codec::{CompressionAlgo, EncodeOptions, decode, encode};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`], one `OpenEXR` image
/// per packet.
#[derive(Debug)]
pub struct ExrDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl ExrDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Default for ExrDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for ExrDecoder {
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`], one `OpenEXR` image
/// per frame.
#[derive(Debug)]
pub struct ExrEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    options: EncodeOptions,
}

impl ExrEncoder {
    /// An encoder that bounds the packet it allocates by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            options: EncodeOptions::default(),
        }
    }
}

impl Default for ExrEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for ExrEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        &[
            vaco_pixfmt::PixFmt::Rgbaf32le,
            vaco_pixfmt::PixFmt::Rgbaf32be,
            vaco_pixfmt::PixFmt::Rgbf32le,
            vaco_pixfmt::PixFmt::Rgbf32be,
            vaco_pixfmt::PixFmt::Rgba,
            vaco_pixfmt::PixFmt::Rgb24,
        ]
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
                let bytes = codec::encode(frame, &self.options)?;
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

    /// `-compression`, the one `AVOption` the reference's own `exr` encoder
    /// exposes (`ffmpeg -h encoder=exr`) that this crate can honour --
    /// `-format`/`-gamma` are not, since this crate always writes `f32`
    /// channels. Any other key is silently ignored, matching
    /// [`vaco_codec_core::Encoder::set_option`]'s own documented default.
    ///
    /// # Errors
    /// [`Error::Option`] for a `compression` value that is none of
    /// `none`/`rle`/`zip1`/`zip16` or their numeric tag (`0`-`3`).
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        if key == "compression" {
            self.options.compression = Some(match value.trim() {
                "0" | "none" => CompressionAlgo::None,
                "1" | "rle" => CompressionAlgo::Rle,
                "2" | "zip1" => CompressionAlgo::Zip1,
                "3" | "zip16" => CompressionAlgo::Zip16,
                other => {
                    return Err(Error::Option {
                        name: "compression".to_owned(),
                        detail: format!("unknown compression type: {other:?}"),
                    });
                }
            });
        }
        Ok(())
    }
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        ExrDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        ExrEncoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static EXR_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "exr",
    long_name: "`OpenEXR` image",
    id: vaco_codec_core::CodecId::Exr,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static EXR_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "exr",
    long_name: "`OpenEXR` image",
    id: vaco_codec_core::CodecId::Exr,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_encoder,
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
    use vaco_frame::FrameData;
    use vaco_pixfmt::PixFmt;

    #[allow(
        clippy::many_single_char_names,
        reason = "r/g/b/a/w/h read naturally for pixel-channel test fixtures"
    )]
    fn checker_frame(w: u32, h: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgbaf32le, w, h).expect("alloc");
        for mut plane in frame.planes_mut() {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    for (x, chunk) in row.chunks_exact_mut(16).enumerate() {
                        let r = (x as f32 * 0.1) % 1.0;
                        let g = (y as f32 * 0.2) % 1.0;
                        let b = 0.5f32;
                        let a = 1.0f32;
                        chunk[0..4].copy_from_slice(&r.to_ne_bytes());
                        chunk[4..8].copy_from_slice(&g.to_ne_bytes());
                        chunk[8..12].copy_from_slice(&b.to_ne_bytes());
                        chunk[12..16].copy_from_slice(&a.to_ne_bytes());
                    }
                }
            }
        }
        frame
    }

    fn frame_floats(frame: &Frame) -> Vec<f32> {
        let plane = frame.plane(0).expect("plane 0");
        let mut out = Vec::new();
        for row in plane.rows_iter() {
            for chunk in row.chunks_exact(4) {
                let bytes: [u8; 4] = chunk.try_into().expect("4 bytes");
                out.push(f32::from_ne_bytes(bytes));
            }
        }
        out
    }

    #[test]
    fn round_trips_rgba_f32() {
        let frame = checker_frame(5, 3);
        let encoded = codec::encode(&frame, &EncodeOptions::default()).expect("encode");
        let mut budget = Budget::new(Limits::permissive());
        let decoded = codec::decode(&encoded, &mut budget).expect("decode");
        let FrameData::Video { width, height, .. } = decoded.data else {
            panic!("video");
        };
        assert_eq!((width, height), (5, 3));
        let a = frame_floats(&frame);
        let b = frame_floats(&decoded);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-6, "{x} vs {y}");
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"not an exr", &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_) | Error::Unsupported(_) | Error::UnexpectedEof));
    }

    #[test]
    fn send_receive_protocol_shape() {
        let frame = checker_frame(2, 2);
        let mut enc = ExrEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("receive packet");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("begin drain");
        assert!(matches!(enc.receive(), Err(Error::Eof)));

        let mut dec = ExrDecoder::new(Limits::permissive());
        dec.send(Some(&packet)).expect("send packet");
        let decoded = dec.receive().expect("receive frame");
        let FrameData::Video { width, height, .. } = decoded.data else {
            panic!("video");
        };
        assert_eq!((width, height), (2, 2));
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("begin drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    /// Every `-compression` value must round-trip through decode exactly
    /// (all four are lossless) and `none` must actually be larger than
    /// `zip16` on smoothly varying content, not merely parse without error.
    #[test]
    fn compression_round_trips_and_actually_changes_size() {
        let frame = checker_frame(64, 64);
        let mut sizes = Vec::new();
        for (key_value, algo) in [
            ("none", CompressionAlgo::None),
            ("rle", CompressionAlgo::Rle),
            ("zip1", CompressionAlgo::Zip1),
            ("zip16", CompressionAlgo::Zip16),
        ] {
            let mut enc = ExrEncoder::new(Limits::permissive());
            enc.set_option("compression", key_value).expect("set_option");
            assert_eq!(enc.options.compression, Some(algo));
            enc.send(Some(&frame)).expect("send frame");
            let packet = enc.receive().expect("receive packet");
            let mut budget = Budget::new(Limits::permissive());
            let decoded = codec::decode(packet.payload(), &mut budget).expect("decode");
            assert_eq!(frame_floats(&frame), frame_floats(&decoded));
            sizes.push((key_value, packet.payload().len()));
        }
        let none_size = sizes[0].1;
        let zip16_size = sizes[3].1;
        assert!(
            none_size > zip16_size,
            "expected none ({none_size}) > zip16 ({zip16_size}): {sizes:?}"
        );
    }

    #[test]
    fn set_option_rejects_a_malformed_compression() {
        let mut enc = ExrEncoder::new(Limits::permissive());
        assert!(matches!(
            enc.set_option("compression", "bogus"),
            Err(Error::Option { .. })
        ));
    }

    /// A key this encoder has no use for is a silent no-op, matching
    /// `Encoder::set_option`'s own documented default.
    #[test]
    fn set_option_ignores_a_key_this_encoder_has_no_use_for() {
        let mut enc = ExrEncoder::new(Limits::permissive());
        enc.set_option("gamma", "2.2").expect("silently ignored");
    }
}
