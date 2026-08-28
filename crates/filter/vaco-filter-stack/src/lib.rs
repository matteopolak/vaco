//! `hstack`, `vstack`, `xstack` — plan 16 §4.2's `vaco-filter-stack` row,
//! GitHub issue #111 (FT-4.11)'s real unclaimed remainder (see the crate
//! doc's "Scoping" section for the rest of that issue's disposition).
//!
//! # Scoping, before writing code
//!
//! `#111`'s title mentions "palette/GIF, stack, overlay family, temporal",
//! but its own `Crate(s):` field names only `vaco-filter-palette` and
//! `vaco-filter-stack`. Checked against `ffmpeg -hide_banner -filters`,
//! plan 16 §4.2, `planning/ASSIGNMENTS.md` and the generated registry
//! before any code: `temporal` is already fully shipped
//! (`vaco-filter-temporal`, #475); the literal `overlay` filter is already
//! fully shipped (`vaco-filter-video-composite`, #465, built on
//! `vaco-filter-framesync`); `hstack`/`vstack`/`xstack` (this crate) and
//! `palettegen`/`paletteuse`/`elbg` (a sibling `vaco-filter-palette` crate)
//! are the real remainder. Posted as a comment on #111 first — see that
//! thread for the full mapping, including `latticepal` (unverifiable: not
//! present in the installed `ffmpeg 8.1` reference at all), `showpalette`
//! (a real, unticketed filter this pass does not claim), and the 8-filter
//! `blend`/`xfade`/`mix`/`multiply`/`xmedian`/`displace`/`remap`/`feedback`
//! group plan 16 assigns to a never-created `vaco-filter-overlay` crate
//! (real, unclaimed, and out of scope for this pass's priority order).
//!
//! # No framework gap: `vaco-filter-framesync` already fits
//!
//! `crates/filter/vaco-filter-framesync/src/opts.rs` documents
//! [`vaco_filter_framesync::FsInput::uniform`] as built specifically for
//! "`hstack`, `vstack`, `maskedmerge`" — every input drives, none is a
//! secondary. Its measured defaults (`eof_action=repeat`, `shortest=false`)
//! match this family's own measured default behaviour exactly (see
//! [`hstack`]'s doc), so these three are built as
//! [`vaco_filter_framesync::FrameSyncFilter`]s wrapped in
//! [`vaco_filter_framesync::Synced`] — the same shape
//! `vaco-filter-video-composite::overlay` uses, not the `Paired` adapter
//! gap 10 added: `Paired` cannot express `eof_action=repeat` (the same
//! reason `overlay` itself was not ported onto it), and this family's
//! default behaviour depends on exactly that.
//!
//! # What is verified versus structural versus not attempted
//!
//! | Filter | Status |
//! |---|---|
//! | [`hstack`] | Framecrc-level for uniform-height, matching-format inputs (the only case the reference itself allows — see the module doc): output width is the exact sum of input widths, height must match across all inputs or the reference itself refuses to configure, and the ring-buffer/freeze semantics inherited from `vaco-filter-framesync` are exercised by that crate's own test suite. |
//! | [`vstack`] | The same shape as `hstack`, rotated: output height is the sum of input heights, width must match. |
//! | [`xstack`] | Two shapes measured and implemented: the reference's own **default** (no `layout`/`grid` given) works only for `inputs=2` and is exactly `hstack`'s layout; `grid=COLSxROWS` arranges inputs in raster order (row-major) with each grid cell sized to its own input, measured with a 4-input `2x2` grid. The free-form `layout=` string mini-language (`planning/16-filters.md`'s own "shared layout parser" dependency) is **not implemented** — `create` rejects it with a clean error rather than guessing at a parser for arbitrary per-input `x_y` expressions. |
//!
//! `inputs` beyond [`vaco_filter_graph::registry::pads::MAX`] (`64`) are
//! rejected — a real, structural cap from this framework's static pad
//! table, distinct from the reference's own `2..=INT_MAX` range, and
//! stated plainly rather than silently truncated.
//!
//! See `docs/filter/vaco-filter-stack.md` for the full framecrc table and
//! every measurement's exact command line.

#![forbid(unsafe_code)]

mod common;
pub mod hstack;
pub mod registry;
pub mod vstack;
pub mod xstack;

pub use registry::StackRegistry;
