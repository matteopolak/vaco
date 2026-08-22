//! Exp-Golomb coding, ITU-T H.264 clause 9.1 — the codec-level layer.
//!
//! # What this crate is, and what `vaco-bitstream` already is
//!
//! `vaco-bitstream` is layer 0 and carries the two Exp-Golomb codes a *container*
//! needs to look inside a parameter set: `ue(v)` and `se(v)`. That is deliberate
//! — a demuxer must be able to read an SPS without depending on a codec crate
//! (D14.1), so those two live at the bottom.
//!
//! This crate is the rest of clause 9.1, the part only a codec needs:
//!
//! | Here | Why not in `vaco-bitstream` |
//! |---|---|
//! | `te(v)` truncated, clause 9.1.1 | needs a `cMax` that only slice syntax supplies |
//! | `me(v)` mapped, clause 9.1.2 | carries Table 9-4, which is macroblock-level knowledge |
//! | order-`k` signed, and 64-bit forms | only entropy coding uses them |
//! | the whole **encoder** side | a demuxer never writes Exp-Golomb |
//! | the mappings as pure functions, and codeword **costs** | rate-distortion, not parsing |
//! | [`BoundedGolomb`] — reads charged against a [`Budget`](vaco_limits::Budget) | layer 0 has no budget in scope |
//!
//! It also carries a **faster `ue(v)`**. See [`GolombDecode::ue_v`] for the
//! mechanism — the short version is that a codeword with a prefix of 15 zeros or
//! fewer is already inside the 32-bit word the reader peeked, so it can be
//! extracted and skipped in one step instead of two. The two implementations are
//! held to agreement by a differential property test, because parsers already
//! written against `vaco-bitstream` must keep decoding identically.
//!
//! # The three things that keep this safe on untrusted input
//!
//! 1. **No unbounded loop anywhere.** Every prefix is a `leading_zeros` over a
//!    fixed-width word, capped at 31 (or 63 for the `u64` form). An all-zero
//!    buffer is rejected in constant time. This is the difference between a
//!    clean rejection and a fuzz hang, and it is why `ue_v64` writes its
//!    two-iteration loop with an explicit ceiling inside.
//! 2. **No panics.** `unwrap`/`expect`/`panic`/`indexing_slicing` are denied
//!    workspace-wide, and every table lookup here goes through `get`. The
//!    encoder clamps out-of-domain values and debug-asserts instead of
//!    panicking, so a caller bug is loud in development and survivable in
//!    production.
//! 3. **Bounds are stated, not assumed.** [`GolombDecode`]'s `*_max` family
//!    takes the ceiling at the read site, and [`BoundedGolomb`] additionally
//!    charges fuel per element so a syntax *loop* is bounded too.
//!
//! # Example
//!
//! ```
//! use vaco_bitstream::{BitReader, BitWriter};
//! use vaco_codec_golomb::{
//!     ChromaArrayType, GolombDecode, GolombEncode, MbPartPredMode,
//! };
//!
//! let mut w = BitWriter::new();
//! w.put_ue_v(42);
//! w.put_se_v(-7);
//! w.put_te_v(1, 0);
//! w.put_me_v(ChromaArrayType::WithChroma, MbPartPredMode::Inter, 47);
//! w.put_ue_k(3, 1000);
//! let bytes = w.finish();
//!
//! let mut r = BitReader::new(&bytes);
//! assert_eq!(r.ue_v(), 42);
//! assert_eq!(r.se_v(), -7);
//! assert_eq!(r.te_v(1), 0);
//! assert_eq!(r.me_v(ChromaArrayType::WithChroma, MbPartPredMode::Inter), 47);
//! assert_eq!(r.ue_k(3), 1000);
//! r.check()?;
//! # Ok::<(), vaco_bitstream::BitstreamError>(())
//! ```
//!
//! # Specification
//!
//! ITU-T H.264 (ISO/IEC 14496-10), clause 9.1 — 9.1 for `ue(v)`, 9.1.1 for
//! `se(v)` and `te(v)`, 9.1.2 and Table 9-4 for `me(v)`. ITU-T H.265 clause 9.2
//! defines the same `ue(v)`/`se(v)` codes and adds no variant this crate lacks.
//! Nothing here was taken from any implementation.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the reader and writer, `vaco-limits` for
//! [`BoundedGolomb`]'s fuel, `vaco-core` for the shared error type. No external
//! runtime dependencies.

#![forbid(unsafe_code)]

mod bounded;
pub mod map;
mod read;
mod tables;
mod write;

pub use bounded::BoundedGolomb;
pub use read::GolombDecode;
pub use tables::{
    ChromaArrayType, MbPartPredMode, cbp_code_num_count, cbp_from_code_num, code_num_from_cbp,
};
pub use write::{GolombEncode, ue_v_cost};

// Re-exported so a caller can name the error type without also depending on
// `vaco-bitstream` directly.
pub use vaco_bitstream::{BitstreamError, Result};
