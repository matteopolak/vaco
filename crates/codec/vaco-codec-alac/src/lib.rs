//! ALAC (Apple Lossless) native decode and encode.
//!
//! # What it is
//!
//! A lossless predictive codec for mono and stereo PCM, registered under
//! [`vaco_codec_core::CodecId::Alac`] as `DECODER_ALAC`/`ENCODER_ALAC`.
//!
//! # How it works
//!
//! [`cookie`] parses Apple's `ALACSpecificConfig` and
//! `ALACChannelLayoutInfo`, using the published magic-cookie description and a
//! cookie extracted from real `ffmpeg -c:a alac` output.
//! [`frame_codec`] implements the adaptive predictor, Rice coding, and
//! reversible mid/side stereo transform; [`decoder::AlacDecoder`] validates
//! packet metadata before allocating audio.
//!
//! The interop contract is two-way and byte-measured: the decoder reads real
//! ffmpeg ALAC packets, and the encoder's output is accepted by the `alac`
//! dev-dependency oracle. `tests/oracle_alac_crate.rs` pins both directions;
//! preserve those checks rather than relying on a self-round-trip alone.
//!
//! Escape-mode framing remains the implementation's choice where the reference
//! permits multiple legal encodings; `frame_codec.rs` documents that boundary.
//! [`vaco_limits::Limits`] bounds attacker-controlled sample/channel allocation.
//!
//! Dependencies are `vaco-codec-core`, `vaco-frame`, `vaco-sampfmt`,
//! `vaco-chlayout`, `vaco-packet`, `vaco-bitstream`, and `vaco-limits`. The
//! `alac` crate is dev-only and is never used as a source dependency.
//!
//! Only 16/32-bit mono/stereo PCM is wired up; wider channel layouts are
//! recognised structurally but have no [`vaco_chlayout::ChannelLayout`] mapping.
//!
//! [`MAX_CHANNELS`]: frame_codec (see its module doc)

#![forbid(unsafe_code)]

mod cookie;
mod decoder;
mod encoder;
mod frame_codec;
mod predictor;
mod rice;

pub use cookie::{AlacChannelLayoutInfo, AlacCookie, AlacSpecificConfig};
pub use decoder::AlacDecoder;
pub use encoder::AlacEncoder;

use vaco_codec_core::{Caps, CodecId, Decoder, DecoderDesc, Encoder, EncoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

fn make_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(AlacDecoder::new(limits))
}

fn make_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(AlacEncoder::new(limits))
}

/// Registered as this crate's `decoder` fragment (plan 19 §3.4).
pub static DECODER_ALAC: DecoderDesc = DecoderDesc {
    name: "alac",
    long_name: "ALAC (Apple Lossless Audio Codec)",
    id: CodecId::Alac,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_decoder,
};

/// Registered as this crate's `encoder` fragment (plan 19 §3.4).
pub static ENCODER_ALAC: EncoderDesc = EncoderDesc {
    name: "alac",
    long_name: "ALAC (Apple Lossless Audio Codec)",
    id: CodecId::Alac,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_encoder,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_chlayout::ChannelLayout;
    use vaco_frame::{Frame, FrameData};
    use vaco_limits::Budget;
    use vaco_packet::Packet;
    use vaco_sampfmt::SampleFmt;

    fn make_frame(samples: &[i32], fmt: SampleFmt, layout: ChannelLayout) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame =
            Frame::alloc_audio(&mut budget, fmt, layout, samples.len() as u32, 44100).unwrap();
        let bytes = fmt.bytes_per_sample();
        let mut plane = frame.plane_mut(0).unwrap();
        let row = plane.row_mut(0).unwrap();
        for (i, &s) in samples.iter().enumerate() {
            let off = i * bytes;
            if let Some(dst) = row.get_mut(off..off + bytes) {
                match bytes {
                    2 => dst.copy_from_slice(&(s as i16).to_le_bytes()),
                    4 => dst.copy_from_slice(&s.to_le_bytes()),
                    _ => {}
                }
            }
        }
        frame
    }

    /// Reads plane 0 at whichever width `frame`'s own `format` declares --
    /// `frame_codec::decode` matches its output `SampleFmt` to the packet's
    /// actual bit depth (`S16P` for a 16-bit stream, not always `S32P`), so
    /// a caller must read at that width rather than assuming one.
    fn plane0_samples(frame: &Frame) -> Vec<i32> {
        let FrameData::Audio { planes, format, .. } = &frame.data else {
            panic!("audio frame")
        };
        let Some(plane) = planes.first() else {
            panic!("plane 0")
        };
        let data = plane.data.as_slice();
        match format {
            SampleFmt::S16P => data
                .chunks_exact(2)
                .map(|c| i32::from(i16::from_le_bytes(c.try_into().unwrap())))
                .collect(),
            _ => data
                .chunks_exact(4)
                .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
                .collect(),
        }
    }

    /// End-to-end through the registered descriptors: build a decoder/
    /// encoder the way `vaco-registry` would (`make` fields only), and
    /// round-trip a frame through encode -> `Packet` -> decode.
    #[test]
    fn registered_descriptors_round_trip_a_frame() {
        let samples: Vec<i32> = (0..2048).map(|i| ((i * 41) % 3001) - 1500).collect();
        let frame = make_frame(&samples, SampleFmt::S16P, ChannelLayout::MONO);

        let mut enc = (ENCODER_ALAC.make)(Limits::permissive());
        enc.send_frame(Some(&frame)).unwrap();
        let packet = enc.receive_packet().unwrap();

        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, packet.payload()).unwrap();

        let mut dec = (DECODER_ALAC.make)(Limits::permissive());
        dec.send_packet(Some(&packet)).unwrap();
        let decoded = dec.receive_frame().unwrap();
        assert_eq!(plane0_samples(&decoded), samples);
    }

    #[test]
    fn descriptors_are_registered_under_the_shared_codec_id() {
        assert_eq!(DECODER_ALAC.id, CodecId::Alac);
        assert_eq!(ENCODER_ALAC.id, CodecId::Alac);
        assert_eq!(DECODER_ALAC.name, "alac");
        assert_eq!(ENCODER_ALAC.name, "alac");
    }
}
