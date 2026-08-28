//! Built-in [`crate::Kernel`] adapters, wired to real production kernels.
//!
//! These exist for two reasons: they are what `verify` actually runs, and
//! they are the worked example a new kernel family copies. See each module's
//! doc for why that particular kernel was picked.

pub mod fir_mc;
pub mod masked_select;
pub mod scale_affine;
