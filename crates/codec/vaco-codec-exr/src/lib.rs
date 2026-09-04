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
//! gaps — deep data, multi-part files, non-RGB(A) channel layouts, and channel
//! subsampling, plus HTJ2K compression not implemented by `exr` — surface as
//! [`vaco_core::Error::Unsupported`] from
//! [`codec::decode`]. Scan-line and tiled images are supported, with only the
//! largest resolution level decoded. Extending the unsupported shapes means
//! updating the shared scope gate and reader pipeline together, not changing
//! this file.
//!
//! # Dependencies
//!
//! `exr` (the wrapped decoder/encoder), `vaco-codec-core`,
//! `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;

pub use codec::{CompressionAlgo, EncodeOptions, decode, encode, parameters};

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
    use exr::meta::{BlockDescription, Requirements, magic_number, sequence_end};
    use exr::prelude::{AttributeValue, Compression, LineOrder, MetaData, Text, Vec2};
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

    fn encoded_header() -> exr::meta::header::Header {
        let encoded = codec::encode(&checker_frame(4, 2), &EncodeOptions::default())
            .expect("encode header fixture");
        MetaData::read_from_buffered(encoded.as_slice(), false)
            .expect("read encoded metadata")
            .headers
            .first()
            .expect("one header")
            .clone()
    }

    fn header_only_exr(
        headers: &[exr::meta::header::Header],
        requirements: Requirements,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        magic_number::write(&mut bytes).expect("write magic");
        requirements.write(&mut bytes).expect("write requirements");
        exr::meta::header::Header::write_all(headers, &mut bytes, requirements.has_multiple_layers)
            .expect("write headers");
        bytes
    }

    fn single_part_requirements(header: &exr::meta::header::Header) -> Requirements {
        Requirements {
            file_format_version: 2,
            is_single_layer_and_tiled: header.blocks.has_tiles(),
            has_long_names: false,
            has_deep_data: false,
            has_multiple_layers: false,
        }
    }

    fn deep_header_only_exr(mut header: exr::meta::header::Header) -> Vec<u8> {
        header = header.with_encoding(
            Compression::ZIP1,
            BlockDescription::ScanLines,
            LineOrder::Increasing,
        );
        header.deep_data_version = Some(1);
        header.max_samples_per_pixel = Some(1);

        let requirements = Requirements {
            file_format_version: 2,
            is_single_layer_and_tiled: false,
            has_long_names: false,
            has_deep_data: true,
            has_multiple_layers: false,
        };
        let mut bytes = Vec::new();
        magic_number::write(&mut bytes).expect("write magic");
        requirements.write(&mut bytes).expect("write requirements");
        for (name, value) in header.all_named_attributes() {
            let value = if name == exr::meta::header::standard_names::BLOCK_TYPE {
                AttributeValue::Text(Text::from("deepscanline"))
            } else {
                value
            };
            exr::meta::attribute::write(name, &value, &mut bytes).expect("write attribute");
        }
        sequence_end::write(&mut bytes).expect("write header end");
        bytes
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
        assert!(matches!(
            err,
            Error::InvalidData(_) | Error::Unsupported(_) | Error::UnexpectedEof
        ));
    }

    #[test]
    fn decode_charges_the_rgba_staging_buffer_before_allocating_it() {
        let encoded = codec::encode(&checker_frame(4, 4), &EncodeOptions::default())
            .expect("encode budget fixture");
        let mut limits = Limits::permissive();
        limits.max_alloc_single = 400;
        limits.max_alloc_total = 400;
        let mut budget = Budget::new(limits);

        assert!(matches!(
            codec::decode(&encoded, &mut budget),
            Err(Error::LimitExceeded { .. })
        ));
    }

    #[test]
    fn decode_refuses_the_staging_allocation_before_reading_pixels() {
        let header = encoded_header();
        let requirements = single_part_requirements(&header);
        let bytes = header_only_exr(&[header], requirements);
        let mut limits = Limits::permissive();
        limits.max_alloc_single = 127;
        let mut budget = Budget::new(limits);

        assert!(matches!(
            codec::decode(&bytes, &mut budget),
            Err(Error::LimitExceeded { .. })
        ));
        assert_eq!(budget.committed(), 0);
    }

    #[test]
    fn parameters_refuses_a_layer_without_the_exact_rgb_channel_shape() {
        let mut header = encoded_header();
        let red = header
            .channels
            .list
            .iter_mut()
            .find(|channel| channel.name.eq("R"))
            .expect("red channel");
        red.name = Text::from("Y");
        let requirements = single_part_requirements(&header);
        let bytes = header_only_exr(&[header], requirements);

        assert!(codec::parameters(&bytes).is_none());
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            codec::decode(&bytes, &mut budget),
            Err(Error::Unsupported("exr: non-RGB channel layout"))
        ));
    }

    #[test]
    fn parameters_refuses_subsampled_rgb_channels() {
        let mut header = encoded_header().with_encoding(
            Compression::RLE,
            BlockDescription::ScanLines,
            LineOrder::Increasing,
        );
        let red = header
            .channels
            .list
            .iter_mut()
            .find(|channel| channel.name.eq("R"))
            .expect("red channel");
        red.sampling = Vec2(2, 1);
        let requirements = single_part_requirements(&header);
        let bytes = header_only_exr(&[header], requirements);

        assert!(codec::parameters(&bytes).is_none());
    }

    #[test]
    fn parameters_refuses_deep_data() {
        let bytes = deep_header_only_exr(encoded_header());
        assert!(codec::parameters(&bytes).is_none());
    }

    #[test]
    fn parameters_refuses_multipart_files() {
        let first = encoded_header();
        let mut second = first.clone();
        second.own_attributes.layer_name = Some(Text::from("second"));
        let requirements = Requirements {
            file_format_version: 2,
            is_single_layer_and_tiled: false,
            has_long_names: false,
            has_deep_data: false,
            has_multiple_layers: true,
        };
        let bytes = header_only_exr(&[first, second], requirements);

        assert!(codec::parameters(&bytes).is_none());
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            codec::decode(&bytes, &mut budget),
            Err(Error::Unsupported("exr: multipart image"))
        ));
    }

    #[test]
    fn parameters_refuses_compression_the_decoder_dependency_cannot_decode() {
        for compression in [Compression::HTJ2K32, Compression::HTJ2K256] {
            let mut header = encoded_header();
            header.compression = compression;
            let requirements = single_part_requirements(&header);
            let bytes = header_only_exr(&[header], requirements);

            assert!(codec::parameters(&bytes).is_none(), "{compression:?}");
            let mut budget = Budget::new(Limits::permissive());
            assert!(matches!(
                codec::decode(&bytes, &mut budget),
                Err(Error::Unsupported("exr: HTJ2K compression"))
            ));
        }
    }

    #[test]
    fn huge_dimensions_fail_before_rgba_size_or_allocation_can_panic() {
        let header = exr::meta::header::Header::new(
            Text::from("huge"),
            Vec2(1_073_741_822, 1_073_741_822),
            encoded_header().channels.list,
        )
        .with_encoding(
            Compression::ZIP16,
            BlockDescription::ScanLines,
            LineOrder::Increasing,
        );
        let requirements = single_part_requirements(&header);
        let bytes = header_only_exr(&[header], requirements);
        let mut limits = Limits::permissive();
        limits.max_dimension = u32::MAX;
        limits.max_frame_bytes = u64::MAX;
        limits.max_alloc_single = u64::MAX;
        limits.max_alloc_total = u64::MAX;
        let mut budget = Budget::new(limits);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            codec::decode(&bytes, &mut budget)
        }));
        assert!(matches!(result, Ok(Err(Error::LimitExceeded { .. }))));
    }

    #[test]
    fn parameters_accepts_the_same_rgb_layer_decode_accepts() {
        let encoded = codec::encode(&checker_frame(4, 2), &EncodeOptions::default())
            .expect("encode parameters fixture");
        let params = codec::parameters(&encoded).expect("supported parameters");
        let video = params.video.expect("video parameters");
        assert_eq!((video.width, video.height), (4, 2));
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

    /// Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`/
    /// `vaco-codec-webp`'s encoders: never set `Packet::duration`.
    #[test]
    fn send_propagates_the_input_frames_real_duration() {
        let mut frame = checker_frame(2, 2);
        frame.duration = vaco_core::Duration::from_micros(40_000);
        let mut enc = ExrEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("receive packet");
        assert_eq!(packet.duration, vaco_core::Duration::from_micros(40_000));
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
            enc.set_option("compression", key_value)
                .expect("set_option");
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
