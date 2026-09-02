# `vaco-resample`

Implements `planning/17-scale-resample-tx.md` Part B. The `swresample` equivalent:
sample-format conversion, channel rematrixing, and polyphase rate conversion.

---

## 1. What it is

Four independently usable stages and one composition of them.

| Item | Job |
|---|---|
| `convert::convert` | element type and packed ↔ planar, in one pass |
| `build_matrix` / `Rematrix` | channel layout remapping and mixing |
| `RateConvert` | stateful polyphase sample-rate conversion |
| `Dither` | quantisation dither for the float → integer path |
| `Resampler` | all four, wired together, with the streaming contract |

A codec that only needs `s16 → f32` calls `convert::convert`. A mixer that only
needs 5.1 → stereo builds a `MixMatrix`. The reference exposes only the fused
context; exposing the parts costs one extra layer of composition and is strictly
better.

---

## 2. How it works

### 2.1 The pipeline

```
input bytes ──▶ read_planes ──▶ [rematrix] ──▶ [rate convert] ──▶ [dither] ──▶ write_planes ──▶ output bytes
                (any format)     f32 or f64 planar, internally               (any format)
```

Rematrixing runs on whichever side has fewer channels: folding 5.1 to stereo
before the resampler is a 3x saving on the expensive stage, and up-mixing
reverses the argument. `Pipeline::stage` holds both orderings so they cannot
drift apart.

**The direct path.** When there is no rematrix, no rate change and no dither, the
whole operation is a format conversion and it bypasses the internal float type
entirely. That is not an optimisation: it is what keeps `s32 → s16` exact,
because the reference converts it with an arithmetic shift and a trip through
`f32` would lose seven bits.

**The internal type** is `f64` when either endpoint carries more than 24
significant bits (`s32`, `s64`, `f64`) and `f32` otherwise. Anything narrower
round-trips through `f32` exactly, so the wider type would only cost bandwidth.

### 2.2 Rate conversion is exact-rational

`44100 → 48000` reduces to `147/160`. The position of output `k` is `k·q/p`
input samples, carried as an integer pair and advanced by

```rust
frac += q % p;  idx += q / p;
if frac >= p { frac -= p; idx += 1; }
```

which cannot drift over any stream length. When the reduced denominator `p` is
at most `MAX_EXACT_PHASES` (4096) the bank has exactly `p` phases, so
`phase(k) = (k·q) mod p` is not merely drift-free but exact — there is no phase
quantisation error at all. Every rate pair anyone uses is far below the cap.

### 2.3 Edges are mirrored, not zero-primed

Plan 17 §B.5.4 prescribes prepending and appending `centre` zeros. **The
reference does not do that**, and the difference is directly observable: feed
constant `1.0` and the output is flat `1.0` from the very first sample, with no
fade-in. Probing with impulses at inputs 0, 1, 2 and 5 identifies the rule, and
the two ends are not symmetric:

```
head:  x[-k]    = x[k]      whole-sample mirror about 0
tail:  x[N-1+k] = x[N-k]    half-sample mirror about N - 1/2
```

Mirroring depends only on the stream, never on how it was chunked, so it costs
nothing in chunk-invariance.

### 2.4 The filter design, recovered

See §5. The short version:

```
factor  = min(1, (out_rate/in_rate) · cutoff)        cutoff defaults to 0.97
taps    = ceil(filter_size / factor), rounded up to even
centre  = taps/2 − 1
h[φ][j] = w( t / (taps/2) ) · factor · sinc(factor · t),  t = j − centre − φ/P
bank   /= Σ_j h[0][j]
```

`w` is a Kaiser window at `β = 9` by default. Note the last line: the scale comes
from **phase 0 alone**, not a per-phase normalisation. Plan 17 §B.5.2 prescribes
per-phase; the reference does not do it, and doing it changes every coefficient.

---

## 3. Fidelity — measured, per path

Grades are D11's. Everything below was measured against `FFmpeg` 8.1 on
`aarch64-apple-darwin`; the recording lives in `tests/common/golden.rs` and the
commands that produced it are in §5.

| # | Path | Grade | Bound and measurement |
|---|---|---|---|
| 1 | Integer ↔ integer format conversion | **Exact** | Arithmetic shift with a 128 bias for `u8`. Pinned value-by-value at every boundary. |
| 2 | Integer → float | **Exact** | Scale by `2^-(n-1)`. `s16 -32768 → -1.0`, `32767 → 0.999969482421875`. |
| 3 | `f64` → integer, `f32` → `s32`/`u8`/`s64` | **Exact** | `round_ties_even`, plus the reference's `i64`→`i32`→clamp overflow shape. |
| 4 | **`f32` → `s16`** | **Equivalent**, ≤ 1 LSB | Half-up everywhere. The reference is half-up in whole 16-sample blocks and ties-to-even in the trailing `len % 16`; see §6.1. Identical for any buffer length that is a multiple of 16, and elsewhere it differs only at exact half-LSB ties. |
| 5 | Packed ↔ planar | **Exact** | A permutation. |
| 6 | Rematrix, all 23 recorded layout pairs, float output | **Exact** | Bit-identical `f64` coefficients, 23/23. |
| 7 | Rematrix, identity for all 40 standard layouts | **Exact** | Asserted exhaustively. |
| 8 | Rematrix normalisation | **Exact** | Global row-peak ceiling of `1.0` for integer output, none for float. |
| 9 | Rematrix, integer output, default (irrational) mix levels | **Equivalent**, ≤ 1 LSB | The reference quantises to Q15 with a residual-carrying scheme we could not pin (§6.2). We mix in float and quantise once. |
| 10 | **Rate conversion, any upsampling ratio** | **Equivalent**, ≥ 300 dB | Measured 304.3–307.4 dB, max abs 7.8e-16 — `f64` machine precision. Plan 17 §B.14.1 rated the float path "No, and we should not try". |
| 11 | **Rate conversion, downsampling** | **Equivalent**, ≥ 100 dB | Measured 113.5 dB (`48 → 44.1 k`) and 101.0 dB (`48 → 32 k`). Residual is a ~5.8e-6 relative shape difference in the taps (§6.3). |
| 12 | Output sample count | **Exact** | `ceil(in · p / q)`. 100 in at `44.1 → 48 k` gives 109; 1000 gives 1089; 44100 gives exactly 48000. |
| 13 | Dither, four position-seeded methods | **Divergent by design** | Our PRNG is ours. The reference's default is `none`, so no default path is affected. |
| 14 | Noise-shaping dither, all seven curves | **Equivalent, by design, measured** | Ours, not the reference's — generated from a published psychoacoustic curve (Terhardt ATH), not transcribed (§13 below). Measured 7.2-11.0 dB perceptually-weighted noise reduction against plain TPDF, end to end. |
| 15 | Matrix encodings beyond `dolby`/`dplii` | **Exact** | The reference itself never builds a distinct matrix for `dplii_x`/`dplii_z`/`dolby_ex`/`dolby_headphone` — measured byte-identical to `none` for every impulse through 7.1 into both stereo and 5.1. We reproduce that fallback rather than reject the option. |
| 16 | `s64` conversions | **Unmeasured** | The reference has no `s64le` raw muxer, so there is no direct entry point. Defined by symmetry with `s32`. |
| 17 | Timestamp compensation — hard | **Exact** | A drift past `min_hard_comp` inserts or drops precisely the measured deficit: 4801-sample (0.100021s) jump → exactly 4801 samples inserted; a full one-second jump → exactly 48000; symmetric for a negative (surplus) drift. The 0.1s boundary itself is exclusive, matching the reference to the sample (§12 below). |
| 18 | Timestamp compensation — `async` resolution | **Exact** | `async=1` and `async>1` reproduce the explicit `(min_comp, max_soft_comp)` thresholds those legacy values are documented as meaning; verified by driving both configurations through identical drift scenarios and comparing sample counts. |
| 19 | Timestamp compensation — `first_pts` | **Exact** | A stream's real nonzero start does not itself read as drift (measured: zero inserted samples whether `first_pts` is unset or matches); an explicit `first_pts` that disagrees with the real start reliably reproduces the drift as if it were real. |
| 20 | Timestamp compensation — soft | **Equivalent, by design** | The reference does not publish *how* a soft correction reshapes the sample stream, only that it stays within `max_soft_comp` samples/sec — see §12.4. Our stretch mechanism is original DSP (linear interpolation), not a transcription. |

Plan 17 §B.14.1 expected classes 1–5 to be Exact, class 6/7 (rematrix) Exact
"with effort", class 8 (integer rate conversion) "Medium-low confidence" and
class 9 (float rate conversion) "No, and we should not try". **Rate conversion
came out far better than the plan expected** — bit-exact in `f64` for every
upsampling ratio — because the filter design turned out to be recoverable
exactly rather than only fittable. The three-person-week class-8 investigation
the plan budgets was not needed.

---

## 4. Scope: what is in and what is not

**In**, and measured:

- All 12 sample formats, all 144 ordered pairs, both layouts, both directions.
- Rate conversion at any rational ratio up to `MAX_RATE_RATIO` (§8b), Kaiser / Blackman–Nuttall / cubic
  filters, `filter_size`, `phase_shift`, `cutoff`, `kaiser_beta`,
  `exact_rational`, `linear_interp`.
- Rematrixing over the 40 standard layouts plus custom and unspecified ones,
  with `clev` / `slev` / `lfe_mix_level` / `rematrix_volume` /
  `rematrix_maxval`.
- `dolby` and `dplii` matrix encoding, and `dplii_x` / `dplii_z` / `dolby_ex` /
  `dolby_headphone` reproducing the reference's own unencoded fallback (§3 row 15).
- `none` / `rectangular` / `triangular` / `triangular_hp` dither, and all
  seven noise-shaping curves (`lipshitz`, `f_weighted`, `modified_e_weighted`,
  `improved_e_weighted`, `shibata`, `low_shibata`, `high_shibata`) — see §13.
- Streaming: chunk-invariance, drain, reset, partial output buffers.
- Timestamp compensation: soft, hard, `async`, `first_pts`, and the manual
  API (`Resampler::advance_pts` / `Resampler::set_compensation`) — see §12.

**Out**, deliberately, each with a reason:

| Deferred | Why |
|---|---|
| `used_chlayout`, `set_channel_mapping`, user-supplied matrices | The plumbing exists (`MixMatrix::from_rows` is public); the option-level wiring is not. |
| An explicit SIMD tier | See §7.3 — it did not pay, measured. |
| `s64` conformance | No raw `s64le` muxer to probe through. |

---

## 5. Provenance

Every number this crate pins came out of `FFmpeg 8.1` on
`aarch64-apple-darwin`, recorded 2026-08-22. **The golden table is a recording,
not a wish list** — if a row looks wrong, re-probe and say what changed rather
than editing it.

Plan 13 §1b's rule applies throughout: use the shortest path to the thing under
test. Here the resampler *is* reached through `aresample`, which is its own
direct entry point rather than a wrapping layer, and the raw `f64le` demuxer and
muxer put no parsing between the command line and the numbers.

### 5.1 Float → integer rounding

```sh
# 65 536 exact half-LSB ties, (k + 0.5)/32768 for every k in [-32768, 32767)
ffmpeg -f f32le -ar 48000 -ac 1 -i ties.f32 \
       -af aresample=out_sample_fmt=s16 -f s16le -
```

Every tie rounds `floor(q + 0.5)`. Repeating with `-f f64le` / `dbl` gives
ties-to-even instead. Truncating the *same* input to 7, 31, 37 or 127 samples
moves some of them; cross-referencing which move against which stay identifies
the 16-sample block boundary (§6.1).

### 5.2 Integer conversion and overflow

```sh
ffmpeg -f s32le -ar 48000 -ac 1 -i sweep.s32 -af aresample=out_sample_fmt=s16 -f s16le -
ffmpeg -f f32le -ar 48000 -ac 1 -i specials.f32 -af aresample=out_sample_fmt=s16 -f s16le -
```

`s32 -32768 → -1` and `32767 → 0` prove an arithmetic shift, not rounding.
`1e30 → -1`, `-1e30 → 0`, `inf → -1`, `NaN → 0` but `2.0 → 32767` prove the
`i64`-saturate / `i32`-truncate / clamp sequence — nothing else reproduces all
five.

### 5.3 Mix matrices

```sh
# sample k carries 1.0 on channel k and nothing else: the output is the matrix
ffmpeg -f f64le -ar 48000 -ch_layout 5.1 -i eye6.f64 \
       -af aresample=ochl=stereo:out_sample_fmt=dbl -f f64le -
```

Reading back in `dbl` gives the coefficients at full precision, which is what
made two structural facts visible:

- `5.1 → mono` has an `FC` coefficient of `0.999999982885729`, which is
  `f32(1/√2) · f64(1/√2) · 2` and nothing else. A centre-only output is a
  **composition** — `stereo→mono ∘ 5.1→stereo` — not a direct fold.
- `FC → L` in `5.1 → stereo` is `0.7071067690849304` (single-rounded, because
  `clev` is a `float` option) while `SL → BL` in `7.1 → 5.1` is
  `0.7071067811865476` (double, because that fold is a hardcoded constant).

Adding `:out_sample_fmt=s16` shows the integer path is additionally scaled by
`1/peak`; float output is not scaled at all.

### 5.4 The coefficient bank

```sh
# 48k -> 96k reduces to 1/2, so an impulse reads both phases off directly
ffmpeg -f f64le -ar 48000 -ch_layout mono -i impulse.f64 \
       -af aresample=96000:out_sample_fmt=dbl -f f64le -ar 96000 -
```

Phase 0 comes back an *exact* unit impulse, which alone pins the cutoff:
only `factor = 1` makes `sinc` vanish at every non-zero integer, so the `min` in
`min(1, ratio · cutoff)` is outside the product. Confirmed independently by
`cutoff=0.5` at `48 → 96 k` producing byte-identical output to the default.

Fitting the 32 remaining numbers over `(window, β, cutoff, window span)` gives
Kaiser `β = 9.000000000001` with a residual of 5e-11 — the precision the probe
was recorded at.

Repeating downsampling (`48 → 32 k`, `-af aresample=32000`) supplies the two
pieces upsampling cannot see: `cutoff = 0.97` (the centre tap measures
`0.646672861 = 0.97 · 2/3` plus the normalisation excess, and `cutoff=0.9` moves
it to `0.900009`) and the tap stretch (measured support 49.5 input samples for
`T = 32, factor = 0.6467`, which is `T/factor`).

### 5.5 Edge handling

```sh
ffmpeg -f f64le -ar 48000 -ch_layout mono -i dc.f64 -af aresample=96000:out_sample_fmt=dbl -f f64le -ar 96000 -
```

Flat `1.0` in, flat `1.0` out, from sample 0 — no zero-priming. An impulse at
input 1 gives `0.4295627` at output 1, which is `h[1][16] + h[1][14]` to the last
digit: the mirrored copy at input `-1` contributing through tap 14. The
half-sample tail mirror was found the same way; a whole-sample tail mirror is
measurably wrong.

---

## 6. Known divergences

### 6.1 `f32 → s16` ties in a trailing partial block

`convert::F32_TO_S16_TAIL_DIVERGENCE` carries the full record. In summary: the
reference's rounding is **not a function of the sample value alone**. It
processes whole 16-sample blocks in a vector kernel that rounds half-up and the
trailing `len % 16` samples in a scalar kernel that rounds ties-to-even.

We round half-up unconditionally. Reproducing the reference would make our
output a function of how the caller chunked the stream, and chunk-invariance is
this crate's central contract. This is the D17.1 shape: not "matching would be
awkward", but "matching would require abandoning a stronger guarantee". The
divergence is one LSB, at exact half-LSB ties, in at most 15 samples of any
buffer, and zero whenever the length is a multiple of 16.

### 6.2 Integer rematrix at default mix levels

For `s16` output with the default `1/√2` mix levels, the reference's Q15
coefficients for `5.1 → stereo` are `FL 13573, FC 9597, BL 9598` — they sum to
exactly 32768, so a residual is being carried, but the scheme that reproduces
which tap absorbs it in `5.1 → stereo` does not reproduce `7.1 → stereo` or
`5.1 → mono`. Three quantisation models were tested against six recorded
matrices; none fits all of them.

We mix in float and quantise once at the output. The float matrix itself is
bit-identical (§3 row 6), so the divergence is bounded by the rounding of one
multiply: **≤ 1 LSB**, which plan 17 §B.14.3 already declares as the class-6
fallback ("the entire possible disagreement for a two-tap fixed-point mix").

### 6.3 Downsampling filter shape

`101–113 dB` rather than the `300 dB` the upsampling path reaches. The gap is a
relative `5.8e-6` in the tap weights, pinned by
`downsampling_stretches_the_filter`. Tap count, window span (five variants),
window argument (phase-shifted versus tap-index) and the `f32`/`f64` rounding of
`cutoff` were each scanned and none accounts for it. It is above plan 17
§B.14.3's declared `≥ 100 dB` floor and is recorded rather than papered over.

### 6.4 Very short streams through `ffmpeg` itself

`ffmpeg -i` with fewer than about 16 input samples emits nothing at all, where
`ceil(n · p / q)` says it should emit some. That is a property of the CLI's
frame handling, not of the resampler: our `out_samples` and our actual output
follow the formula, which the reference also follows for every input length
above that threshold. `a_stream_shorter_than_the_filter_still_drains` pins our
behaviour.

### 6.5 Exotic layouts

`22.2 → stereo` and `hexadecagonal → stereo` are **Divergent**, and it is the
reference that looks wrong. In `22.2 → stereo` the reference leaves `SL` and
`SR` at exactly zero while folding `BL`/`BR` normally, and it leaves every
channel above `BC` at zero — but in `7.1 → stereo` it folds `SL`/`SR` at `slev`,
and in `hexadecagonal → stereo` it folds `TFL`/`TFR` at `1/√2` while dropping
`TFC`. No single rule reproduces all three. We apply the general fold rules,
which match every common layout exactly and lose less energy on the exotic ones.
These two pairs are not in the golden table; adding them would pin a behaviour
we deliberately do not reproduce.

---

## 7. Performance

Apple M5, `aarch64-apple-darwin`, `bench` profile, `divan`, min of 100, two
agreeing runs. `cargo bench -p vaco-resample`.

### 7.1 The convolution kernel — the plan's rule measured backwards

PF-0.0's authoring rule is *"never carry a single vector accumulator; use
four"*, worth up to 4x. Against the naive single-accumulator loop:

| | 32 taps | 50 taps | 256 taps |
|---|---|---|---|
| `dot4`, `f32` | **1.55x** | **1.33x** | 0.62x |
| `dot8`, `f32` | 0.52x | 0.49x | 0.19x |
| `dot4`, `f64` | **1.68x** | **1.16x** | 0.77x |
| `dot8`, `f64` | 0.77x | 0.68x | 0.29x |

**Four accumulators are slower than one** at exactly the tap counts a resampler
uses — 32 is the default and 50 is what a 3:2 downsample stretches it to. Eight
are faster than either, everywhere, by 1.9x to 5.2x.

The rule is about *lane width*, not about the number four. Four `f32` lanes is
precisely one NEON register, so `as_chunks::<4>` pins the loop to one vector per
iteration and removes the unrolling LLVM performs on a plain `iter().zip()`
reduction by itself. Eight gives it two registers and it wins. The "naive" form
is not naive: it is the shape the compiler is best at.

This is the **third** recorded case on this project of a confident performance
assumption measuring backwards, after D12's widening-MAC gap and PF-0.1's
branchless CABAC decision.

### 7.2 Element conversion

8192 samples per iteration:

| Kernel | Time | Per sample |
|---|---|---|
| `s32 → s16` contiguous | 224 ns | 0.027 ns |
| `s16 → f32` contiguous | 445 ns | 0.054 ns |
| `s16 → f32` **stride 2** | 2583 ns | 0.315 ns |
| `f32 → s16` contiguous | 1989 ns | 0.243 ns |

The stride-1 specialisation in `convert_elems` is worth **5.8x**: a runtime
`step_by` blocks vectorisation outright. It stays.

Reproducing the reference's overflow behaviour exactly costs **1.50x** on
`f32 → s16` (1.853 µs against 1.239 µs for a plain `clamp`). Kept — exactness on
the down-conversion path is worth more than 1.5x on one converter, and the
`clip_exact` / `clip_naive` benchmark pair exists so that a future decision to
drop it is made against a number.

### 7.3 Why no `vaco-simd` (yet)

Every kernel here is either lanewise (format conversion) or a plain reduction
(the convolution), and LLVM vectorises both. Format conversion runs at 5.9 G
samples/s, which is memory-bandwidth territory and what plan 17 §B.15 scenario 4
predicts. The dot-product comparison above says the winning shape is the one
that gives the compiler *more* room, not less.

The one kernel where an explicit tier would pay is `f32 → s16`, which is 4.5x
slower per sample than `s16 → f32` because of the `i64`-saturate /
`i32`-truncate / clamp emulation. Anyone taking that on must preserve those
semantics exactly — `1e30 → -1` and `2.0 → 32767` are both load-bearing — and
must keep the scalar reference as the differential oracle.

### 7.4 End to end

4096 frames per iteration, `cargo bench -- pipeline`:

| Scenario | Throughput |
|---|---|
| format conversion only (`s16 → f32`, stereo) | 5.9 G frames/s |
| 5.1 → stereo downmix, 48 k, `f32` | 159 M frames/s |
| 7.1 → 5.1 downmix, 48 k, `f32` | 86 M frames/s |
| `44.1 → 48 k`, stereo, `s16` | 43 M frames/s |
| `96 → 48 k`, stereo, `f32` | 81 M frames/s |
| full pipeline, 5.1 → stereo, `44.1 → 48 k`, `f32 → s16` | 41 M frames/s |

Coefficient-bank setup is 180 µs at `filter_size = 32` and 1.2 ms at 256, which
matters for short CLI runs and is why it is benchmarked separately.

A like-for-like comparison against the reference binary was **not** run: it needs
a harness that isolates the resampler from `ffmpeg`'s I/O and frame plumbing, and
that is a separate piece of work. Plan 17 §B.15's parity target is therefore
still open.

---

## 8. How to change it

- **A new element converter or a rounding change** goes in `convert::elem`, and
  the arm in `convert_elems`'s 36-way match. Add the measured pairs to
  `tests/convert.rs` in the same commit; a rounding rule with no recorded probe
  behind it is a guess.
- **A new fold rule** goes in `mix::raw_matrix` phase 4. Re-probe the affected
  layout pair, add it to `MATRIX_PAIRS`, and expect
  `mix_matrices_match_the_reference` to tell you whether the rule generalised.
- **The filter design** is `design::build_bank`. Every constant in it is
  measured; changing one without re-probing will move `rate_conversion_grades`
  and `downsampling_stretches_the_filter` together, which is the signal that you
  changed the design rather than fixed it.
- **The convolution kernel** is `rate::kernel`. Add a variant, add a bench arm,
  and change `Internal::dot` only if the table in §7.1 moves. Do not change it
  on reasoning.
- **Regenerating the goldens**: the commands are in §5. Record at full `f64`
  precision (`out_sample_fmt=dbl`, `-f f64le`) — a `round()` in the recording
  script is how the exact-zero property of phase 0 nearly went unnoticed.

### Gotchas

- `Rematrix::apply` and `RateConvert::process` **append** to their output
  vectors. They do not clear them. The pipeline clears once per call.
- `RateConvert` keeps a `head` buffer of the first `centre + 1` samples for the
  whole stream. That is deliberate: the head mirror can be needed at any point
  while `base` is still small, and `centre + 1` samples is nothing.
- `trim` must not drop below `consumed - 2·taps`, because the tail mirror at
  flush reaches backwards from the end of the stream.
- The direct path is chosen at construction, so a resampler built with matching
  formats and rates will not start mixing if you later change your mind. Build a
  new one.

---

## 8b. Limits, and the bounds the fuzzer asked for

A resampler derives its buffer sizes *and its loop trip counts* from a ratio of
two attacker-chosen integers, so the allocation budget alone does not bound it.
Five separate caps stand in front of that, and each exists because a fuzz run
found the case it covers.

| Constant | Value | What it stops |
|---|---|---|
| `rate::MAX_RATE_RATIO` | 1024 | The output count. `8 Hz -> 335872 Hz` at `filter_size = 8192` took **47.7 s** for eight input samples: 335 872 outputs, each an 8192-tap convolution. The bank is 32 KB and the phase count is 1, so nothing else saw anything unusual. |
| `design::MAX_STRETCH` | 256 | The work per output sample. `1/factor` is unbounded as `cutoff` goes to zero, so a one-phase bank of a million taps allocates a harmless 4 MB and then costs a million MACs per sample. |
| `design::MAX_TAPS` | 2²⁰ | An absolute ceiling behind the other two. |
| `FUEL_PER_COEFFICIENT` | 32 | Bank *generation*. Every coefficient is a Bessel series plus a `sin`, so a 16 MiB bank fits the allocation cap comfortably and still takes a fifth of a second to fill. This is `Budget::consume_fuel`, which is the mechanism `vaco-limits` provides for work bounded by something other than memory. |
| `RateConvert::work_fuel_cap` | `Limits::fuel` | Bank **use**, not generation — the gap the other three leave open. `50 Hz -> 31232 Hz` (ratio ~625, under `MAX_RATE_RATIO`) with `filter_size = 23301` (under `MAX_TAPS`) and 25 channels (under `max_channels`) took **15-23 s** for 51 input samples: none of the other bounds saw anything unusual individually, but `taps × channels × output_samples` came to ~1.9×10¹⁰ multiply-adds. `RateConvert::process`/`flush` now charge `taps × channels` fuel per output sample actually emitted, against the same `fuel` allowance `Limits` already declares — a fresh counter rather than the constructor's `Budget`, because that borrow does not survive past `RateConvert::new` and a streaming call has none to charge against. Found fuzzing while verifying #520; regression: `tests/streaming.rs::a_large_filter_at_a_legal_ratio_is_refused_by_processing_fuel_not_by_construction`, seed `fuzz/seeds/resample_convert/huge-filter-legal-ratio-many-channels`. |
| `timestamp::MAX_COMPENSATION_SAMPLES` | 10×192 000 | Any single timestamp-compensation request — automatic (from a pts observation) or manual (`Resampler::set_compensation`) — so an adversarial pts cannot ask for an unbounded silence insertion or stretch window. See §12. |

The bounds live here rather than in `vaco-limits` deliberately: "ratio of two
sample rates" and "tap" are domain knowledge that crate should not carry. What
it supplies is the mechanism — `check_sample_rate`, `check_channels`,
`consume_fuel` — and these are the domain constants that use it.

The widest ratio the permissive limits admit at all is `2822400 / 8000 = 352.8`,
so `MAX_RATE_RATIO` refuses nothing a real conversion asks for.

---

## 9. Configuration

No environment variables. Everything is `ResampleOptions`, which carries the
reference's option names and aliases:

| Name (aliases) | Default | Note |
|---|---|---|
| `clev` / `center_mix_level` | `1/√2` | stored `f32`; the single-rounding is observable |
| `slev` / `surround_mix_level` | `1/√2` | |
| `lfe_mix_level` | `0.0` | applied as `lfe · 1/√2`, measured |
| `rmvol` / `rematrix_volume` | `1.0` | |
| `rematrix_maxval` | `0.0` | means "derive from the output format": `1.0` integer, none float |
| `matrix_encoding` | `none` | `dolby`, `dplii` implemented; the rest `Unsupported` |
| `resampler` | `swr` | `soxr` accepted, warned, aliased |
| `filter_size` | `32` | |
| `phase_shift` | `10` | only used when `exact_rational` cannot apply |
| `linear_interp` | `false` | |
| `exact_rational` | `true` | |
| `cutoff` / `resample_cutoff` | `0.0` | means the measured default of `0.97` |
| `filter_type` | `kaiser` | `cubic`, `blackman_nuttall`, `kaiser` |
| `kaiser_beta` | `9.0` | measured |
| `precision`, `cheby` | — | accepted for `soxr` compatibility, ignored with a note |
| `dither_method` | `none` | four implemented, seven aliased |
| `dither_scale` | `1.0` | |
| `output_sample_bits` | `0` | means the output format's own depth |
| `dither_seed` | `0` | vaco extension |
| `min_comp` | `f32::MAX` | seconds; the master switch — see §12 |
| `min_hard_comp` | `0.1` | seconds; exclusive boundary, measured |
| `comp_duration` | `1.0` | seconds |
| `max_soft_comp` | `0.0` | samples/sec; `0` disables soft |
| `async` | `0.0` | resolves into the three above — see §12.2 |
| `first_pts` | `i64::MIN` | means "unset"; input-rate samples |

`ResampleOptions::set_from_str` parses a `k=v:k=v` string, as `aresample=` takes
it. The endpoint format, rate and layout are **not** options here — they live in
`AudioSpec`, where they are typed. See `src/opts.rs` for why this is hand-written
rather than `#[derive(Options)]`, and what would have to land first.

Allocation goes through a `vaco_limits::Budget`, which `Resampler::new` takes
positionally. A degenerate rate pair asks for an enormous coefficient bank, and
the budget is what refuses it.

---

## 10. Dependencies

| Crate | Why |
|---|---|
| `vaco-core` | the shared `Error` taxonomy |
| `vaco-sampfmt` | the twelve formats and their geometry |
| `vaco-chlayout` | the 36-channel vocabulary and 40 standard layouts |
| `vaco-limits` | `Budget`, required to size the coefficient bank |
| `vaco-frame` | one intra-doc link on `AudioRef::from_frame_planes` |
| `tracing` | the "accept, warn, and alias" path for options we cannot honour |

Dev: `proptest`, `divan`.

**Not taken**, and both are measurements rather than preferences:

- **`vaco-simd`** — see §7.3. Every kernel is lanewise or a plain reduction and
  LLVM already vectorises both; the hand-shaped four-accumulator variant the
  plans recommend measured *slower* than the compiler's own version.
- **`vaco-opts`** — needs an `OptValue` impl for `SampleFmt` and `ChannelLayout`,
  which live in crates this one does not own.

`rubato` was rejected in plan 17 §B.13.1 on model fit and that assessment stands:
it covers the rate-conversion core and none of the option surface, the integer
paths, the rematrixing or the chunk-invariance contract.

---

## 11. Tests

`cargo test -p vaco-resample` — 80 tests, none requiring `ffmpeg` on the machine.

| File | What it holds |
|---|---|
| `tests/conformance.rs` | the recorded-reference comparisons and the grades in §3 |
| `tests/convert.rs` | the numeric contract, value by value |
| `tests/streaming.rs` | chunk invariance, drain, reset, delay, degenerate calls, the two resource bounds (`an_absurd_rate_ratio_is_refused`, `a_large_filter_at_a_legal_ratio_is_refused_by_processing_fuel_not_by_construction`) |
| `tests/properties.rs` | `proptest` invariants plus two exhaustive sweeps |
| `tests/timestamp.rs` | soft/hard/`async`/`first_pts`/manual-API compensation — the measured numbers from §12, reproduced against the crate rather than against `ffmpeg` at test time |
| `tests/dither.rs` | noise-shaping: distinctness, actual application, no DC bias, chunk invariance and reset for stateful dither, and the perceptually-weighted spectral measurement from §13 |
| `tests/common/golden.rs` | the recording (§5) |

The load-bearing ones:

- `chunking_does_not_change_the_output` and its property-test counterpart
  `chunking_never_changes_the_output` — plan 17 §B.11 calls this the highest-value
  test in the crate and it is.
- `constant_input_gives_constant_output` — fails immediately on a zero-primed
  implementation.
- `bank_1to2_matches_the_reference` — asserts phase 0 is an *exact* unit impulse,
  which pins the cutoff, the tap alignment and the absence of per-phase
  normalisation in one assertion.
- `every_layout_pair_builds_a_finite_matrix` — all 1600 ordered pairs of the 40
  standard layouts, both output kinds.
- `s16_round_trips_through_f32_exactly` — all 65 536 values.

**Fuzzing**: `fuzz/fuzz_targets/resample_convert.rs` drives the whole resampler
over arbitrary rates, formats, layouts, filter parameters and chunk schedules,
under `Limits::strict()`. It asserts that output never exceeds what
`out_samples` promised — a caller that trusted an under-estimate would overflow
its own buffer — and that the drain terminates.

It found **three slow units and one crash**, across separate campaigns. All
three slow units were real denial-of-service surfaces rather than fuzzer
noise, and all three were fixed by a bound rather than a faster loop (§8b) —
the third (`work_fuel_cap`) while verifying #520's timestamp compensation,
against a config (`filter_size = 23301`, ratio ~625, 25 channels) that no
single existing bound saw as unusual. The crash — `out_samples` apparently
under-reporting — was a bug in **the target, not the crate**: it sliced the
input with `pos * bytes .. (pos + take) * bytes * channels`, multiplying only
the end by the channel count, so it fed five frames while telling `out_samples`
it was feeding one. Plan 13 §1b's rule about the layer between you and the
answer applies to a fuzz harness as much as to a probe. Note that `cargo fuzz` **exits 0 on a slow
unit**: the artifact on disk is the only evidence, which is exactly plan 19
§13's point. Setting `RESAMPLE_FUZZ_DEBUG=1` makes the target print the decoded
`Config` for each input, which is how both were diagnosed:

```sh
RESAMPLE_FUZZ_DEBUG=1 cargo +nightly fuzz run resample_convert --features resample \
    fuzz/artifacts/resample_convert/slow-unit-<hash>
```

Last run, with all three bounds and the corrected target: `exit=0`, `#110855`
execs in 300 s, `cov: 2394 ft: 9923`, `fuzz/artifacts` empty. Throughput went
from 147 exec/s to 1559 once the bounds landed, which is why the run that
followed reached ten times as deep.

---

## 12. Timestamp compensation

Soft, hard, `async`, `first_pts` and the manual API — `src/timestamp.rs`,
wired into `src/resampler.rs`. All measurements below are against `FFmpeg`
9.0.1, `aarch64-apple-darwin`, through the `aresample` filter, fed an
`asetpts`-injected pts anomaly and read back as decoded sample counts (never
the reference's source, per D6/D7). The full commands are in
`src/timestamp.rs`'s module docs, which this section summarises.

### 12.1 The option surface is a three-threshold model, plus `async`

| Option | What it gates |
|---|---|
| `min_comp` | The master switch, in seconds. Default `FLT_MAX` (`f32::MAX`), which disables compensation of **either** kind — confirmed by a full one-second pts jump through plain `aresample=48000` (no `async`) producing exactly the original sample count. |
| `min_hard_comp` | Above this many seconds of drift, the whole discrepancy is corrected in one step. Default `0.1`, and the boundary is exact and exclusive: a 4800-sample (0.100000s) jump produces zero extra samples; 4801 (0.100021s) produces *exactly* 4801, in one step. |
| `max_soft_comp` / `comp_duration` | Below `min_hard_comp`, a soft correction may stretch or squeeze by up to `max_soft_comp` samples/sec, spread over `comp_duration` seconds. |
| `async` | The reference's one-parameter convenience. `0` disables everything; `1` sets `min_comp=0` and leaves `max_soft_comp=0` (hard-only, matching legacy `-async 1`'s "fill/trim only"); `|async| > 1` also sets `max_soft_comp = async`. `min_hard_comp` is untouched in every measurement. |
| `first_pts` | The assumed pts of the first input sample. Unset, a stream's real (nonzero) start is not itself drift — a constant one-second pts offset held from frame 0 produced zero inserted samples under `async=1` whether `first_pts` was left unset or set to match. Explicitly disagreeing (`first_pts=0` against a real start of 48000) reliably reproduced the drift and the expected 48000-sample hard correction. |

### 12.2 Hard compensation: exact, one-shot

Triggered when `|drift| > min_hard_comp`. The deficit — in input-rate samples
— is inserted as silence (positive drift) or dropped from the front of the
next real block (negative drift), in one step, before that block is
processed. This is bit-for-bit the reference's measured behaviour: every
hard-compensation sample count in `tests/timestamp.rs` matches a number
measured off the reference binary exactly, not approximately.

### 12.3 The tracker: reacting to drift, not to a single observation

`Tracker` (in `timestamp.rs`) does not compare the latest declared pts
against the *previous* one — it compares against a baseline plus the actual
count of samples (real, inserted, or dropped) consumed since. That is what
lets soft compensation answer *sustained* drift (a source clock a fraction
of a percent off nominal) rather than only single jumps: probed by scaling a
pts track by `1.0004` (~0.04% skew) against
`min_comp=0:max_soft_comp=1000:comp_duration=1:min_hard_comp=999`, which
produced real, gradually-inserted extra samples (59 over 5 seconds) — while
the same one-shot step-jump scenario that drives hard compensation produced
no measurable change under an identical soft-only configuration. Soft
compensation is the reference's answer to a clock running at the wrong
*rate*; hard compensation is its answer to a discontinuity.

### 12.4 Soft compensation's mechanism is ours, by necessity

The reference's public contract for `swr_set_compensation`-shaped behaviour
promises an aggregate effect — stretch or squeeze by at most `max_soft_comp`
samples/sec — not an algorithm. We reshape the affected span by linear
interpolation (`timestamp::linear_resample`), which is original DSP rather
than a transcription of anything, and is *class-A style* deterministic: a
pure function of the span and the target length, so replaying the same drift
sequence reproduces the same output regardless of how the caller chunked the
stream *up to* a compensation-window boundary. (A soft window claimed
mid-chunk by a different call boundary can produce a different, but still
correct in aggregate, split — compensation is not required to be as strictly
chunk-invariant as the rest of the crate, since which chunk a given pts
observation lands in is itself part of what the caller is telling the
resampler.) Per the owner's byte-exactness ruling, this is a legitimate
divergence: the deviation is small (bounded by
[`timestamp::MAX_COMPENSATION_SAMPLES`], a tiny fraction of any real stream)
and unstructured.

### 12.5 The manual API

`Resampler::advance_pts(pts)` is the automatic side: tell it the pts the next
input chunk is expected to carry, and it queues whatever `§12.1`'s policy
decides. `Resampler::set_compensation(sample_delta, compensation_distance)`
is the direct equivalent of the reference's `swr_set_compensation`: request
`sample_delta` extra or fewer *output* samples over the next
`compensation_distance` output samples, bypassing the pts-driven policy
entirely. Both refuse with `Error::Unsupported` on a resampler built with no
compensation option set and no mixing/resampling/dither reason to build the
dsp pipeline (the direct format-conversion path, §2.1) — refusing explicitly
rather than silently no-op'ing, since there is no state to compensate
through. Set `async`, `first_pts`, a `min_comp` below its disabled default,
or `flags=+res` to force the pipeline.

### 12.6 A bug found while measuring this: processing fuel

Verifying hard compensation's cost characteristics led to fuzzing
`RateConvert` harder than before, which surfaced a real, unrelated
denial-of-service gap in the rate-conversion engine itself — see §8b's
`RateConvert::work_fuel_cap` row. Fixed in the same pass, since it lives in
this crate.

### 12.7 What was refused rather than approximated

Nothing in this feature's option surface was refused — `min_comp`,
`min_hard_comp`, `comp_duration`, `max_soft_comp`, `async` and `first_pts`
are all implemented, matching the reference's names, defaults and ranges
(`ffmpeg -h resampler=swr`, 9.0.1). What is *not* claimed is bit-exactness on
the soft-compensation sample stream itself (§12.4) — stated as Equivalent,
not Exact, per the grading rule in §3.

---

## 13. Noise-shaping dither

All seven curves — `src/dither/noise_shape.rs`, wired into `src/dither.rs`
and `src/resampler.rs`. This absorbs PF-1.6.

### 13.1 Why every curve is generated, not transcribed

The clean-room problem is stated in full in `src/dither.rs`'s module docs and
in plan 17 §B.6: the Lipshitz/F-weighted/E-weighted family is published in a
paywalled 1991 JAES paper this session could not verify the exact
coefficients of, and the Shibata curves originate in SSRC, whose licensing is
not on D3's allowlist. Fabricating numbers presented as "the published
coefficients" without being able to verify them would be worse than not
implementing the feature at all — it is exactly the kind of unverifiable
citation `planning/AGENT-CONSTRAINTS.md` warns against. So every curve here
is our own design, fit to a target we can and do cite in full: Terhardt's
published absolute-threshold-of-hearing formula.

### 13.2 The generation method, and what it took to get right

1. **Target.** Terhardt's ATH approximation, converted to a linear power
   ratio and normalised to unit mean — more headroom before the ear notices
   means more noise is allowed there.
2. **Minimum-phase spectral factorization.** A naive first attempt fit a
   *zero-phase* target directly by weighted least squares. Measured result:
   a 17-tap filter with `Σ|c_k| ≈ 6455` — a real, causal (one-sided) filter
   cannot have zero phase at every frequency, so forcing it produces
   pathologically large, oscillating coefficients. The standard fix — the
   homomorphic (cepstral) method: log the target magnitude, fold its cepstrum
   into a causal sequence, transform back — gives a filter whose *magnitude*
   matches the target with whatever phase minimum-phase implies, which is all
   an error-feedback filter needs.
3. **Tapered truncation.** The resulting impulse response is infinite; an
   exponential taper (`0.82ᵏ`) before truncating to *K* taps keeps it
   well-behaved, the same idea `design::build_bank` already uses for the
   (also infinite) resampling sinc.
4. **Aggressiveness.** `f_weighted`→`improved_e_weighted` are the same design
   at increasing order (3/5/7/9 taps — each literally contains the shorter
   one's coefficients as a prefix, since they are truncations of one target).
   `low_shibata`/`shibata`/`high_shibata` fix the order at 14 taps and scale
   the whole vector 0.5×/1.0×/1.5× — chosen after an earlier attempt that
   fit `high_shibata` as an independent design came out *less* aggressive
   than plain `shibata` by the measured dB range, the wrong direction for a
   name implying more. Scaling one design is monotonic by construction; a
   second independent fit is not, and measuring caught it rather than the
   ordering being assumed correct.

Full derivation, including the exact formula, is in
`src/dither/noise_shape.rs`'s module docs.

### 13.3 Real error feedback needed real state, which broke an assumption

`Dither` (§3 row 13) is `Copy` and a pure function of `(seed, channel,
position)` — true for the four simple methods, and stated as a design
principle in the module docs. Real error feedback is not: `e[n]` in
`e[n] = quantise(x[n] + Σc_k·e[n−k]) − (x[n] + Σc_k·e[n−k])` depends on the
actual sample value at every prior position, not on position alone, which is
exactly what lets it shape noise around real content instead of a fixed
pattern. `NoiseShapeState` (`src/dither.rs`) is the fix: genuine mutable
per-channel history, held in `Pipeline`, reset by `Resampler::reset`. Still
fully deterministic and still chunk-invariant — `tests/dither.rs`'s
`noise_shaping_is_chunk_invariant` pins it — just not in the *pure function
of position* sense the other four methods get for free.

### 13.4 A small TPDF term, and a measured reason for how small

Pure error feedback on near-constant input is a textbook failure mode
(idle tones — periodic limit cycles with no source to break them up), so a
small TPDF term is mixed in alongside the shaped feedback. How small was
measured, not assumed: an initial attempt mixed it in at the *same*
amplitude plain TPDF dither uses, on the reasoning that real dithered
noise-shaped quantisers use both at comparable strength. Measured end to
end (§13.5's methodology), that configuration was **worse than plain TPDF**
for every one of the seven curves — the flat TPDF component was large enough
to swamp the shaped component's own spectral benefit. A quarter of that
amplitude (still enough to break up idle tones) fixed it; see the
measurement in `src/dither/noise_shape.rs`.

### 13.5 Measured: perceptually-weighted noise, end to end

`tests/dither.rs::perceptually_weighted_noise_is_lower_than_tpdf` feeds
silence through the real `Resampler` at `output_sample_bits=8` (so the `s16`
output *is* the dither/quantisation noise) and compares Terhardt-weighted
spectral power against plain TPDF on the identical path:

| Curve | Improvement over TPDF, end to end |
|---|---|
| `lipshitz` | +9.82 dB |
| `f_weighted` | +10.10 dB |
| `modified_e_weighted` | +10.22 dB |
| `improved_e_weighted` | +9.67 dB |
| `shibata` | +9.52 dB |
| `low_shibata` | +10.97 dB |
| `high_shibata` | +7.24 dB |

These land close to the ~10.9 dB some published second-order E-weighted
shapers report for *their own* weighting curve — a coincidence of comparable
order, not a claim of matching it, since both the target curve and the
design method are ours. The test pins the *sign* of the improvement (every
curve strictly below plain TPDF), not these exact figures.

`tests/dither.rs` additionally pins: every curve produces a distinct,
non-trivial coefficient set (`every_noise_shaping_name_produces_a_distinct_nonempty_curve`);
the Shibata family is one curve at three scales, exactly
(`shibata_family_is_a_single_curve_scaled_by_strength`); noise shaping is
actually applied, and is distinguishable from plain TPDF, not just from `none`
(`noise_shaping_dither_changes_the_output_versus_none`,
`noise_shaping_dither_differs_from_plain_tpdf`); no DC bias is introduced at
a deliberately shallow bit depth (`noise_shaping_has_no_dc_bias`); and
`Resampler::reset` actually clears the feedback history
(`noise_shaping_reset_reproduces_a_fresh_stream`).

### 13.6 A wrapper bug found while fixing this

`vaco-filter-audio/src/aresample.rs` re-implemented its own copy of the
`dither_method` name-to-`DitherMethod` mapping rather than calling
`DitherMethod::from_name`, and that copy still aliased every noise-shaping
name to `triangular_hp` — correct when written, silently stale the moment
this crate implemented the real curves, because nothing there re-derives
from the crate it wraps. Fixed in the same pass by delegating instead of
duplicating; see that file's `dither()` for the exact shape
. `vaco-filter-audio`'s own module doc still correctly lists
`first_pts` and the timestamp-compensation option group as unforwarded —
that gap is real, separate, and not touched here, since it is an
unimplemented feature in a crate this pass does not own, not a wrapper
silently going stale.

### 13.7 A fuzz-coverage gap found in the same pass

`fuzz/fuzz_targets/resample_convert.rs`'s `Config::dither` mapped `cfg.dither
% 4` to only the four non-noise-shaping methods — meaning the stateful
`NoiseShapeState` code path added by this work was entirely unreached by the
fuzzer that exists specifically to reach configuration-driven code paths like
it. Widened to `% 11` to cover all eleven. 60s+ run afterward: clean, no
artifacts, coverage grew (2510→2527 edges over an 11753-exec, 60s run from a
fresh corpus).
