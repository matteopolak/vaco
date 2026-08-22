# `vaco-scale` — scaling, pixel-format and colour conversion

Implements `planning/17-scale-resample-tx.md` Part A: the `swscale` equivalent.
After the codecs, the hottest path in the project.

---

## 1. What it is

One crate that turns a picture in any byte-addressable pixel format, at any size,
into a picture in any other — resampling, converting colour space and range,
changing chroma subsampling, changing bit depth, and dithering on the way down.

```rust
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};
use vaco_pixfmt::PixFmt;

let src = ImageSpec::new(PixFmt::Yuv420p, 1920, 1080);
let dst = ImageSpec::new(PixFmt::Rgb24, 1280, 720);
let mut scaler = Scaler::new(&src, &dst, &ScaleOptions::default())?;
// scaler.scale_frame(&input, &mut output)?;
# Ok::<(), vaco_core::Error>(())
```

`#![forbid(unsafe_code)]`, like everything outside `vaco-hw-*` (D2). The one
vectorised kernel goes through `vaco-simd`'s capability tokens.

---

## 2. How it works

### 2.1 The pipeline

A conversion is lowered **once**, at `Scaler::new`, into a `Plan`. The plan is
always the same three stages, with the identities deleted:

```
  unpack + expand depth
    -> resample every channel onto a common "mid" grid      (up)
    -> 3x3 affine colour transform                          (colour)
    -> resample every channel onto its destination grid     (down)
    -> quantise + dither + pack
```

A colour matrix mixes channels, so every channel it touches must be at one
resolution — that is the only reason the mid grid exists. When no matrix is
needed the mid grid *is* the destination grid, the `down` stage vanishes, and
every channel resamples exactly once with its own coefficient bank. A chroma
subsampling change and a chroma siting change then fall out for free, as a size
ratio and a phase on that one bank.

### 2.2 Channel order is not a conversion

`vaco-pixfmt` indexes components **logically** — 0 is Y or R, 1 is Cb or G, 2 is
Cr or B, 3 is alpha — so `rgb24 -> bgr24`, `nv12 -> nv21` and `rgba -> argb` are
identity plans. Where the bytes sit is entirely `geometry.rs`'s business, and the
middle of the pipeline never learns which format it started as. That is what
turns an *n*×*m* format matrix into *n* + *m* pieces of code.

### 2.3 Bands, not frames

Work is done in horizontal bands of the destination. A band's output is a pure
function of the source rows its vertical filters reach, so intermediates are
band-sized (a 4K `yuv420p -> rgb24` never materialises a 100 MB `i32` picture),
bands are disjoint `chunks_mut` slices the compiler proves cannot alias, and
**thread count cannot change the output**. That last property is asserted by
`tests/properties.rs::thread_count_never_changes_the_output`, not assumed.

### 2.4 Numeric model

| Stage | Representation |
|---|---|
| unpack | container load, shift, mask; expanded to the working depth by **bit replication** |
| working depth | `max(src, dst)` component depth, clamped to 8..=16, carried in `i32` |
| filter coefficients | 14-bit fixed point, each row summing to exactly `1 << 14` |
| between an H and a V pass | 7 extra fractional bits (an 8-bit picture travels as 15-bit) |
| colour matrix | 13-bit fixed point out to R'G'B', 15-bit out to Y'CbCr |
| pack | round or Bayer-dither down to the destination depth, then clamp |

Filter accumulation is `i64`, so no input can overflow it; each bank records its
`Sum |c|` so a future `i32` kernel can select itself on a proof rather than on
the depth happening to be eight.

---

## 3. Fidelity against the reference

Measured, not asserted. `cargo test -p vaco-scale --test reference -- --nocapture`
reproduces the table; `paths_graded_exact_stay_exact` turns a regression in an
Exact path into a test failure. ffmpeg 8.1, `aarch64-apple-darwin`, 128×96
structure-plus-noise input.

Grades are D11's: **Exact** = byte-identical; **Equivalent** = differs within a
stated bound; **Divergent** = differs in ways we cannot justify.

| Conversion | Differing | Max err | PSNR dB | Interior | Grade |
|---|---:|---:|---:|---:|:--|
| `yuv444p -> rgb24`, bt709 tv→pc | 0 / 36864 | 0 | — | 0 | **Exact** † |
| `rgb24 -> yuv444p`, bt709 pc→tv | 0 / 36864 | 0 | ∞ | 0 | **Exact** |
| `rgb24 -> bgr24` (permutation) | 0 / 36864 | 0 | ∞ | 0 | **Exact** |
| `rgba -> bgra` (permutation) | 0 / 49152 | 0 | ∞ | 0 | **Exact** |
| `yuv420p -> nv12` (repack) | 0 / 18432 | 0 | ∞ | 0 | **Exact** |
| `gray -> rgb24` | 0 / 36864 | 0 | ∞ | 0 | **Exact** |
| `yuv420p10le -> yuv420p`, dither off | 0 / 18432 | 0 | ∞ | 0 | **Exact** |
| `yuv420p` 2× down, bilinear | 0 / 4608 | 0 | ∞ | 0 | **Exact** |
| `yuv420p` 2× up, bilinear | 0 / 18432 | 0 | ∞ | 0 | **Exact** |
| `yuv420p` 2× down, area | 0 / 4608 | 0 | ∞ | 0 | **Exact** |
| `yuv420p -> rgb24`, bt709 tv→pc | 7831 / 36864 | **1** | 54.9 | 7059 | Equivalent |
| `yuv420p -> rgba`, bt709 tv→pc | 7831 / 49152 | **1** | 56.1 | 7074 | Equivalent |
| `rgb24 -> yuv420p`, bt709 pc→tv | 824 / 18432 | **1** | 61.6 | 0 | Equivalent |
| `yuv420p` 4× down, lanczos | 148 / 4608 | **1** | 63.1 | 82 | Equivalent |
| `yuv420p -> yuv422p` (chroma up) | 249 / 24576 | 5 | 61.6 | **0** | Divergent (edges) |
| `yuv444p -> yuv420p` (chroma down) | 144 / 18432 | 5 | 65.2 | **0** | Divergent (edges) |
| `yuv420p` 3:2 down, bicubic | 791 / 18432 | 11 | 54.9 | 355 | Divergent |
| `yuv420p` 2× up, bicubic | 1140 / 18432 | 19 | 48.4 | 490 | Divergent |
| `yuv420p -> yuv420p10le` (widen) | 13819 / 18432 | 3 | 54.7 | 7924 | Divergent |
| `yuv420p -> rgb24`, 2× down, bicubic | 8626 / 9216 | 224 | 12.4 | 6905 | Divergent |

† All 282 raw differences on that row are the reference's own clipping defect
(§5.1); zero are ours. The harness counts them separately.

"Interior" excludes a four-sample border of plane 0. Two rows that look
Divergent are **identical everywhere except the edge**, which is a far more
useful statement than the whole-image count: only the reference's boundary rule
differs, and only there.

### What each remaining divergence is

* **`yuv420p -> rgb24` (max 1 LSB, 21% of samples).** The 4:4:4 form of the same
  conversion is byte-exact, so the matrix and both quantisations are right. The
  reference uses a coarser chroma path when the *input* is subsampled: a linear
  model that fits its 4:4:4 output on all 65 536 `(Y, V)` pairs fits none of its
  4:2:0 output at any shift. Not recovered.
* **Chroma up/down at ±5.** Edge rows only; see below.
* **Bicubic at ±11 to ±19.** The kernel is right (§5.2) and bilinear, area and
  lanczos are Exact or ±1, so the machinery is right. Two things are not
  recovered: the reference's boundary rule, and a ±3/16384 difference in its
  coefficient quantisation. Its boundary rule is *not* the weight folding we use,
  and it is not consistent between adjacent output positions either — measured
  directly, output 1 of a 2× upscale folds the out-of-range tap onto the boundary
  sample while output 2 drops it and renormalises.
* **Depth widening at ±3.** We expand by bit replication, so 8-bit 255 becomes
  16-bit 65535. The reference does that for `gray` and a **plain left shift** for
  planar Y'CbCr, where 255 becomes 65280. We took the principled rule rather than
  reproduce an inconsistency; the cost is 1/256 of full scale on a widening
  conversion.
* **Scaled conversions that also change colour space.** Our chroma model
  (resample onto the destination chroma grid, then replicate) is exactly right
  when unscaled and wrong when scaled — measured against an impulse, the
  reference's chroma response for a 2× downscale to `rgb24` occupies a single
  output row where ours occupies two. The output is structurally sound, not
  garbage: `a_flat_colour_survives_a_scaled_colour_conversion` holds for every
  ratio and every colour. This is the largest open gap and the first thing to
  attack next.

### Class A reproducibility

Every path here is integer arithmetic with a defined rounding rule, so output is
identical across thread count, band split and lane width. Asserted by test, not
by intention.

---

## 4. Scope

### Implemented

* Every **byte-addressable** format in `vaco-pixfmt`: planar and packed Y'CbCr at
  8–16 bits, all subsamplings, the NV and P0xx families, packed RGB including the
  16-bit bitfield packings (`rgb565`, `rgb444`, …), planar RGB, gray, alpha, and
  both endiannesses.
* Six resampling kernels: point, bilinear, bicubic (Mitchell–Netravali),
  lanczos, gaussian, area.
* Range conversion; the H.273 matrices that have a linear R'G'B' form; chroma
  subsampling and siting; ordered (Bayer) dither.
* Slice threading, plane-copy and no-op fast paths, the full `sws` option surface.

### Not implemented — refused at plan time, never approximated

| | Why |
|---|---|
| palette (`pal8`) | needs frame side data, which the plan cannot capture yet |
| Bayer mosaics | needs demosaicing |
| floating-point formats | the pipeline is integer end to end |
| sub-byte packings (`monow`, `rgb4`) | not addressable by load-shift-mask |
| XYZ | needs the transfer stage |
| hardware surfaces | not in this address space |
| transfer functions, primaries conversion, tone mapping, `gamma=1` | the whole float colour path |
| constant-luminance, `ICtCp`, `IPT-C2`, `YCgCo-R`, ST 2085 matrices | not a linear transform on R'G'B' |
| error-diffusion and arithmetic dither | `Bayer` and `none` only |
| alpha premultiply and `alphablend` | alpha passes through |
| the streaming `SliceSession` API | `scale_frame` and `scale_planes` only |
| two-stage decimation above `max_taps` | the kernel is narrowed instead |

`uyyvyy411` is refused for a subtler reason: its descriptor says luma is
`step = 1, offset = 1`, which walks straight through the chroma samples at bytes
0, 3 and 6. The layout is real; the *linear description* of it is not. This is
detected structurally (`geometry::check_no_overlap`) rather than by name, so a
future table entry with the same problem is refused too.

---

## 5. How the reference's behaviour was recovered

Per plan 13 §1b, every probe below uses the shortest path to the thing under
test — a `-vf scale` invocation on the rawvideo demuxer, never a nested
filtergraph. The reference is an oracle we query and never a source we read.

### 5.1 The colour matrices are exact, and are 13-bit and 15-bit

```sh
# Y'CbCr -> R'G'B': sweep all 65536 (Y, V) pairs at U = 128.
ffmpeg -f rawvideo -pix_fmt yuv444p -s 256x256 -i - \
  -vf scale=in_range=tv:out_range=pc:in_color_matrix=bt709 \
  -f rawvideo -pix_fmt rgb24 -
```

Fitting `clip((Cy·(Y−16) + Cc·(C−128) + k) >> S)` over that sweep gives a
**unique** solution at `S = 13`, `k = 1 << 12`, with every coefficient equal to
`round(coefficient × 2^13)`. It reproduces all 65 536 outputs.

The reverse direction, fitted the same way over 60 000 random `(R, G, B)`
triples, is `S = 15` with **`k = (1 << 14) + (1 << 8)`** — `half + half/64`, the
residue of a two-stage `>> 9` then `>> 6`. Worth 1/128 of an output LSB and
reproduced because D6 makes byte-identity the contract, not because it is
principled.

### 5.2 `bicubic` is Mitchell–Netravali (0, 0.6), not Catmull–Rom

Plan 17 §A.7.1 says the default bicubic is Catmull–Rom `(0, 0.5)`. It is not.
Scaling an impulse 8× and reading the response at sixteen phases, then
least-squares fitting a cubic to each piece, gives

```
  |x| < 1     :  1.3986 x³ − 2.3981 x² − 0.0007 x + 1.0000
  1 <= |x| < 2:  −0.5996 x³ + 2.9983 x² − 4.7977 x + 2.3990
```

which is `(B, C) = (0, 0.6)` to within 0.002 — and `(0, 0.5)` is nowhere near.
The four taps of a 2× upsample are `0.871875, 0.240625, −0.084375, −0.028125`,
and `filter::tests::bicubic_matches_the_measured_reference_taps` pins them.

### 5.3 Chroma is replicated, not interpolated, on the way to R'G'B'

A single chroma sample raised on a flat background produces an exact 2×2 block of
changed R'G'B' output, with no ringing in the neighbouring rows or columns —
nearest-neighbour upsampling. The same probe against `yuv444p` output shows a
four-tap bicubic response, so the rule is about the *destination* being
unsubsampled, not about the source. `full_chroma_int` selects the interpolated
form here; the reference accepts the flag and ignores it in 8.1.

The mirror image: horizontal chroma *decimation* out of R'G'B' is a plain pair
average whatever the scaler flag says, while the vertical axis takes the selected
kernel. Measured with a single-pixel impulse.

### 5.4 Gray is always full range

`gray -> rgb24` is the identity, `gray -> yuv420p` compresses into 16..235,
`yuv420p -> gray` expands out of it, and `-in_range=tv` on a gray input changes
none of it. So `ImageSpec::effective_range` returns `Full` for gray regardless of
signalling.

### 5.5 Depth conversion is asymmetric

Expansion is bit replication for `gray` (255 → 65535) and a plain shift for
planar Y'CbCr (255 → 65280). Reduction, with `sws_dither=none`, is
`min(max, (v + half) >> shift)` — a shift, not a full-scale rescale. Default
dither for any depth reduction is Bayer.

### 5.6 A deviation we do **not** reproduce

Converting Y'CbCr to R'G'B', the reference emits **0** where the pre-clip value
reaches 512 or more, instead of saturating. It is a table overrun, reachable from
ordinary out-of-gamut chroma — at BT.709 limited range, `Y = 225, U = 255` is
enough, and the whole `U >= 240` corner is affected for bright pixels.

```sh
printf '\xe1\xff\x80' | ffmpeg -f rawvideo -pix_fmt yuv444p -s 1x1 -i - \
  -vf scale=in_range=tv:out_range=pc:in_color_matrix=bt709 \
  -f rawvideo -pix_fmt rgb24 - | xxd    # 0000ff, not ffffff
```

D17 says to reproduce an observable deviation. We do not, for one reason: the
value read is whatever lies past the end of a table, so it is a property of one
build's memory layout rather than of the algorithm, and committing to it would be
committing to something the next reference build may change without notice. We
saturate. `tests/reference_divergence.rs` asserts the deviation still exists, so
its disappearance is a test failure rather than a silent change, and
`vaco_scale::REFERENCE_CLIP_DIVERGENCE` names it in the API.

---

## 6. How to change it

| To add | Touch |
|---|---|
| a pixel format | nothing here, if it is byte-addressable — `vaco-pixfmt`'s table |
| a scaling filter | one variant and one formula in `filter::Kernel` |
| a colour matrix | `vaco-color`; this crate quantises whatever it returns |
| a vectorised kernel | `fast.rs`, plus the agreement test |
| an option | `options.rs`; the derive does parsing, help and serialisation |

### The kernel seam

`fast.rs` holds the vectorised bodies. The rule it exists to enforce: **a kernel
there may only make the generic path faster, never different.** Every entry has a
scalar reference in `exec.rs` that defines its semantics and a test that runs
both over randomised input at every length, including the tails. Adding one is
purely additive — if it is wrong a test says so, and if it is missing the generic
path already works.

The authoring pattern is `vaco-simd`'s: scalar reference, one
`#[inline(always)]` body generic over `S: Lanes`, a dispatching wrapper, a
`KernelSet` entry resolved once at plan time. `#[inline(always)]` is a
correctness-of-codegen requirement, not a tuning knob — it is how the dispatched
level's target-feature context reaches the body.

### Gotchas

* **Component width follows the channel index, not the plane.** Channels 1 and 2
  — and only those — take the chroma decimation, which is why packed `yuyv422`
  (all three channels in plane 0) still decimates correctly. `ceil` division, so
  an odd width keeps a chroma sample for its last pixel.
* **The container is not the component.** `rgb565`'s blue field is five bits at
  shift zero but lives in a 16-bit word; reading one byte of it picks the wrong
  half on a big-endian host. `geometry::container_bits` takes the maximum over
  every component sharing the slot.
* **A bank can be an identity without looking like one.** A bilinear 64 → 64 bank
  has taps `[0, 1, 0]`. `FilterBank::is_identity` checks structurally, so the
  planner deletes it instead of running a three-tap filter over an unscaled
  picture.
* **Band height must be a multiple of the destination's vertical chroma
  decimation**, or a band gets half a chroma row.

---

## 7. Configuration

Options are declared once through `vaco-opts` and parse from a filtergraph-style
string: `scaler=lanczos:param0=4:threads=3`. Every reference name and alias is
preserved, including the ones we would not have chosen.

| Option | Default | Note |
|---|---|---|
| `sws_flags` | `0` | legacy algorithm bitmask; `bicublin` normalises to two kernels |
| `scaler`, `scaler_sub` | `auto` → bicubic | luma and chroma algorithm |
| `param0`, `param1` | NaN | bicubic `B`/`C`, lanczos `a`, gaussian `σ` |
| `src_range`, `dst_range` | `false` | force full range |
| `sws_dither` | `auto` | `auto` = Bayer whenever depth drops |
| `max_taps` | 64 | above it the kernel narrows |
| `threads` | 0 | **0 and 1 both run on the calling thread** |
| `src_h_chr_pos` … | −513 | 1/256 of a chroma sample; −513 is "unset" = no shift |
| `gamma`, `bitexact` | `false` | accepted; `gamma` is reported by `unimplemented()` |

`threads = 0` meaning "serial" is a deliberate difference from the reference,
which reads it as "auto". A library that silently spawns a pool inside a filter
graph that is already parallel makes things slower, and the caller is the one who
knows. Set it above 1 to opt in; the pool is built once, in `Scaler::new`.

Options this crate accepts but does not act on are reported by
`Scaler::unimplemented_options`, so a caller can warn. Refusing an option the
reference accepts is a worse failure than ignoring it — but ignoring it
*silently* is worse than both.

---

## 8. Performance

`cargo bench -p vaco-scale`. Medians on an Apple M-series machine shared with
other builds, against `ffmpeg -benchmark -threads 1 -filter_threads 1` over 60
frames on the same host. Re-measure on a quiet machine before drawing a
conclusion from a small difference.

| Scenario, 1080p, single-threaded | Ours | Reference | Ratio |
|---|---:|---:|---:|
| `yuv420p -> rgb24` | 6.1 ms | 0.67 ms | **9.1× slower** |
| `yuv420p -> nv12` | 2.4 ms | 0.33 ms | **7.2× slower** |
| 1080p → 720p, bicubic | 8.9 ms | 1.5 ms | **5.9× slower** |
| `rgb24 -> yuv420p` | 11.3 ms | — | |
| `yuv420p10le -> yuv420p` | 5.5 ms | — | |
| 720p → 1080p, lanczos | 14.0 ms | — | |
| cold `Scaler::new` | 198 µs | — | paid once per stream |

That gap is real and it is structural, not a missing intrinsic: the generic path
materialises `i32` component planes and makes up to four passes over them, where
the reference fuses read, convert and write into one row kernel. Closing it means
adding fused kernels to `fast.rs` for the hot format pairs — which is what the
seam is designed for and what the differential test makes safe.

Slice threading, on the 1080p → 720p bicubic case:

| threads | 1 | 2 | 4 | 8 |
|---|---:|---:|---:|---:|
| median | 9.6 ms | 6.7 ms | 4.1 ms | 3.2 ms |
| speed-up | 1.00× | 1.44× | 2.35× | **3.02×** |

Measured side by side, in one file, on two row widths (plan 12 PF-0.1):

| A/B | ratio |
|---|---|
| colour matrix row, SIMD vs scalar, 1920 samples | **2.73×** |
| colour matrix row, SIMD vs scalar, 63 samples (tail-dominated) | **1.62×** |

### Two results that contradicted an expectation

Recorded because plan 12 asks for exactly these.

**Clamping the intermediate between the horizontal and vertical passes made
fidelity *worse*.** The reasoning for it was that code values are code values, so
an out-of-range intermediate is meaningless. Measured: 3:2 bicubic improved from
max 11 to 10, but 2× bicubic worsened from 19 to 22 and the chroma downsample
from 5 to 7. Reverted. **Keeping seven fractional bits** between the same two
passes, on the other hand, took bilinear and area from "close" to **Exact** and
cut every other filter's error count by 3–5×. Precision between passes matters;
clipping between them does not.

**Special-casing a one-tap bank as a gather was worth 2.2× on the canonical
conversion** — `yuv420p -> rgb24` went from 13.3 ms to 6.1 ms. That was not
expected to matter: it looked like a trivial multiply by one. It matters because
chroma replication produces a one-tap bank on *both* axes for every Y'CbCr to
R'G'B' conversion, so what looked like a degenerate case is the common one, and
it was paying an `i64` multiply and a shift per chroma sample per axis.

---

## 9. Testing

| Suite | What it holds |
|---|---|
| unit tests | geometry, bank normalisation, the measured colour coefficients, Bayer |
| `tests/properties.rs` | `proptest` over format pairs, sizes, kernels and phases |
| `tests/reference.rs` | the fidelity table above, and the Exact-path gate |
| `tests/reference_divergence.rs` | pins the reference defect we do not reproduce |
| `fuzz/fuzz_targets/scale_convert.rs` | arbitrary format pairs, sizes, options |

The property tests worth knowing about:

* **A flat colour survives a scaled colour conversion**, for every ratio and
  every colour — the one place a structural error in the chroma model would hide
  behind a plausible-looking fidelity number instead of showing as garbage.
* **A constant image survives every kernel and every ratio.** The single most
  valuable property in the crate — it catches normalisation, edge-clamping and
  rounding bugs at once, and it is why bank rows are forced to sum to exactly
  `1 << 14` rather than approximately.
* **Thread count never changes the output**, at 0, 2, 3, 5 and 8 workers.
* **Every format pair at 1×1, 1×7, 7×5, 3×3, 17×9 and 2×2** plans and runs.
* **Channel permutations round-trip exactly** in both directions.

The fuzz target found one real defect: `i32::MIN` reachable in a coefficient slot
from a degenerate kernel, where `c.abs()` overflows in debug. Fixed by widening
before taking the absolute value; the same shape existed in `fast::fits_i32`.

---

## 10. Dependencies

`vaco-core` (errors), `vaco-pixfmt` (the format table), `vaco-color` (matrices,
levels), `vaco-frame` (the frame API), `vaco-limits` (every buffer is sized
through a `Budget`), `vaco-opts` (the option surface), `vaco-simd` (the kernel
substrate), plus `bitflags` and `rayon`.

No media-specific external crate, so D11's single-occurrence rule has nothing to
enforce here — stated explicitly so the CI check's empty result for this crate
reads as correct rather than as a gap. Plan 17 §A.14 assessed `yuv` and
`dcv-color-primitives` and declined both on model fit; nothing here revisits that.
