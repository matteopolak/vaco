//! MPEG-1/2/2.5 Layer I/II/III audio decode.
//!
//! Layers I, II and III share one frame header, one set of bit-rate/sample-
//! rate tables (`vaco-format-mpegaudio`) and one 32-band polyphase synthesis
//! filterbank (`synthesis`). Layer III adds side information, Huffman
//! decoding, requantisation, stereo processing, alias reduction and the
//! IMDCT — everything in the `layer3` module.
//!
//! Every frame states its own layer and sample rate, so one decoder handles
//! all three; [`DECODER_MP1`], [`DECODER_MP2`] and [`DECODER_MP3`] just wrap
//! it under the three names the registry lists separately.
//!
//! # Fixed-point
//!
//! Decode here runs in `f32`, not the fixed-point contract the ISO
//! reference decoder uses. Output is close but not bit-identical; see this
//! crate's `docs/codec/vaco-codec-mpegaudio.md` for measured error per
//! layer and channel mode.

#![forbid(unsafe_code)]

mod bitalloc;
mod decoder;
mod huffman;
mod huffman_data;
mod layer1;
mod layer2;
mod layer3;
mod synthesis;
mod tables;

pub use decoder::MpegAudioDecoder;

use vaco_codec_core::{Caps, CodecId, Decoder, DecoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

fn make(limits: Limits) -> Box<dyn Decoder> {
    Box::new(MpegAudioDecoder::new(limits))
}

pub const DECODER_MP1: DecoderDesc = DecoderDesc {
    name: "mp1",
    long_name: "MP1 (MPEG audio layer 1)",
    id: CodecId::Mp1,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make,
};

pub const DECODER_MP2: DecoderDesc = DecoderDesc {
    name: "mp2",
    long_name: "MP2 (MPEG audio layer 2)",
    id: CodecId::Mp2,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make,
};

pub const DECODER_MP3: DecoderDesc = DecoderDesc {
    name: "mp3",
    long_name: "MP3 (MPEG audio layer 3)",
    id: CodecId::Mp3,
    media_type: MediaType::Audio,
    caps: Caps::DELAY,
    supported_rates: &[],
    make,
};
