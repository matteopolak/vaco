//! `vaco-filter-key` — the keying/masking family of
//! `planning/16-filters.md` §4.2's crate table.
//!
//! # Scope, honestly stated
//!
//! The plan's row for this crate is 20 filters, all verified against
//! `ffmpeg -filters`/`ffmpeg -h filter=<name>` (8.1) with no discrepancy
//! from the plan in either direction: `chromakey`, `chromahold`,
//! `colorkey`, `colorhold`, `hsvkey`, `hsvhold`, `lumakey`,
//! `backgroundkey`, `despill`, `premultiply`, `unpremultiply`,
//! `premultiply_dynamic`, `maskedmerge`, `maskedclamp`, `maskedmax`,
//! `maskedmin`, `maskedthreshold`, `maskfun`, `threshold`, `hysteresis`,
//! `floodfill`.
//!
//! Ten are implemented: [`premultiply`] (which registers `premultiply`
//! and `unpremultiply`) and [`maskedmerge`], carried over from a prior
//! (mis-scoped) brief; [`colorkey`], [`colorhold`], [`masked_pick`]
//! (`maskedmax`/`maskedmin`), [`maskedclamp`], [`maskedthreshold`] and
//! [`threshold`], added in this pass.
//!
//! One important correction from measurement: a brief for this crate
//! once said `vaco-filter-framesync` should carry `maskedmerge`'s
//! `masked*` siblings. It does not — `ffmpeg -h filter=maskedclamp` (and
//! `maskedmax`/`maskedmin`/`maskedthreshold`/`threshold`) expose no
//! `eof_action`/`shortest`/`ts_sync_mode` section, the same check
//! `maskedmerge.rs` already used to justify *not* using framesync. All
//! six are lockstep multi-input filters through
//! [`vaco_filter_core::adapt::Paired`], the adapter `vaco-filter-core`
//! added for exactly this shape.
//!
//! Ten are **not** implemented, left honestly rather than guessed:
//!
//! - `chromakey`/`chromahold` (YUV analogues of `colorkey`/`colorhold`):
//!   a probe on a uniform-colour `yuv420p` frame produced two different
//!   alpha values on pixels that share the same subsampled chroma
//!   sample, which means getting this byte-exact needs replicating
//!   whatever chroma-upsampling the reference does internally — not
//!   simply reusing `colorkey`'s RGB-distance formula on Y/U/V.
//! - `hsvkey`/`hsvhold`: not attempted (time budget; the HSV distance
//!   metric — circular hue, linear sat/val, and how the three combine —
//!   was not probed).
//! - `lumakey`: a `threshold=0.5:tolerance=0.1` sweep found the
//!   transparent band is **not symmetric** around the threshold (roughly
//!   `26`-`31` wide below it, `32`-`48` wide above it, in 8-bit terms) —
//!   ruling out the simple `|y - threshold| <= tolerance` band this
//!   crate's other keying filters use. The real formula needs more
//!   probing than this pass had time for.
//! - `backgroundkey`: needs cross-frame history (a running background
//!   estimate), which is a materially different filter shape (temporal,
//!   not per-frame) from everything else in this crate.
//! - `despill`: spill-suppression colour math not attempted.
//! - `maskfun`: a probe on a spatially uniform input returned `0`
//!   (`fill`'s default) for every input value, meaning `low`/`high`/`sum`
//!   describe a *neighbourhood* comparison (consistent with "Create
//!   Mask" being a segmentation-style operation), not the per-pixel
//!   threshold its option names suggest at a glance. Not a per-pixel
//!   arithmetic filter like this crate's other additions; needs its own
//!   investigation.
//! - `maskedthreshold`'s `mode=diff` (falls back to `mode=abs`; see
//!   `maskedthreshold.rs`'s doc for the probe that ruled out a simple
//!   pick-based formula).
//! - `premultiply_dynamic`: single-input, "as needed" — i.e. it decides
//!   whether to premultiply or unpremultiply from some property of the
//!   frame this crate's `Frame` type has no equivalent field for yet, and
//!   guessing that heuristic risked exactly the false-confirmation
//!   failure mode this project's constraints document warns about.
//! - `hysteresis`: unlike its `masked*` siblings, `ffmpeg -h
//!   filter=hysteresis` **does** expose the full `eof_action`/`shortest`/
//!   `repeatlast`/`ts_sync_mode` surface — framesync-shaped, per
//!   measurement — but its connected-component growing algorithm was not
//!   attempted.
//! - `floodfill`: a genuine flood-fill/region-growing algorithm, out of
//!   scope for this pass's time budget.
//!
//! # Shape
//!
//! One module per filter (or filter family), each exposing `pub const
//! DESC: FilterDesc` and a crate-private `create`, dispatched by
//! [`registry::KeyRegistry`]. See [`sample`] for the shared bit-depth
//! access this crate's filters are written against, and [`keying`] for
//! the distance/ramp formula `colorkey`/`colorhold` share.

#![forbid(unsafe_code)]

pub mod sample;

mod common;
mod keying;

pub mod colorhold;
pub mod colorkey;
pub mod masked_pick;
pub mod maskedclamp;
pub mod maskedmerge;
pub mod maskedthreshold;
pub mod premultiply;
pub mod threshold;

pub mod registry;

pub use registry::KeyRegistry;
