//! TIFF, wrapping the `tiff` crate (D11).
//!
//! # What it is
//!
//! [`codec::decode`]/[`codec::encode`] translate bytes to and from
//! [`vaco_frame::Frame`], covering multi-page TIFF's 8- and 16-bit
//! grayscale/grayscale+alpha/RGB/RGBA. [`TiffDecoder`]/[`TiffEncoder`] wrap
//! those in the `vaco_codec_core::SendReceive` protocol every codec in this
//! tree shares.
//!
//! # How it works
//!
//! A packet is the whole file; a TIFF can carry several pages (IFDs), so a
//! packet may yield several frames from one `send`, hence
//! [`Caps::SUBFRAMES`]. Encode buffers frames until drain
//! ([`Caps::DELAY`]) and writes one page per frame into a single (possibly
//! multi-page) TIFF.
//!
//! # How to change it
//!
//! [`codec`] is the only module that knows the `tiff` crate's types. A
//! coverage gap (a colour type or compression the `tiff` crate does not
//! decode) surfaces as [`vaco_core::Error::Unsupported`] from
//! [`codec::page_from_result`]; extending coverage means adding a match arm
//! there, and the mirror image in [`codec::to_encodable`] for encode.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds every page allocation via
//! `Budget::check_frame`, checked before decode touches a pixel.
//!
//! # Dependencies
//!
//! `tiff` (the wrapped decoder/encoder), `vaco-codec-core`,
//! `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;

pub use codec::{CompressionAlgo, EncodeOptions, decode, encode};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`]: one TIFF packet in,
/// one or more page frames out.
#[derive(Debug)]
pub struct TiffDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl TiffDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES),
            limits,
        }
    }
}

impl Default for TiffDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for TiffDecoder {
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`]: every frame sent
/// before a drain becomes one page of a single TIFF.
#[derive(Debug)]
pub struct TiffEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    pending: Vec<Frame>,
    options: EncodeOptions,
}

impl TiffEncoder {
    /// An encoder that bounds the packet it allocates by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::DELAY),
            limits,
            pending: Vec::new(),
            options: EncodeOptions::default(),
        }
    }
}

impl Default for TiffEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for TiffEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        &[
            vaco_pixfmt::PixFmt::Gray8,
            vaco_pixfmt::PixFmt::Gray16le,
            vaco_pixfmt::PixFmt::Gray16be,
            vaco_pixfmt::PixFmt::Rgb24,
            vaco_pixfmt::PixFmt::Rgb48le,
            vaco_pixfmt::PixFmt::Rgb48be,
            vaco_pixfmt::PixFmt::Rgba,
            vaco_pixfmt::PixFmt::Rgba64le,
            vaco_pixfmt::PixFmt::Rgba64be,
        ]
    }

    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                let bytes = codec::encode(&self.pending, &self.options)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &bytes)?;
                packet.pts = self.pending.first().map_or(vaco_core::Timestamp::NONE, |f| f.pts);
                self.pending.clear();
                self.machine.emit(packet);
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else {
                    return Ok(());
                };
                self.pending.push(frame.clone());
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.machine.flush();
    }

    /// `-compression_algo`, the one `AVOption` the reference's own `tiff`
    /// encoder exposes (`ffmpeg -h encoder=tiff`). Any other key is
    /// silently ignored, matching
    /// [`vaco_codec_core::Encoder::set_option`]'s own documented default.
    ///
    /// # Errors
    /// [`Error::Option`] for a `compression_algo` value that is none of
    /// `raw`/`lzw`/`deflate`/`packbits` or their numeric tag (`1`/`5`/
    /// `32946`/`32773`).
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        if key == "compression_algo" {
            self.options.compression_algo = match value.trim() {
                "1" | "raw" => CompressionAlgo::Raw,
                "5" | "lzw" => CompressionAlgo::Lzw,
                "32946" | "deflate" => CompressionAlgo::Deflate,
                "32773" | "packbits" => CompressionAlgo::Packbits,
                other => {
                    return Err(Error::Option {
                        name: "compression_algo".to_owned(),
                        detail: format!("unknown compression algorithm: {other:?}"),
                    });
                }
            };
        }
        Ok(())
    }
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        TiffDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        TiffEncoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static TIFF_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "tiff",
    long_name: "TIFF image",
    id: vaco_codec_core::CodecId::Tiff,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static TIFF_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "tiff",
    long_name: "TIFF image",
    id: vaco_codec_core::CodecId::Tiff,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::DELAY,
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
    fn round_trips_single_page() {
        for format in [PixFmt::Rgb24, PixFmt::Rgba, PixFmt::Gray8] {
            let frame = checker_frame(9, 5, format);
            let encoded = codec::encode(std::slice::from_ref(&frame), &EncodeOptions::default())
                .expect("encode");
            let mut budget = Budget::new(Limits::permissive());
            let decoded = codec::decode(&encoded, &mut budget).expect("decode");
            assert_eq!(decoded.len(), 1);
            assert_eq!(frame_bytes(&frame), frame_bytes(&decoded[0]), "{format:?}");
        }
    }

    #[test]
    fn round_trips_multi_page() {
        let frames: Vec<Frame> = (0..3).map(|_| checker_frame(4, 4, PixFmt::Rgb24)).collect();
        let encoded = codec::encode(&frames, &EncodeOptions::default()).expect("encode");
        let mut budget = Budget::new(Limits::permissive());
        let decoded = codec::decode(&encoded, &mut budget).expect("decode");
        assert_eq!(decoded.len(), frames.len());
        for (input, output) in frames.iter().zip(&decoded) {
            assert_eq!(frame_bytes(input), frame_bytes(output));
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"not a tiff", &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_) | Error::Unsupported(_)));
    }

    #[test]
    fn send_receive_protocol_shape() {
        let frame = checker_frame(3, 3, PixFmt::Rgb24);
        let mut enc = TiffEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("begin drain");
        let packet = enc.receive().expect("receive packet");
        assert!(matches!(enc.receive(), Err(Error::Eof)));

        let mut dec = TiffDecoder::new(Limits::permissive());
        dec.send(Some(&packet)).expect("send packet");
        let decoded = dec.receive().expect("receive frame");
        let FrameData::Video { width, height, .. } = decoded.data else {
            panic!("video");
        };
        assert_eq!((width, height), (3, 3));
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("begin drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    /// Every `-compression_algo` value must round-trip through decode
    /// exactly (all four are lossless) and actually take effect: `raw`
    /// (uncompressed) must be strictly larger than `deflate` on compressible
    /// content, not merely parse without error.
    #[test]
    fn compression_algo_round_trips_and_actually_changes_size() {
        let frame = checker_frame(32, 32, PixFmt::Rgb24);
        let mut sizes = Vec::new();
        for (key_value, algo) in [
            ("raw", CompressionAlgo::Raw),
            ("lzw", CompressionAlgo::Lzw),
            ("deflate", CompressionAlgo::Deflate),
            ("packbits", CompressionAlgo::Packbits),
        ] {
            let mut enc = TiffEncoder::new(Limits::permissive());
            enc.set_option("compression_algo", key_value)
                .expect("set_option");
            assert_eq!(enc.options.compression_algo, algo);
            enc.send(Some(&frame)).expect("send frame");
            enc.send(None).expect("begin drain");
            let packet = enc.receive().expect("receive packet");
            let mut budget = Budget::new(Limits::permissive());
            let decoded = codec::decode(packet.payload(), &mut budget).expect("decode");
            assert_eq!(decoded.len(), 1);
            assert_eq!(frame_bytes(&frame), frame_bytes(&decoded[0]));
            sizes.push((key_value, packet.payload().len()));
        }
        let raw_size = sizes[0].1;
        let deflate_size = sizes[2].1;
        assert!(
            raw_size > deflate_size,
            "expected raw ({raw_size}) > deflate ({deflate_size}): {sizes:?}"
        );
    }

    /// The numeric spelling (`ffmpeg`'s own tag values) must reach the same
    /// algorithm as the name.
    #[test]
    fn compression_algo_accepts_both_the_name_and_the_number() {
        let mut by_name = TiffEncoder::new(Limits::permissive());
        by_name.set_option("compression_algo", "deflate").expect("set_option");
        let mut by_number = TiffEncoder::new(Limits::permissive());
        by_number.set_option("compression_algo", "32946").expect("set_option");
        assert_eq!(
            by_name.options.compression_algo,
            by_number.options.compression_algo
        );
    }

    #[test]
    fn set_option_rejects_a_malformed_compression_algo() {
        let mut enc = TiffEncoder::new(Limits::permissive());
        assert!(matches!(
            enc.set_option("compression_algo", "bogus"),
            Err(Error::Option { .. })
        ));
    }

    /// A key this encoder has no use for is a silent no-op, matching
    /// `Encoder::set_option`'s own documented default.
    #[test]
    fn set_option_ignores_a_key_this_encoder_has_no_use_for() {
        let mut enc = TiffEncoder::new(Limits::permissive());
        enc.set_option("b", "1000000").expect("silently ignored");
    }
}
