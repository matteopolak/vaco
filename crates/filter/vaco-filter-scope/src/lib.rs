//! T3 measurement/visualisation video filters — `planning/16-filters.md`
//! §4.2's `vaco-filter-scope` row, GitHub issue #480 ("FT-4.12g"). Four
//! implemented: `histogram`, `waveform`, `datascope`, `thistogram`.
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
//! | [`thistogram`] | **Framecrc-level for `slide=replace` (the default) and `slide=frame`.** No text is drawn — every row/column is data — so unlike `datascope` this one has no permanent verification ceiling. The reference turned out to be *stateful*: a persistent `width x 256` canvas that gets exactly one new column per frame, confirmed with a 4-frame sequence. The per-column intensity formula (`round(count/max*255)`, checked against three ratios including an exact `0.5` tie, to rule out `histogram`'s own `ceil` rule as well as plain truncation) and both measured `slide` ring-buffer behaviours (`replace` overwrites only its own column; `frame` clears the whole canvas on wraparound) are exact. `slide=scroll`/`rscroll`/`picture` are rejected at creation with a clean error rather than guessed at. |
//! | `vectorscope` | **Partially cracked, not shipped.** The coordinate mapping is now fully measured and would be exact: `x = component_x` directly, `y = 255 - component_y` (the same inversion `waveform`/`thistogram` use), confirmed with an isolated single dot at a known `(cb, cr)`. The *intensity* accumulation is not: it is measurably nonlinear (confirmed independent of frame size by decoupling hit-count from total pixel count — same hit count gives the same output regardless of a `100`-pixel or `10000`-pixel frame), roughly zero below a threshold hit-count/intensity product and curving upward before saturating at `255`. Linear-additive (`waveform`'s own model), `ceil`/`round`/floor variants, single-parameter power laws, and an exponential-decay (IIR-towards-255) model were each tried against the measured curve and none fit cleanly across its full range — a curve-fit guess here would look measured without being it, so this is reported as characterised but unresolved, not shipped. |
//! | `pixscope` | **Structurally characterised further, still not shipped.** The font mechanism is confirmed shared with `datascope` (identical glyph family, no font option). Its "zoom" box does not magnify: a `7x7`-source-pixel `o=1` window over a hand-built checkerboard shows the source's own pixel period unchanged inside a `1`px-bordered outline — a location marker, not a magnifier. This pass additionally measured the *shape* of its statistics panel against a clean all-black source: one marker box (`10x10`px) plus **eight** distinct text lines in two groups — a `3`-line, `~230-240`px-wide block (likely a position/value header) and a `4`-line, consistently `9`px-pitched, narrower block (plausibly one line per statistic — min/max/average and a fourth). Exact row/column spans for every line are recorded in the module doc. The actual label text and numeric formatting were not extracted (no OCR path against a different, independently-sourced font — see `crate::font8x8`'s doc — and reconstructing eight lines of text by hand-decoding glyph shapes was judged not worth the risk of misreading a label into a wrong assumption). |
//! | `oscilloscope` | Not attempted this pass. Confirmed to share `datascope`/`pixscope`'s font mechanism (no font option; same non-antialiased glyph-grid signature reported for the family generally), but its own layout (the trace itself, the grid, and its `st`-gated statistics text) was not separately measured. |
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
pub mod thistogram;
pub mod waveform;

pub use registry::ScopeRegistry;
