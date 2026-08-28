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
//! C-32a lands §8.8's in-loop deblocking filter (`crate::loopfilter`): lossy
//! VP9 content now decodes bit-exactly rather than within a loop-filter
//! tolerance — see `crate::loopfilter`'s module doc and
//! `docs/codec/vaco-codec-vp9.md`'s Verification table. Profiles 1-3 and
//! threading (the rest of epic #32) remain explicitly out of scope.
#![forbid(unsafe_code)]

pub mod decode;
pub mod framebuf;
pub mod header;
pub mod interpredict;
pub mod loopfilter;
pub mod mvpred;
pub mod predict;
pub mod refframe;
pub mod superframe;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use decode::{VP9_DECODER, Vp9Decoder};
