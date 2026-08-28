//! Vorbis I decode, native.
//!
//! Covers codebook decode (spec section 3), both floor types (sections 6 and
//! 7), residue types 0/1/2 (section 8), mapping type 0 including channel
//! coupling (section 4.2.4/4.3.5), and mode/window selection through
//! inverse-MDCT overlap-add (section 4.3) — see [`decoder`] for exactly
//! where the pipeline stops on a truncated packet.
//!
//! Vorbis's bit-packing (spec section 2.1.2) is least-significant-bit-first,
//! the opposite of `vaco-bitstream`'s shared MSB-first
//! [`vaco_bitstream::BitReader`], so this crate owns its own reader —
//! [`bitreader::BitReaderLsb`] — rather than bending the shared one.
//!
//! Decode-only: no Vorbis encoder is in scope for this crate. Vorbis is
//! patent-free by design (its own spec exists precisely to establish that),
//! so nothing here carries [`vaco_codec_core::Caps::PATENT_ENCUMBERED`].

#![forbid(unsafe_code)]

mod bitreader;
mod codebook;
mod decoder;
mod floor0;
mod floor1;
mod floor1_table;
mod ident;
mod mdct;
mod residue;
mod setup;

pub use decoder::VorbisDecoder;

use vaco_codec_core::{Caps, CodecId, Decoder, DecoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

fn make(limits: Limits) -> Box<dyn Decoder> {
    Box::new(VorbisDecoder::new(limits))
}

/// The registry descriptor for Vorbis decode.
///
/// [`Caps::DELAY`] because the inverse-MDCT's 50% overlap-add means the
/// first audio packet after setup produces no output at all — its second
/// half is buffered until the next packet's first half is available to add
/// against it (spec section 4.3.8).
pub const DECODER_VORBIS: DecoderDesc = DecoderDesc {
    name: "vorbis",
    long_name: "Vorbis",
    id: CodecId::Vorbis,
    media_type: MediaType::Audio,
    caps: Caps::DELAY,
    supported_rates: &[],
    make,
};
