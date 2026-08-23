//! T2 geometry video filters — GitHub issue #470 (`FT-4.7`).
//!
//! # Scope: `planning/16-filters.md` §4.3's row, minus what is already taken
//!
//! The authoritative membership is `planning/16-filters.md`'s
//! `vaco-filter-geometry` row: `crop`, `pad`, `rotate`, `transpose`,
//! `hflip`, `vflip`, `shear`, `scroll`, `il`, `field`, `tile`, `untile`,
//! `framepack`, `fillborders`, `swaprect`, `swapuv`, `shuffleframes`,
//! `shufflepixels`, `shuffleplanes`, `extractplanes`, `alphaextract`,
//! `alphamerge`, `mergeplanes`, `addroi`, `ccrepack`, `lenscorrection`,
//! `perspective`, `stereo3d`, `tiltandshift` — corrected onto this crate by
//! the orchestrator after an earlier version of this doc guessed at
//! membership from `ffmpeg -filters` output alone and got it wrong in both
//! directions (see the two corrections below). `crop`, `pad`, `transpose`,
//! `hflip`, `vflip` are already registered by
//! `crates/filter/vaco-filter-video-geometry` (its T1 set) and are **not**
//! re-registered here (D19; `cargo xtask dup-check` would refuse it).
//! `rotate` turned out to be a second, independent overlap: `cargo xtask
//! gen-registry` refused it because `vaco-filter-video-composite` (issue
//! #465) had already registered it — a genuinely concurrent collision
//! between two independently-briefed agents, discovered mechanically, not
//! by reading a plan. `zoompan`, `scale2ref` and `cropdetect` were briefly
//! registered here in an earlier draft before the orchestrator corrected
//! this crate's scope: the plan actually puts `zoompan`/`scale2ref` in
//! `vaco-filter-scale` and `cropdetect` in `vaco-filter-analysis`; all three
//! were removed.
//!
//! ## Registered here (18)
//!
//! [`scroll`], [`field`], [`il`], [`tile`], [`untile`], [`fillborders`] (4
//! of its 7 modes — see that module), [`swaprect`], [`swapuv`],
//! [`shuffleframes`], [`shuffleplanes`], [`alphaextract`], [`pixelize`]
//! (kept from the earlier draft — not in the plan's row, but a genuine
//! spatial-geometry filter with no other owner found; flagged for the
//! orchestrator to confirm), [`perspective`], [`framepack`],
//! [`mergeplanes`], [`alphamerge`], [`extractplanes`].
//!
//! The last four were declined in an earlier pass of this crate (see the
//! git history of this doc comment) on the theory that a multi-input or
//! multi-output filter needed a capability `vaco-filter-core`'s adapters
//! did not have. `planning/INTERFACE-GAPS.md` gap 10 records why that was
//! wrong and what closed it: `Paired`/`Fanout`
//! (`vaco_filter_core::adapt`), added specifically so this crate did not
//! have to hand-roll the `Activity`-level synchronisation
//! `vaco-filter-audio`'s `amix`/`amerge` do. `framepack` and `mergeplanes`
//! use [`Paired`](vaco_filter_core::adapt::Paired) (`mergeplanes`
//! generalising it past two inputs, since its own input count is fixed at
//! construction from its `map<N>s` options); `extractplanes` uses
//! [`Fanout`](vaco_filter_core::adapt::Fanout). `alphamerge` turned out to
//! need **neither**: measured against the reference (see that module's
//! doc), it carries the full `eof_action`/`shortest`/`repeatlast`/
//! `ts_sync_mode` surface `vaco-filter-framesync`'s `Synced` already
//! provides — the same adapter `vaco-filter-video-composite`'s `overlay`
//! uses — so it is a third data point that the framesync surface, not
//! this crate's registration list, is what decides which adapter a
//! multi-input filter wants.
//!
//! ## Considered and left out, with the reason
//!
//! * **`rotate`** — already registered by `vaco-filter-video-composite`
//!   (issue #465). Not this crate's to own.
//! * **`shear`** — the reference's per-row resampling did not match a
//!   plain shear-about-centre formula at every row of a probe designed to
//!   pin it (two rows fit a `cy=h/2` hypothesis exactly, the other two did
//!   not, under either flooring or truncating). Shipping a formula two of
//!   four measured rows contradict is exactly the mistake `planning/
//!   AGENT-CONSTRAINTS.md`'s "an oracle you wrote shares your misreading"
//!   entry warns about — not implemented.
//! * **`lenscorrection`** — the radial-distortion normalisation convention
//!   (what the `cx`/`cy`/`k1`/`k2` parameters are normalised *by*) was not
//!   pinned down to the same confidence as `rotate`'s trig convention in
//!   the time available. Not implemented rather than guessed.
//! * **`shufflepixels`** — its permutation is seeded by a `seed` option and
//!   almost certainly reproduces a specific PRNG's output sequence bit for
//!   bit (the option table's `seed=-1` default and range up to
//!   `UINT32_MAX` reads exactly like a generator seed, not a hash). This
//!   crate has not measured or identified that generator; implementing
//!   *some* shuffle would compile and pass an identity-permutation test
//!   while producing frames that do not match the reference at any other
//!   seed, which is the "confidently wrong" failure mode this crate is
//!   trying to avoid throughout. Not implemented.
//! * **`addroi`** — needs a region-of-interest concept in the frame side-
//!   data model. `vaco_frame::FrameSideData` has no such variant today, and
//!   adding one means editing `vaco-frame`, a crate this agent does not
//!   own. Flagged rather than worked around.
//! * **`ccrepack`** — CEA-708 closed-caption bitstream repacking; a
//!   specialised bitstream transform with its own byte-packing rules, not
//!   a variation on this crate's affine/plane-copy machinery. Deferred for
//!   time.
//! * **`stereo3d`, `tiltandshift`** — `stereo3d` covers a large matrix of
//!   input/output stereo arrangements including colour computations for
//!   its anaglyph modes; `tiltandshift` accumulates a slit-scan buffer
//!   across the *whole* stream, not a per-frame or bounded-window
//!   operation. Both are substantial standalone algorithms rather than
//!   incremental additions to what this crate already has, and were not
//!   reachable in the time budget. Deferred, not attempted partially.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create(&Instantiate) -> Result<Instance, String>`,
//! dispatched by [`registry::T2GeometryRegistry`] — the same shape as
//! `vaco-filter-audio`, `vaco-filter-plumbing` and the sibling
//! `vaco-filter-video-geometry`. Shared helpers: [`geom`] (byte-level plane
//! addressing, a smaller crate-local copy of the sibling crate's own
//! private module of the same name — see that module's doc for why it is
//! not a shared dependency instead), [`fill`] (solid-colour fill via
//! `vaco-scale`, for limited-range-correct `black`/`color` options),
//! [`warp`] (the 4-point projective transform `perspective` solves) and
//! [`sample`] (the nearest/bilinear per-pixel sampler `perspective` uses).
//! [`geom::blit`] (a second, additive copy of `tile.rs`'s private
//! same-named helper — see that function's doc for why) is what
//! `framepack`'s `sbs`/`tab` layouts place a source frame with.
#![forbid(unsafe_code)]

pub mod alphaextract;
pub mod alphamerge;
pub mod extractplanes;
pub mod field;
pub mod fill;
pub mod fillborders;
pub mod framepack;
mod geom;
pub mod il;
pub mod mergeplanes;
pub mod perspective;
pub mod pixelize;
mod sample;
pub mod scroll;
pub mod shuffleframes;
pub mod shuffleplanes;
pub mod swaprect;
pub mod swapuv;
pub mod tile;
pub mod untile;
mod warp;

pub mod registry;

pub use registry::T2GeometryRegistry;
