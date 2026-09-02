//! WebP: native lossless (`VP8L`) codec, lossy encode routed through
//! `vaco-codec-vp8`, and `image-webp` kept only as the fallback for
//! `VP8X`-wrapped files (alpha, animation, metadata chunks) this crate does
//! not yet handle natively (C-19).
//!
//! [`codec::decode`] translates WebP bytes (still or animated) to
//! [`vaco_frame::Frame`]s — natively for a bare `VP8L` file, via
//! `image-webp` for everything `VP8X`-wrapped. [`codec::encode`] goes the
//! other way for a single frame, natively, always lossless; a caller that
//! wants lossy sets the `"lossless"` option to `"0"` on [`WebpEncoder`],
//! which routes through [`codec::encode_lossy`] and `vaco-codec-vp8`'s real
//! encoder instead. [`WebpDecoder`]/[`WebpEncoder`] wrap those in the
//! `vaco_codec_core::SendReceive` protocol every codec in this tree shares.
//!
//! A packet is the whole file. An animated WebP can yield several frames
//! from one packet — `image_webp::WebPDecoder` composites dispose/blend
//! onto the canvas itself, unlike GIF/APNG — hence [`Caps::SUBFRAMES`] on
//! decode. Encode has no animation path at all, so it runs one frame in,
//! one packet out.
//!
//! [`vp8l`] is the native lossless bitstream (decode and encode). [`codec`]
//! is the byte-level glue: RIFF sniffing, `Frame`-to-ARGB packing, and the
//! lossy path through `vaco-codec-vp8` + `vaco-scale`. A pixel-format
//! coverage gap belongs in [`codec::frame_to_argb`]'s match; `VP8X`
//! features (alpha via a separate chunk, animation, ICCP/EXIF) going
//! native is future work, not required by C-19.
//!
//! # Dependencies
//!
//! `vaco-codec-vp8` (lossy encode), `vaco-scale` (pixel-format conversion
//! for lossy encode's `Yuv420p` requirement), `image-webp` (the `VP8X`
//! fallback), `vaco-codec-core`, `vaco-frame`/`vaco-pixfmt`/`vaco-pool`,
//! `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;
mod vp8l;

pub use codec::{decode, encode};

use vaco_codec_core::{Accept, Caps, Encoder, Machine, SendReceive};
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`]: one WebP image per
/// frame, lossless by default — set the `"lossless"` option to `"0"` for a
/// lossy (`VP8`) image via `vaco-codec-vp8` instead.
#[derive(Debug)]
pub struct WebpEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    lossless: bool,
}

impl WebpEncoder {
    /// An encoder that bounds the packet it allocates by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            lossless: true,
        }
    }
}

impl Default for WebpEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

// `WebpEncoder` implements `Encoder` directly, not `SendReceive` +
// `AsEncoder`: `AsEncoder<T>`'s impl does not forward `set_option` (it has
// no `SendReceive::set_option` to forward *from* — the trait does not have
// one), so anything wrapped that way is unreachable from the CLI's
// `-lossless`-style options regardless of what the inner type wants to do
// with them. `vaco-codec-vp8`'s `Vp8Encoder` hit the same wall and took the
// same way out; fixing `AsEncoder` itself belongs to `vaco-codec-core`'s
// owner, not this crate.
impl Encoder for WebpEncoder {
    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        match self.machine.accept(frame.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = frame else {
                    return Ok(());
                };
                let bytes = if self.lossless {
                    codec::encode(frame)?
                } else {
                    codec::encode_lossy(frame, &self.limits)?
                };
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &bytes)?;
                packet.pts = frame.pts;
                self.machine.emit(packet);
                Ok(())
            }
        }
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        if self.lossless {
            &[
                vaco_pixfmt::PixFmt::Gray8,
                vaco_pixfmt::PixFmt::Ya8,
                vaco_pixfmt::PixFmt::Rgb24,
                vaco_pixfmt::PixFmt::Rgba,
            ]
        } else {
            &[
                vaco_pixfmt::PixFmt::Yuv420p,
                vaco_pixfmt::PixFmt::Gray8,
                vaco_pixfmt::PixFmt::Ya8,
                vaco_pixfmt::PixFmt::Rgb24,
                vaco_pixfmt::PixFmt::Rgba,
            ]
        }
    }

    /// `"lossless"`: `"0"`/`"false"` switches to a lossy `VP8` image via
    /// `vaco-codec-vp8`; anything else (including never calling this) keeps
    /// the default, native `VP8L` lossless path. Every other key is
    /// accepted silently, matching the reference's own behaviour for an
    /// `AVOption` a codec has no use for.
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        if key == "lossless" {
            self.lossless = value != "0" && !value.eq_ignore_ascii_case("false");
        }
        Ok(())
    }
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        WebpDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(WebpEncoder::new(limits))
}

pub static WEBP_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "webp",
    long_name: "WebP",
    id: vaco_codec_core::CodecId::Webp,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make: make_decoder,
};

pub static WEBP_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "webp",
    long_name: "WebP (lossless by default; -lossless 0 for VP8 lossy)",
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
        enc.send_frame(Some(&frame)).expect("send frame");
        let packet = enc.receive_packet().expect("receive packet");
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));
        enc.send_frame(None).expect("begin drain");
        assert!(matches!(enc.receive_packet(), Err(Error::Eof)));

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

    #[test]
    fn lossless_option_switches_to_vp8_lossy() {
        let frame = checker_frame(8, 8, PixFmt::Rgb24);
        let mut enc = WebpEncoder::new(Limits::permissive());
        enc.set_option("lossless", "0").expect("set option");
        enc.send_frame(Some(&frame)).expect("send frame");
        enc.send_frame(None).expect("begin drain");
        let packet = enc.receive_packet().expect("receive packet");
        // A lossy image is a "VP8 " chunk, not "VP8L".
        assert_eq!(packet.payload().get(12..16), Some(b"VP8 ".as_slice()));
    }
}
