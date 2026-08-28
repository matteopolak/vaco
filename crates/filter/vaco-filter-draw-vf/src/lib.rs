//! Pure-geometry drawing filters over one video input — `planning/16-filters.md`
//! §4.2's `vaco-filter-draw-vf` row, GitHub issue #473 (FT-4.10, "T2
//! text/drawing"). Two implemented: `drawbox`, `drawgrid`.
//!
//! # Scoped against the issue's own crate guess, as every prior wave has had to
//!
//! #473's title names `drawtext, drawbox, drawgrid, drawgraph` together and
//! its `Crate(s):` field says `vaco-filter-text` for all four — a
//! roadmap-era guess, the same shape #478/#480/#111 each turned out to be
//! wrong in a different way. `planning/16-filters.md` §4.2's own two
//! crate-mapping tables disagree with that field and with each other's
//! naive reading: `drawbox`/`drawgrid` (plus `qrcode`/`qrcodesrc`, not
//! claimed here — they need an external `qrcode` crate dependency, a
//! reviewed decision under D10 this pass does not make) are this crate's
//! own row, `vaco-filter-draw-vf`; `drawgraph`/`adrawgraph` are listed
//! under `vaco-filter-scope`'s row instead (where this pass implements
//! them, alongside that crate's other six filters); `drawtext` alone
//! stays under `vaco-filter-text`, blocked on #462's `TextRenderer`
//! (itself blocked on a `rustybuzz` provenance question under D10) — not
//! attempted here. Reported on #473 before writing code, the same
//! discipline #480/#111's own scoping comments used.
//!
//! # What is verified versus not attempted
//!
//! | Filter | Status |
//! |---|---|
//! | [`drawbox`] | **Framecrc-exact for `gbrp`-family 8-bit, no-alpha sources — pure geometry draws no text, so unlike this project's scope-filter family nothing forecloses byte-exactness here.** Every geometry option (`x`/`y`/`w`/`h`/`thickness`) is a `vaco-expr` expression evaluated exactly once (no per-frame re-evaluation exists to probe — confirmed `n`, the frame counter, is not even a bound name). `t` inside those expressions is not time; it is the filter's own resolved `thickness` value, confirmed by varying `thickness` and watching `x=t` track it exactly. The colour blend is `floor(src*(1-a)+color*a)` per channel, pinned at three different alpha values landing on the floored (not rounded) result; `thickness=fill` and `replace=true` are both implemented as the reference's own documented meaning. Not implemented: `box_source`; any pixel format other than planar RGB, 8-bit, no alpha (see the module's own doc for why converting an arbitrary colour into a YUV frame's own colour model was out of scope this pass, not merely untried). |
//! | [`drawgrid`] | **Framecrc-exact under the same scope as `drawbox`, and the same expression/blend mechanism.** The one thing measured specifically for this filter: grid lines repeat in *both* directions from the `(x, y)` offset (`(coordinate - offset) mod period`), confirmed by a probe whose offset was more than one period from the origin lighting a line on the *other* side of it — not merely forward from the offset, which a first guess might assume. |
//! | `drawgraph`, `adrawgraph` | See `vaco-filter-scope`'s own doc — implemented there, not in this crate. |
//! | `qrcode`, `qrcodesrc` | Not attempted. `planning/16-filters.md` names an external `qrcode` crate dependency for this row; adding one is a reviewed decision under D10 this pass does not make unilaterally. |
//! | `drawtext` | Not attempted, and explicitly not this crate's row regardless of #473's own `Crate(s):` field — `vaco-filter-text`'s, blocked on #462. |
//!
//! See `docs/filter/vaco-filter-draw-vf.md` for the full framecrc table and
//! every measurement's exact command line.

#![forbid(unsafe_code)]

mod color;
pub mod drawbox;
pub mod drawgrid;
pub mod registry;

pub use registry::DrawVfRegistry;
