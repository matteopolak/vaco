//! Plan 16 §4.4's multimedia/T1-plumbing row (`vaco-filter-mm`, GitHub
//! #479, FT-4.12f): `concat`, `select`/`aselect`, `segment`/`asegment`,
//! `streamselect`/`astreamselect`, `trim`/`atrim`, `loop`/`aloop`,
//! `reverse`/`areverse`, `cue`/`acue`, `realtime`/`arealtime`,
//! `latency`/`alatency`, `bench`/`abench`, `perms`/`aperms`,
//! `metadata`/`ametadata`, `sidedata`/`asidedata`, `sendcmd`/`asendcmd`,
//! `split`/`asplit`, `setpts`/`asetpts`, `null`/`anull`,
//! `interleave`/`ainterleave` — 37 of the row's 41 names — plus the
//! graph-plumbing/source-sink filters this crate carried under its
//! previous name (`vaco-filter-plumbing`, FT-4.3, GitHub #467):
//! `copy`/`acopy`, `settb`/`asettb`, `nullsrc`/`anullsrc`,
//! `nullsink`/`anullsink`, `color`.
//!
//! `color`/`nullsrc`/`nullsink`/`anullsrc` are **not** in the §4.4 row — the
//! plan places them in `vaco-filter-source`/`vaco-filter-asource` instead.
//! Left here rather than moved: those two crates exist but do not yet
//! register these four names, this crate does not own them under the
//! single-writer rule, and deleting working filters with nothing to replace
//! them would regress the CLI. See `planning/FILTER-CRATE-DIVERGENCE.md`.
//!
//! Not landed from the §4.4 row, with reasons: `avsynctest` (a synthetic
//! A/V generator disproportionate to this row's plumbing character — the
//! `vaco-filter-aeffects` precedent for `surround`/`headphone`);
//! `cmdsocket`/`acmdsocket` (need a real listening socket, out of a filter's
//! normal scope); `aeval` (deferred for time — the reference's own
//! documentation calls it "slow… for faster processing use a dedicated
//! filter", the lowest-priority item left when the row's time budget ran
//! out). `sendcmd`/`asendcmd` implement the command-script grammar, the
//! enter/leave edge detection and per-frame passthrough in full, but cannot
//! dispatch a parsed command to another named filter instance — that needs
//! a graph-level "send this node a command" API `vaco-filter-core` does not
//! expose yet; see `sendcmd`'s module doc.
//!
//! `select`'s `scene` variable and its `ceil`-not-`round` multi-output
//! routing (a real bug, found by reading the reference's own documentation
//! and confirmed against `ffmpeg 8.1`) landed alongside `metadata`, since
//! both were already this crate's before the row's other filters arrived.
//! `streamselect`'s huge-`inputs=` allocation (found by fuzzing, fixed by
//! reordering a pad-count check ahead of the allocation it was meant to
//! bound) is the row's one fuzzing finding so far — see `streamselect`'s
//! module doc and `docs/filter/vaco-filter-mm.md`'s exactness table for the
//! full account of every filter's fidelity.
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
//! `create`. [`registry::MmRegistry`] dispatches by name.
//!
//! Every per-filter `Options`/`State`/`Mode` type is `pub(crate)` — see
//! `vaco-filter-audio`'s crate doc for why that, not a `dup-check` allowlist
//! row, is the right response to ~35 filters converging on the same short
//! type names.
#![forbid(unsafe_code)]

pub mod color;
pub mod concat;
pub mod interleave;
pub mod looping;
pub mod metadata;
pub mod misc;
pub mod nullsink;
pub mod nullsrc;
pub mod passthrough;
pub mod reverse;
pub mod segment;
pub mod select;
pub mod sendcmd;
pub mod setpts;
pub mod settb;
pub mod split;
pub mod streamselect;
pub mod trim;

pub mod registry;

pub use registry::MmRegistry;
