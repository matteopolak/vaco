//! Bit-exact inverse transforms for block-based video codecs.
//!
//! H.264 and HEVC specify their inverse transforms *normatively*: a conforming
//! decoder must produce exactly the value the standard's integer arithmetic
//! defines, bit for bit. There is no fidelity trade-off to make and no room for
//! "close enough" — this crate exists to get that arithmetic right once, tested
//! against the standard's own equations, so every codec crate that needs it
//! reuses the same definition (D19).
//!
//! # What is, and is not, in scope
//!
//! Each standard splits "turn a coded block back into a residual" into two
//! processes: **scaling** (dequantisation — multiplies by a level-scale table
//! that depends on QP, picked per-component from the SPS/PPS) and
//! **transformation** (the fixed matrix or butterfly this crate implements).
//! This crate implements only the transformation half — the half that is a
//! pure function of the scaled coefficients, with no quantisation-parameter or
//! slice-header context. A codec crate performs the scaling step (its own
//! `LevelScale` tables, its own QP plumbing) and passes the *already-scaled*
//! coefficients in here.
//!
//! | Module | Standard clause | What it implements |
//! |---|---|---|
//! | [`h264`] | ITU-T H.264 §8.5.12.2, §8.5.13.2, §8.5.10, §8.5.11.1 | 4×4 and 8×8 residual transforms, the `Intra_16x16` luma DC Hadamard, the 2×2/2×4 chroma DC Hadamard |
//! | [`hevc`] | ITU-T H.265 §8.6.4.2 | 4×4–32×32 DCT-II (one shared integer matrix, per eq. 8-317) and the 4×4 DST-VII used for intra luma |
//! | [`mpeg2`] | ISO/IEC 13818-2 Annex A / IEEE 1180 | the classical real-valued 8×8 IDCT, built on [`vaco_tx`]'s existing DCT machinery, to the accuracy the standard requires rather than a normative bit pattern |
//!
//! # Why this is not built on `vaco-tx`'s transforms, except for MPEG-2
//!
//! `vaco-tx` already has a complete, tested DCT-I/II/III family in `f32`, `f64`
//! and a bit-exact `i32` (Q31, `[-1, 1)`) fixed-point contract. It was checked
//! first, per D19, and it is exactly what [`mpeg2`] reuses — MPEG-2 and
//! JPEG-family codecs do not mandate one specific integer algorithm, only an
//! accuracy bound (IEEE 1180 / Annex A), so any sufficiently accurate DCT
//! qualifies and duplicating one would be exactly the mistake D19 exists to
//! catch.
//!
//! H.264's and HEVC's transforms are a different kind of thing. They are
//! **not** the mathematical DCT that `vaco-tx` computes, scaled and quantised
//! — they are deliberately simplified, standard-defined integer approximations
//! with their own arithmetic contract (H.264: plain add/subtract/shift with a
//! single rounding step at the very end; HEVC: exact integer matrix
//! multiplication with one fixed shift *between* the two 1-D passes). Neither
//! contract is `vaco-tx`'s Q31 `[-1, 1)` scaling, and forcing them through it
//! would mean re-deriving the standard's own integer tables from a rescaled
//! Q31 basis and hoping the rounding lines up — more risk for no shared code,
//! since the two transforms do not actually share an implementation once you
//! write them out. What genuinely is shared: [`vaco_tx::fixed::round_shift`]
//! (`(x + 2^(s-1)) >> s`, saturating) is *exactly* the "add a rounding offset,
//! shift, don't panic on adversarial input" operation both standards specify
//! for their final normalisation step, so this crate reuses it there instead
//! of writing a second copy.
//!
//! # Untrusted input
//!
//! Coefficients arrive from an entropy decoder fed attacker-controlled bytes.
//! The standards bound the *legal* range of every intermediate value (and say
//! "the bitstream shall not contain data" that violates it) — but a
//! non-conforming or adversarial bitstream can still hand this crate
//! `i32::MIN` in any coefficient slot. Every function here is defined for
//! *all* `i32` inputs, not just in-range ones:
//!
//! - Interior butterfly stages (H.264) use [`i32::wrapping_add`] /
//!   [`i32::wrapping_sub`]; a plain right shift never panics in Rust. Wrapping
//!   is exactly equivalent to ordinary arithmetic for every input a conforming
//!   encoder produces, and simply cannot panic for the inputs it does not.
//! - The HEVC matrix multiply widens every product to `i64` before summing
//!   (32 terms of `i32::MAX * 90` cannot overflow `i64`), and every point
//!   where a value is narrowed back to `i32` — the inter-pass shift and the
//!   final output — goes through a saturating conversion
//!   ([`vaco_tx::fixed::round_shift`] or [`vaco_tx::fixed::clamp_i32`]).
//!
//! So on out-of-range input the result is a defined, deterministic wrap or
//! saturate — never a panic, never UB, and not a claim of conformance to
//! anything the standard specifies for that input (the standard specifies
//! nothing there either).
//!
//! # Provenance
//!
//! The transform matrices are format-dictated: every conforming decoder
//! contains the identical numeric table, which is textbook merger doctrine
//! (D7/D15) — the table is a fact about the format, not an authorial choice.
//! They were transcribed directly from the primary standard text (ITU-T
//! H.264 (02/2016) §8.5.10–§8.5.13, ITU-T H.265 (08/2021) §8.6.4, obtained
//! from public mirrors, never from `FFmpeg` or any other codec's source per
//! D7/D15) and cross-checked computationally: the reconstructed 32×32 HEVC
//! matrix is verified near-orthogonal (`row_a · row_b ≈ 0` for `a != b`) and
//! its size-4/8/16 subsamples (per eq. 8-317) reduce to the well-known
//! H.264/HEVC 4-point and 8-point cores. See `docs/signal/vaco-codec-dsp-idct.md`
//! for the full derivation.
#![forbid(unsafe_code)]

pub mod h264;
pub mod hevc;
pub mod mpeg2;
mod util;
