//! Audio filters: `aresample`, `aformat`, `volume`, `amix`, `amerge`,
//! `channelmap`, `channelsplit`, `join`, `pan`, `asetnsamples`, `asetrate`.
//! Plus `amultiply` and `adecorrelate`, the two `vaco-filter-amix`-row
//! filters plan 16 §4.3 lists — see each module's own doc for what is
//! measured (`amultiply`, bit-exact) versus structural (`adecorrelate`,
//! which cannot be measured at all; see its doc for why).
//!
//! Built against `vaco-filter-core` (the `Filter` trait,
//! the `Simple`/`AudioFilter`/`Sourced` adapters and format negotiation) and
//! `vaco-filter-graph` (the [`FilterRegistry`](vaco_filter_graph::FilterRegistry)
//! trait a DSL builder uses to turn a parsed `name=args` into an
//! [`Instance`](vaco_filter_graph::Instance)).
//!
//! # Shape
//!
//! One module per filter, each exposing:
//!
//! * `pub const DESC: FilterDesc` — the static descriptor, named by this
//!   crate's `vaco-component.toml` fragments so `vaco-registry` can list it.
//! * `pub(crate) fn create(req: &Instantiate<'_>) -> Result<Instance, String>` —
//!   parses the filter's arguments and builds a runnable instance.
//!
//! Every per-filter `Options`/`State` type is `pub(crate)`: with ~35 T1/T2
//! audio filters converging on names like `Options` and `State`, making them
//! crate-private both hides an implementation detail that has no business
//! being public and — since `cargo xtask dup-check` scans literal `pub struct`
//! / `pub enum` lines — keeps this crate off the D19 duplicate-name ledger
//! without needing an allowlist row for a name eleven different modules want.
//!
//! [`registry::AudioRegistry`] aggregates all eleven `create` functions behind
//! one [`FilterRegistry`](vaco_filter_graph::FilterRegistry) impl.
//!
//! # What is real versus what is structural
//!
//! Every filter here does *something* correct on its documented common path.
//! The ones with genuine plumbing risk — `amix`'s uneven input endings,
//! `pan`'s expression grammar, `asetnsamples`'s exact re-blocking — got the
//! most attention; see `docs/filter/vaco-filter-audio.md` for exactly what was
//! exercised versus left as a structurally-present but lightly-tested path.
#![forbid(unsafe_code)]

pub mod adecorrelate;
pub mod aformat;
pub mod amerge;
pub mod amix;
pub mod amultiply;
pub mod aresample;
pub mod asetnsamples;
pub mod asetrate;
pub mod channelmap;
pub mod channelsplit;
pub mod join;
pub mod pan;
mod sample;
pub mod volume;

pub mod registry;

pub use registry::AudioRegistry;
