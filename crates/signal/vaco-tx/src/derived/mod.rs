//! Transforms built on top of the complex FFT.
//!
//! Every non-FFT transform in the inventory reduces to a complex FFT plus
//! `O(n)` pre/post processing. That is what lets one crate cover the whole
//! surface without a second algorithm family, and it is why a bug in the FFT
//! shows up in all six kinds at once — which the tests exploit.

pub(crate) mod dct;
pub(crate) mod dct1;
pub(crate) mod dct4;
pub(crate) mod mdct;
pub(crate) mod rdft;
