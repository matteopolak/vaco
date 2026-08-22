//! Re-export of `vaco_core::dict`.
//!
//! This module briefly carried a local implementation, written when `vaco-core`
//! was frozen but unimplemented. `vaco-core` now owns it; this exists only so
//! call sites need not change.
pub use vaco_core::dict::*;
