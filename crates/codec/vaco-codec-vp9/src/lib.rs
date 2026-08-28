//! VP9 video decode (VP9 Bitstream & Decoding Process Specification v0.6).
//!
//! Scope of this package: C-29/C-30 (header parsing, superframes, the
//! probability model, the partition/mode-info bitstream walk, coefficient
//! token decode, dequantization, inverse transforms and intra prediction)
//! plus C-31 (inter prediction: reference-frame management, motion-vector
//! prediction, compound prediction, sub-pel interpolation) — a real decoder
//! for both key and inter frames.
//!
//! Backward probability adaptation (§8.3/8.4) is *not* implemented: see
//! `crate::header`'s module doc and `planning/TECH-DEBT.md` for what that
//! means for the frames this crate does and does not fully verify.
//!
//! The loop filter / profiles 1-3 / threading (epic #32) are explicitly out
//! of scope. A stream whose loop filter level is nonzero will decode every
//! pixel this crate is responsible for bit-exactly and then differ from a
//! reference decoder by the filter's small (single-digit) per-pixel
//! smoothing — expected, not a decode bug.
#![forbid(unsafe_code)]

pub mod decode;
pub mod framebuf;
pub mod header;
pub mod interpredict;
pub mod mvpred;
pub mod predict;
pub mod refframe;
pub mod superframe;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use decode::{VP9_DECODER, Vp9Decoder};
