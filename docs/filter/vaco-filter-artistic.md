# vaco-filter-artistic

T3 artistic/stylisation video filters — `planning/16-filters.md` §4.2's
`vaco-filter-artistic` row. Six implemented: `amplify`, `delogo`, `epx`,
`noise`, `removelogo`, `vignette`.

## Scope reconciliation — this crate replaces a dispatch brief that named the wrong crate

The dispatch brief for this work (GitHub issue #478, "FT-4.12e") asked for a
`vaco-filter-effect` crate covering roughly two dozen filters (`sobel`,
`prewitt`, `roberts`, `kirsch`, `edgedetect`, `morpho`, `erosion`,
`dilation`, `deflate`, `inflate`, `shuffleframes`, `shufflepixels`,
`shuffleplanes`, `swaprect`, `swapuv`, `tmix`, `lagfun`, `random`,
`photosensitivity`, `noise`, `vignette`, `pixelize`, among others). Per
`planning/AGENT-CONSTRAINTS.md`'s "the plan already partitions the filters;
do not invent a crate", `planning/16-filters.md` §4.2's table was checked
before writing anything, and `vaco-filter-effect` is not a row in it. Almost
the entire named list already has a home, and, for most of it, is already
built and committed (`planning/ASSIGNMENTS.md`, checked 2026-08-28):

| Filters | Actual crate | Status |
|---|---|---|
| `sobel`, `prewitt`, `roberts`, `kirsch`, `scharr`, `edgedetect`, `morpho`, `erosion`, `dilation`, `deflate`, `inflate`, `median`, `convolution` | `vaco-filter-convolve` | done (#468, `agent:blur2`) |
| `swaprect`, `swapuv`, `shuffleframes`, `shuffleplanes`, `pixelize` | `vaco-filter-geometry` | done (#470, `agent:geom2`) |
| `tmix`, `lagfun`, `random` | `vaco-filter-temporal` | done (#475, `agent:temporal`) |
| `photosensitivity` | `vaco-filter-analysis` | in progress (#477, `agent:analysis2`) throughout this crate's time in the tree — not this crate's to touch |
| `shufflepixels` | (declined in `vaco-filter-geometry`) | its `seed` option reads as a generator seed nobody has identified; that crate documented the same "not implemented, not guessed" call this crate makes for `noise` |

What was actually left, unclaimed, in this crate's real row: `noise`,
`vignette`, `amplify`, `delogo`, `removelogo`, `epx`, `xbr`, `hqx`,
`super2xsai`, `cover_rect`, `find_rect`. This crate implements six of those
eleven; the other five are listed under "Left as a follow-up" below, each
with the specific reason it stopped.

## What it is

One module per filter (`src/{amplify,delogo,epx,noise,removelogo,vignette}.rs`),
each exposing `pub const DESC: FilterDesc` and a crate-private `fn create`,
aggregated by
[`registry::ArtisticRegistry`](../../crates/filter/vaco-filter-artistic/src/registry.rs)
— the same shape `vaco-filter-convolve`/`vaco-filter-geometry` use.
`src/common.rs` carries the small 8-bit-plane helpers every crate in this
family carries its own copy of (format validation, frame metadata copying,
the `planes` bitmask) — D19 governs shared *types*, not these tiny
per-crate predicates.

### `amplify`

Amplifies (or leaves alone) each pixel's deviation from a temporal window
average — the filter's own purpose is surfacing *subtle* frame-to-frame
change while leaving large real motion untouched. Fully measured against
the reference (`ffmpeg 8.1`, `-bitexact`, a 1x1 `gray` source carrying a
hand-built per-frame sequence via `geq`), because none of the shape beyond
the option names is in the reference's own documentation:

- The averaging window is symmetric and *includes* the centre: `radius=r`
  averages the `2r+1` frames `[center-r..center+r]`. Confirmed independently
  at `r=1` and `r=2`.
- Readiness needs **one frame more of history than the window strictly
  requires** — the first output is for input index `radius+1`, not
  `radius`, with no compensating emission at EOF (total output count is
  exactly `input_count - 2*radius - 1`). This crate reproduces the measured
  rule via a `2*radius+2`-slot ring buffer without knowing why the
  reference wants the extra frame.
- The gate is `tolerance < |dev| <= threshold`; outside that window the
  centre pixel passes through completely unchanged. A `40`-unit sustained
  step was left untouched at every `threshold` tried (`0`, `1`, `2`, the
  default `10`) — consistent with the filter's stated purpose of ignoring
  real motion by default.
- `low`/`high` clamp the **delta's magnitude**, asymmetrically by sign
  (`delta.max(-low)` if negative, `delta.min(high)` if positive) — not the
  final pixel value directly. The default `low=high=65535` never binds for
  8-bit content, which is why every probe that did not set them looked
  unclamped.

See `src/amplify.rs`'s module doc for the full derivation, including the
alternate hypotheses tried and ruled out.

### `epx`

Scales by 2x (`n=2`, Scale2x) or 3x (`n=3`, Scale3x), implemented from
`https://www.scale2x.it/algorithm` (`provenance/sources.toml`'s
`scale2x-algorithm`) — the AdvanceMAME project's own published
specification, not the reference's source. Both scale factors reduce to a
handful of neighbour-equality comparisons with no lookup table at all, and
**both are framecrc-exact**: `n=2` matched several hand-picked pixels
including one exercising every branch of the four-way conditional, and the
same corner pixel re-probed at `n=3` matched the full nine-value block,
including the more elaborate edge-midpoint branches (`E1`/`E3`/`E5`/`E7`)
that `n=2` doesn't have.

### `vignette`

Darkens (or, `mode=backward`, brightens) radially from a centre point using
the classic `cos^4` optical vignetting law. Framecrc-exact for
`dither=0`. Full derivation, including the probe that pinned the exact
pixel-coordinate and truncation convention, is in `src/vignette.rs`'s
module doc. Three documented gaps:

1. `dither=1` — the filter's own **default** — perturbs the truncated
   output by up to ±1, reproducibly across runs despite no `seed` option. A
   bounded sweep (comparing `dither=0` and `dither=1` over a 32x32 uniform
   frame and looking for short-period tiling in the difference map) found
   no evidence of a simple ordered-dither matrix — a real negative result,
   not an unattempted one.
2. Chroma-plane handling is scaled around the neutral point `128`
   (structurally reasoned, matches the *shape* measured on a `yuv420p`
   probe) but left a small residual this pass did not chase down.
3. `aspect != 1` reproduces every interior pixel exactly but not the
   reference's extra hard clipping right at the frame's extreme corners.

### `noise`

Adds per-component (`c0`..`c3`) pseudo-random noise, `all_seed`/
`all_strength`/`all_flags` setting every component's suboption at once. The
one framecrc-exact case: with every component's `strength` at its default
of `0` (plain `-vf noise`), the reference is a byte-identical no-op, and
this module reproduces that exactly (checked directly: it never touches its
RNG state at `strength=0`). Any `strength > 0` uses this crate's own
`SplitMix64` PRNG and is not framecrc-verified — reproducing the reference's
actual generator would need its source (D7), the same conclusion
`vaco-filter-temporal::random` already reached.

### `delogo`

Replaces a rectangular region with values interpolated from its border —
confirmed content-independent (a `200`-valued box surrounded by flat `50`
comes back entirely `50`). The measured formula (a bilinear blend of
horizontal and vertical border interpolation, weighted by the product of
orthogonal distances — see `src/delogo.rs`'s module doc for the exact
derivation) matched three of a `4x4` test box's four columns exactly but
diverged on the fourth at every row tried, and several alternate
conventions (shifted distance origins, plain four-corner bilinear, an
unweighted 50/50 blend, squared/min-based weights) did not fix the fourth
column without breaking the other three. Shipped as **structural, not
framecrc-verified** — the same bar `vaco-filter-blur::gblur` sets for the
same reason.

### `removelogo`

Like `delogo`, but the region comes from a bitmap mask file rather than a
fixed rectangle. `ffmpeg -h filter=removelogo` documents only `filename`/`f`
— the mask *format* itself was probed directly against the reference:

- It is a plain PGM (`P5`): a hand-built mask was accepted with no error and
  drove the same border-replacement behaviour `delogo` documents.
- A masked pixel is thresholded, not blended: bisecting the mask byte value
  found `10` leaves a pixel untouched and `32` replaces it; the exact cutoff
  was not narrowed further, so this module uses a conservative `> 16`
  threshold sitting inside that measured bracket rather than a guessed
  exact value.
- The pixel fill reuses `delogo::fill_box` over the mask's bounding
  rectangle, then restores any *inactive* pixel inside that rectangle to
  its original value — so a non-rectangular mask only touches what it
  marks. This means it inherits `delogo`'s own unresolved fourth-column
  discrepancy rather than duplicating a second guess at the same formula.
- The mask file is genuinely untrusted input (its own header declares its
  width/height), so the pixel buffer is sized through
  `vaco_limits::Budget::alloc` rather than a raw `Vec::with_capacity`, and
  `fuzz/fuzz_targets/removelogo_pgm_parse.rs` fuzzes the parser directly —
  the one target in this crate, since every other filter here only ever
  sees decoded frames from a trusted pipeline stage. Ran clean:
  `cargo +nightly fuzz run removelogo_pgm_parse --no-default-features
  --features filter-artistic -- -max_total_time=30` — exit 0, 9,778,796
  executions, `fuzz/artifacts` empty.

Structural, **not** framecrc-verified, for the same reason `delogo` is not.

## Left as a follow-up

| Filter | Why it stopped |
|---|---|
| `hqx` | **D7, not time.** hq2x/hq3x/hq4x classify each pixel into one of 256 neighbourhood patterns and look up a hand-tuned rule per pattern — an *authorial* table (designed by visual experimentation, not dictated by any format constraint), and the only source found for the exact table was the reference's own implementation. |
| `xbr` | Independently published (Hyllian's own description), so D7 is not the blocker — but genuinely **larger than `epx`, not "a fraction of the cost"**: a shader-level reference implementation samples a 5x5 neighbourhood (`epx`'s is 3x3) with roughly 8-12 weighted-distance comparisons per corner via coefficient matrices, versus `epx`'s single boolean check. Deferred for time, with that corrected complexity estimate on record. |
| `super2xsai` | Also independently published (predates and is separate from FFmpeg), so not a D7 case either. Three independent attempts sourced the `INTERPOLATE`/`Q_INTERPOLATE` arithmetic helpers but not a complete, precise statement of the diagonal pixel-selection logic that picks which computed value becomes which output sub-pixel. Left unattempted rather than reconstructed from partial information — guessing the selection structure risks exactly the "confidently wrong" failure mode this project's conventions warn against. |
| `cover_rect`, `find_rect` | Template matching against a second bitmap input at multiple mipmap scales. `find_rect` reports a *position*, not a transformed frame (`ffmpeg -h filter=find_rect` has no pixel-modifying options at all) — decided the right verification shape is a `showinfo`-style metadata assertion, not a framecrc pixel diff, before attempting any implementation. `cover_rect` itself takes no coordinate options, which reads as depending on `find_rect`'s detection via frame metadata when the two are chained. Neither filter's underlying correlation search was implemented in the time available. |

## Framecrc comparison table

One comparison loop throughout: `ffmpeg -bitexact -f lavfi -i <deterministic
source> -vf "<filter>=<args>" -f rawvideo -pix_fmt <fmt> -` against the
reference, cross-checked against this crate's pure functions and pinned
into unit tests. No `vaco` CLI/muxer exists yet in this tree to drive an
actual `vaco ... -f framecrc -` invocation (`planning/14-cli.md` is still a
plan document), so "framecrc comparison" here means the same rawvideo-diff
methodology `vaco-filter-convolve`/`vaco-filter-blur` already established
for exactly this reason.

| Filter | Args | Source | Result |
|---|---|---|---|
| `amplify` | `radius=1:factor=5:threshold=10:tolerance=0` | `gray`, 1x1, 10-frame step sequence | **exact** |
| `amplify` | `radius=2` variant of the same sequence | `gray`, 1x1, 14 frames | **exact** |
| `amplify` | `low=3:high=3` / `low=3:high=100` | same, `factor=5` | **exact** (delta-clamp verified) |
| `epx` | `n=2` | `gray`, 4x4 checkerboard | **exact** |
| `epx` | `n=3` | same source | **exact** |
| `vignette` | `dither=0` (default angle/x0/y0/aspect/mode) | `gray`, 8x8 | **exact** — forward mode |
| `vignette` | `dither=0:mode=backward` | `gray`, 16x8 | **exact** |
| `vignette` | `dither=0` (default) | `yuv420p`, 8x8 | **not verified** — chroma residual |
| `vignette` | `dither=1` (the default) | any | **not verified** — bounded sweep found no simple pattern |
| `vignette` | `dither=0:aspect=2` | `gray`, 16x8 | **partial** — interior exact, extreme corners diverge |
| `noise` | *(no args, `strength=0`)* | `gray`, 8x8 | **exact** — verified identity |
| `noise` | any `strength > 0` | any | **not verified** — PRNG not reproduced |
| `delogo` | `x=3:y=3:w=4:h=4` | `gray`, 10x10 flat + `200` box | **exact** — content-independence confirmed |
| `delogo` | `x=3:y=3:w=4:h=4` | `gray`, 10x10 step function | **partial** — 3 of 4 columns exact, 4th column diverges every row |
| `removelogo` | mask covering `x=2:y=2:w=4:h=4` | `gray`, 8x8 flat + `200` box | **exact** — content-independence confirmed (mask format + threshold measured) |
| `removelogo` | same mask | `gray`, 8x8 step function | **partial** — inherits `delogo`'s fourth-column discrepancy |

## How to change it

- All six filters follow `vaco-filter-convolve`'s convention: a `pub const
  DESC`, an `Opts` struct via `vaco_opts::Options` with a `parse` inherent
  method, a `Filter` struct implementing `vaco_filter_core::adapt::FrameFilter`,
  and a `create` function the registry dispatches to.
- `amplify` is this crate's only filter that changes the *frame count*
  (fewer outputs than inputs) rather than just per-pixel values — it
  buffers via a `VecDeque<Frame>` sized to `2*radius+2`, not the
  `Simple`/one-in-one-out shape the others use directly (it still wraps in
  `Simple`, since `Simple` handles per-frame delivery; the buffering lives
  inside `Filter::filter_frame`).
- `epx` is this crate's only filter that changes *frame dimensions* — its
  `configure` hook rewrites the output `LinkFormat`'s width/height, following
  `vaco-filter-video-geometry::scale`'s pattern.
- `removelogo` reuses `delogo`'s `Rect`/`fill_box` (both `pub(crate)`)
  rather than a second interpolation engine — fixing `delogo`'s fourth-column
  discrepancy fixes it for both filters in one place.
- If you resolve `delogo`'s fourth-column discrepancy or `vignette`'s
  `dither=1` generator, update both the relevant module's doc comment (each
  states exactly what was and was not verified) and the table above in the
  same change.
- If a `rand`-family dependency is ever approved for the workspace, `noise`
  is the filter whose correctness bar changes from "algorithmically
  faithful" to "must match bit for bit" — though reproducing FFmpeg's
  actual generator (unpublished, reachable only via its source) would still
  be needed even then.

## Configuration

No crate-level configuration, environment variables, or feature flags.
Runtime configuration is entirely per-filter-instance, via each filter's
`Opts` (parsed from the filtergraph argument string — see each module's
`ffmpeg -h filter=<name>` transcription in its doc comment for the exact
option surface). `removelogo`'s `filename` option additionally names a
mask file on disk, read and parsed at filter-creation time.

## Dependencies

`vaco-core`, `vaco-expr` (for `vignette`'s and `delogo`'s expression
options), `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-limits` (for `removelogo`'s bounded mask
allocation), `bitflags` (for `noise`'s flag-letter option). No external
crate beyond what the workspace already declares; no new dependency was
added to the workspace's `Cargo.toml`.
