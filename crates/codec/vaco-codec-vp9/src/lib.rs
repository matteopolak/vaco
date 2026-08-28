//! VP9 video decode (VP9 Bitstream & Decoding Process Specification v0.6).
//!
//! Scope of this package (C-29/C-30): uncompressed + compressed header
//! parsing, superframes, the probability model (defaults and the compressed
//! header's forward update), the partition/mode-info bitstream walk,
//! coefficient token decode, dequantization, inverse transforms and intra
//! prediction — enough to reconstruct real pixels for **key frames**
//! (all-intra).
//!
//! Backward probability adaptation (§8.3/8.4) is *not* implemented: every
//! key frame's `setup_past_independence()` unconditionally resets the
//! probability model before that frame's own forward update runs, so
//! backward adaptation can never affect — or be verified against — any
//! bitstream this crate can fully decode (key frames only). See
//! `crate::header`'s module doc and `planning/TECH-DEBT.md`.
//!
//! Inter prediction (C-31) and the loop filter / profiles 1-3 / threading
//! (epic #32) are explicitly out of scope. A stream whose loop filter level
//! is nonzero will decode every pixel this crate is responsible for
//! bit-exactly and then differ from a reference decoder by the filter's
//! small (single-digit) per-pixel smoothing — expected, not a decode bug.
#![forbid(unsafe_code)]

pub mod decode;
pub mod framebuf;
pub mod header;
pub mod predict;
pub mod superframe;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use decode::{VP9_DECODER, Vp9Decoder};
