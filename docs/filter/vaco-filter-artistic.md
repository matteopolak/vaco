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
comes back entirely `50`). The originally-shipped formula (a two-stage
horizontal/vertical blend weighted by the *product* of orthogonal
distances) matched three of a `4x4` test box's four columns exactly but
diverged on the fourth at every row tried.

**Re-derived and replaced** with a wider probe (`w=10:h=6` on a 20x20
frame) specifically because the narrow `4x4` box couldn't distinguish the
old formula from a simpler one on only three data points. The real formula
is plain four-point inverse-distance weighting from all four border
samples at once (`out = Σ(border/dist) / Σ(1/dist)`, no horizontal/vertical
split) — confirmed independently in both the horizontal and vertical
directions, and a strict accuracy improvement over the formula it replaced
(mean absolute error `1.4` vs `6.4`, max `17` vs `27`, over the 60-pixel
probe box). One column/row — whichever sits at `dist == 1` from a border
whose value genuinely differs from the local background — still
under-shoots the reference by `13`-`17` counts; the non-anomalous
`dist == 1` column (facing a border that matches the background) is
already exact, which is why the gap tracks *contrast*, not raw distance,
and a content-independent per-axis correction was tried and rejected (see
`src/delogo.rs`'s module doc). Shipped as **structural, not
framecrc-verified**, now with a much smaller and more precisely bounded
residual than the formula it replaced.

### `removelogo`

Like `delogo`, but the region comes from a bitmap mask file rather than a
fixed rectangle. `ffmpeg -h filter=removelogo` documents only `filename`/`f`
— the mask *format* itself was probed directly against the reference:

- It is a plain PGM (`P5`): a hand-built mask was accepted with no error and
  drove the same border-replacement behaviour `delogo` documents.
- A masked pixel is thresholded, not blended. **Re-measured with a
  byte-by-byte bisection** (every mask value from `10` to `32` against
  `ffmpeg -vf removelogo`, not just the two original endpoints): the exact
  cutoff is `16` inactive, `17` active. The `> 16` threshold this module
  already shipped turned out to be the measured cutoff itself, not a
  conservative guess inside a bracket.
- The pixel fill reuses `delogo::fill_box` over the mask's bounding
  rectangle, then restores any *inactive* pixel inside that rectangle to
  its original value — so a non-rectangular mask only touches what it
  marks. This means it inherits `delogo`'s own (now much smaller)
  anomalous-column discrepancy rather than duplicating a second guess at
  the same formula.
- The mask file is genuinely untrusted input (its own header declares its
  width/height), so the pixel buffer is sized through
  `vaco_limits::Budget::alloc` rather than a raw `Vec::with_capacity`, and
  `fuzz/fuzz_targets/removelogo_pgm_parse.rs` fuzzes the parser directly —
  the one target in this crate, since every other filter here only ever
  sees decoded frames from a trusted pipeline stage. Ran clean:
  `cargo +nightly fuzz run removelogo_pgm_parse --no-default-features
  --features filter-artistic -- -max_total_time=30` — exit 0, 9,778,796
  executions, `fuzz/artifacts` empty.

Structural, **not** framecrc-verified, for the same reason `delogo` is not
— its own mask-format and threshold measurements are now exact, not just
`delogo`'s interpolation core.

## Left as a follow-up

| Filter | Why it stopped |
|---|---|
| `hqx` | **D7, not time.** hq2x/hq3x/hq4x classify each pixel into one of 256 neighbourhood patterns and look up a hand-tuned rule per pattern — an *authorial* table (designed by visual experimentation, not dictated by any format constraint), and the only source found for the exact table was the reference's own implementation. |
| `xbr` | Independently published (Hyllian's own algorithm), so D7 is not the blocker in principle — but **the public specification found is not the reference's own variant, and no combination tried reproduces its output.** Fetched the algorithm's own author's MIT-licensed reference shaders (`libretro/glsl-shaders`' `xbr-lv2.glsl`/`xbr-lv3.glsl`, `provenance/sources.toml`), transliterated the `lv2` formula (default params: `Y_WEIGHT=48` BT.601 luma, `EQ_THRESHOLD=15`, `CORNER_C`, `SMOOTH_TIPS`) to Rust, and grid-searched all 4 corner-detection variants (`A`/`B`/`C`/`D`) × horizontal/vertical/transpose flips × rotation direction (64 combinations) against `ffmpeg 9.0.1`'s actual `xbr=n=2` output on a 16x16 synthetic corpus. Best match: `484`/`3072` bytes still differ (`~16%`), and the differences are full-value swaps (`00ff00` vs `3fbf00`), not rounding — a **structured** deviation, which `AGENT-CONSTRAINTS.md`'s "structured deviation is a bug" rule blocks from shipping regardless of the byte-exactness ruling. `xbr-lv3` (a materially different formula: `smoothstep` blending, two extra 15°/75° diagonal rules, a different luma matrix, `corner_type` selecting from more variants) was inspected but not similarly swept — the combinatorics of `lv2` alone already ruled out the most-likely default. Net finding: the reference's chosen rule set is neither `lv2`'s default nor any simple reorientation of it, and further narrowing needs either a source neither `lv2` nor `lv3` supplies, or substantially more probing time than this pass had. Not shipped. |
| `super2xsai` | Also independently published (predates and is separate from FFmpeg) — checked again this pass via fresh web search, since D7 is not the blocker. The original author's own homepage (`vdnoort.home.xs4all.nl/emulation/2xsai`, cited by three prior attempts) now 302-redirects to an unrelated domain — the source is no longer available there at all. No independent reimplementation found publishes the complete diagonal pixel-selection logic either (checked `janert/pixelscalers`, which explicitly does not implement 2xSaI). Four independent attempts across this crate's history have now failed to find a complete, precise statement of the algorithm outside the reference's own source. Left unattempted rather than reconstructed from partial information. |
| `cover_rect`, `find_rect` | Template matching against a second bitmap input at multiple mipmap scales. `find_rect` reports a *position*, not a transformed frame (`ffmpeg -h filter=find_rect` has no pixel-modifying options at all) — confirmed this pass by actually running it (`ffmpeg -vf find_rect=object=...,metadata=print`): it writes `lavfi.rect.{x,y,w,h,score}` frame metadata, which this tree can now express (`Frame::set_metadata`, closed for `vaco-filter-deinterlace::idet`). The **scoring formula itself does not decompose simply**: an exact single-pixel-corner match scores `0.000000`, but a single full-scale (`0`→`255`) one-pixel perturbation inside an 8x8 object at `mipmaps=3` (default) scores `0.053213`, not the `1/64 ≈ 0.015625` a plain mean-absolute-difference (optionally averaged again over 2-3 box-filtered mip levels, which cancels out algebraically for a single-pixel delta) would give — so the mipmap/scoring construction is more involved than a simple pyramid mean. Untangling it, plus the object file itself potentially being *any* image format ffmpeg's `image2` demuxer accepts (not a fixed, filter-owned format like `delogo`'s mask), makes this a larger, more cross-cutting task than a single filter's worth of scope — not attempted further this pass. |

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
| `delogo` | `x=3:y=3:w=4:h=4` | `gray`, 10x10 step function | **partial** — 3 of 4 columns exact, 4th diverges (mean abs err 1.4 over the box) |
| `delogo` | `x=3:y=3:w=10:h=6` | `gray`, 20x20 step function | **partial** — 54 of 60 pixels exact/rounding, one anomalous column (mean abs err 1.4, max 17) |
| `removelogo` | mask covering `x=2:y=2:w=4:h=4` | `gray`, 8x8 flat + `200` box | **exact** — content-independence confirmed (mask format + threshold now exact, not bracketed) |
| `removelogo` | same mask | `gray`, 8x8 step function | **partial** — inherits `delogo`'s anomalous-column discrepancy |

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
  rather than a second interpolation engine — fixing `delogo`'s
  anomalous-column discrepancy fixes it for both filters in one place.
- If you resolve `delogo`'s anomalous-column discrepancy or `vignette`'s
  `dither=1` generator, update both the relevant module's doc comment (each
  states exactly what was and was not verified) and the table above in the
  same change. The discrepancy tracks *contrast at the border*, not
  geometry — a per-axis "average with an interior estimate" correction
  reproduces the anomalous case but breaks a non-anomalous one, so whatever
  the real rule is, it isn't purely a function of `dist == 1`.
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
