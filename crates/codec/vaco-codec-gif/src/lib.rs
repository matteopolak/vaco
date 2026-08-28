//! GIF, wrapping the `gif` crate (D11).
//!
//! # What it is
//!
//! [`codec::decode`]/[`codec::encode`] translate bytes to and from
//! [`vaco_frame::Frame`]; [`GifDecoder`]/[`GifEncoder`] wrap those in the
//! `vaco_codec_core::SendReceive` protocol every codec in this tree shares.
//!
//! # How it works
//!
//! A packet is the whole file. GIF can carry several frames, each declaring
//! its own position, size and disposal method, so decode composites every
//! frame onto a shared canvas itself (`gif::ColorOutput::RGBA` resolves
//! palette and transparency per pixel, but not cross-frame compositing) and
//! yields every composited frame from one `send`, hence [`Caps::SUBFRAMES`].
//! Encode runs the other way: frames are buffered ([`Caps::DELAY`]) until
//! the caller drains, then written as one GIF (animated when more than one
//! frame was sent).
//!
//! # How to change it
//!
//! [`codec`] is the only module that knows the `gif` crate's types. A
//! compositing-policy change (dispose/blend) belongs in
//! [`codec::composite`]/[`codec::decode`]; a pixel-format-coverage gap in
//! encode belongs in [`codec::to_rgba8`].
//!
//! # Dependencies
//!
//! `gif` (the wrapped decoder/encoder, including its own `NeuQuant` palette
//! quantiser for encode), `vaco-codec-core`, `vaco-frame`/`vaco-pixfmt`/
//! `vaco-pool`, `vaco-packet`, `vaco-limits`.

#![forbid(unsafe_code)]

mod codec;

pub use codec::{decode, encode};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`]: one GIF packet in,
/// one or more composited RGBA frames out.
#[derive(Debug)]
pub struct GifDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl GifDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::SUBFRAMES),
            limits,
        }
    }
}

impl Default for GifDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for GifDecoder {
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
/// before a drain becomes one GIF.
#[derive(Debug)]
pub struct GifEncoder {
    machine: Machine<Packet>,
    limits: Limits,
    pending: Vec<Frame>,
}

impl GifEncoder {
    /// An encoder that bounds the packet it allocates by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::DELAY),
            limits,
            pending: Vec::new(),
        }
    }
}

impl Default for GifEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for GifEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        &[vaco_pixfmt::PixFmt::Rgba, vaco_pixfmt::PixFmt::Rgb24, vaco_pixfmt::PixFmt::Gray8]
    }

    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                let bytes = codec::encode(&self.pending)?;
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
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        GifDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        GifEncoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static GIF_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "gif",
    long_name: "CompuServe GIF (Graphics Interchange Format)",
    id: vaco_codec_core::CodecId::Gif,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::SUBFRAMES,
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static GIF_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "gif",
    long_name: "CompuServe GIF (Graphics Interchange Format)",
    id: vaco_codec_core::CodecId::Gif,
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

    fn checker_frame(w: u32, h: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, w, h).expect("alloc");
        for mut plane in frame.planes_mut() {
            for y in 0..plane.rows() {
                let row_bytes = plane.row_bytes();
                if let Some(row) = plane.row_mut(y) {
                    for x in 0..row_bytes / 4 {
                        let base = x * 4;
                        // Fully opaque, distinct per-pixel colours so
                        // NeuQuant quantisation has something to preserve.
                        row[base] = ((x * 40) % 256) as u8;
                        row[base + 1] = ((y * 80) % 256) as u8;
                        row[base + 2] = 128;
                        row[base + 3] = 255;
                    }
                }
            }
        }
        frame
    }

    #[test]
    fn round_trips_single_frame() {
        let frame = checker_frame(6, 4);
        let encoded = codec::encode(std::slice::from_ref(&frame)).expect("encode");
        let mut budget = Budget::new(Limits::permissive());
        let decoded = codec::decode(&encoded, &mut budget).expect("decode");
        assert_eq!(decoded.len(), 1);
        let FrameData::Video { width, height, .. } = decoded[0].data else {
            panic!("video");
        };
        assert_eq!((width, height), (6, 4));
    }

    #[test]
    fn round_trips_multi_frame_animation() {
        let frames: Vec<Frame> = (0..3).map(|_| checker_frame(5, 5)).collect();
        let encoded = codec::encode(&frames).expect("encode");
        let mut budget = Budget::new(Limits::permissive());
        let decoded = codec::decode(&encoded, &mut budget).expect("decode");
        assert_eq!(decoded.len(), frames.len());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"not a gif", &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_) | Error::UnexpectedEof));
    }

    #[test]
    fn send_receive_protocol_shape() {
        let frame = checker_frame(4, 4);
        let mut enc = GifEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("begin drain");
        let packet = enc.receive().expect("receive packet");
        assert!(matches!(enc.receive(), Err(Error::Eof)));

        let mut dec = GifDecoder::new(Limits::permissive());
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
