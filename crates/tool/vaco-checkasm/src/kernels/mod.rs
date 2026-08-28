//! Built-in [`crate::Kernel`] adapters, wired to real production kernels.
//!
//! These exist for two reasons: they are what `verify` actually runs, and
//! they are the worked example a new kernel family copies. See each module's
//! doc for why that particular kernel was picked.

pub mod blockdsp;
pub mod fir_mc;
pub mod fmtconvert;
pub mod lpc;
pub mod masked_select;
pub mod mecmp;
pub mod scale_affine;
