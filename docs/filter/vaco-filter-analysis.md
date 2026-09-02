# vaco-filter-analysis

T2/T3 video analysis and detection filters (FT-4.12d, GitHub issue #477):
`psnr`, `ssim`, `identity`, `msad`, `signalstats`, `blackdetect`,
`blackframe`, `bbox`, `entropy`, `cropdetect` — ten of `planning/16-filters.md`
§4.2's twenty-three-filter row (the row is video, checked against the
plan directly rather than via the earlier, wrongly-cited §4.3).

**2026-08-23 continuation pass**: added `entropy` and `cropdetect` (this
wave's own two best-value picks), extended `signalstats` from 15 to 25 of
its 28 documented keys (`SAT*`, `HUE*`, `*DIF`, `*BITDEPTH`), closed
`blackframe`'s documented exact-threshold boundary gap, and investigated —
without shipping — `bitplanenoise` and `siti`, both of which looked like
clean closed forms and measured out to not be. See "What landed" and the
per-filter sections below for the specifics.

## Row membership, checked against the reference

All twenty-three names in the plan's row (`psnr, ssim, ssim360, xpsnr, vif,
vmafmotion, msad, identity, blackdetect, blockdetect, bitplanenoise,
entropy, siti, signalstats, readeia608, readvitc, showinfo,
photosensitivity, scdet, bbox, codecview, blackframe, cropdetect,
signature`) were checked against `ffmpeg -hide_banner -filters` and
`ffmpeg -h filter=<name>` (ffmpeg 8.1, 2026-08-23). **All twenty-three exist
in the reference with that exact name.** The row matches the reference in
both directions — nothing to add, nothing to drop.

## What landed, and why the other thirteen did not

**Landed** (ten): `psnr`, `ssim`, `identity`, `msad`, `signalstats`,
`blackdetect`, `blackframe`, `bbox`, `entropy`, `cropdetect`.

**Explicitly named as likely-to-leave, left**: `vmafmotion`, `ssim360`,
`vif`, `signature`. `vif` needs a wavelet natural-scene-statistics model
from a separate paper (Sheikh & Bovik 2006) not implemented in this wave;
`vmafmotion`/`ssim360` build on machinery this crate did not extend to
them; `signature` (MPEG-7 video signature) is a standalone algorithm
(region partitioning, per-region feature vectors, a matching layer) out of
proportion to this wave.

**Judgement call, left**: `xpsnr`. Not simply weighted `psnr` — the
reference's XPSNR applies per-block perceptual weighting from local
activity, and getting that weighting right needed a measurement pass this
wave did not have time for. Shipping an under-measured `xpsnr` in the crate
other filters are verified against was judged worse than leaving it out.

**Not explicitly named, left for pace/scope reasons**:

* `showinfo` — measured (`ffprobe -show_frames` through it) to write **no**
  frame metadata at all; its output is a console log line, a channel this
  workspace's filter framework does not model. Interface gap 11 (the
  metadata dictionary) does not help it.
* `codecview` — visualises motion vectors, which are not a
  `vaco_frame::FrameSideData` variant this workspace has yet. A
  decoder-side gap, not a measurement-formula gap.
* `readeia608`, `readvitc` — need bit-accurate waveform decoding (EIA-608
  line-21 encoding, SMPTE VITC bi-phase marks). Substantial to get right
  and, without a captured real line to decode against, an "oracle that
  shares your misreading" risk.
* `blockdetect`, `scdet`, `photosensitivity` — each is a multi-frame or
  full-academic-paper algorithm (a block-grid period search over two
  tunable ranges; a scene-cut heuristic combining mean-absolute-frame-
  difference with its own frame-to-frame delta; a rolling-window temporal
  luminance-flash detector) that this pass did not have time to pin down
  precisely enough to trust, given what `bitplanenoise` and `siti` (next)
  cost to *not* trust on an optimistic first read.
* `bitplanenoise`, `siti` — investigated at length in the 2026-08-23
  continuation pass and **not shipped as a guess**, which is the point of
  recording them here rather than folding them into the bullet above:
  - `bitplanenoise`'s noise ratio, on a fixture engineered to be maximally
    noisy (alternating rows, guaranteeing every adjacent-row bit differs),
    holds its **numerator constant at exactly `4`** while its denominator
    tracks frame width exactly (`w=3` &#8594; `2/3`, `w=4` &#8594; `4/4`,
    `w=8` &#8594; `4/8`, `w=16` &#8594; `4/16`, ...) — but height has **no**
    effect at all (`w=4` scores `1.0` at every height from `2` to `32`
    tried). Horizontal, vertical, and vertical-with-wraparound per-pixel
    bit-difference hypotheses were each checked against this data and none
    reproduced "numerator pinned to 4, independent of height, dependent
    only on width" — measured, not guessed, and left rather than shipped
    on a formula that could not explain its own calibration fixture.
  - `siti`'s `SI` (the ITU-T P.910 Sobel-gradient-magnitude standard
    deviation) matched the textbook formula **exactly** on a
    maximum-contrast fixture: a 16x16 frame split `0`/`255` down the middle,
    interior-pixels-only, population (not sample) standard deviation, gives
    `356.925648...`, and the reference prints `356.93` — a match to every
    digit `%.2f` can show. The *same* formula on the *same* spatial pattern
    at `100`/`120` (a linear rescale of the same step) predicts `27.99`
    (`356.93 * 20/255`); the reference measures `33.59`. Because Sobel and
    population variance are both linear/quadratic in the input, a genuine
    amplitude-independent formula **cannot** match at one amplitude and
    miss by ~20% at another — this rules out "the constant is slightly
    off" and points at something amplitude-dependent (a gamma step, a
    quantised intermediate, or a formula this pass has not found) that a
    single flat-field probe cannot see. `TI` was not independently pinned
    down either, for the same reason: the one two-frame test constructed
    to isolate it also disagreed with a linear std-of-difference model
    (predicted `10.0`, measured `12.00` then `11.50` — not even self-
    consistent between two structurally-identical steps).
  Both are documented here in the spirit of `ssim`'s own "not byte-exact,
  and here is the exact arithmetic that proves it" entry below, not
  scope-cut for time.
* `xpsnr` — re-measured in this pass (`ffmpeg -h filter=xpsnr`) as a
  correction worth recording even though it changes nothing shipped:
  **`xpsnr` *does* carry the full `framesync` option surface**
  (`eof_action`/`shortest`/`repeatlast`/`ts_sync_mode`), unlike `psnr`/
  `ssim`/`identity`/`msad`, which measure like `framepack` (see below).
  So if `xpsnr` is ever implemented, it is the one filter in this crate's
  row that *would* want `vaco-filter-framesync`, not `vaco-filter-core::Paired`
  — the opposite of the other four. Still left unimplemented for the
  reason already on record: the per-block perceptual weighting itself,
  not the adapter choice, is the unmeasured part.

## The interface this crate depends on: `Frame::metadata()` (gap 11)

Every filter here is a measurement whose only output channel is
`vaco_frame::Frame`'s metadata dictionary
([`Frame::metadata`]/[`Frame::set_metadata`]/[`Frame::metadata_get`],
backed by `FrameSideData::Metadata`) — interface gap 11, closed the day
before this crate was dispatched. `vaco-filter-temporal::freezedetect` is
the worked example this crate's shape follows: one module per filter, the
actual measurement factored into a plain function that does not touch
`FilterContext` (so it is unit-testable directly), with the adapter
(`Simple`/`Paired`) as a thin wrapper.

**Verification seam.** Like `freezedetect`, this crate cannot be exercised
end to end through `vaco-probe -show_frames` — that path is still refused
(D5, `vaco-probe` has no decoders and does not depend on the filter graph).
Every filter here is instead verified at the seam gap 11 names: given a
hand-built `Frame`, does the metadata block this filter writes render
correctly. `crates/app/vaco-probe/tests/frame_tags.rs` already proves the
render side (`show::tags` reproduces the reference's `[FRAME_TAGS]`/`"tags"`
block byte for byte, including "nothing to report → no tags at all"); this
crate's own tests prove the write side.

## Why `psnr`/`ssim`/`identity`/`msad` use `vaco-filter-core::Paired`, not `vaco-filter-framesync`

The brief suggested `vaco-filter-framesync` for these four (they all take a
`reference` input). Measured and found **not** to match:

```text
$ ffmpeg -h filter=psnr | tail -3
psnr AVOptions:
   stats_file        <string>     ..FV....... Set file where to store per-frame difference information
$ ffmpeg -h filter=alphamerge | tail -12
framesync AVOptions:
   eof_action        <int>        ...
   shortest          <boolean>    ...
   repeatlast        <boolean>    ...
   ts_sync_mode      <int>        ...
```

`psnr` (and `ssim`, `identity`, `msad`, checked individually) carries **no**
`eof_action`/`shortest`/`repeatlast`/`ts_sync_mode` section at all. That is
exactly the measurement `vaco-filter-core::adapt::Paired`'s own doc uses to
distinguish `framepack` (strict lockstep, no per-input timeline) from
`alphamerge` (the full framesync surface) — these four filters measure like
`framepack`, not like `alphamerge`. So they are
[`vaco_filter_core::adapt::PairedFilter`]s, and neither `vaco-filter-core`
nor `vaco-filter-framesync` needed a change.

## What was added to `vaco-filter-vdsp`

`git log --oneline -3 -- crates/filter/vaco-filter-vdsp` showed one prior
commit (`scene_sad`/`plane_sad`/`block_sad`/`normalised_sad`, for
`freezedetect`) before this crate started, and picked up a second commit
mid-wave (`comb_score`, for `vaco-filter-deinterlace`) landed by another
agent — both untouched by this crate's additions. This crate added two
kernels, per its own explicit invitation to extend rather than duplicate:

* `plane_sse` — sum of squared per-sample differences, `psnr`'s MSE
  numerator. A different reduction from `plane_sad`'s sum-of-absolute-
  differences (squaring changes which differences dominate), so not
  expressible as a transform of the existing sum.
* `identical_count` — `(same, total)` bit-exact sample counts, `identity`'s
  numerator/denominator.

`msad` needed **no** addition: it is exactly `normalised_sad`, already
there for `freezedetect`, reused directly — confirmed by measurement (see
below), not assumed from the name.

## Per-filter accounting

### `psnr` — byte-exact

Formula: `MSE = sum((a-b)^2)/n` per plane; `PSNR = 10*log10(255^2/MSE)`, or
the literal string `"inf"` when `MSE == 0`. `mse_avg` averages the
per-component MSEs **weighted by sample count**; `psnr_avg` is
`10*log10(255^2/mse_avg)` (not an average of the per-component PSNRs).

Distinguishing input: a flat pair (`128` vs `110`, every pixel) has a
closed-form MSE (`(a-b)^2` exactly, no averaging error), which independently
pins the `MAX=255` constant (not `256`) and the `log10` (not natural log)
choice — the self-identical case alone cannot catch either, since `0/0`-
style MSE hides both bugs. Measured against `ffmpeg 8.1`:
`mse.y="324.000000"`, `psnr.y="23.025354"`, matched exactly by this crate's
implementation and asserted in `psnr::tests::flat_pair_matches_the_closed_form`.
`mse_avg`'s sample-weighting was confirmed on an asymmetric yuv420p input
(luma differs, chroma does not): reference `mse_avg=21675.000000` matches
`(32512.5*256 + 0*64 + 0*64)/384` exactly and does **not** match the plain
mean (`10837.5`).

**Not reproduced**: tag insertion order for non-planar/non-YUV formats this
crate has not measured (only `gray`/planar-YUV/`gbrp` have measured
component labels — see `crate::video::component_labels`).

### `ssim` — implemented from the published paper; **not byte-exact against the reference on any input**

Implemented from Z. Wang, A. C. Bovik, H. R. Sheikh, E. P. Simoncelli,
"Image Quality Assessment: From Error Visibility to Structural Similarity",
*IEEE Transactions on Image Processing*, vol. 13, no. 4, pp. 600-612, April
2004 — an 11x11 circularly-symmetric Gaussian window (`sigma=1.5`, unit
sum), sliding with stride 1, no padding (a window only where it fully
fits), `K1=0.01`, `K2=0.03`, `L=255`.

This looked at first like it had a clean closed-form oracle: two *flat*
planes force zero variance/covariance everywhere, collapsing the formula to
`(2*mu_x*mu_y+C1)/(mu_x^2+mu_y^2+C1)`, independent of the windowing
algorithm entirely. Computed precisely for `128` vs `110`:
`(2*128*110+6.5025)/(128^2+110^2+6.5025) = 28166.5025/28490.5025 =
0.988628`, `dB = -10*log10(1-0.988628) = 19.441551` — exactly what this
crate's implementation produces (asserted in
`ssim::tests::flat_pair_matches_the_closed_form`).

**`ffmpeg 8.1` measures `0.988625`/`19.440596` on that same input.** A ~3e-6
discrepancy. A first, sloppier hand-check (rounding both six-digit numbers
by eye) missed it; re-deriving the fraction exactly with `python3` caught
it. Because the formula is forced to this exact value for *any*
zero-variance windowing, **the reference is not evaluating the textbook
floating-point Gaussian-window formula unmodified even in this degenerate
case** — most plausibly a fixed-point/quantised Gaussian kernel, which D7
forbids reading from source to confirm. Stated plainly: this crate's `ssim`
values are not byte-exact against the reference, on any input, including
the flat-field one. What is verified is that the implementation matches the
*published paper's* formula exactly.

`ssim.All` averages per-component scores **weighted by sample count** (like
`psnr`, unlike `identity`/`msad`) — confirmed the same way, on the same
asymmetric yuv420p fixture: reference `All=0.555556` matches the
sample-weighted combination of `Y=0.333334, U=1, V=1` and not the plain mean
(`0.777778`).

### `identity` — byte-exact

Formula: fraction of samples where `a == b` exactly, per plane —
**not** a continuous difference measure. Distinguishing input: half a
plane bit-identical, the other half differing by `1` (small) scores `0.5`;
the same layout differing by `255` (maximum) *also* scores `0.5` — a
continuous metric (`1 - mean_abs_diff/255`) would print `~0.998` and
`0.5` respectively, so this pair of inputs is what rules that hypothesis
out (a self-identical/fully-different pair alone cannot, since both
hypotheses agree at the extremes). Measured against `ffmpeg 8.1` and
matched exactly.

`identity_avg` averages per-component fractions **unweighted by sample
count** — the opposite of `psnr`/`ssim`. Confirmed on the same asymmetric
yuv420p fixture used for `psnr`: reference `identity_avg=0.833333` matches
the plain mean of `(0.5, 1.0, 1.0)` and not the sample-weighted combination
(`0.666667`).

**Not reproduced**: tag insertion order. Measured reference order is `V Y
U` for yuv420p and `B R G` for `gbrp` (neither ascending nor any obvious
rule found in the time available); this crate writes ascending order (`Y U
V` / `G B R`). Values are byte-exact; block order is not.

### `msad` — byte-exact

Formula: exactly `vaco_filter_vdsp::normalised_sad` (mean absolute
difference, normalised by `255`) per plane — confirmed by feeding a flat
pair differing by `18` (`18/255 = 0.070588...`) and matching the reference's
`msad.y="0.070588"` exactly; the self-identical case alone cannot pin the
normalisation constant (`0/255` and `0/256` are both `0`).

`msad_avg` averages unweighted, like `identity` and unlike `psnr`/`ssim` —
same asymmetric-fixture confirmation (`msad_avg=0.166667` matches
`(0.5+0+0)/3`, not `(0.5*256)/384=0.333333`). Same tag-order caveat as
`identity` (measured `V Y U` / `B R G`; this crate writes ascending order).

### `entropy` — byte-exact

Formula: Shannon entropy of the 256-bucket sample histogram
(`-sum(p_i*log2(p_i))`, `p_i=count_i/total`), `mode=diff` substituting
`|hist_i - hist_(i-1)|` for `hist_i`. `normalized_entropy` divides by `8.0`
(`log2(256)`) unconditionally, confirmed against the alternative of
dividing by `log2(distinct values present)` on a skewed three-value
fixture where the two disagree sharply (measured `0.064487`, matching
`entropy/8`; the alternative would give `0.3255`). `mode=diff` normalises
its histogram-of-deltas by `total` (the plane's own sample count), not by
`sum(delta)` — the same skewed fixture separates these (`0.691776`
measured, matching `/total`; `/sum(delta)` would give `0.631636`). One
genuine bug caught by this crate's own tests before it shipped: a
zero-entropy (single-bucket) plane computes `-1.0 * log2(1.0)`, which is
IEEE-754 **negative** zero, printing `"-0.000000"` — normalised to `0.0`
in [`crate::entropy::shannon`] rather than trusted to print the sign the
reference happens to use (not independently confirmed against the
reference's own zero-entropy formatting, since the flat-frame fixture used
to catch this is a fixture this crate built, not a probe of `ffmpeg`
itself — recorded honestly as an assumption, not a measurement).

Distinguishing input: a 16x16 plane holding every 8-bit value exactly once
forces a uniform 256-outcome distribution, whose entropy is `log2(256)=8`
as an algebraic identity — this alone cannot separate the two
`normalized_entropy` denominators above (both give `1.0` when all 256
buckets are used), which is why the skewed fixture is what the tests
actually assert against.

### `cropdetect` — byte-exact at the default `round`; other `round` values are a known, disclosed gap

`mode=black` only (`mode=mvedges` needs motion vectors — see interface gap
14). Scans the luma plane for the raw bounding box of every sample
`> limit` (the same `>`, not `>=`, `bbox` uses), accumulated as a **running
union** across frames rather than recomputed fresh each frame (confirmed:
`man ffmpeg-filters`'s own wording for `reset_count`, "0 indicates never
reset, and returns the largest area encountered during playback"). The
first `skip` frames (default `2`) are measured to carry no tags and not
contribute to the union at all. `w`/`h`/`x`/`y` floor the raw box to the
nearest multiple of `round`, centred; `x1`/`x2`/`y1`/`y2` stay raw and
unrounded. Confirmed exactly on a deliberately non-round-aligned rectangle
at the default `round=16` (raw `44x54` at `(10,5)` &#8594; `w=32,h=48,
x=16,y=8`) and at every power-of-two `round` tried (`2,4,8,16,32`).

**Not confirmed: `round` at several non-power-of-two values.** A sweep from
`round=1` to `round=44` against the same fixed `44x54` box found the plain
floor-and-centre rule diverges at `round &isin; {3,6,7,9,13,15}` (e.g.
`round=9` measures `w=36,h=36`; floor-and-centre predicts `w=36,h=54` — the
height prediction is wrong by 18, not a rounding-direction quibble). No
consistent alternative formula was found in the time available (a
chroma-plane-halved variant fixed some of these and broke matches the
plain formula already had). This crate ships plain floor-and-centre for
every `round` and documents the six known-divergent values here rather
than guessing further or silently narrowing the option's accepted range.

Distinguishing input: the same off-grid rectangle as above, extended with
a second, smaller rectangle in a later frame to confirm the reported box
is the running union (must stay at the first, larger rectangle) and not
just the current frame's own bounds — and a wholly-black frame in between
to confirm it does not reset the union to empty.

### `signalstats` — 25 of 28 keys, byte-exact for what is implemented

The reference exports 28 default keys (`man ffmpeg-filters`, `signalstats`
— `TOUT`/`VREP`/`BRNG` are `stat=`/`out=` option *values*, not keys the
filter emits unconditionally, and are **not implemented**: a per-pixel
outlier/repetition/broadcast-range classification pass this crate has not
measured). This crate implements 25: the original 15
`{Y,U,V}{MIN,LOW,AVG,HIGH,MAX}`, plus, added in the 2026-08-23 continuation
pass:

* `SAT{MIN,LOW,AVG,HIGH,MAX}` — the same percentile machinery as `Y*`
  above, over `floor(sqrt((U-128)^2+(V-128)^2))`. Confirmed on
  `color=red`/`blue`/`green` (`yuv420p`); green's `U=91,V=81` measures
  `SATMAX=59`, matching `floor(59.82)` and ruling out `round` (`60`) — red
  and blue's own values happen to floor and round identically, so green is
  the fixture that actually pins the rounding direction.
* `HUE{MED,AVG}` — `floor((atan2(U-128,V-128) in degrees + 180) mod 360)`.
  The `atan2` argument order and the `+180` offset were each checked
  against all three colours and are the only combination that fits (red:
  measured `161`; blue: `279`; green: `38`). `HUEMED` is implemented as the
  same 50%-cumulative-count rule `LOW`/`HIGH` use at 10%/90%, which is a
  reasonable generalisation but — unlike everything else in this crate's
  accounting — **not independently confirmed**: every fixture measured
  here is a flat colour field, where every notion of "median" and
  "average" coincide, so no probe available in the time budget actually
  exercised the difference.
* `{Y,U,V}DIF` — mean absolute difference against the previous frame
  (`0` before the first frame). Confirmed on a two-frame fixture where
  exactly half of `Y` changes by `20`: measured `YDIF=10` (the mean over
  *every* sample), ruling out "mean over only the changed samples" (which
  would give `20`).
* `{Y,U,V}BITDEPTH` — `popcount` of the bitwise OR of every distinct
  sample value present, formatted as a plain integer (not
  [`crate::fmt::g6`]). Confirmed on a flat plane at `100`
  (`0b1100100`, `YBITDEPTH=3`) and a two-level plane at `{100,120}`
  (`0b1111100`, `YBITDEPTH=5`) — the two-level fixture rules out
  "popcount of one representative sample". **This corrects a claim made
  when this crate first shipped**: the original text below said `BITDEPTH`
  "measured `1` for a constant plane", generalising from a fixture that
  happened to use `128` (`0b10000000`, exactly one set bit); `100` is an
  equally constant plane and measures `3`, so the true rule is
  value-dependent popcount, not "always `1` for any flat plane".

**Still not implemented**: `TOUT`/`VREP`/`BRNG` (see above).

Percentile rule (`LOW`=10th percentile, `HIGH`=90th, `man ffmpeg-filters`):
smallest value `v` whose cumulative histogram count is `>= total*fraction`.
Measured on a 10x10 plane holding every value `0..=99` exactly once:
`YMIN=0, YLOW=9, YAVG=49.5, YHIGH=89, YMAX=99`, matched exactly
(`signalstats::tests::uniform_ramp_matches_hand_computed_stats`). A second,
skewed-distribution test
(`signalstats::tests::skewed_distribution_uses_cumulative_count_not_sorted_index`)
rules out an alternative "10th/90th distinct value after sorting" reading
that the uniform case cannot distinguish (both agree when every value
appears exactly once).

`AVG` uses C's `%g` (six significant digits, trailing zeros trimmed) —
[`crate::fmt::g6`] — confirmed against three measured reference outputs
(`"49.5"`, `"61.5234"`, `"43.502"`), not [`crate::fmt::fixed6`], which would
print extra digits `signalstats` never does.

### `blackdetect` — byte-exact for the full-range case

`man ffmpeg-filters`, quoted verbatim: *"The filter also attaches metadata
to the first frame of a black segment with key `lavfi.black_start` and to
the first frame after the black segment ends with key `lavfi.black_end`
[...] This metadata is added regardless of the minimum duration
specified."* Confirmed: tags fire on every black/non-black transition
(`blackdetect::tests::transition_tags_land_on_the_documented_frames`
reproduces the reference's own 5fps transcript exactly, frame 0 and frame
5). `black_end`/`black_start` use the transitioning frame's own timestamp,
formatted with `freezedetect`'s exact `%.6f`-then-trim rule
([`crate::fmt::trimmed_time`]).

Distinguishing input: a run whose last black frame (pts 2) and breaking
frame (pts 10, not 3) are unevenly spaced tells "the tag carries the
breaking frame's own pts" apart from a wrong-neighbour hypothesis ("the last
black frame's pts") — the same class of bug `freezedetect`'s `freeze_end`
had, per `planning/AGENT-CONSTRAINTS.md`.

**Not implemented**: limited-range (`[16,235]`) pixel-black-threshold
scaling — this crate only implements the full-range (`[0,255]`) case; a
limited-range frame would need `vaco-color`'s range signalling threaded
through, left for a future extension.

### `blackframe` — byte-exact, including the exact-threshold boundary

`man ffmpeg-filters`, quoted verbatim: *"the percentage of pixels in the
picture that are below the threshold value"*, exported as
`lavfi.blackframe.pblack`. Measured to be a plain integer percentage
(`"100"`, not `"100.000000"`) and to **floor**, not round: a frame where
exactly 2 of 3 pixels are black (`66.67%`) prints `"66"`
(`blackframe::tests::percentage_floors_rather_than_rounds`) — a case
`round`/`floor` disagree on, unlike `100%`/`0%` where every rounding rule
agrees.

**Closed in the 2026-08-23 continuation pass**: this crate's own
falsification table (below) recorded the `sample <= threshold` boundary as
an untested edge — none of the original fixtures placed a sample exactly
at the default threshold (`32`). `blackframe::tests::sample_exactly_at_threshold_counts_as_black`
closes it: a sample exactly equal to `threshold` counts as black (`<=`,
matching the implementation already shipped, not `<`), verified by
falsifying the predicate to `<` and watching the new test fail.

### `bbox` — byte-exact

`man ffmpeg-filters`: *"the bounding box containing all the pixels with a
luma value greater than [`min_val`]"* — strict `>`, not `>=`, confirmed by
`bbox::tests::pixel_exactly_at_min_val_is_excluded` (added during this
crate's falsification pass: introducing `>=` breaks this test and no other,
since none of the other fixtures place a sample exactly at the default
`min_val=16` boundary). `x2`/`y2` are inclusive (measured: a 20-wide box at
`x1=10` reports `x2=29`, not `30`). A frame with nothing above `min_val`
carries no tags at all, matching the crate-wide "nothing to report, no
tags" convention.

## Formatting rules, and why there are three of them

[`crate::fmt`] documents (and tests) three distinct `lavfi.<filter>.<key>`
value-formatting rules found across this crate's ten filters — measured
individually rather than assumed to generalise from `freezedetect`'s rule,
per `planning/AGENT-CONSTRAINTS.md`'s caution that a formula fitting one
filter is not evidence for the next:

| Rule | Filters | Example |
|---|---|---|
| `%f`, six decimals, never trimmed | `psnr`, `identity`, `msad`, `entropy` | `"1.000000"` |
| `%.6f` then trim trailing zeros then a trailing `.` | `blackdetect` | `"0"`, `"1.000001"` |
| C's `%g`, six significant digits | `signalstats`, `cropdetect` (its `limit` field only) | `"49.5"`, `"61.5234"`, `"0.094118"` |

`blackframe`, `bbox` and `cropdetect` (its `x1`/`x2`/`y1`/`y2`/`w`/`h`/`x`/`y`
fields) use plain integer formatting (`u64`/`usize`/`i64` `to_string()`), a
fourth, degenerate case not needing its own helper. `signalstats`'s new
`*BITDEPTH` fields (added this pass) also use plain integer formatting,
not `g6` — see that section above.

## Two averaging rules, measured to disagree

`psnr`'s `mse_avg` and `ssim`'s `All` average per-component values
**weighted by sample count**; `identity`'s and `msad`'s `_avg` fields
average **unweighted**. Both are confirmed on the *same* asymmetric
yuv420p fixture (luma differs, chroma does not) fed to all four filters —
see [`crate::fmt::weighted_average`] and [`crate::fmt::simple_average`]'s
docs, and the per-filter sections above, for the exact numbers.

## Falsification

Six specific formula/constant choices were each deliberately broken, the
relevant test(s) confirmed to fail, then reverted:

| Change | Test(s) that failed |
|---|---|
| `psnr`'s `MAX` 255 → 256 | `psnr::flat_pair_matches_the_closed_form` (not `self_identical_is_infinite`, which cannot see the constant) |
| `weighted_average` divides by `values.len()` instead of `total_weight` | `fmt::weighted_average_matches_measured_psnr_mse_avg`, and (knock-on) `psnr::flat_pair_matches_the_closed_form` |
| `vaco-filter-vdsp::identical_count`'s `sa == sb` → `sa != sb` | `vaco-filter-vdsp::identical_count_distinguishes_from_normalised_sad`, `identity::self_identical_is_one`, `identity::average_is_unweighted` |
| `blackframe`'s `sample <= threshold` → `sample < threshold` | none of the existing fixtures sit exactly at the default threshold (32) — a real gap, left as a finding rather than silently claimed covered |
| `bbox`'s `sample > min_val` → `sample >= min_val` | none of the existing fixtures sit exactly at the default `min_val` (16) either — closed by adding `bbox::pixel_exactly_at_min_val_is_excluded`, which does fail under `>=` |
| `blackdetect`'s `now_black && !self.was_black` → `now_black && self.was_black` | `blackdetect::all_black_fires_mid_grey_does_not`, `blackdetect::transition_tags_land_on_the_documented_frames`, `blackdetect::frames_outside_a_transition_carry_no_metadata` |

The `blackframe`/`bbox` boundary gaps are recorded honestly rather than
silently left uncovered: `bbox`'s was already closed; `blackframe`'s
equivalent boundary is now closed too (see its section above).

**2026-08-23 continuation pass, six more:**

| Change | Test(s) that failed |
|---|---|
| `entropy`'s `MAX_ENTROPY` `8.0` → `7.0` | `entropy::uniform_all_values_once_scores_the_maximum`, `entropy::skewed_histogram_normalizes_by_eight_not_by_distinct_count`, `entropy::diff_mode_normalizes_deltas_by_total_not_by_their_own_sum` |
| `cropdetect`'s `sample > threshold` → `sample < threshold` | `cropdetect::non_aligned_rectangle_matches_the_reference_exactly`, `cropdetect::box_is_a_running_union_not_the_current_frame`, `cropdetect::all_black_frame_does_not_erase_the_accumulated_box` |
| `blackframe`'s `sample <= threshold` → `sample < threshold` (the gap this pass closed) | `blackframe::sample_exactly_at_threshold_counts_as_black` |
| `signalstats`'s saturation `floor` → `round` | `signalstats::green_saturation_floors_rather_than_rounds` (red/blue's own values floor and round identically, so only green catches this) |
| `signalstats`'s `bit_depth` (popcount of the OR of every value) → popcount of the first sample only | `signalstats::bitdepth_unions_every_distinct_value_present` (not `bitdepth_is_popcount_of_the_value_not_always_one_for_flat_planes`, which is flat and cannot see the difference) |
| `signalstats`'s `*DIF` (mean over every sample) → mean over only the changed samples | `signalstats::dif_averages_over_every_sample_not_just_the_changed_ones` |

One bug this pass's falsification-by-construction caught before it ever
needed a deliberate revert: `entropy`'s zero-entropy case computed IEEE-754
negative zero (`-1.0 * 0.0f64.log2()`) and printed `"-0.000000"`, failing
its own `flat_plane_is_zero_entropy`/`diff_mode_on_a_flat_histogram_is_zero`
tests on the first run, before any deliberate falsification — fixed by
normalising `-0.0` to `0.0` in [`crate::entropy::shannon`].

## Fuzzing

`fuzz/fuzz_targets/filter_analysis_options.rs` — arbitrary filtergraph text
against every registered name's option parser, routed through the real
`vaco_filter_graph::parse` pipeline (mirrors
`filter_temporal_options.rs`/`filter_denoise_options.rs` exactly). No
frames are constructed — this target is `create`-only, matching every other
filter-crate options fuzzer in this workspace, since no decoder exists to
hand it real pixel data (D5).

* Original eight names, 30 seconds: **513,730 executions, 0 crashes,
  `fuzz/artifacts/filter_analysis_options/` empty.**
* 2026-08-23 continuation pass, `NAMES` extended to ten (`entropy`,
  `cropdetect` added), 30 seconds under `cargo +nightly fuzz run
  filter_analysis_options -- -max_total_time=30`: **274,484 executions, 0
  crashes, `fuzz/artifacts/` empty.** `fuzz/fuzz_targets/filter_analysis_options.rs`
  is the only fuzz file this pass touched — `fuzz/Cargo.toml` needed no
  change (the `//! fuzz-crate: vaco-filter-analysis` marker and target
  registration already existed from the first pass), so `cargo xtask
  gen-fuzz` was not run this time.

## Not committed: generated/contended files

`crates/registry/vaco-registry/src/generated.rs`, `docs/README.md` and both
`Cargo.lock`s are regenerated/contended across the concurrent agents in
this tree and are left out of both this crate's commits, per this crate's
brief and `planning/AGENT-CONSTRAINTS.md`'s own guidance to leave them for
the orchestrator to sweep. `cargo xtask gen-registry` was run locally to
validate this pass's `vaco-component.toml` fragment (adding `entropy`/
`cropdetect`) and succeeded.

`cargo run -p xtask -- dup-check` is clean as of this pass (`47 shared
names, all accounted for, 0 known duplicates`) — the `GeneratorRegistry`
duplicate this doc previously recorded as pre-existing has since been
resolved by another agent; not this crate's change to claim.
