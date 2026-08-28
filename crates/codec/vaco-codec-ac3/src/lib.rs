//! AC-3 decode: sync frame, BSI, exponent strategies, the parametric bit-
//! allocation model, mantissa dequantisation, rematrixing, IMDCT and
//! windowed overlap-add.
//!
//! # Patent posture (D4/D9)
//!
//! AC-3 is D9's GREEN list ("GREEN and shippable: … AC-3 …") and D4's default
//! build already covers "decode-only support for codecs whose essential
//! patents have lapsed" — so **AC-3 decode ships in the default build**,
//! same as any other GREEN codec. Nothing in this crate encodes; D4 keeps
//! every encoder (AC-3's included) opt-in and out of tree here regardless of
//! patent status, which is a separate policy from decode.
//!
//! **E-AC-3 decode does not.** D9 is explicit that E-AC-3's last-patent-
//! expiry rests on "a single hedged secondary source" and says "do not ship
//! E-AC-3 until confirmed by counsel" — a decode-side statement, not an
//! encode-side one, so the AC-3 default-decode reasoning above does not
//! transfer. [`eac3`] exists only behind the non-default
//! `patent-unverified-eac3-decode` feature (see this crate's `Cargo.toml`
//! for why that is not spelled `patent-encumbered-eac3-decode`: D9's own
//! word is "unresolved," not "encumbered," and the feature name should not
//! claim more legal certainty than the decision record does). The
//! acceptance bar for turning this on — counsel's written confirmation
//! recorded before the feature is enabled — has not happened in this
//! session; nobody here is in a position to make that call. This is
//! reported, not resolved.
//!
//! # No registry-to-instance path yet
//!
//! Same gap `vaco-codec-mpegaudio` reports: until very recently `DecoderDesc`
//! had no constructor function pointer at all (fixed elsewhere, in flight
//! concurrently with this crate), so
//! [`DECODER_AC3`] exists for capability listing only. Call
//! [`decode::decode_frame`] directly to actually decode — that is also how
//! this crate's own accuracy was measured against `ffmpeg`, since no CLI
//! path reaches a codec decoder yet either.
//!
//! # Verification
//!
//! See this crate's `docs/codec/vaco-codec-ac3.md` and
//! this crate's own conformance test for the measured accuracy matrix. In
//! summary: the bitstream syntax (sync frame, BSI, exponent grouping) is
//! high-confidence and independently checkable (a wrong field width desyncs
//! the frame, which is directly observable). The parametric bit-allocation
//! model's masking-curve constants could not be verified against the
//! primary specification text in this environment and are the dominant
//! source of measured error — see `crate::tables_bitalloc`'s module docs.

#![forbid(unsafe_code)]

pub mod audblk;
pub mod bitalloc;
pub mod decode;
pub mod exponent;
pub mod imdct;
pub mod mantissa;
pub mod tables;
pub mod tables_bitalloc;

#[cfg(feature = "patent-unverified-eac3-decode")]
pub mod eac3;

pub use decode::{DecodeOptions, DecodedFrame, StreamState, decode_frame};

use vaco_codec_core::{Caps, CodecId, DecoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

mod decoder;
pub use decoder::Ac3Decoder;

pub const DECODER_AC3: DecoderDesc = DecoderDesc {
    name: "ac3",
    long_name: "ATSC A/52A (AC-3)",
    id: CodecId::Ac3,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits: Limits| Box::new(Ac3Decoder::new(limits)),
};
