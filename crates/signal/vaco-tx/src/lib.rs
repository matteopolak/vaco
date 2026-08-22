//! FFT, MDCT, RDFT, DCT and DST in `f32`, `f64` and bit-exact `i32`.
//!
//! Every transform-coded audio codec — AAC, AC-3, MP3, Vorbis, Opus/CELT, DTS,
//! ATRAC — is a windowing stage, a quantiser and one of these transforms. This
//! crate is the transform, and nothing else: it contains no codec knowledge, no
//! windows and no I/O.
//!
//! ```
//! use std::sync::Arc;
//! use vaco_tx::{Plan, Tx};
//!
//! # fn main() -> vaco_core::Result<()> {
//! let plan = Plan::<f32>::fft(1024, false)?;
//! let mut tx = Tx::new(Arc::clone(&plan));
//!
//! let input: Vec<f32> = (0..2048).map(|i| (i as f32).sin()).collect();
//! let mut output = vec![0.0f32; plan.output_len()];
//! tx.execute(&mut output, &input);
//! # Ok(())
//! # }
//! ```
//!
//! # The two things worth knowing before using it
//!
//! **[`Plan::new`] is total over transform lengths.** For [`TxKind::Fft`] every
//! `len` from 1 to 2^24 succeeds. Smooth lengths run mixed-radix Stockham;
//! coprime composites run Good–Thomas; primes run Rader; anything left runs
//! Bluestein. A codec asks for the length its bitstream specifies and gets a
//! plan — it never has to carry a fallback for "the transform crate cannot do
//! this size". [`Plan::describe`] reports which rule fired.
//!
//! **`i32` is a specification, not an optimisation.** Several codecs define
//! fixed-point decoding normatively and are conformance-tested against exact
//! output. The Q31 arithmetic contract — round-half-up, saturate, divide by the
//! radix at every stage — is stated in [`fixed`], pinned by golden vectors, and
//! versioned with the crate. Changing it is a codec-affecting decision.
//! `docs/signal/vaco-tx.md` states the contract in full.
//!
//! # Layout and conventions
//!
//! | Topic | Convention |
//! |---|---|
//! | Complex data at the API | interleaved `[re, im, re, im, …]` slices of `T`. No public complex type, ours or anyone's. |
//! | Complex data internally | **split-complex**: separate `re`/`im` arrays, so a complex multiply is lanewise with zero shuffles. Converted once in, once out. |
//! | Float scaling | unnormalised. `inverse(forward(x)) = n·x`. |
//! | Fixed scaling | each transform is divided by a documented constant — `n` for FFT/RDFT/MDCT/DCT, `2(n∓1)` for DCT-I/DST-I. `inverse(forward(x)) = x/n`. |
//! | `scale` | applied to the output. `T::IDENTITY_SCALE` is compared at plan time and the pass is dropped, so the default costs nothing. |
//!
//! # Reproducibility
//!
//! | Path | Class | What is guaranteed |
//! |---|---|---|
//! | `i32`, every kind and size | **A** | bit-identical across architecture, lane width and build profile. There is no SIMD path for `i32`, so this is structural rather than tested-for. |
//! | `f32`/`f64` | **C** | relative RMS error `≤ 2^-20` (`f32`) and `2^-48` (`f64`) against an `f64` direct evaluation, at every supported size. |
//!
//! The `f32` SIMD and scalar paths do in fact agree bit for bit today, because
//! they are the same source monomorphised twice and neither uses FMA — the
//! differential tests assert exact equality. That is a stronger property than
//! Class C promises and it is deliberately *not* part of the contract: it must
//! not become something a codec depends on.
//!
//! # Module map
//!
//! | Module | Role |
//! |---|---|
//! | [`fixed`] | the normative Q31 primitives |
//! | [`reference`] | direct `O(n²)` transform definitions, for tests and conformance work |
#![forbid(unsafe_code)]
// `#[inline(always)]` is not a tuning knob here, for the same reason it is not
// one in `vaco-simd`: it is how the dispatched level's target-feature context
// reaches a kernel body, and it is how the const-generic butterflies collapse
// into straight-line code. A kernel that fails to inline is still correct and
// silently slow — invisible to every test in the suite. Turned off once, at the
// root, rather than annotated onto forty functions.
#![allow(
    clippy::inline_always,
    reason = "mandatory for target-feature propagation and for const-generic unrolling"
)]
// `n`, `m`, `k`, `p`, `q`, `r`, `re`, `im` are this domain's own names — they are
// the symbols in Cooley & Tukey, in Rader, in every reference this crate cites.
// Renaming them to `transform_length` and `sub_transform_index` would make the
// code harder to check against the papers, not easier.
#![allow(
    clippy::many_single_char_names,
    reason = "the transform literature's own notation; renaming would obscure the derivations"
)]

mod butterfly;
mod derived;
mod engine;
mod factor;
pub mod fixed;
mod num;
mod plan;
pub mod reference;
mod simd;

pub use num::TxSample;
pub use plan::{Decomposition, Direction, Plan, PlanDescription, Tx, TxFlags, TxKind};

// Implementation detail of the generic kernels. `pub` so that `TxSample`'s
// supertrait chain and the SIMD dispatch hook resolve; sealed, so nothing
// outside this crate can implement them.
#[doc(hidden)]
pub use num::{Arith, Lane, StageView};
