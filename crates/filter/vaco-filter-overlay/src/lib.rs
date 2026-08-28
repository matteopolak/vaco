//! `blend`, `xfade`, `mix`, `multiply`, `xmedian`, `displace`, `remap`,
//! `feedback` — plan 16 §4.2's `vaco-filter-overlay` row, the real
//! unclaimed remainder GitHub issue #111 (FT-4.11)'s title meant by
//! "overlay family" (not the literal `overlay` filter, already shipped in
//! `vaco-filter-video-composite`, #465 — see that crate and this one's own
//! scoping comment on #111 for the full mapping).
//!
//! # Multi-input framework fit, checked per filter rather than assumed
//!
//! `vaco-filter-stack` established that `Paired` (gap 10) and
//! `Synced`/`FrameSyncFilter` are different, non-interchangeable shapes —
//! `Paired` cannot express `eof_action=repeat`. Each filter here was
//! checked against `ffmpeg -h filter=<name>` for the actual measured
//! option surface before picking one:
//!
//! | Filter | Arity | Framesync surface? | Adapter |
//! |---|---|---|---|
//! | `blend` | 2 (fixed) | Full (`eof_action`/`shortest`/`repeatlast`/`ts_sync_mode`) | `Synced`/`FrameSyncFilter`, `FsInput::dual` |
//! | `multiply` | 2 (fixed) | None | `Paired` |
//! | `mix` | N (`2..=32767`, capped here at [`vaco_filter_graph::registry::pads::MAX`]) | None (its own `duration=longest/shortest/first`) | `Synced`/`FrameSyncFilter`, hand-built per-input roles |
//! | `xmedian` | N (`3..=255`, same cap) | Full | `Synced`/`FrameSyncFilter`, `FsInput::uniform` |
//! | `xfade` | 2 (fixed) | None (its own `duration`/`offset` timing) | `Synced`/`FrameSyncFilter`, `FsInput::dual` |
//! | `feedback` | 2 in, **2 out** | — | **No existing adapter fits.** See below. |
//!
//! `feedback` is `VV->VV` — two inputs *and* two outputs, one of which
//! feeds back into the graph as the filter's own next-frame input. Every
//! adapter in `vaco-filter-core::adapt` was checked: `Simple`/`Blocked`
//! are 1-in-1-out, `Sourced` is 0-in-1-out, `Fanout` is 1-in-N-out, `Paired`
//! is N-in-**1**-out. None is 2-in-2-out. This is a real, structural gap —
//! filed as `planning/INTERFACE-GAPS.md` gap 23 rather than worked around
//! inside this crate, per the standing rule. `feedback` is not implemented.
//!
//! # What is verified versus structural versus not attempted
//!
//! | Filter | Status |
//! |---|---|
//! | [`blend`] | **18 of ~30 named blend-mode formulas measured and framecrc-exact** (each pinned against a full `0..=255` gradient sweep at a fixed second operand, cross-checked against a second operand value where the sample could be ambiguous): `normal`, `multiply`, `screen`, `darken`, `lighten`, `average`, `difference`, `subtract`, `addition`, `burn`, `dodge`, `exclusion`, `and`, `or`, `xor`, `negation`, `addition128`/`grainmerge`, `difference128`/`grainextract`. Per-component (`c0..c3`) modes and `opacity` (measured: `out = floor(a + opacity*(mode(a,b)-a))`) are implemented generically, so they apply to every mode above. **Not implemented**: `hardlight`, `overlay`, `softlight`, `hardmix`, `linearlight`, `vividlight`, `pinlight`, `reflect`, `phoenix`, `extremity`, `freeze`, `glow`, `heat`, `softdifference`, `geometric`, `harmonic`, `bleach`, `stain`, `interpolate`, `hardoverlay`, `multiply128` — their raw output curves were captured but do not cleanly match a formula this pass could confirm at more than one point; guessing was declined. `c0_expr`/`all_expr` (arbitrary per-component expressions) are not implemented. |
//! | [`multiply`] | **Structural, not confirmed exact.** Measured against a `0..=255` gradient at a fixed second operand: the shape is a normalized `a*b*scale + offset` (default `scale=1`, `offset=0.5`), and 5 of 6 sample points at `offset=0` match `round(a*b/255)` exactly, but the extremum (`a=255, b=150` gives `149`, not the expected `150`) does not — plausibly floating-point rounding baked into the reference's own implementation, not reproduced bit-for-bit here. Documented rather than hidden. |
//! | [`mix`] | **Framecrc-exact for `duration=longest` (default) and `duration=shortest`.** Weighted sum (`weights`, default `"1 1"`) with `scale` (default `0` meaning "normalize by the sum of weights" — measured, not assumed), built on hand-constructed `FsInput` roles rather than `apply_opts`'s truth table, since `duration=first` (stop when input `0` ends, regardless of the others) has no equivalent in `vaco-filter-framesync`'s built-in `eof_action` vocabulary. `duration=first` itself is **not implemented**. |
//! | [`xmedian`] | **Framecrc-exact for `percentile=0.5` (the default, the true median)** across an odd input count. Other percentiles and even input counts (which need a documented interpolation rule between the two central values) are **not implemented**. |
//! | [`xfade`] | **`transition=fade` is framecrc-exact** (`out = floor(a + progress*(b-a))`, `progress = clamp((pts-offset)/duration, 0, 1)`, pinned at 10 points across a 10-frame transition window). The other 57 named transitions and `expr` (custom) are **not attempted** — each is its own per-pixel geometry formula. |
//! | `displace`, `remap` | **Scoped, not implemented.** Both are `VVV->V`, a fixed 3-input shape with no framesync surface — `Paired` (already proven to generalise past 2 inputs by `vaco-filter-geometry::mergeplanes`) fits architecturally. What was not measured in the time available: the exact displacement-map encoding (`displace`'s two map planes' zero-point and scale) and the exact map-to-source-coordinate convention (`remap`'s `x`/`y` map semantics, and its `fill`/edge behaviour) — implementing either without that would be a guess. |
//! | `feedback` | **Not implementable against the current framework surface** — see above. `planning/INTERFACE-GAPS.md` gap 23. |
//!
//! See `docs/filter/vaco-filter-overlay.md` for the full framecrc table,
//! every raw measurement, and the exact command lines.

#![forbid(unsafe_code)]

mod common;
pub mod blend;
pub mod mix;
pub mod multiply;
pub mod registry;
pub mod xfade;
pub mod xmedian;

pub use registry::OverlayRegistry;
