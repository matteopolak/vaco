# vaco-filter-motion

`planning/16-filters.md` §4.2's `vaco-filter-motion` row: `mestimate`,
`minterpolate`, `framerate`, `deshake`. The crate did not exist when first
claimed (no `crates/filter/vaco-filter-motion` directory, no row in
`planning/ASSIGNMENTS.md`) — genuinely unclaimed T3 long-tail work under
epic #57 (FT-4.12). Extended later, in the same crate, with `stabdetect`/
`stabtransform` — this crate's own two-pass stabiliser, matching the same
row's note that a `vidstabdetect`/`vidstabtransform`-equivalent pair
belongs here.

## What it is

Four filters, one module each (`framerate.rs`, `deshake.rs`,
`stabdetect.rs`, `stabtransform.rs`), registered through
[`registry::MotionRegistry`](../../crates/filter/vaco-filter-motion/src/registry.rs).
`common.rs` holds what `deshake` and the `stabdetect`/`stabtransform` pair
share: the `3x3` grid median block-motion estimate
([`common::estimate_motion`], over
[`vaco_filter_vdsp::motion::search_block`]), the translation-only warp
([`common::warp_translate`], over `vaco_filter_vdsp::affine`), and the
`edge`/`crop` border policy ([`common::EdgeMode`]). Extracted into
`common.rs` rather than duplicated across the two call sites, per this
project's "grep for the concept before writing it" rule applied *within*
one crate.

## `framerate`

A real, working frame-rate converter that blends between the two
bracketing input frames rather than merely duplicating (see
`vaco-filter-video-format::fps` for the duplicate-only sibling). **Not**
the reference's own algorithm: the reference does a block-level
motion-compensated blend by default; this ships a plain per-pixel linear
cross-fade with a whole-frame scene-cut gate (via
[`vaco_filter_vdsp::normalised_sad`]) that falls back to nearest-frame
selection across a cut — a real, named divergence, not a partial
implementation of the reference's own algorithm.

## `deshake`

Single-pass, causal, translation-only stabilisation: the shared grid
motion estimate feeds a per-frame translation, an exponential moving
average (fixed `alpha = 0.15`) tracks the "intentional" path, and the
frame is warped back toward it. The reference's own `deshake` does a full
affine (rotation + zoom) correction; this is translation-only, named as
such. Verified by a property test (a synthetic jittery sequence's
frame-to-frame difference goes down, not up — the one property that
distinguishes a working stabiliser from one with the correction direction
backwards), not a framecrc comparison, because there is no reference
algorithm to reproduce.

## `stabdetect` / `stabtransform`

This crate's own two-pass stabiliser: `stabdetect` (pass 1) writes a
transform file, `stabtransform` (pass 2) reads it and applies smoothing
plus a warp. **Not** the reference's `vidstabdetect`/`vidstabtransform`,
and deliberately so — see the finding below.

### Why not `vidstabdetect`/`vidstabtransform`, and why probing settled it

`vidstabdetect`/`vidstabtransform` need `libvidstab` (GPL) compiled into
the reference. Measured directly: this environment's own
`ffmpeg -h filter=vidstabdetect` reports `Unknown filter` (`ffmpeg 8.1`,
built without `--enable-libvidstab`) — there is no reference binary to
probe against for this pair at all, not merely a licence reason to avoid
building one. `planning/16-filters.md`'s row already anticipated this:
register `stabdetect`/`stabtransform` under this crate's own names and do
not claim `.trf` file-format compatibility. Consequently there is no
framecrc oracle for either filter; both are verified the same way
`deshake` is, by a jitter-reduction property test — including one that
drives the *real* file `stabdetect` writes into a real `stabtransform`
(`registry::tests::stabdetect_then_stabtransform_reduces_jitter_through_a_real_file`),
so the two filters' independently-designed file format is checked against
itself, not just against each module's own hand-built fixtures.

The option *names* below (`result`, `shakiness`, `mincontrast`,
`accuracy`, `stepsize`, `tripod`, `input`, `smoothing`, `optalgo`,
`maxshift`, `crop`, `invert`, `relative`, `zoom`, `optzoom`, `zoomspeed`,
`interpol`) are taken from `ffmpeg`'s own published user documentation
(`ffmpeg-filters.html`, Tier A per `planning/research/07-legal-patents-licensing.md`
§1.6.1 — published man-page-equivalent text is always open, D7), so a
filtergraph string written against the familiar vocabulary parses. The
file format and the algorithm behind every option are original.

### Transform file

`vaco-stab-transforms v1` (checked by `stabtransform`; a file starting
with anything else — including a real `.trf` file — is rejected with a
clear message rather than silently misparsed): a magic first line, then
one `dx dy` pair per frame, in plain decimal text — the frame's motion
relative to its reference (previous frame, or a fixed tripod frame),
measured in luma pixels. `fileformat` (ascii/binary) is parsed but this
crate always writes the plain-text form.

### `stabdetect` (pass 1)

Per frame, the shared grid motion estimate against either the previous
frame (default) or a single fixed reference frame captured at
`tripod`'s 1-based index (matching the reference's own documented
semantics: "compensate all movements ... and keep the camera view
absolutely still"). `shakiness` (1-10) widens the search range
(`range = shakiness * 4`); `mincontrast` is honoured directly — a frame
whose overall luma contrast falls below `mincontrast * 255` reports zero
motion rather than search noise. `accuracy`/`stepsize` are parsed but do
not change behaviour: the shared search is already exhaustive within its
range, so there is no accuracy-vs-speed knob to connect them to honestly.
Never modifies pixel data — every frame passes through unchanged, exactly
like the reference's own pass 1.

### `stabtransform` (pass 2)

Reads the whole file at creation (small: two `f64`s of text per frame,
and pass 1 has already finished by the time pass 2 starts), computes the
absolute trajectory (running sum of the relative vectors) and a
**centred** moving-average smoothing of the whole trajectory with window
radius `smoothing` (`smoothing=10` averages 21 samples — the reference's
own documented `value*2+1` window). This is the genuine two-pass
advantage over `deshake`'s causal-only exponential average: with the
whole path known up front, the smoothed path can use future samples too.
Verified directly
(`stabtransform::tests::centred_smoothing_uses_future_samples_a_causal_average_cannot`):
a single spike surrounded by zeros is visibly damped by its still-zero
neighbours on *both* sides, which a causal average cannot do until after
the spike has already happened. `smoothing=0` is the reference's own
documented "static camera" special case (smoothed path held at the
trajectory's own start for every frame) and is what `tripod=1` maps onto.

Only `optalgo=avg` (moving average) is implemented; `optalgo=gauss` (the
reference's own default) is parsed but not distinguished from `avg` — a
named scope cut. The correction (`trajectory - smoothed`, the exact sign
convention `deshake`'s own test already proved correct — getting this
backwards makes the sequence *more* jittery, and did during development,
see below) is clamped to `maxshift`, negated when `invert=1`, and applied
with the shared translation-only warp. `zoom`/`optzoom`/`zoomspeed`/
`interpol`/`relative` are parsed for option-surface completeness and do
not change behaviour.

## A real bug, caught by the crate's own test before it shipped

The first version of `stabtransform`'s correction formula computed
`smoothed - trajectory` (backwards from `deshake`'s own proven
`trajectory - smoothed`). `stabtransform`'s jitter-reduction test failed
immediately and honestly: `raw=419780 corrected=801978` — the "corrected"
sequence was *more* jittery than the input, the exact failure mode the
test exists to catch. Fixed by flipping the subtraction order to match
`deshake`'s convention; the same test then passed. Left in the module doc
as a fix worth naming, per this project's own "every fix falsified"
convention — the bug was caught by running the test, not by inspection.

## Not attempted

`mestimate` (report a dense per-macroblock motion vector field as the
reference's own diagnostic side-data) and `minterpolate` (needs that same
field plus an occlusion-aware bidirectional interpolator: `dup`, `blend`,
`mci` modes) — both substantially larger than this crate's other four
filters and not reached in this pass's time budget. Free for a future
pass; `vaco_filter_vdsp::motion::search_block` is already the primitive
either would build on.

## How to change it

- New motion filters go in their own `src/<name>.rs`, registered in
  `src/registry.rs` and `vaco-component.toml`.
- Extend `common.rs` for any new grid-search, median or translation-warp
  need shared across filters; a second private copy of any of these three
  is exactly the duplication this doc's own history (the `deshake`
  refactor) exists to prevent.
- If `mestimate`/`minterpolate` are picked up, `common::estimate_motion`'s
  single motion vector is not the right shape for either — both need a
  genuine per-block field, not a single median.

## Configuration

No crate-level configuration. Per-filter options are documented in each
module's own doc comment and match `ffmpeg -h filter=<name>`'s or (for
`stabdetect`/`stabtransform`) `ffmpeg-filters.html`'s option table for
every option name, default and range — including options this crate
parses but does not act on, called out explicitly rather than silently
accepted.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-vdsp` (`motion::search_block`, `affine`,
`plane_sad`, `normalised_sad` — reused, not duplicated).

## Fuzzing

`fuzz/fuzz_targets/filter_motion_options.rs` (option parsing for all four
registered names through the real filtergraph parser). Not yet extended
for `stabdetect`'s/`stabtransform`'s file-path options — both accept an
arbitrary path the same way `vaco-filter-deinterlace::fieldhint` already
does, so the same fuzz-target shape applies and is a real, named follow-up
rather than an oversight.
