//! QOI ("Quite OK Image format") decode and encode.
//!
//! # What it is
//!
//! QOI is a whole-image, single-frame lossless format: one header, a run of
//! seven simple chunk types over a 64-entry cache of recently seen pixels, no
//! entropy coder, no sub-image structure. [`codec::decode`]/[`codec::encode`]
//! are the pure functions; [`QoiDecoder`]/[`QoiEncoder`] wrap them in the
//! send/receive protocol every codec in this tree shares
//! (`vaco_codec_core::SendReceive`).
//!
//! # How it works
//!
//! A QOI image is exactly one packet in and one frame out — there is no
//! reordering and no packet ever yields more than one frame — so both wrappers
//! declare [`Caps::empty`] and lean on `vaco_codec_core::Machine` for the
//! protocol bookkeeping, the same way `vaco_codec_core::mock::MockDecoder`
//! does. [`QOI_DECODER`]/[`QOI_ENCODER`] register these under
//! `vaco_codec_core::CodecId::Qoi`, and `vaco-component.toml` is what makes
//! `-c:v qoi` reach them.
//!
//! # How to change it
//!
//! [`codec`] is the only module that knows the byte format. If a future QOI
//! extension appears, it lives there; the `SendReceive` wrappers in this file
//! should never need to change for it.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds the decoded frame the way it bounds every
//! other decoder in this tree — `width`/`height` come straight from the
//! attacker-controlled header, so they are validated by
//! `vaco_frame::Frame::alloc_video` (via `Budget::check_frame`) before a byte
//! of pixel data is touched.
//!
//! # Dependencies
//!
//! `vaco-codec-core` (the protocol), `vaco-frame`/`vaco-pixfmt`/`vaco-pool`
//! (the decoded picture), `vaco-packet` (the encoded bytes), `vaco-limits`
//! (allocation bounds).

#![forbid(unsafe_code)]

mod codec;
mod reader;

pub use codec::{decode, encode, parameters};

use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// A [`SendReceive`] decoder over [`Packet`]/[`Frame`], one QOI image per
/// packet.
#[derive(Debug)]
pub struct QoiDecoder {
    machine: Machine<Frame>,
    limits: Limits,
}

impl QoiDecoder {
    /// A decoder that bounds every allocation by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Default for QoiDecoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for QoiDecoder {
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
                // `codec::decode` is a pure function over bytes alone and has
                // no packet to read a timestamp off; stamping the frame with
                // the packet's own `pts` here is what lets a muxer downstream
                // of a decode-then-encode leg place this image in time at
                // all. Found round-tripping through a timestamped container:
                // every muxed packet reached the sink with `pts` unset and
                // the muxer refused it.
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

/// A [`SendReceive`] encoder over [`Frame`]/[`Packet`], one QOI image per
/// frame.
#[derive(Debug)]
pub struct QoiEncoder {
    machine: Machine<Packet>,
    limits: Limits,
}

impl QoiEncoder {
    /// An encoder that bounds the packet it allocates by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

impl Default for QoiEncoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SendReceive for QoiEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        &[vaco_pixfmt::PixFmt::Rgb24, vaco_pixfmt::PixFmt::Rgba]
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
                // Mirrors the decoder's own `pts` stamp: `codec::encode` is a
                // pure function over the frame's pixels alone, so the packet
                // it hands back has no timing of its own until this copies
                // the source frame's `pts` onto it.
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
}

fn make_decoder(limits: Limits) -> Box<dyn vaco_codec_core::Decoder> {
    Box::new(vaco_codec_core::AsDecoder(vaco_codec_core::Validated::new(
        QoiDecoder::new(limits),
    )))
}

fn make_encoder(limits: Limits) -> Box<dyn vaco_codec_core::Encoder> {
    Box::new(vaco_codec_core::AsEncoder(vaco_codec_core::Validated::new(
        QoiEncoder::new(limits),
    )))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static QOI_DECODER: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "qoi",
    long_name: "QOI (Quite OK Image format) image",
    id: vaco_codec_core::CodecId::Qoi,
    media_type: vaco_core::MediaType::Video,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static QOI_ENCODER: vaco_codec_core::EncoderDesc = vaco_codec_core::EncoderDesc {
    name: "qoi",
    long_name: "QOI (Quite OK Image format) image",
    id: vaco_codec_core::CodecId::Qoi,
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
    use vaco_core::{Error, Timestamp};
    use vaco_frame::{FrameData, FrameFlags};
    use vaco_pixfmt::PixFmt;

    /// A deterministic non-uniform image, so encoding exercises the full/diff/
    /// luma chunk mix rather than only runs and indices.
    #[allow(
        clippy::many_single_char_names,
        reason = "r/g/b/w/h read naturally for pixel-channel test fixtures"
    )]
    fn checker_frame(w: u32, h: u32, rgba: bool) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let format = if rgba { PixFmt::Rgba } else { PixFmt::Rgb24 };
        let mut frame = Frame::alloc_video(&mut budget, format, w, h).expect("alloc");
        let FrameData::Video { planes, .. } = &mut frame.data else {
            panic!("video frame")
        };
        let plane = &mut planes[0];
        let stride = plane.stride;
        let bpp = if rgba { 4 } else { 3 };
        let buf = plane.data.make_mut();
        for y in 0..h as usize {
            for x in 0..w as usize {
                let base = y * stride + x * bpp;
                let r = ((x * 37 + y * 91) % 256) as u8;
                let g = ((x * 61 + y * 17) % 256) as u8;
                let b = ((x * 5 + y * 251) % 256) as u8;
                buf[base] = r;
                buf[base + 1] = g;
                buf[base + 2] = b;
                if rgba {
                    buf[base + 3] = if (x + y) % 3 == 0 { 128 } else { 255 };
                }
            }
        }
        frame.pts = Timestamp::new(0);
        frame.flags = FrameFlags::KEY;
        frame
    }

    fn frame_bytes(frame: &Frame) -> Vec<u8> {
        let FrameData::Video {
            format,
            width,
            height,
            planes,
        } = &frame.data
        else {
            panic!("video frame")
        };
        let bpp = if *format == PixFmt::Rgba { 4 } else { 3 };
        let plane = &planes[0];
        let mut out = Vec::new();
        for y in 0..*height as usize {
            let row = &plane.data.as_slice()[y * plane.stride..y * plane.stride + *width as usize * bpp];
            out.extend_from_slice(row);
        }
        out
    }

    #[test]
    fn round_trips_rgb() {
        for (w, h) in [(1, 1), (2, 3), (7, 5), (64, 64)] {
            let frame = checker_frame(w, h, false);
            let encoded = codec::encode(&frame).expect("encode");
            let mut budget = Budget::new(Limits::permissive());
            let decoded = codec::decode(&encoded, &mut budget).expect("decode");
            assert_eq!(frame_bytes(&frame), frame_bytes(&decoded), "{w}x{h}");
        }
    }

    #[test]
    fn round_trips_rgba() {
        for (w, h) in [(1, 1), (3, 2), (9, 9)] {
            let frame = checker_frame(w, h, true);
            let encoded = codec::encode(&frame).expect("encode");
            let mut budget = Budget::new(Limits::permissive());
            let decoded = codec::decode(&encoded, &mut budget).expect("decode");
            assert_eq!(frame_bytes(&frame), frame_bytes(&decoded));
        }
    }

    #[test]
    fn solid_colour_uses_run_length() {
        // A fully uniform image should compress far below its raw size: this
        // is the property that would fail if QOI_OP_RUN capped at the wrong
        // length or never fired at all.
        let frame = {
            let mut budget = Budget::new(Limits::permissive());
            let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 64, 64).expect("alloc");
            let FrameData::Video { planes, .. } = &mut frame.data else {
                panic!("video")
            };
            let plane = &mut planes[0];
            let stride = plane.stride;
            let buf = plane.data.make_mut();
            for y in 0..64usize {
                for x in 0..64usize {
                    let base = y * stride + x * 3;
                    buf[base] = 10;
                    buf[base + 1] = 20;
                    buf[base + 2] = 30;
                }
            }
            frame
        };
        let encoded = codec::encode(&frame).expect("encode");
        assert!(encoded.len() * 4 < 64 * 64 * 3);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"nope", &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_) | Error::UnexpectedEof));
    }

    #[test]
    fn rejects_truncated_header() {
        let mut budget = Budget::new(Limits::permissive());
        let err = codec::decode(b"qoif\x00", &mut budget).unwrap_err();
        assert!(matches!(err, Error::UnexpectedEof | Error::InvalidData(_)));
    }

    #[test]
    fn rejects_zero_dimensions() {
        let mut budget = Budget::new(Limits::permissive());
        let mut header = Vec::new();
        header.extend_from_slice(b"qoif");
        header.extend_from_slice(&0u32.to_be_bytes());
        header.extend_from_slice(&10u32.to_be_bytes());
        header.push(3);
        header.push(0);
        let err = codec::decode(&header, &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn send_receive_protocol_shape() {
        let frame = checker_frame(4, 4, false);
        let mut enc = QoiEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("receive packet");
        assert!(matches!(enc.receive(), Err(Error::NeedMoreInput)));
        enc.send(None).expect("begin drain");
        assert!(matches!(enc.receive(), Err(Error::Eof)));

        let mut dec = QoiDecoder::new(Limits::permissive());
        dec.send(Some(&packet)).expect("send packet");
        let decoded = dec.receive().expect("receive frame");
        assert_eq!(frame_bytes(&frame), frame_bytes(&decoded));
        assert!(matches!(dec.receive(), Err(Error::NeedMoreInput)));
        dec.send(None).expect("begin drain");
        assert!(matches!(dec.receive(), Err(Error::Eof)));
    }

    /// Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`/
    /// `vaco-codec-webp`'s encoders: never set `Packet::duration`.
    #[test]
    fn send_propagates_the_input_frames_real_duration() {
        let mut frame = checker_frame(4, 4, false);
        frame.duration = vaco_core::Duration::from_micros(40_000);
        let mut enc = QoiEncoder::new(Limits::permissive());
        enc.send(Some(&frame)).expect("send frame");
        let packet = enc.receive().expect("receive packet");
        assert_eq!(packet.duration, vaco_core::Duration::from_micros(40_000));
    }
}
