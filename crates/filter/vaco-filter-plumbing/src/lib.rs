//! T1 graph plumbing, cutting/joining and sources/sinks: `split`/`asplit`,
//! `null`/`anull`, `copy`/`acopy`, `setpts`/`asetpts`, `settb`/`asettb`,
//! `select`/`aselect`, `trim`/`atrim`, `concat`, `nullsrc`/`anullsrc`,
//! `nullsink`/`anullsink`, `color` — 20 of the 24 filters FT-4.3 (GitHub
//! #467) names.
//!
//! # The other four: `buffer`/`abuffer`/`buffersink`/`abuffersink`
//!
//! Plan 16 §1.13 puts these **in `vaco-filter-core`**, not in a leaf crate,
//! because they need privileged access to link internals — a buffer source
//! pushes directly into the link queue and a buffer sink holds frames with
//! no downstream. The real `vaco-filter-core` (not the plan's draft) already
//! ships this as the graph's own I/O API: [`vaco_filter_core::Graph::add_source`]
//! / `add_sink` / `send` / `recv` / `close_source` / `source_wants` /
//! `sink_format`, exercised throughout this crate's own tests. There is
//! nothing left for a leaf crate to implement — a `Filter` impl here would be
//! a second, unprivileged, functionally-dead mechanism under the same names.
//! Mapping the DSL spellings `buffer`/`abuffer`/`buffersink`/`abuffersink`
//! onto that native API is `vaco-filter-graph` or `vaco-cli-core`'s job, both
//! outside this crate's scope. Left open in GitHub #467; see this crate's
//! closing comment.
//!
//! # Shape
//!
//! One module per filter (or per closely related pair — `null`+`anull`+
//! `copy`+`acopy` in `passthrough.rs`, the two `setpts` variants together, and
//! so on), each exposing `pub const DESC: FilterDesc` and a crate-private
//! `create`. [`registry::PlumbingRegistry`] dispatches by name.
//!
//! Every per-filter `Options`/`State`/`Mode` type is `pub(crate)` — see
//! `vaco-filter-audio`'s crate doc for why that, not a `dup-check` allowlist
//! row, is the right response to ~35 filters converging on the same short
//! type names.
#![forbid(unsafe_code)]

pub mod color;
pub mod concat;
pub mod nullsink;
pub mod nullsrc;
pub mod passthrough;
pub mod select;
pub mod setpts;
pub mod settb;
pub mod split;
pub mod trim;

pub mod registry;

pub use registry::PlumbingRegistry;
