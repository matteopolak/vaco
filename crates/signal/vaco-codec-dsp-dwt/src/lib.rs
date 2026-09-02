//! Discrete wavelet transform primitives.
//!
//! [`vc2`] implements SMPTE ST 2042-1 / Dirac's seven reversible integer
//! lifting filters (exact round-trip, `#\[forbid(unsafe_code)\]`); [`cdf97`]
//! implements JPEG 2000's genuinely floating-point, irreversible 9/7
//! filter (round-trip within a stated numeric tolerance, not exact).
//! [`lift`] is the shared 1D lifting-step engine both build on.

#![forbid(unsafe_code)]

pub mod cdf97;
pub mod lift;
pub mod vc2;
