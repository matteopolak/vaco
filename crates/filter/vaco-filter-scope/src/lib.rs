//! T3 measurement/visualisation video filters — `planning/16-filters.md`
//! §4.2's `vaco-filter-scope` row, GitHub issue #480 ("FT-4.12g"). Three
//! implemented: `histogram`, `waveform`, `datascope`.
//!
//! # Scoped the way #478 was scoped, before writing code
//!
//! `planning/ASSIGNMENTS.md`, every sibling `FT-4.12*`/`FT-4.11` GitHub
//! issue, and the generated registry were checked before this crate was
//! created, to confirm the row was genuinely unclaimed (posted as a comment
//! on #480 first — see that thread for the full elimination):
//! palette/stack/the overlay family belong to **#111** (a separate, still
//! open issue); the T1-tier `vaco-filter-scale` remainder (`scale2ref`,
//! `colorspace`, `colordetect`, `pixdesctest`, `zoompan`) is a real but
//! separately-unticketed gap, not claimed here; the twelve names in this
//! crate's row are the actual unclaimed T3 remainder.
//!
//! # What is verified versus structural versus not attempted
//!
//! | Filter | Status |
//! |---|---|
//! | [`histogram`] | Framecrc-level for the single-plane case: the bar-height formula (`ceil(count/max * level_height)`) and the `scale_height` gradient are both measured exactly against the reference, at two different count ratios to rule out a rounding coincidence. Multi-plane `stack` display is a documented, unverified extrapolation of the same rule. |
//! | [`waveform`] | Framecrc-level for `mode=column`, `mirror=true` (the default): the accumulation model (`intensity*255` added per hit, into a `255-v`-indexed row) is measured directly, including that it truly accumulates rather than saturating on the first hit. `mode=row` and `mirror=false` are not implemented. |
//! | [`datascope`] | **Structural, not framecrc-level, and permanently so.** The bitmap-font hypothesis (see below) held: the value-grid mechanism, the `passthrough`-format/independent-size link shape, the "canvas is always solid `0`, never a copy of the source" rule, and the source-sample-to-grid-cell mapping (`x`/`y` offsets, raster order) are all measured directly against the reference and implemented for `mode=mono`. But this crate draws with its own independently-sourced font (`crate::font8x8`, Unscii, not the reference's embedded table — a D7 requirement), so no frame containing text can ever match the reference byte-for-byte, no matter how exactly everything else is measured. `mode=color`/`color2`, `axis`, `opacity`, RGB pixel formats and multi-plane `components` are not implemented; see the module's own doc for exactly what was and was not probed. |
//! | `thistogram` | Attempted, not shipped. The reference's output shape (`width x 256`, `width` a temporal window in frames) was measured, but this crate did not resolve the temporal buffering semantics (which column a given frame lands in, how the window scrolls) with enough confidence in the time available to ship a formula rather than a guess. |
//! | `vectorscope` | Attempted, not shipped. Output shape (`256x256`, one axis per selected colour component) was confirmed, but `vectorscope` has no `intensity` option the way `waveform` does, so its accumulation/scaling rule is a different, unmeasured formula — not assumed to be the same as `waveform`'s. |
//! | `oscilloscope`, `pixscope` | Not shipped this pass, but the shared blocker named below (no text-rendering primitive) is now resolved for this filter family: both were confirmed by direct pixel measurement to use the same embedded-bitmap-font mechanism `datascope` now implements (`pixscope`'s statistics overlay renders the identical glyph family). Next in this crate's queue, in the order the coordinator specified — `datascope` was "the simplest test of the glyph path", these are not. |
//!
//! # The bitmap-font hypothesis (resolved: held)
//!
//! `oscilloscope`/`datascope`/`pixscope` were previously reported as
//! blocked on the same prerequisite as `drawtext`: a working
//! `TextRenderer` (fontdb, shaping, glyph cache — GitHub `FT-3.5`/#462),
//! itself blocked on a `rustybuzz` provenance question under D10. The
//! hypothesis tested this pass was that these three do not need that
//! stack at all — that the reference draws them with a small, compiled-in,
//! fixed-width bitmap font instead. It held, checked two ways: `ffmpeg -h
//! filter=datascope`, `-h filter=pixscope` and `-h filter=oscilloscope`
//! (`ffmpeg 8.1`) expose no font/fontfile/fontsize option on any of the
//! three; and pixel-dumping both `datascope`'s and `pixscope`'s rendered
//! output (an all-black and an all-white synthetic source through each)
//! shows crisp, non-antialiased glyphs on an exact pixel grid pitch, with
//! `pixscope`'s statistics-overlay glyphs visually matching `datascope`'s
//! digit family. This is a materially smaller prerequisite than #462 in
//! full, and does not touch that issue's `rustybuzz` question at all — see
//! `crate::font8x8`'s doc for the font itself and why it is not, and
//! cannot be, the reference's own table. A comment to this effect belongs
//! on #462, since it means two separate packages (`drawtext`'s shaped-text
//! stack, and this family's fixed-width blit) were sharing one blocked
//! issue.
//! | `graphmonitor`, `agraphmonitor` | **Not expressible against the current `vaco-filter-core` surface**, checked directly rather than assumed: a filter's `FilterContext` exposes only its own node's pads (`input_link`/`output_link`/`peek_input`, all keyed to the local `NodeLinks`) — there is no API to enumerate other nodes, their links, or their queue depths, which is exactly what these two filters draw. Recorded as `planning/INTERFACE-GAPS.md` gap 22 rather than worked around. |
//! | `ciescope` | Not attempted, and explicitly **not a D7 case**: every `system` value (`ntsc`/`ebu`/`smpte`/`240m`/`apple`/`widergb`/`cie1931`/`hdtv`/`uhdtv`/`dcip3`) names a published international standard's primaries (BT.709, BT.2020, DCI-P3, SMPTE-C, and so on), and the CIE 1931 standard observer data itself is public. The blocker is that reproducing the reference's exact chromaticity-diagram *rendering* (the spectral locus curve's rasterisation, anti-aliasing, gamut-triangle line drawing) is not itself something a published colorimetry standard specifies — it would need extensive black-box probing of the reference's own drawing choices, which this pass's time did not cover. |
//! | `drawgraph`, `adrawgraph` | Not attempted. These plot *frame metadata* rather than pixel data, so they need a working metadata-producing filter upstream to test against at all (gap 11's metadata dictionary and gap 13's console-log channel both closed elsewhere in this tree — the mechanism exists), plus the same line/bar/dot rendering-exactness question `waveform` answered cheaply because it draws single-pixel hits, not connected line segments. Deferred for time, not blocked on an interface gap. |
//!
//! See `docs/filter/vaco-filter-scope.md` for the full framecrc table and
//! every measurement's exact command line.

#![forbid(unsafe_code)]

mod common;
pub mod datascope;
mod font8x8;
pub mod histogram;
pub mod registry;
pub mod waveform;

pub use registry::ScopeRegistry;
