# vaco-filter-temporal

T1/T2 temporal and interleave filters (FT-4.12b, GitHub issue #475):
`framestep`, `tpad`, `tmix`, `tblend`, `tmedian`, `tlut2`, `tmidequalizer`,
`decimate`, `mpdecimate`, `deflicker`, `lagfun`, `freezedetect`,
`freezeframes`, `dejudder`, `fsync`, `random`. `fps` — the seventeenth name
in `planning/16-filters.md` §4.3's row — is **not** registered here: it
already exists as `vaco_filter_video_format::fps`, and this crate's brief
explicitly said not to register it a second time.

## Group membership, checked rather than assumed

Every one of the row's seventeen names was checked against
`ffmpeg -hide_banner -filters` and `ffmpeg -h filter=<name>` (ffmpeg 8.1,
2026-08-23). All seventeen exist in the reference with exactly the arity the
row implies (`freezeframes` is the row's one `VV->V` filter; every other
name is `V->V`, `N->V`, or `V->N`/`V->V` with dynamic pad count for
`decimate`). Nothing in the row is missing from the reference, and nothing
in the reference's temporal-filter family is missing from the row — the plan
matched the binary exactly here, in both directions.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::TemporalRegistry`](../../crates/filter/vaco-filter-temporal/src/registry.rs)
— the same shape `vaco-filter-denoise` and `vaco-filter-audio-eq` use.
`src/video.rs` is the shared plane-decode/encode helper (`PlaneBuf`, an
`f32` view of one plane, plus small option-parsing helpers) most filters are
built on; it is a line-for-line-equivalent duplicate of
`vaco-filter-denoise::video`'s `PlaneBuf`/`sample_layout`, not a shared
dependency — see that module's doc for why. `src/rng.rs` is a small,
dependency-free SplitMix64 generator `random` uses instead of pulling in a
`rand`-family crate for one filter.

## `vaco-filter-vdsp`: a new crate, created by this brief's own instruction

The row's extra-deps column calls for `vdsp (scene_sad)`. Neither a
`scene_sad` implementation nor the `vaco-filter-vdsp` crate plan
`16-filters.md` §4.1 places it in existed anywhere under `crates/filter/`
when this work started (`grep -rln scene_sad crates/filter` found nothing).
Per this crate's brief, the kernel is written where the plan says it lives
rather than duplicated inside `vaco-filter-temporal`:
[`vaco-filter-vdsp`](../../crates/filter/vaco-filter-vdsp/src/lib.rs) now
exists with exactly `plane_sad`, `block_sad` and `normalised_sad` — the
minimum this crate's three consumers (`decimate`, `mpdecimate`,
`freezedetect`) need. `framerate`'s real motion-compensated blend, `scdet`,
`identity`/`msad` and `minterpolate` still need the rest of §4.1's `vdsp`
kernel set (`edge_common`, `motion_estimation`, the box-blur core,
`transform`) and should extend this crate rather than re-deriving the same
sums.

## Multi-input filters go through `vaco-filter-framesync`

`freezeframes` is this row's one two-video-input filter. It is built on
`vaco_filter_framesync::{FrameSyncFilter, Synced}` with `FsInput::dual`
roles (input 0 drives, input 1 is sampled) — the same shape
`overlay`/`blend`/`lut2` use — rather than a hand-rolled two-pad `Filter`
impl. See [`freezeframes.rs`](../../crates/filter/vaco-filter-temporal/src/freezeframes.rs)'s
module doc for how "the frame at index `replace`" is reconstructed from
`FrameSync`'s timestamp-based sampling, which has no native "Nth frame of
stream 2" primitive.

Despite the name, `tlut2` does **not** need framesync: it was measured
(`ffmpeg -h filter=tlut2`'s single `#0: default` pad, confirmed with a
two-frame raw-video probe) to be temporal — one input, comparing the current
frame against its own immediately preceding one — unlike the two-*stream*
`lut2` it is named after.

## `freezedetect`: metadata export (interface gap 11, closed)

`freezedetect` only ever had one output channel worth reporting to a
caller — the reference writes `lavfi.freezedetect.freeze_start`,
`.freeze_duration` and `.freeze_end` into the frame's metadata dictionary,
and until `vaco_frame::Frame` grew one, this filter's `Filter::events()` accessor was the only way to get the
same information out — real detection, but not the reference's export
mechanism, and unreachable from anywhere that only sees `Frame`s (`vaco-probe`,
`metadata`/`select`'s `metadata()` expression function once those exist).

Now wired: `step` calls `frame.set_metadata(...)` at the same two points
`events()` already tracked them (the confirming frame gets `freeze_start`
alone; the frame that breaks the run gets `freeze_duration` then
`freeze_end`, together). `events()` stays, for tests only — comparing a
`Vec<FreezeEvent>` is less code than parsing tags back out of a `Frame`.

Two things were measured against `ffmpeg 8.1` rather than assumed, because
both are easy to get subtly wrong:

- **Value formatting**: six decimal digits, trailing zeros trimmed, then a
  bare trailing `.` trimmed — `0.0` prints `0`, not `0.000000`; `1.001000`
  prints `1.001`; `1.000001` keeps all six digits. See
  `freezedetect::format_lavfi_time`.
- **Which frame's timestamp `end`/`duration` use**: the frame that *breaks*
  the freeze (the first one that differs again), not the last frozen frame.
  The two are indistinguishable at a uniform frame rate — a uniform-rate test
  alone would have passed with either formula, the same shape of false
  confirmation `tblend`'s 256-vs-255 divisor mistake was. Distinguished with
  an irregular-timestamp test
  (`freeze_end_uses_the_breaking_frame_not_the_last_frozen_one`) and
  confirmed on the real binary at 10 fps, at `29.97`, and at `24000/1001`.

A frame with nothing to report carries no `Metadata` side-data entry at all
(`Frame::metadata()` returns `&[]`), not an empty one — and a freeze still
open when the stream ends never gets an `end`/`duration` tag, matching the
reference, which has no later frame to attach one to either.

## Correctness discipline: independent oracles per filter

| Filter | Independent oracle |
|---|---|
| `framestep` | `step=1` is the identity, byte-for-byte; kept-frame count on a synthetic stream is `ceil(L/step)`, and kept pts values are an arithmetic progression — both counted directly, not re-derived from the filter's own state. |
| `tmix` | `frames=1` is the identity; three known constant frames average to their hand-computed arithmetic mean. |
| `tblend` | Measured against the reference (see below) for 22 of 40 blend modes, each cross-checked in tests against an *independently written* integer formula (not a call back into `Mode::apply`); `average`/`multiply` on constant frames are the brief's named closed forms. |
| `tmedian` | An odd trailing window of constant frames medians to that constant, for any correct median; distinct values pick the middle one by hand. |
| `tlut2` | Default `c0..c3="x"` is the identity on every frame, including the first (this crate's documented `y := x` choice for "no previous frame yet"); `c0="y"` reproduces the previous frame's sample exactly. |
| `tmidequalizer` | `sigma=0` is the identity for any window; a constant-brightness stream is unaffected by `sigma` (its own trailing mean is itself). |
| `decimate` | A synthetic stream of `cycle`-frame groups, each holding one byte-identical duplicate pair (metric exactly `0.0`), drops exactly one frame per group — `total - total/cycle`, computed independently and checked against actual output length. |
| `mpdecimate` | A stream of `N` distinct values each repeated `k` times keeps exactly `N` frames, independent of `k` — the first occurrence of each value is never a duplicate of anything before it, and every exact repeat scores zero block SAD unconditionally. |
| `deflicker` | A constant-brightness stream is the identity (every mean of a constant sequence is that constant); a single bright outlier is scaled *down* toward the window's hand-computed arithmetic mean, never up. |
| `lagfun` | Pinned against a measured 5-frame reference byte sequence (`200,50,50,200,0` through `decay=0.5` → `200,100,50,200,100`); a non-decreasing stream is the identity (`max(in, decayed_prev) == in` whenever `in` never drops); `decay=0` is the identity after frame one. |
| `freezedetect` | A genuinely frozen synthetic stream (identical frames spanning more than `duration`) must report a freeze event; a moving one (a ramp) must report none; a freeze shorter than `duration` never confirms. Metadata placement/formatting checked separately against `ffmpeg 8.1` — see the section below. |
| `freezeframes` | With known `first`/`last`/`replace` and a five-frame source against a one-frame replace stream, the expected output sequence is computed directly from the inputs and compared to the filter's actual output. |
| `dejudder` | Alternating instantaneous durations (`2,4,2,4,...`) settle, once the window fills, to their hand-computed mean (`3`); a perfectly even stream is unaffected. |
| `fsync` | A target list with a repeated timestamp duplicates the held frame exactly that many times — output count from input count is predictable and counted directly, mirroring `fps`'s own upsampling oracle. |
| `random` | Output is a **permutation** of the input multiset (every shuffle, correct or not, preserves this) — checked via a sorted-multiset comparison of a tagged synthetic stream; a stream shorter than the cache never shuffles at all, so it must equal the input in order. |
| `tpad` | `start=0, stop=0` is the identity; `start_mode=clone`/`stop_mode=clone` reproduce the edge frame byte-for-byte; `add` mode fills black (sample `0`) on `gray8`. |

## `tmix=frames=1`: measured identity fast path

The exact default-weight, auto-scale `frames=1` case returns the newest frame
directly. This preserves the existing copy-on-write ownership and avoids
decoding and re-encoding every plane through `PlaneBuf`; weighted windows and
explicit scales still use the general mixing path. On a 640×360 `yuv420p`
stream (120 frames), ten rotated A/B/ffmpeg rounds measured median named
`CPU Counters` Cycles of 0.067× and CPU-seconds of 0.962× versus the
unoptimised path; wall time was 0.993×. The candidate was 0.559× ffmpeg's
Cycles in the same session. Output was byte-exact against both the
unoptimised binary and ffmpeg, with identical output at `-threads 1`, `2`,
`4`, and `8`. The weighted `frames=3:weights=1 2 1` path was also unchanged
byte-for-byte by the candidate.

## `tblend`: measured, not guessed, and only where measurement succeeded

`ffmpeg -h filter=tblend` lists 40 named blend modes with no documented
per-mode formula. Rather than transcribe a blend-mode reference from memory
(D7/D17: measure the shipped binary, never recall), every formula this crate
implements was pinned by feeding `ffmpeg -f rawvideo -pix_fmt gray -s 1x1`
sequences of known byte values through `-vf tblend=all_mode=<mode>` and
reading the exact output bytes back. First, which operand is which:
`tblend=all_expr=A`/`=B` on a two-frame `[0x32, 0xc8]` stream showed `A` is
the *current* frame and `B` the *previous* one. Then a 10-value probe
sequence (`0,255,128,64,192,32,160,96,224,16`, each consecutive pair a fresh
`(A,B)`) pinned the exact integer formula per mode, including which of
`floor`/`round`/`ceil` — and, for `dodge`/`burn`, that the reference divides
by **256**, not 255: a first hypothesis (255-denominator, `ceil`) fit 8 of 9
probed points and was wrong on the ninth (`A=224,B=96` measured `74`, not
the `73` that hypothesis predicts). Solving algebraically from that one
disagreement, per `AGENT-CONSTRAINTS.md`'s "two probes that disagree are not
noise", pointed at a 256-denominator formula that then fit all nine points
for both filters (`dodge` ceiling its quotient, `burn` flooring its own).

22 of 40 option values are implemented this way: `normal`, `average`,
`addition`, `addition128`/`grainmerge` (confirmed identical), `subtract`,
`multiply`, `multiply128`, `screen`, `darken`, `lighten`, `difference`,
`difference128`/`grainextract` (confirmed identical), `negation`,
`exclusion`, `overlay`, `hardlight`, `dodge`, `burn`, `and`, `or`, `xor`,
`divide`. The remaining 17 (`phoenix`, `pinlight`, `reflect`, `softlight`,
`vividlight`, `hardmix`, `glow`, `heat`, `freeze`, `extremity`,
`softdifference`, `geometric`, `harmonic`, `bleach`, `stain`, `interpolate`,
`hardoverlay`) are recognised names that return a clean `Unsupported`-style
error at creation rather than a guessed formula — `cN_expr`, evaluated
through `vaco-expr` with `A`/`B` bound, covers arbitrary custom blends in
the meantime. `opacity` (`out = A*(1-opacity) + mode_result*opacity`) was
measured the same way and is implemented for every mode, confirmed on
`multiply`.

`tblend`'s mode math was measured only at 8-bit sample depth (`max_val ==
255`). For any other depth, the measured formula is applied after rescaling
samples into the 8-bit domain and rescaling the result back out — exact at
8-bit (a no-op rescale) and the natural depth-proportional generalisation
elsewhere, but **not itself reference-verified beyond 8-bit**, and
meaningless for the bitwise modes (`and`/`or`/`xor`) at any depth but 8.

## Structural simplifications, named rather than silently approximated

| Filter | Gap |
|---|---|
| `tmedian`, `tmidequalizer` | Trailing window (`2*radius+1` input frames ending at the current one) instead of the reference's centred window, matching `vaco-filter-denoise::atadenoise`'s documented choice for the same reason: zero added latency, no special-cased stream edges, at the cost of the reference's frame alignment. |
| `tmidequalizer` | Implements "pull toward the trailing temporal mean by `sigma`", not the reference's histogram-domain Temporal Midway Equalization — real temporal smoothing, not a byte-identical algorithm. |
| `decimate` | The similarity metric is a whole-plane `normalised_sad`, not the reference's per-`blockx`×`blocky`-block grid; those two options are parsed and stored but do not sub-divide the frame. `ppsrc` (a second, pre-processed input for the metric) is not implemented — this crate's `decimate` is always single-input. |
| `mpdecimate` | The per-8×8-block metric's unit is a documented choice (plain summed absolute luma difference, `0..=16320`), not a measured match to the reference's internal `hi`/`lo` scale — the reference's own `-h` output gives no unit for either. The threshold *logic* (compare every block to `hi`/`lo`, drop iff no block exceeds `hi` and the `lo`-exceeding fraction is under `frac`) is the part this crate is confident in. |
| `dejudder` | Smooths timestamps to a cycle-averaged rate rather than reproducing the reference's specific pulldown-pattern-aware correction. |
| `fsync` | The target-timestamp file format is this crate's own definition (one seconds value per line, `#`-comments, blank lines skipped) — every line format tried against the reference (plain integers, decimal seconds) failed identically with `Invalid data found when processing input`, which did not disambiguate the reference's actual grammar within this pass's budget. |
| `random` | A real reservoir shuffle (fill a cache, then swap-and-emit), seeded reproducibly with `SplitMix64` — but not the reference's own bit stream, which would require reading its source. |
| `tpad` | Colour parsing covers `black`/`white`/six primary-secondary names/`#rrggbb(aa)` hex — no `vaco-filter-draw` crate exists yet for a fuller palette, and this row's dependency list does not call for adding one. `start_duration`/`stop_duration` (time-based padding) are accepted and ignored: this crate has no negotiated-frame-rate access at option-parse time. |
| `freezeframes` | "The frame at index `replace`" is reconstructed by counting visible changes in `FrameSync`'s time-sampled secondary input, exact for a `replace` input holding one still frame per "chapter" (the filter's evident purpose) but not a general frame-index primitive. |

## Configuration

No crate-level configuration; every option above is the filter's own
`ffmpeg -h filter=<name>` surface, parsed via `Instantiate::named` (see
`src/video.rs`'s `str_opt`/`f64_opt`/`i64_opt`/`usize_opt`/`bool_opt`/
`planes_mask_opt` helpers) or, for `tblend`/`tlut2`, through `vaco-expr`
with this crate's own variable bindings (`A`/`B` for `tblend`, `x`/`y` for
`tlut2`).

## How to change it

- Add a filter: new `src/<name>.rs` module exporting `pub const DESC` and
  `pub(crate) fn create`, registered in `src/registry.rs`'s `NAMES` and
  `create` match, and a `[[component]]` row appended to
  `vaco-component.toml` (only after `DESC` is actually exported — see
  `AGENT-CONSTRAINTS.md`'s registry-ordering warning), then
  `cargo xtask gen-registry`.
- Add a `tblend` mode: measure it first (probe pairs the way this doc
  describes), add a `Mode` variant, its `from_name` mapping and its
  `apply` arm, and a fixture-pair test — do not add a mode without a
  measurement backing it.
- Extend `vaco-filter-vdsp`: it is intentionally minimal today (three
  functions). Whoever implements `framerate`, `scdet`, `identity`/`msad`
  or `minterpolate` should add their kernel there rather than duplicating
  scene-difference math a second time.

## Dependencies

`vaco-core`, `vaco-expr`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync`, `vaco-filter-vdsp`, `smallvec`.

## Fuzzing

`fuzz/fuzz_targets/filter_temporal_options.rs` drives arbitrary filtergraph
text through every one of this crate's sixteen registered names via the real
`vaco_filter_graph::parse` pipeline (not a hand-built `Instantiate`), the
same shape `filter_denoise_options.rs` uses. 30 seconds, ffmpeg 8.1-era
build: **320,041 executions, 0 crashes**, `fuzz/artifacts/` empty.
