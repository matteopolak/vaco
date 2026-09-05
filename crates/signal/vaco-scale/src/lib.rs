#![forbid(unsafe_code)]
//! Image scaling, pixel-format conversion and colour-space conversion.
//!
//! The `swscale` equivalent, and after the codecs the hottest path in the
//! project.
//!
//! ```
//! use vaco_scale::{ImageSpec, ScaleOptions, Scaler};
//! use vaco_pixfmt::PixFmt;
//!
//! let src = ImageSpec::new(PixFmt::Yuv420p, 1920, 1080);
//! let dst = ImageSpec::new(PixFmt::Rgb24, 1280, 720);
//! let mut scaler = Scaler::new(&src, &dst, &ScaleOptions::default())?;
//! assert!(!scaler.is_noop());
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # The shape, in one paragraph
//!
//! A conversion is lowered once into a plan, and the plan is three stages:
//! resample every channel onto a common grid, apply an affine colour transform,
//! resample every channel onto its destination grid. Format knowledge lives
//! entirely in [`geometry`] and [`rowio`], which turn a `vaco-pixfmt` descriptor
//! into read and write instructions — so the middle of the pipeline never learns
//! whether the picture arrived as `bgr24` or `nv21`, and adding a format costs a
//! table row in `vaco-pixfmt` and no code here at all. That is what turns an
//! *n*×*m* format matrix into *n* + *m* pieces of code.
//!
//! # Adding things
//!
//! | To add | Touch |
//! |---|---|
//! | a pixel format | nothing here — `vaco-pixfmt`'s table, if it is byte-addressable |
//! | a scaling filter | one variant and one formula in [`filter::Kernel`] |
//! | a colour matrix | `vaco-color`, which this crate only quantises |
//! | a fused fast path | [`fast`], which is checked against the general path |
//!
//! # What is implemented
//!
//! Every byte-addressable format in the table: planar and packed `Y'CbCr` at
//! 8–16 bits, every subsampling, the NV and P0xx families, packed RGB including
//! the 16-bit bitfield packings, gray, and alpha. All six resampling kernels.
//! Range conversion, the H.273 matrices with a linear R'G'B' form, chroma
//! subsampling and siting, and ordered dither.
//!
//! **Not implemented, and refused rather than approximated:** palette, Bayer,
//! XYZ, hardware surfaces, tone mapping, and the constant-luminance and
//! `ICtCp`-family matrices. Transfer-characteristic and primaries conversion use
//! a scalar `f64` stage; floating-point pixel formats are reached through integer
//! proxies. `docs/signal/vaco-scale.md` has the full list and the measured
//! fidelity of everything that *is* implemented.
//!
//! # A reference deviation we do not reproduce
//!
//! See [`REFERENCE_CLIP_DIVERGENCE`].

pub mod colour;
pub mod dither;
pub mod exec;
pub mod fast;
pub mod filter;
pub mod geometry;
pub mod options;
pub mod plan;
pub mod rowio;
mod scaler;
pub mod spec;
mod special;

pub use exec::{DstPlane, SrcPlane};
pub use filter::{FilterBank, Kernel};
pub use options::{DitherKind, ScaleOptions, ScalerKind, SwsFlags};
pub use plan::Plan;
pub use scaler::{Scaler, supports_conversion, supports_input, supports_output};
pub use spec::ImageSpec;

/// The one place this crate knowingly differs from the reference binary.
///
/// Converting `Y'CbCr` to `R'G'B'`, the reference emits **0** where the pre-clip
/// value reaches 512 or more, instead of saturating to 255. It is a table
/// overrun and it is reachable from ordinary out-of-gamut chroma — at BT.709
/// limited range, `Y = 225, U = 255` is enough, and the whole `U >= 240` corner
/// of the cube is affected for bright pixels.
///
/// D17 says to reproduce an observable deviation. We do not, for one reason:
/// the value read is whatever lies past the end of a table, so it is a property
/// of one build's memory layout rather than of the algorithm, and committing to
/// it would be committing to something the next reference build may change
/// without notice. We saturate, and `tests/reference_divergence.rs` asserts the
/// deviation still exists so that its disappearance is a test failure rather
/// than a silent change.
///
/// Probe (ffmpeg 8.1):
///
/// ```text
/// printf '\xe1\xff\x80' | ffmpeg -f rawvideo -pix_fmt yuv444p -s 1x1 -i - \
///     -vf scale=in_range=tv:out_range=pc:in_color_matrix=bt709 \
///     -f rawvideo -pix_fmt rgb24 - | xxd
/// ```
pub const REFERENCE_CLIP_DIVERGENCE: &str =
    "ycbcr->rgb pre-clip values >= 512 emit 0 in the reference; we saturate to 255";
