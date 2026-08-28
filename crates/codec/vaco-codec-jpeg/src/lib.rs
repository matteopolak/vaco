//! JPEG (ITU-T T.81 / ISO/IEC 10918-1) native decode and encode.
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209`.
//!
//! # What this crate covers
//!
//! Baseline and extended-sequential decode (`SOF0`/`SOF1`), progressive
//! decode (`SOF2`, Annex G: spectral selection and successive
//! approximation), 8-bit and 12-bit sample precision, the four subsampling
//! layouts that map onto an existing [`vaco_pixfmt::PixFmt`]
//! (4:4:4/4:2:2/4:2:0/4:4:0, plus grayscale), JFIF `APP0` and Adobe `APP14`
//! recognition, restart markers, and a baseline encoder. See [`decode`] and
//! [`encode`] for the pure functions, and their own module docs for what is
//! deliberately out of scope: arithmetic entropy coding (Annex D), lossless
//! JPEG (Annex H), four-component CMYK/YCCK output (no matching `PixFmt`),
//! arbitrary (non-power-of-two) sampling factors, and progressive encode.
//!
//! # The "spec-exact" IDCT
//!
//! ITU-T T.81 Annex A.3.3 gives the inverse DCT an accuracy bound, not a
//! mandated bit pattern, unlike H.264/HEVC. [`idct`] names the literal `f64`
//! evaluation this crate uses "spec-exact" (plan 15's own term) to
//! distinguish it from the many faster, less accurate integer
//! approximations a conformant decoder is equally free to use — this crate
//! offers only the accurate one today.
//!
//! # How to change it
//!
//! [`tables`] holds every standard constant (zig-zag order, Annex K's
//! default quantization and Huffman tables). [`header`] parses every
//! marker's payload into a plain struct; [`decode`] is the only module that
//! interprets those structs against entropy-coded data, so a new marker or
//! scan variant starts there. [`marker`] just names byte values.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds every decode: component and block-grid
//! sizes come straight from the attacker-controlled `SOF`, so
//! [`vaco_limits::Budget::alloc`] validates them before a coefficient is
//! stored. [`EncodeOptions`] controls the encoder's quality and restart
//! interval.
//!
//! # Dependencies
//!
//! `vaco-codec-dsp-idct` for the inverse transform, `vaco-tx` for the
//! forward transform this crate builds directly (that crate is
//! inverse-only), `vaco-bitstream` for header-segment byte reading (the
//! entropy-coded bitstream itself needs JPEG's own byte-stuffing model,
//! which is why [`bits`] is a small purpose-built reader rather than a
//! wrapper), `vaco-frame`/`vaco-pixfmt` for the decoded picture,
//! `vaco-packet` for the encoded bytes, `vaco-limits` for allocation
//! bounds, `vaco-codec-core` for the decoder/encoder protocol.

#![forbid(unsafe_code)]

mod bits;
mod decode;
mod encode;
mod header;
mod huffman;
mod idct;
mod marker;
mod tables;

pub use decode::decode;
pub use encode::{EncodeOptions, encode};

use vaco_codec_core::{Accept, Caps, CodecId, DecoderDesc, Machine};
use vaco_core::{MediaType, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`vaco_codec_core::Decoder`] over [`Packet`]/[`Frame`]: one JPEG image
/// (a still image, or one Motion JPEG frame) per packet, matching the
/// pure-function `decode` this wraps.
#[derive(Debug)]
pub struct JpegDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl JpegDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Default for JpegDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl vaco_codec_core::Decoder for JpegDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match self.machine.accept(packet.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = packet else {
                    return Ok(());
                };
                let mut budget = Budget::new(self.limits.clone());
                let mut frame = decode::decode(pkt.payload(), &mut budget)?;
                frame.pts = pkt.pts;
                frame.duration = pkt.duration;
                self.machine.emit(frame);
                Ok(())
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }
}

/// A [`vaco_codec_core::Encoder`] over [`Frame`]/[`Packet`]: one JPEG image
/// per frame.
#[derive(Debug)]
pub struct JpegEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    options: EncodeOptions,
}

impl JpegEncoder {
    /// An encoder using `options`, bounding the packet it allocates by
    /// `limits`.
    #[must_use]
    pub fn new(limits: Limits, options: EncodeOptions) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
            options,
        }
    }
}

impl Default for JpegEncoder {
    fn default() -> Self {
        Self::new(Limits::default(), EncodeOptions::default())
    }
}

impl vaco_codec_core::Encoder for JpegEncoder {
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
                let bytes = encode::encode(frame, &self.options)?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &bytes)?;
                packet.pts = frame.pts;
                packet.duration = frame.duration;
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
}

/// This build's JPEG decoder descriptor.
pub static JPEG_DECODER: DecoderDesc = DecoderDesc {
    name: "jpeg",
    long_name: "JPEG (ITU-T T.81 / ISO/IEC 10918-1), native baseline and progressive decode",
    id: CodecId::Jpeg,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(JpegDecoder::new(limits)),
};

/// This build's JPEG encoder descriptor (baseline only; see [`EncodeOptions`]).
pub static JPEG_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "jpeg",
    long_name: "JPEG (ITU-T T.81 / ISO/IEC 10918-1), native baseline encode",
    id: CodecId::Jpeg,
    media_type: MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(JpegEncoder::new(limits, EncodeOptions::default())),
};
