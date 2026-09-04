//! T3 measurement and visualisation filters from `planning/16-filters.md`
//! §4.2's `vaco-filter-scope` row.
//!
//! All twelve names in the row are implemented and exposed through
//! [`ScopeRegistry`]: `histogram`, `thistogram`, `waveform`, `vectorscope`,
//! `oscilloscope`, `datascope`, `pixscope`, `ciescope`, `graphmonitor`,
//! `agraphmonitor`, `drawgraph`, and `adrawgraph`.
//!
//! Each module records its black-box reference measurements and its remaining
//! rendering limits. Several data-driven paths are byte-exact; text-rendering
//! filters use the independently sourced Unscii bitmap font; `oscilloscope`
//! and `ciescope` retain documented rasterisation residuals. The public
//! implementation and verification summary is in
//! `docs/filter/vaco-filter-scope.md`.

#![forbid(unsafe_code)]

pub mod ciescope;
mod common;
pub mod datascope;
pub mod drawgraph;
mod font8x8;
pub mod graphmonitor;
pub mod histogram;
pub mod oscilloscope;
pub mod pixscope;
pub mod registry;
pub mod thistogram;
pub mod vectorscope;
pub mod waveform;

pub use registry::ScopeRegistry;
