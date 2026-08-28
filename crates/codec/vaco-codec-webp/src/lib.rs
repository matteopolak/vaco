//! WebP, wrapping the `image-webp` crate (D11).
//!
//! # What it is
//!
//! [`codec::decode`] translates WebP bytes (still or animated) to
//! [`vaco_frame::Frame`]s; [`codec::encode`] goes the other way for a single
//! frame, lossless only. [`WebpDecoder`]/[`WebpEncoder`] wrap those in the
//! `vaco_codec_core::SendReceive` protocol every codec in this tree shares.
//!
//! # How it works
//!
//! A packet is the whole file. An animated WebP can yield several frames
//! from one packet — `image_webp::WebPDecoder` composites dispose/blend
//! onto the canvas itself, unlike GIF/APNG — hence [`Caps::SUBFRAMES`] on
//! decode. Encode has no animation path in `image-webp` at all, so it runs
//! one frame in, one packet out, the same shape as `vaco-codec-qoi`.
//!
//! # How to change it
//!
//! [`codec`] is the only module that knows the `image_webp::` types. A
//! pixel-format-coverage gap belongs in [`codec::encode`]'s colour-type
//! match; lossy (`VP8`) encode or animation (`ANMF`) encode would need a
//! backend `image-webp` does not provide (plan 15 §4A.4 routes lossy WebP
//! through `vaco-codec-vp8` once it exists, per C-19).
//!
//! # Dependencies
//!
//! `image-webp` (the wrapped decoder/encoder), `vaco-codec-core`,
//! `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;

pub use codec::{decode, encode};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`]: one WebP packet in,
/// one or more frames out.
#[derive(Debug)]
pub struct WebpDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl WebpDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES),
            limits,
        }
    }
}

impl Default for WebpDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for WebpDecoder {
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`]: one lossless WebP
/// image per frame.
#[derive(Debug)]
pub struct WebpEncoder {
    machine: Machine<Packet>,
    limits: Limits,
}

impl WebpEncoder {
    /// An encoder that bounds the packet it allocates by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Default for WebpEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for WebpEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        &[
            vaco_pixfmt::PixFmt::Gray8,
            vaco_pixfmt::PixFmt::Ya8,
            vaco_pixfmt::PixFmt::Rgb24,
            vaco_pixfmt::PixFmt::Rgba,
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
        WebpDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        WebpEncoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static WEBP_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "webp",
    long_name: "WebP",
    id: vaco_codec_core::CodecId::Webp,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static WEBP_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "webp",
    long_name: "WebP (lossless only)",
    id: vaco_codec_core::CodecId::Webp,
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
    clippy::integer_division,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_core::Error;
    use vaco_frame::FrameData;
    use vaco_pixfmt::PixFmt;

    fn checker_frame(w: u32, h: u32, format: PixFmt) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, format, w, h).expect("alloc");
        let bpp = match format {
            PixFmt::Rgb24 => 3,
            PixFmt::Rgba => 4,
            PixFmt::Gray8 => 1,
            _ => panic!("unsupported test format"),
        };
        for mut plane in frame.planes_mut() {
            for y in 0..plane.rows() {
                let row_bytes = plane.row_bytes();
                if let Some(row) = plane.row_mut(y) {
                    for x in 0..(row_bytes / bpp) {
                        let base = x * bpp;
                        for c in 0..bpp {
                            row[base + c] = ((x * 37 + y * 91 + c * 53) % 256) as u8;
                        }
                    }
                }
            }
        }
        frame
    }

    fn frame_bytes(frame: &Frame) -> Vec<u8> {
        let plane = frame.plane(0).expect("plane 0");
        let mut out = Vec::new();
        for row in plane.rows_iter() {
            out.extend_from_slice(row);
        }
        out
    }

    #[test]
    fn round_trips_lossless() {
        for format in [PixFmt::Rgb24, PixFmt::Rgba] {
            let frame = checker_frame(9, 5, format);
            let encoded = codec::encode(&frame).expect("encode");
            let mut budget = Budget::new(Limits::permissive());
            let decoded = codec::decode(&encoded, &mut budget).expect("decode");
            assert_eq!(decoded.len(), 1);
            assert_eq!(frame_bytes(&frame), frame_bytes(&decoded[0]), "{format:?}");
        }
    }

    #[test]
    fn gray_round_trips_as_rgb() {
        // WebP's decoder always exposes RGB(A), never a dedicated grayscale
        // colour space, so an `L8`-encoded image comes back as RGB with
        // every channel equal to the source gray sample rather than as
        // `Gray8` again — this checks the pixel meaning survives, not the
        // byte layout.
        let frame = checker_frame(6, 4, PixFmt::Gray8);
        let encoded = codec::encode(&frame).expect("encode");
        let mut budget = Budget::new(Limits::permissive());
        let decoded = codec::decode(&encoded, &mut budget).expect("decode");
        assert_eq!(decoded.len(), 1);
        let gray = frame_bytes(&frame);
        let rgb = frame_bytes(&decoded[0]);
        assert_eq!(rgb.len(), gray.len() * 3);
        for (g, rgb_px) in gray.iter().zip(rgb.chunks_exact(3)) {
            assert_eq!(rgb_px, [*g, *g, *g]);
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"not a webp", &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_) | Error::UnexpectedEof));
    }

    #[test]
    fn send_receive_protocol_shape() {
        let frame = checker_frame(4, 4, PixFmt::Rgba);
        let mut enc = WebpEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("receive packet");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("begin drain");
        assert!(matches!(enc.receive(), Err(Error::Eof)));

        let mut dec = WebpDecoder::new(Limits::permissive());
        dec.send(Some(&packet)).expect("send packet");
        let decoded = dec.receive().expect("receive frame");
        let FrameData::Video { width, height, .. } = decoded.data else {
            panic!("video");
        };
        assert_eq!((width, height), (4, 4));
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("begin drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }
}
