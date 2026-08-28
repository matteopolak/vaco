//! Quality metrics for C10 `quality-band` (work package X-04, `#253`):
//! PSNR and SSIM for video, a spectral distance for audio.
//!
//! # What is here, and what is not
//!
//! - **PSNR** ([`psnr`]) — the standard definition, per plane or averaged.
//! - **SSIM** ([`ssim`]) — from Wang, Bovik, Sheikh & Simoncelli, *IEEE
//!   Transactions on Image Processing* 13(4), 2004, "Image Quality
//!   Assessment: From Error Visibility to Structural Similarity" — cited by
//!   its bibliographic reference, never transcribed from `tests/tiny_ssim.c`
//!   (GPL, on the project's hard do-not-reuse list per plan 13 §0.1/§1.11.2).
//! - **A spectral distance for audio** ([`spectral`]) — log-spectral
//!   distance, a standard signal-processing quantity, computed via
//!   `vaco_tx::reference::rdft` (this project's own O(n²) DFT oracle,
//!   already reviewed as the right tool for "downstream conformance work"
//!   in that module's own docs).
//! - **VMAF is cut.** D10's dependency gates require pure Rust, zero FFI, no
//!   `-sys` crate. No permissively-licensed pure-Rust VMAF implementation
//!   was found during this session — the reference implementation is
//!   `libvmaf`, a C library, and every Rust wrapper on crates.io as of this
//!   session binds it via FFI rather than reimplementing it. Reimplementing
//!   VMAF's own trained SVM model from its paper is out of scope for this
//!   pass. This is a named cut, not a silent one: `docs/vaco-conformance.md`
//!   should record the same when this lands, and `Registry::names` simply
//!   never lists a `"vmaf"` entry, so a case that asks for one gets a clear
//!   "unknown metric" rather than a silent substitute.
//!
//! # How these plug into the harness
//!
//! Both implement [`crate::compare::quality::Metric`] and are registered by
//! [`crate::compare::quality::default_registry`]. A [`Pair`](crate::compare::Pair)
//! that carries decoded [`crate::compare::quality::Signal`]s (via
//! `Pair::with_signals`) makes `compare::quality::compare` actually measure
//! rather than skip — see that function's own docs for what still has to
//! supply those signals (decoding a bitstream to raw samples, which is a
//! separate integration this crate does not do yet).

pub mod psnr;
pub mod sample;
pub mod spectral;
pub mod ssim;

pub use psnr::Psnr;
pub use spectral::SpectralDistance;
pub use ssim::Ssim;
