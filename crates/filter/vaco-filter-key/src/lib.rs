//! `vaco-filter-key` — the keying/masking family of
//! `planning/16-filters.md` §4.2's crate table.
//!
//! # Scope, honestly stated
//!
//! The plan's row for this crate is 20 filters: `chromakey`, `chromahold`,
//! `colorkey`, `colorhold`, `hsvkey`, `hsvhold`, `lumakey`,
//! `backgroundkey`, `despill`, `premultiply`, `unpremultiply`,
//! `premultiply_dynamic`, `maskedmerge`, `maskedclamp`, `maskedmax`,
//! `maskedmin`, `maskedthreshold`, `maskfun`, `threshold`, `hysteresis`,
//! `floodfill`. This pass implements three of them — [`premultiply`]
//! (which registers `premultiply` and `unpremultiply`) and [`maskedmerge`]
//! — carried over from a prior (mis-scoped) brief; the other 17 are not
//! started.
//!
//! # Shape
//!
//! One module per filter (or filter family), each exposing `pub const
//! DESC: FilterDesc` and a crate-private `create`, dispatched by
//! [`registry::KeyRegistry`]. See [`sample`] for the shared bit-depth
//! access this crate's filters are written against.

#![forbid(unsafe_code)]

pub mod sample;

mod common;

pub mod maskedmerge;
pub mod premultiply;

pub mod registry;

pub use registry::KeyRegistry;
