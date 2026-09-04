//! Quality metrics for `quality-band`: PSNR and SSIM for video, plus spectral
//! distance for audio.
//!
//! # What is here, and what is not
//!
//! - **PSNR** ([`psnr`]) — the standard definition, per plane or averaged.
//! - **SSIM** ([`ssim`]) — from Wang, Bovik, Sheikh & Simoncelli, *IEEE
//!   Transactions on Image Processing* 13(4), 2004, "Image Quality
//!   Assessment: From Error Visibility to Structural Similarity" — cited by
//!   its bibliographic reference and implemented independently of the GPL
//!   `tests/tiny_ssim.c` fixture.
//! - **A spectral distance for audio** ([`spectral`]) — log-spectral
//!   distance, a standard signal-processing quantity, computed via
//!   `vaco_tx::reference::rdft`, the workspace's O(n²) reference DFT.
//! - **VMAF is omitted.** Dependency policy requires pure Rust without FFI or
//!   `-sys` crates, and no suitable permissively licensed implementation was
//!   identified. The registry does not advertise `"vmaf"`, so requests fail
//!   clearly as an unknown metric instead of using a substitute.
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
