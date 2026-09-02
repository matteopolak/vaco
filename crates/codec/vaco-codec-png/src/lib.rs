//! PNG and APNG, wrapping the `png` crate (D11).
//!
//! [`codec::decode`]/[`codec::encode`] translate bytes to and from
//! [`vaco_frame::Frame`], covering plain PNG (any of the four colour types
//! `EXPAND` leaves standing, at 8 or 16 bits) and APNG (`acTL`/`fcTL`/`fdAT`),
//! compositing each animation frame onto a shared canvas per its dispose and
//! blend operations the same way the reference decoder's pipeline does.
//! [`PngDecoder`]/[`PngEncoder`] wrap those pure functions in the
//! `vaco_codec_core::SendReceive` protocol every codec in this tree shares.
//!
//! A packet is the whole file; APNG can yield several frames from one
//! packet, so both wrappers declare [`Caps::SUBFRAMES`] and queue every
//! decoded frame with one `Machine::emit_all` call. Encoding runs the other
//! way: frames are buffered ([`Caps::DELAY`]) until the caller drains with
//! `send(None)`, at which point every buffered frame becomes one PNG (one
//! frame) or one APNG (more than one).
//!
//! [`codec`] is the only module that knows the `png` crate's types — no
//! `png::` type appears in this crate's public API, which is the D11
//! boundary. A colour-metadata mapping gap (an arbitrary `gAMA`/`cHRM` pair
//! with no H.273 code point) or a coverage gap belongs in
//! [`codec::map_color_info`] or [`codec::decode`] respectively.
//!
//! [`vaco_limits::Limits`] bounds every allocation this crate makes; see
//! [`codec::decode`]'s own docs for where it does and does not currently
//! route through it (the `png` crate has its own fixed, smaller cap it
//! enforces first).
//!
//! # Dependencies
//!
//! `png` (the wrapped decoder/encoder), `vaco-codec-core` (the protocol),
//! `vaco-frame`/`vaco-pixfmt`/`vaco-pool`/`vaco-color` (the decoded picture
//! and its colour metadata), `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;

pub use codec::{EncodeOptions, Predictor, decode, encode};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`]: one PNG packet in,
/// one or more composited frames out.
#[derive(Debug)]
pub struct PngDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl PngDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES),
            limits,
        }
    }
}

impl Default for PngDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for PngDecoder {
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
                // `codec::decode` has no packet to read a timestamp off;
                // stamp every output frame with the packet's own `pts` so a
                // single-image PNG still lands somewhere in time downstream.
                // An APNG frame already carries its own `fcTL` delay as
                // `duration`/`time_base`, which this does not disturb.
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
/// before a drain becomes one PNG (one frame) or one APNG (more than one).
#[derive(Debug)]
pub struct PngEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    pending: Vec<Frame>,
    options: EncodeOptions,
}

impl PngEncoder {
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

impl Default for PngEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for PngEncoder {
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
            vaco_pixfmt::PixFmt::Ya8,
            vaco_pixfmt::PixFmt::Ya16le,
            vaco_pixfmt::PixFmt::Ya16be,
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
                let mut budget = Budget::new(self.limits.clone());
                let bytes = codec::encode(&self.pending, &mut budget, &self.options)?;
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

    /// `-pred` and `-compression_level`, the two `AVOption`s the reference's
    /// own `png`/`apng` encoders expose (`ffmpeg -h encoder=png`). Any other
    /// key is silently ignored, matching [`vaco_codec_core::Encoder::set_option`]'s
    /// own documented default for an option this codec has no use for.
    ///
    /// # Errors
    /// [`Error::Option`] for a `pred` value outside `0`-`5`/`none`-`mixed`,
    /// or a `compression_level` that does not parse as an integer `0`-`9`.
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "pred" => {
                self.options.pred = Some(match value.trim() {
                    "0" | "none" => Predictor::None,
                    "1" | "sub" => Predictor::Sub,
                    "2" | "up" => Predictor::Up,
                    "3" | "avg" => Predictor::Avg,
                    "4" | "paeth" => Predictor::Paeth,
                    "5" | "mixed" => Predictor::Mixed,
                    other => {
                        return Err(Error::Option {
                            name: "pred".to_owned(),
                            detail: format!("unknown prediction method: {other:?}"),
                        });
                    }
                });
                Ok(())
            }
            "compression_level" => {
                let level: i64 = value.trim().parse().map_err(|_| Error::Option {
                    name: "compression_level".to_owned(),
                    detail: format!("not an integer: {value:?}"),
                })?;
                let level = u8::try_from(level).ok().filter(|v| *v <= 9).ok_or_else(|| {
                    Error::Option {
                        name: "compression_level".to_owned(),
                        detail: format!("must be 0-9, got {level}"),
                    }
                })?;
                self.options.compression_level = Some(level);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        PngDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        PngEncoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static PNG_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "png",
    long_name: "PNG (Portable Network Graphics) image",
    id: vaco_codec_core::CodecId::Png,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static PNG_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "png",
    long_name: "PNG (Portable Network Graphics) image",
    id: vaco_codec_core::CodecId::Png,
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
    use vaco_pixfmt::PixFmt;

    fn checker_frame(w: u32, h: u32, format: PixFmt) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, format, w, h).expect("alloc");
        let bpp = match format {
            PixFmt::Rgb24 => 3,
            PixFmt::Rgba => 4,
            PixFmt::Gray8 => 1,
            PixFmt::Ya8 => 2,
            _ => panic!("unsupported test format"),
        };
        for mut plane in frame.planes_mut() {
            for y in 0..plane.rows() {
                let row_bytes = plane.row_bytes();
                if let Some(row) = plane.row_mut(y) {
                    for x in 0..row_bytes / bpp {
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
    fn round_trips_rgb_and_rgba() {
        for format in [PixFmt::Rgb24, PixFmt::Rgba, PixFmt::Gray8, PixFmt::Ya8] {
            let frame = checker_frame(9, 5, format);
            let mut budget = Budget::new(Limits::permissive());
            let encoded = codec::encode(
                std::slice::from_ref(&frame),
                &mut budget,
                &EncodeOptions::default(),
            )
            .expect("encode");
            let decoded = codec::decode(&encoded, &mut budget).expect("decode");
            assert_eq!(decoded.len(), 1);
            assert_eq!(frame_bytes(&frame), frame_bytes(&decoded[0]), "{format:?}");
        }
    }

    #[test]
    fn round_trips_apng() {
        let frames: Vec<Frame> = (0..3).map(|i| checker_frame(4 + i, 4, PixFmt::Rgba)).collect();
        // APNG requires one canvas size; re-encode against the first frame's
        // dimensions only (the test exercises multi-frame plumbing, not
        // per-frame resizing).
        let frames: Vec<Frame> = frames
            .into_iter()
            .map(|_| checker_frame(4, 4, PixFmt::Rgba))
            .collect();
        let mut budget = Budget::new(Limits::permissive());
        let encoded =
            codec::encode(&frames, &mut budget, &EncodeOptions::default()).expect("encode apng");
        let decoded = codec::decode(&encoded, &mut budget).expect("decode apng");
        assert_eq!(decoded.len(), frames.len());
        for (input, output) in frames.iter().zip(&decoded) {
            // Compositing collapses to 8-bit RGBA regardless of source
            // format, and every synthetic frame here already was RGBA8.
            assert_eq!(frame_bytes(input), frame_bytes(output));
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"not a png", &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_) | Error::UnexpectedEof));
    }

    #[test]
    fn send_receive_protocol_shape() {
        let frame = checker_frame(3, 3, PixFmt::Rgb24);
        let mut enc = PngEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("begin drain");
        let packet = enc.receive().expect("receive packet");
        assert!(matches!(enc.receive(), Err(Error::Eof)));

        let mut dec = PngDecoder::new(Limits::permissive());
        dec.send(Some(&packet)).expect("send packet");
        let decoded = dec.receive().expect("receive frame");
        assert_eq!(frame_bytes(&frame), frame_bytes(&decoded));
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("begin drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    /// One frame through, `send(None)` to drain, take the packet.
    fn encode_one(frame: &Frame, options: &[(&str, &str)]) -> Vec<u8> {
        let mut enc = PngEncoder::new(Limits::permissive());
        for (key, value) in options {
            enc.set_option(key, value).expect("set_option");
        }
        enc.send(Some(frame)).expect("send frame");
        enc.send(None).expect("begin drain");
        enc.receive().expect("receive packet").payload().to_vec()
    }

    /// `-compression_level` moves output size monotonically, mirroring the
    /// real `ffmpeg png` encoder measured directly: 0 (no compression) is
    /// far larger than 9 (max) on the same pixels. This is CL reachability
    /// end to end through `set_option`, the same channel the CLI drives.
    #[test]
    fn compression_level_moves_output_size_the_expected_direction() {
        let frame = checker_frame(64, 64, PixFmt::Rgb24);
        let none = encode_one(&frame, &[("compression_level", "0")]);
        let max = encode_one(&frame, &[("compression_level", "9")]);
        assert!(
            none.len() > max.len(),
            "expected level 0 ({}) > level 9 ({})",
            none.len(),
            max.len()
        );
    }

    /// `-pred paeth` and `-pred 4` name the same filter (measured against
    /// real `ffmpeg`: both produce byte-identical PNGs on the same input),
    /// so both spellings must reach the same code path here too.
    #[test]
    fn pred_accepts_both_the_name_and_the_number() {
        let frame = checker_frame(16, 16, PixFmt::Rgba);
        let by_name = encode_one(&frame, &[("pred", "paeth")]);
        let by_number = encode_one(&frame, &[("pred", "4")]);
        assert_eq!(by_name, by_number);
    }

    /// `-pred none` must actually take effect and differ from the default
    /// (`paeth`) -- not merely parse without erroring.
    #[test]
    fn pred_none_differs_from_the_default() {
        let frame = checker_frame(16, 16, PixFmt::Rgba);
        let default = encode_one(&frame, &[]);
        let none = encode_one(&frame, &[("pred", "none")]);
        assert_ne!(default, none);
    }

    #[test]
    fn set_option_rejects_a_malformed_value() {
        let mut enc = PngEncoder::new(Limits::permissive());
        assert!(matches!(
            enc.set_option("pred", "sideways"),
            Err(Error::Option { .. })
        ));
        assert!(matches!(
            enc.set_option("compression_level", "10"),
            Err(Error::Option { .. })
        ));
        assert!(matches!(
            enc.set_option("compression_level", "not-a-number"),
            Err(Error::Option { .. })
        ));
    }

    /// A key this encoder has no use for is a silent no-op, matching
    /// `Encoder::set_option`'s own documented default -- `ffmpeg -c:v png
    /// -b:v 1M` exits 0 and writes an unchanged PNG.
    #[test]
    fn set_option_ignores_a_key_this_encoder_has_no_use_for() {
        let mut enc = PngEncoder::new(Limits::permissive());
        enc.set_option("b", "1000000").expect("silently ignored");
    }
}
