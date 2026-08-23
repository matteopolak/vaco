//! Audio test-signal and FIR-coefficient generator sources: plan 16 §4.3's
//! `vaco-filter-asource` row.
//!
//! FT-4.13a (GitHub #481). `anullsrc` and `anullsink` in that row are
//! **not** registered here — they already ship from `vaco-filter-plumbing`
//! (FT-4.3 / GitHub #467). Re-registering either name would be a second,
//! competing `[[component]]` row for the same `ctor`, which `cargo xtask
//! dup-check` exists to catch.
//!
//! `afirsrc` and `afireqsrc` are not implemented: both need the
//! frequency-sampling FIR design method (interpolate a frequency response
//! onto a bin grid, inverse-FFT, window), which needs `vaco-tx` wired up
//! correctly (bin layout, conjugate symmetry for a real output, the
//! circular shift for linear phase) — real signal-processing surface area
//! this crate did not have time to implement and verify correctly. See
//! this crate's closing report (GitHub #481) and
//! `docs/filter/vaco-filter-asource.md` for what that would take.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create`, dispatched by [`registry::AsourceRegistry`].
//! Same pattern as every sibling filter crate.
#![forbid(unsafe_code)]
// `n`, `t`, `x` are this domain's own names -- sample index, time in
// seconds, and the FIR/window design literature's own variable. Same
// reasoning `vaco-tx` gives for its own single-letter names.
#![allow(
    clippy::many_single_char_names,
    reason = "sample index, time, and DSP/window design literature's own variable names"
)]

pub mod aevalsrc;
pub mod afdelaysrc;
pub mod anoisesrc;
pub mod hilbert;
pub mod sinc;
pub mod sine;

mod rng;
mod window;

pub mod registry;

pub use registry::AsourceRegistry;
