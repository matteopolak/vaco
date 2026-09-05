# vaco-filter-video-composite

Video composite filters: `overlay` and `rotate`, and the alpha-blend
primitives they share. FT-4.1c (GitHub issue #465), the compositing child of
epic #54 — `fps` was already done, `overlay` had been deferred because it
needs both frame synchronisation and alpha blending, and this crate closes
that gap.

## What it is

Two filters, one module each (`src/overlay.rs`, `src/rotate.rs`), plus three
shared modules: `blend` (the measured "over" formula and the byte-level plane
walk), `format_opt` (`overlay`'s `format=`/`alpha=` enums), `fill` (a solid
frame for `rotate`'s corners), and `geom` (byte/plane helpers, independently
written — see "Dependencies" for why this does not reach into
`vaco-filter-video-geometry`). [`registry::CompositeRegistry`](../../crates/filter/vaco-filter-video-composite/src/registry.rs)
dispatches by name, same shape as the geometry crate's registry.

`overlay` is a [`vaco_filter_framesync::FrameSyncFilter`](../../crates/filter/vaco-filter-framesync/src/adapt.rs)
— **`vaco-filter-framesync` needed no changes at all** for this filter. It is
literally the crate's own worked example: `mock::Stamp`'s doc describes
probing `overlay` to recover the event-loop semantics, and `FsInput::dual`'s
own doc names `overlay` as the motivating case for `sync=2`/`sync=1`. Every
`eof_action`/`shortest`/`repeatlast` case this crate needed was already
measured and implemented in `vaco-filter-framesync`'s own truth table; this
crate wrote zero synchronisation logic.

`rotate` is an ordinary single-input [`FrameFilter`](../../crates/filter/vaco-filter-core/src/adapt.rs),
the same adapter `crop`/`pad`/`scale` use.

## How it works

### `blend` — the "over" formula, and why straight and premultiplied looked identical at first

`overlay`'s `alpha=` option only changes anything when the *background*
itself carries alpha — see the edge-case table. The formula, with
`a_fg = overlay_alpha/255`, `a_bg = background_alpha/255` (`1.0` when a side
has no alpha component) and `out_a = a_fg + a_bg*(1-a_fg)`:

| `alpha=` | Per-channel formula |
|---|---|
| `straight` (2), `unknown`/`auto` (0 — **share one option value**) | `(fg*a_fg + bg*a_bg*(1-a_fg)) / out_a` |
| `premultiplied` (1) | `fg*a_fg + bg*a_bg*(1-a_fg)/out_a` |

Blending is done per-plane for planar formats (walking each colour plane at
its own, possibly chroma-decimated, resolution) and per-pixel for packed ones
(`rgb24`, `rgba`), both going through [`geom::plane_unit_bytes`](../../crates/filter/vaco-filter-video-composite/src/geom.rs)
so the byte stride is read off `vaco-pixfmt`'s component table rather than
hand-derived per format.

When the resolved format has no alpha channel, the equation reduces exactly to
the foreground sample. `blend` therefore copies the clipped span in each
packed or planar plane. In packed alpha-bearing frames, an opaque background
pixel also makes `out_a` exactly `1`: both alpha modes reduce to
`fg*a_fg + bg*(1-a_fg)`, so that pixel skips the per-channel normalization and
keeps alpha `255`. The remaining alpha-bearing pixels retain the measured
floating-point formula; planar formats retain their alpha snapshots.

### `overlay` reformats through `vaco-scale`, never hand-derives a colour matrix

Main and secondary frames are converted to the resolved blend `PixFmt` (see
`format_opt`) with the same technique `vaco-filter-video-geometry::pad` uses
for its fill colour: a same-size `vaco_scale::Scaler` pass. If a frame is
already the blend format, nothing is copied.

### `rotate` samples by inverse rotation about each plane's own centre

For each output pixel, the corresponding input position is computed by
rotating the output-centred offset by `-angle` and re-centring on the input.
Out-of-bounds samples keep the fill colour a [`fill::solid_frame`](../../crates/filter/vaco-filter-video-composite/src/fill.rs)
pass already painted the whole output canvas with, before any resampling
runs — so there is no separate "is this pixel a corner" test.

## The measured edge-case table

| Filter | Case | Measured behaviour (ffmpeg 8.1) |
|---|---|---|
| `overlay` | `x`/`y` evaluation timing | **Per frame** by default (`eval=frame`, the reference's own default — not `init`). `overlay=x=4*n` visibly moves the overlay four columns every frame. |
| `overlay` | `eval=init` with `t`/`n` in `x`/`y` | Evaluated once, before the first frame, where `t` is undefined; the overlay is placed off-screen (this crate: `to_pixel(NaN) = 0`, a deterministic choice — the reference's own `(int)NaN` cast is C undefined behaviour and was not reproduced). |
| `overlay` | `x`/`y` cross-reference | Each can use the *other's freshly computed* value, not textual order: `x=8:y=x/2` → `(8, 4)`; `x=y/2:y=8` → `(4, 8)`. A genuine cycle (`x=y:y=x+1`) produces **no visible overlay** in the reference — not reproduced exactly; see `overlay.rs`'s doc. |
| `overlay` | `w`/`h`/`W`/`H`/`overlay_w`/`overlay_h` | All five are the **overlay's own** dimensions — not the output's, not the main's. `main_w`/`main_h` are the only spelling for the background. `pos` and `main_t` are **not** valid variables (rejected). |
| `overlay` | `x`/`y` to pixel coordinate | **Truncation toward zero** (`f64 as i64`, C's `(int)` cast) — not floor. `x=5.0`..`5.9` all place at column 5; `x=-0.5` → column 0; `x=-1.5` → column ‑1. |
| `overlay` | overlay partly off an edge | Clipped to the intersection; the visible part composites normally. |
| `overlay` | overlay wholly outside the main frame | Main frame is left byte-identical (proved via a real `vaco-filter-core` `Graph`, `tests_invariants.rs`). |
| `overlay` | second input ends first, `eof_action=repeat` (default) | Last secondary frame held forever (`ExtendMode::Infinity` — from `vaco-filter-framesync`, unchanged). |
| `overlay` | second input ends first, `eof_action=endall` | Whole filter's output ends with it. |
| `overlay` | second input ends first, `eof_action=pass` | Secondary disappears (`None` at `event.get(1)`); main passes through unmodified from then on. |
| `overlay` | `repeatlast=0` | **Identical** to `eof_action=pass`, event for event (5 frames both ways at main=0.5s/secondary=1.0s, both 10fps) — not "nearly but not exactly", which is what an earlier reading of plan 16 §3.3 assumed. |
| `overlay` | `format=yuv420` (default) | **No alpha plane** (`yuv420p`) — the one asymmetry: every wider-chroma or higher-depth `format=` value adds alpha, the plain 8-bit 4:2:0 default does not. |
| `overlay` | `format=yuv420p10/yuv422/yuv422p10/yuv444/yuv444p10/gbrp` | Alpha plane added (`yuva420p10le`/`yuva422p`/… /`gbrap`). |
| `overlay` | `format=rgb` | `rgb24`, no alpha. |
| `overlay` | `format=auto` | The **main input's own chroma family**, alpha added if it does not already have one (probed with `rgba`/`rgba` → `rgba`; `yuv444p` main → `yuva444p`; `nv12` main → `yuva420p`). |
| `overlay` | `alpha=auto` vs `alpha=unknown` | **Share option value `0`** in the reference and are indistinguishable in every configuration probed — both mean "straight". |
| `overlay` | `alpha=straight` vs `alpha=premultiplied` | **Identical output when the background is opaque** (`a_bg=1` makes `out_a=1` regardless of `a_fg`, so the two formulas' division by `out_a` becomes a no-op) — this is what made the first probe (opaque `rgb24` background) look like the option did nothing. A semi-transparent background pair (`a_fg=100/255`, `a_bg=200/255`) diverges: straight `115`, premultiplied `100`, confirmed byte-exact against the reference twice with different alpha pairs. |
| `rotate` | default `out_w`/`out_h` | The **literal strings `"iw"`/`"ih"`** — same size as the input, rotated content clipped to fit — **not** a bounding-box fit. `ffprobe … rotate=PI/4` on a 100×50 input stays 100×50. |
| `rotate` | `ow=rotw(a):oh=roth(a)` | The actual bounding box: `rotw(a) = \|in_w·cos(a)\| + \|in_h·sin(a)\|`, `roth` with sin/cos swapped. 30° on 100×50 → 112×93, which is `round(111.60)`/`round(93.30)` — ordinary rounding, not floor or ceiling. |
| `rotate` | `ow`/`oh` evaluation timing | **Configure-time only.** `ow=rotw(PI/4*t)` fails to configure at all — `t` is `NaN` before the first frame and the reference reports "non-positive or indefinite value nan". This crate raises the same class of error rather than silently picking a size. |
| `rotate` | `angle` evaluation timing | **Per frame.** `angle=PI/8*t` with fixed numeric `ow`/`oh` visibly changes frame to frame. |
| `rotate` | rotation direction | Positive `angle` is **clockwise on screen** (a point to the right of centre moves down under `+15°`, `bilinear=0`). |
| `rotate` | corner fill, default `fillcolor=black` | **Limited-range black** (`Y=16`, `Cb=Cr=128`) for `yuv420p` — the same measurement `pad`'s default fill established, reproduced here because both go through the same `vaco-scale` colour-signalling path rather than either crate's own matrix code. |
| `rotate` by `0` | — | Identity (proptest, exact byte match, arbitrary dimensions). |
| `rotate` by `90°`, four times | — | Returns the original exactly (nearest-neighbour, square frame — see `rotate.rs`'s doc for why bilinear is not expected to be bit-exact here). |

## Whether `vaco-filter-framesync` was sufficient

Completely, for `overlay`. `FsInput::dual`, `apply_opts`'s `eof_action`/
`shortest`/`repeatlast` truth table, and the `Synced` adapter's event loop
needed no changes and no workaround. The one thing this crate wanted and
had to work around locally: `overlay` keeps the **main input's** time base on
the output (measured, and already documented as a fact in
`vaco-filter-framesync`'s own docs), where `Synced::configure` installs the
*common* time base by default. `FrameSyncFilter::configure` runs after that
install specifically so a filter can override it, which is exactly what
`overlay::Overlay::configure` does — a supported seam, not a gap.

## How to change it

- Add a blend mode or format mapping: `blend.rs`/`format_opt.rs` are the
  only places colour-space and alpha-formula decisions live; `overlay.rs`
  itself only computes placement and calls into them.
- Change `rotate`'s resampling: `rotate_into` is a free function taking
  `&Frame`/`&mut Frame` with no `FilterContext`, so it is directly
  proptestable (see its own tests) without a graph.
- Add a new composite filter (`blend`, `hstack`, …): reuse `blend::composite`
  for the alpha math and `geom`'s plane helpers; if it needs multiple
  independent-timeline inputs, wrap it in `vaco_filter_framesync::Synced`
  exactly as `overlay` does.

## Configuration

Options are declared with `#[derive(vaco_opts::Options)]` and parsed via
`OptionsExt::set_from_string(args, "=", ":")`. Enum-valued options
(`eof_action`, `eval`, `format`, `alpha`, `ts_sync_mode`, `bilinear` is a
plain bool) are read as `String`/`bool` fields and parsed against a fixed
vocabulary afterward — the same pattern `vaco-filter-video-geometry` uses for
`transpose`'s `dir`. Defaults and option names were captured with
`LC_ALL=C ffmpeg -h filter=<name>` against ffmpeg 8.1.

## Known gaps (reported, not silently guessed)

- **8-bit samples only.** `geom::ensure_addressable_8bit` rejects hardware
  surfaces, sub-byte packing, palette formats, and any depth other than 8
  bits — including the 10-bit `format=` values `overlay` itself offers
  (`yuv420p10`/`yuv422p10`/`yuv444p10`), which are parsed and mapped to the
  correct `PixFmt` but rejected at `configure` rather than composited with
  wrong byte-pair math.
- **Chroma-plane alpha sampling is nearest, not averaged.** A subsampled
  chroma sample's alpha is read at the corresponding full-resolution
  position, floor-mapped. The reference may average the covering
  full-resolution alpha samples for a chroma pixel; this was not measured.
- **`format=auto`'s family coverage is three probed cases**, generalised to
  4:2:0/4:2:2/4:4:4/GBR/RGB by chroma-subsampling shape. An input outside
  those families returns `Error::Unsupported` rather than a guess.
- **A genuine `x`/`y` mutual cycle** (`x=y:y=x+1`) is not modelled to match
  the reference's own (apparently off-screen) result — see `overlay.rs`'s
  doc for the two-pass evaluation this crate does instead, which reproduces
  every non-pathological case measured.
- **`rotate`'s bilinear kernel** was not bisected against the reference's own
  filter coefficients to sub-pixel precision; a one-pixel disagreement at
  extreme angles is plausible.

## Dependencies

`vaco-filter-core` (`Filter`/`FrameFilter`, `Simple`, negotiation),
`vaco-filter-framesync` (`FrameSyncFilter`, `Synced`, `FsInput`,
`FrameSyncOpts`), `vaco-filter-graph` (`FilterRegistry`/`Instantiate`),
`vaco-scale` (`overlay`'s reformatting, `rotate`'s corner fill),
`vaco-expr` (`x`/`y`/`angle`/`out_w`/`out_h` expressions, `rotw`/`roth`
externs), `vaco-pixfmt`, `vaco-color`, `vaco-frame`, `vaco-opts`,
`vaco-core`.

Not a dependency: `vaco-filter-video-geometry`. Its `geom.rs`/`fill.rs`
solve the same small problems this crate needs (byte-per-pixel-group, a
solid-colour frame via `vaco-scale`) but are `pub(crate)` there, so this
crate's own `geom.rs`/`fill.rs` reimplement the same generic ideas against
the shared public `vaco-pixfmt`/`vaco-scale` surface rather than depending on
a sibling crate's private internals.
