# vaco-filter-analysis

T2/T3 video analysis and detection filters (FT-4.12d, GitHub issue #477):
`psnr`, `ssim`, `identity`, `msad`, `signalstats`, `blackdetect`,
`blackframe`, `bbox` — eight of `planning/16-filters.md` §4.3's
twenty-three-filter row.

## Row membership, checked against the reference

All twenty-three names in the plan's row (`psnr, ssim, ssim360, xpsnr, vif,
vmafmotion, msad, identity, blackdetect, blockdetect, bitplanenoise,
entropy, siti, signalstats, readeia608, readvitc, showinfo,
photosensitivity, scdet, bbox, codecview, blackframe, cropdetect,
signature`) were checked against `ffmpeg -hide_banner -filters` and
`ffmpeg -h filter=<name>` (ffmpeg 8.1, 2026-08-23). **All twenty-three exist
in the reference with that exact name.** The row matches the reference in
both directions — nothing to add, nothing to drop.

## What landed, and why the other fifteen did not

**Landed** (eight): `psnr`, `ssim`, `identity`, `msad`, `signalstats`,
`blackdetect`, `blackframe`, `bbox`.

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
  shares your misreading" risk (`planning/AGENT-CONSTRAINTS.md`).
* `blockdetect`, `bitplanenoise`, `entropy`, `siti`, `photosensitivity`,
  `scdet`, `cropdetect` — individually tractable (each was partially
  measured while scoping this crate, see "Partial measurements recorded for
  a follow-up" below) but did not fit this wave's time budget once the
  eight landed filters were verified to the standard this crate's brief
  demands.

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

### `signalstats` — partial, byte-exact for what is implemented

The reference exports 29 keys (`man ffmpeg-filters`); this crate implements
15: `{Y,U,V}{MIN,LOW,AVG,HIGH,MAX}`. **Not implemented**: `SAT*`/`HUE*` (no
pinned-down saturation/hue definition over YUV samples in the time
available — left out rather than guessed, per this crate's own
false-confirmation caution), `*DIF` (temporal, needs previous-frame state),
`*BITDEPTH` (measured `1` for a constant plane and `8` for a full-range
gradient, ruling out the naive `ceil(log2(distinct values))` reading, but
not pinned down further).

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

### `blackframe` — byte-exact

`man ffmpeg-filters`, quoted verbatim: *"the percentage of pixels in the
picture that are below the threshold value"*, exported as
`lavfi.blackframe.pblack`. Measured to be a plain integer percentage
(`"100"`, not `"100.000000"`) and to **floor**, not round: a frame where
exactly 2 of 3 pixels are black (`66.67%`) prints `"66"`
(`blackframe::tests::percentage_floors_rather_than_rounds`) — a case
`round`/`floor` disagree on, unlike `100%`/`0%` where every rounding rule
agrees.

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
value-formatting rules found across this crate's eight filters — measured
individually rather than assumed to generalise from `freezedetect`'s rule,
per `planning/AGENT-CONSTRAINTS.md`'s caution that a formula fitting one
filter is not evidence for the next:

| Rule | Filters | Example |
|---|---|---|
| `%f`, six decimals, never trimmed | `psnr`, `identity`, `msad` | `"1.000000"` |
| `%.6f` then trim trailing zeros then a trailing `.` | `blackdetect` | `"0"`, `"1.000001"` |
| C's `%g`, six significant digits | `signalstats` | `"49.5"`, `"61.5234"` |

`blackframe` and `bbox` use plain integer formatting (`u64`/`usize`
`to_string()`), a fourth, degenerate case not needing its own helper.

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
silently left uncovered: `bbox`'s is now closed by the added test;
`blackframe`'s equivalent boundary (a sample exactly equal to `threshold`)
remains an untested edge, left for a follow-up.

## Fuzzing

`fuzz/fuzz_targets/filter_analysis_options.rs` — arbitrary filtergraph text
against all eight registered names' option parsers, routed through the real
`vaco_filter_graph::parse` pipeline (mirrors
`filter_temporal_options.rs`/`filter_denoise_options.rs` exactly). No
frames are constructed — this target is `create`-only, matching every other
filter-crate options fuzzer in this workspace, since no decoder exists to
hand it real pixel data (D5). 30 seconds under `cargo +nightly fuzz run
filter_analysis_options -- -max_total_time=30`: **513,730 executions, 0
crashes, `fuzz/artifacts/filter_analysis_options/` empty.**

## Not committed: generated/contended files

`crates/registry/vaco-registry/src/generated.rs` and `docs/README.md` were
regenerated locally to validate this crate's `vaco-component.toml` fragment
(`cargo xtask gen-registry` succeeded, wrote 510 components) but are left
out of this crate's commit, per this crate's brief — both are contended
across the six concurrent agents in this tree. `fuzz/Cargo.lock` was
likewise regenerated (`cargo xtask gen-fuzz`) but left out: its diff pulls
in lockfile entries for other agents' crates that had not been regenerated
into it yet, which is not this crate's change to carry. `fuzz/Cargo.toml`
**is** committed — its regenerated diff is a single, clean, alphabetically-
inserted block for this crate alone.

`cargo run -p xtask -- dup-check` reports one pre-existing failure
(`GeneratorRegistry` defined in both `vaco-filter-asource` and
`vaco-filter-source`) unrelated to this crate — not touched, per the
single-writer rule.
