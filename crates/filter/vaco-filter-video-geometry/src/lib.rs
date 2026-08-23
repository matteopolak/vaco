//! Video geometry filters: `scale`, `crop`, `pad`, `hflip`, `vflip`,
//! `transpose` — the rotate-free half of plan 16 §4.2's `vaco-filter-geometry`
//! row. `rotate` (arbitrary-angle rotation with interpolation) is explicitly
//! out of scope per this crate's brief and is not registered here.
//!
//! # Naming versus GitHub epic #54's real children
//!
//! This crate's brief named it `vaco-filter-video-geometry` before epic
//! #54's actual three child issues were checked. They turned out to name a
//! *different* split: **#464** (`vaco-filter-crop`) wants exactly `crop`,
//! `pad`, `transpose`, `hflip`, `vflip` — everything in this crate except
//! `scale` — and **#463** (`vaco-filter-scale`) wants `scale` bundled with
//! `format`/`noformat`/`setsar`/`setdar`/`setparams` instead, which live in
//! the sibling `vaco-filter-video-format` crate. Given the brief's explicit
//! licence to keep a different split when one is obviously right ("if a
//! different split is obviously right, take it and say so"), `scale` stays
//! here: it is a video-geometry operation by every naming convention this
//! project otherwise uses, and moving it into the metadata-filter crate
//! would be the more surprising choice for a future reader, even though it
//! means this crate's five non-`scale` filters are a one-to-one match for
//! issue #464 under a different crate name. See this crate's closing report
//! (or `docs/filter/vaco-filter-video-geometry.md`) for the full
//! reconciliation.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create(&Instantiate) -> Result<Instance, String>`, dispatched
//! by [`registry::GeometryRegistry`] — the same shape as `vaco-filter-audio`
//! and `vaco-filter-plumbing`. Every per-filter `Options`/`State` type is
//! `pub(crate)`, for the same `dup-check` reason those crates give.
//!
//! [`fill`] is a shared helper: rather than re-derive RGB→`YCbCr` conversion
//! for `pad`'s border colour (a filter with no colour-conversion machinery of
//! its own), a one-pixel RGB24 tile is built and run through
//! [`vaco_scale::Scaler`] into the destination format. That is what makes
//! `pad`'s default fill land on limited-range black (`Y=16`, not `Y=0`) for
//! `yuv420p` without this crate knowing anything about colour matrices — see
//! `fill`'s doc for the measurement that pins that behaviour.
//!
//! # What is real versus what is structural
//!
//! `crop`'s subsampling-aligned rounding and `scale`'s `w`/`h`
//! omission/`-1`/`-2` handling are measured against `ffmpeg 8.1` (`docs/filter/
//! vaco-filter-video-geometry.md` has the full table) and covered by
//! dedicated tests. `transpose` is exact for symmetric-subsampling and
//! unsubsampled formats (4:4:4, 4:2:0, gray, RGB); asymmetric subsampling
//! (4:2:2 and siblings) is not specially handled — see `transpose.rs`'s doc.
#![forbid(unsafe_code)]

pub mod crop;
pub mod fill;
pub mod flip;
mod geom;
pub mod pad;
pub mod scale;
pub mod transpose;

pub mod registry;

#[cfg(test)]
mod tests_invariants;

pub use registry::GeometryRegistry;
