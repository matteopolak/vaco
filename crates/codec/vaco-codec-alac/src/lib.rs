//! ALAC (Apple Lossless) native decode and encode.
//!
//! # What it is
//!
//! A lossless predictive audio codec for mono and stereo PCM, registered
//! under [`vaco_codec_core::CodecId::Alac`] as `DECODER_ALAC`/`ENCODER_ALAC`.
//!
//! # How it works
//!
//! Two layers, split deliberately so their provenance can be told apart:
//!
//! - [`cookie`] parses the real Apple **magic cookie**
//!   (`ALACSpecificConfig`/`ALACChannelLayoutInfo`) that a `.m4a`/`.caf`
//!   container carries as extradata — field order and widths taken from
//!   Apple's own `ALACMagicCookieDescription.txt` and cross-checked against a
//!   cookie extracted from a real `ffmpeg -c:a alac` output file (see
//!   `provenance/vaco-codec-alac.toml` and `cookie.rs`'s pinned regression
//!   test). [`decoder::AlacDecoder::set_extradata`] uses this to learn a
//!   real stream's sample rate and channel layout.
//! - [`frame_codec`] is the actual packet bitstream: Apple's real adaptive
//!   linear predictor ([`predictor`]) plus adaptive Rice-style entropy
//!   coding ([`rice`]), with a reversible mid/side stereo transform. An
//!   earlier version of this module was a self-invented sign-sign LMS
//!   design that could only decode its own encoder's output; `predictor.rs`'s
//!   doc comment has the full history and the from-scratch translation of
//!   Apple's reference source (`codec/dp_dec.c`'s `unpc_block`, Apache
//!   License 2.0, confirmed outside this project's D7/D15 FFmpeg/libav
//!   clean-room rule) that replaced it.
//!
//! **Consequence, stated plainly**: this decoder reads both the container
//! metadata *and* the compressed audio payload of a real Apple/
//! `ffmpeg`-produced ALAC file correctly, and a real ALAC decoder (the
//! `alac` crate, used here only as a dev-dependency oracle — never read as
//! source) reads this crate's own encoder output correctly in turn.
//! `tests/oracle_alac_crate.rs` checks both directions bit-for-bit against
//! real `ffmpeg`-produced cookie/packet bytes:
//! `this_crates_own_decoder_reads_a_real_ffmpeg_alac_packet_bit_for_bit` and
//! `this_crates_own_encoder_output_is_accepted_by_the_oracle_decoder`.
//!
//! # How to change it
//!
//! [`frame_codec`]'s escape-mode (verbatim) framing predates the real
//! predictor and is still this crate's own choice where the reference
//! offers more than one legal encoding (e.g. exact escape-mode trigger
//! thresholds) — `frame_codec.rs`'s own doc says which parts are which.
//! Real interop is the invariant to preserve: any change here should keep
//! `tests/oracle_alac_crate.rs` passing against the `alac` crate, not just
//! this crate's own round-trip tests.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds every allocation, exactly as every other
//! decoder in this tree: the packet header's own `num_samples`/`channels`
//! fields are attacker-controlled and are validated by
//! [`vaco_frame::Frame::alloc_audio`] before a sample is decoded.
//!
//! # Dependencies
//!
//! `vaco-codec-core` (the protocol), `vaco-frame`/`vaco-sampfmt`/
//! `vaco-chlayout` (the decoded audio), `vaco-packet` (the encoded bytes),
//! `vaco-bitstream` (bit-level I/O), `vaco-limits` (allocation bounds). The
//! `alac` crate is a **dev-dependency only** — see `Cargo.toml`, and
//! `Cargo.toml`'s own dev-dependency section is the only place it is
//! permitted to appear.
//!
//! # What did not land
//!
//! - More than 2 channels: the packet header caps at [`MAX_CHANNELS`]
//!   (stereo), and the cookie's `ALACChannelLayoutInfo` tags for 3.0B/4.0B/
//!   5.0D/5.1D/6.1/7.1B are recognised structurally (`cookie.rs`) but have no
//!   [`vaco_chlayout::ChannelLayout`] mapping wired up.
//! - Bit depths other than 16 and 32 (20/24-bit PCM, which real ALAC
//!   supports, are not implemented).
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
