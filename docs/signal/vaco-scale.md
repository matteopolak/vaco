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
    -> affine, transfer, or 3D-LUT colour transform         (colour)
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
| colour matrix | 13-bit fixed point out to R'G'B', 15-bit out to Y'CbCr; `f64` for transfer, primaries, tone and LUT stages |
| pack | round or Bayer-dither down to the destination depth, then clamp |

Filter accumulation is `i64`, so no input can overflow it; each bank records its
`Sum |c|` so a future `i32` kernel can select itself on a proof rather than on
the depth happening to be eight.

### 2.5 Transfer characteristics and primaries

When resolved source and destination transfer characteristics or primaries
differ, the ordinary integer affine stage is replaced by one scalar `f64`
colour stage at the common mid grid:

```text
coded Y'CbCr/R'G'B' -> normalised R'G'B' -> linear light
  -> Bradford-adapted RGB-primary matrix -> destination R'G'B'
  -> destination Y'CbCr/R'G'B' codes
```

There is no intermediate integer rounding: range expansion, both Y'CbCr
matrices, the transfer pair, and the primary matrix are evaluated before final
destination quantisation. The primary transform is
`M_dst^-1 * Bradford(src_white, dst_white) * M_src`, where `M` comes from
`vaco-color`'s H.273/RP 177 chromaticities. Equal primaries skip Bradford.

Omitted primaries and transfer characteristics resolve from explicit matrix
signalling (`bt2020` -> BT.2020 / BT.2020-10, `bt470bg` -> BT.470BG / gamma
2.8, and so on); otherwise RGB defaults to BT.709. These values participate in
the copy-fast-path comparison, so a frame whose colour metadata changes cannot
silently bypass conversion.

### 2.6 Tone mapping, intents, and the 3D LUT

When HDR peak luminance changes, or a non-colourimetric rendering intent needs
gamut mapping, planning builds one bounded `Lut3D`. The default grid is `33³`;
`ScaleOptions::lut3d_size` accepts `9..=65`. Its axes are *coded* source RGB,
not linear RGB: PQ is steep at black, and a uniform coded grid preserves that
shadow detail before the LUT decodes transfer, changes primaries, maps tone and
gamut, and returns destination-linear RGB for the final destination transfer
encode. The table is allocated through the caller's `Budget` at plan time and
is shared across every frame using that `Scaler`.

Samples use tetrahedral interpolation, not trilinear interpolation. The cube is
split by the order of its three fractional coordinates; every resulting
tetrahedron includes the black-to-white diagonal. An identity or affine lattice
cannot distinguish the two methods, so `tests/lut3d_reference.rs` uses
non-affine corners and samples every one of the six coordinate orders against
ffmpeg's `lut3d=interp=tetrahedral` black box.

`ImageSpec::with_hdr_peaks(mastering, content_light)` supplies peak luminance
to the raw-plane API. `scale_frame` obtains the same values from
`MasteringDisplay` and `ContentLightLevel` side data. Mastering peak wins over
content-light peak; missing metadata resolves to 10,000 nits for PQ, 1,000 for
HLG, and 100 for SDR.

| `ScaleOptions::intent` | Policy |
|---|---|
| `relative_colorimetric` (default) | Bradford-adapt white, then clip outside the destination RGB boundary. |
| `absolute_colorimetric` | Keep the original white point, then clip. |
| `perceptual` | Apply the BT.2390 PQ-domain Hermite EETF when peak decreases, then smooth chroma compression. |
| `saturation` | Preserve the neutral/chroma direction and scale it to the destination RGB boundary. |

An HDR source whose peak is above the destination always applies BT.2390's
peak-aware EETF, regardless of intent. The independent `tone_oracle` test owns
the PQ, BT.709, and Hermite equations so it does not reuse either the production
transfer implementation or the production LUT builder; the 1,000-to-100-nit
PQ-to-BT.709 patch set measured a maximum 1 LSB LUT error.

---

## 3. Fidelity against the reference

Measured, not asserted. `cargo test -p vaco-scale --test reference -- --nocapture`
reproduces the table; `paths_graded_exact_stay_exact` turns a regression in an
Exact path into a test failure. ffmpeg 9.0.1, `aarch64-apple-darwin`, 128×96
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
| `yuv420p` 2× down, `Point` (nearest-neighbour) | 0 / 4608 | 0 | ∞ | 0 | **Exact** ‡ |
| `yuv420p` 2× up, bilinear | 0 / 18432 | 0 | ∞ | 0 | **Exact** |
| `yuv420p` 2× down, area | 0 / 4608 | 0 | ∞ | 0 | **Exact** |
| `yuv420p -> rgb24`, bt709 tv→pc | 7831 / 36864 | **1** | 54.9 | 7059 | Equivalent |
| `bt709 Y'CbCr -> bt2020 Y'CbCr`, in gamut | 144 / 36864 | **1** | 72.2 | — | Equivalent |
| `bt2020 Y'CbCr -> bt709 Y'CbCr`, in gamut | 192 / 36864 | **1** | 71.0 | — | Equivalent |
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

‡ `Point`'s own filter bank used to widen its support by the same `1/xscale`
stretch every band-limiting kernel here needs on downscale, which is
correct for `Bilinear`/`Bicubic`/`Lanczos`/`Gaussian` (and `Area`, whose
support *is* meant to track the decimation ratio) but wrong for a hard
nearest-sample pick: on a 2:1 downscale it made two adjacent source samples
land inside the stretched box at equal weight, quantising to a 50/50 blend
of both rather than one exact tap — measured directly against ffmpeg's own
documented `2*d+1` nearest-sample rule on an 8-wide ramp. `Point` now keeps
`xscale = 1` unconditionally (`filter.rs::build_bank`), and this row (never
tested against the reference before, since the filter driving `scale`'s own
`flags=` never reached `Point` until that plumbing bug was also fixed — see
`docs/filter/vaco-filter-video-geometry.md`) is the direct confirmation.

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

### Reproducibility

Integer paths remain Class A: output is identical across thread count, band
split and lane width. The transfer/primaries path is Class C because `f64`
transcendentals are intentionally not a bit-exact reference implementation;
it is checked against an independent high-precision oracle and against ffmpeg's
direct in-gamut Y'CbCr `colorspace` probes (max 1 LSB, at least 67 dB).
Tone and gamut LUTs are also Class C: the table is deterministic for a fixed
grid and options, and tests cover every intent, a published-equation BT.2390
oracle (max 1 LSB on the HDR patch set), and ffmpeg 9.0.1 tetrahedral
interpolation (exact on six non-affine simplex-order samples). They are
intentionally not declared byte-identical to a reference tone mapper because
ffmpeg 9.0.1 exposes no BT.2390 mode in its `tonemap` filter.

---

## 4. Scope

### Implemented

* Every **byte-addressable** format in `vaco-pixfmt`: planar and packed Y'CbCr at
  8–16 bits, all subsamplings, the NV and P0xx families, packed RGB including the
  16-bit bitfield packings (`rgb565`, `rgb444`, …), planar RGB, gray, alpha, and
  both endiannesses.
* Six resampling kernels: point, bilinear, bicubic (Mitchell–Netravali),
  lanczos, gaussian, area.
* Range conversion; the H.273 matrices that have a linear R'G'B' form;
  transfer characteristics; Bradford-adapted primary conversion; HDR peak
  metadata, BT.2390 tone mapping, four ICC-named intents, bounded 3D LUTs with
  tetrahedral interpolation; chroma subsampling and siting; ordered (Bayer)
  dither.
* Float pixel-format proxies (`grayf16`/`grayf32`, `rgbf16`/`rgbf32`) at the
  frame boundary. The colour stage itself retains `f64` until final quantisation.
* Slice threading, plane-copy and no-op fast paths, the full `sws` option surface.

### Not implemented — refused at plan time, never approximated

| | Why |
|---|---|
| palette (`pal8`) | needs frame side data, which the plan cannot capture yet |
| Bayer mosaics | needs demosaicing |
| XYZ | needs the dedicated XYZ pixel-format path |
| sub-byte packings (`monow`, `rgb4`) | not addressable by load-shift-mask |
| hardware surfaces | not in this address space |
| `gamma=1` | needs filter placement inside the linear-light domain, not only colour conversion |
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

### The E2E-GAPS 2160p→1080p gap, isolated and profiled (2026-08-30)

`planning/E2E-GAPS.md` §9 measured `vaco -i uhd.mp4 -vf scale=1920:1080` at 29x
slower than `ffmpeg`, but that number is the whole CLI invocation — decode,
scale and mux together — and never isolated which part is the scaler's own.
Two rounds of that document's profiling had already worked the H.264 decode
loop; this pass is the part that was left.

**Isolating the scaler's cost.** Decode-only (`-c:v rawvideo -f null -`)
against decode+scale (`-vf scale=1920:1080 -c:v rawvideo -f rawvideo`), same
4K 75-frame clip, 8 interleaved launches each, wall clock (`date +%s.%N`; a
niced fuzz sweep was running throughout, so best-of/paired-diff is what
matters, not the absolute figures):

| | decode-only | decode+scale | diff (scaler's share) |
|---|---:|---:|---:|
| best-of-8 | 7.96 s | 9.88 s | — |
| per-round diff, min..max | — | — | **1.30 s .. 1.97 s** (mean 1.71 s) |

So on this clip the scaler itself is on the order of 1.3–2.0 s of the 7.53 s
end-to-end figure in §9 — real, but a minority of it; most of that figure is
still decode, which is being worked separately. `cargo bench -p vaco-scale`'s
`convert::yuv420p_2160p_to_1080p_bicubic` entry (added this round, exact same
resolutions and pixel format as the e2e scenario) reproduces this in-process:
75 frames at its fastest 22.6 ms/frame is 1.70 s, consistent with the CLI
measurement.

**What filter this is.** Default `scaler=auto` resolves to bicubic on both
sides — `ffmpeg`'s `swscale` default and this crate's (§7, §5.2) — so this is
a fair like-for-like comparison, not a quality-for-speed trade. Nothing below
changes a single output byte (checked directly, see "Correctness" below);
only which instructions compute the existing ones.

**Profiling.** `samply record --unstable-presymbolicate` against the release
bench binary (`cargo bench -p vaco-scale --bench scale --no-run`, then
`samply record -- <binary> --bench yuv420p_2160p_to_1080p_bicubic --min-time
12`, using divan's own `--bench` flag and `--min-time` for a long enough
capture — running the binary directly without `--bench` silently no-ops it,
which cost a first attempt). Presymbolication resolved function names but no
line numbers on this binary (no separate dSYM), and — because `filter_h`'s
callers are `#[inline]` — almost everything landed on one call site
(`run_band`'s `build_mid(...)` line), which hid the real hot line entirely.
`dsymutil` on the bench binary plus `llvm-symbolizer --obj=<dSYM> --inlines`
recovers the full inline chain per address and fixes this: the innermost
frame is the real leaf, not the outermost call site. Aggregating self-time by
that innermost frame gives the top cost centres:

| self time | location |
|---:|---|
| 17.3% | `core::slice::index` — bounds-checked `.get()` building `filter_h`'s tap window |
| 14.2% | `filter_h`, unattributed line (inlined slice machinery) |
| 11.4% | `Zip::next` — the tap accumulation loop's iterator advance |
| 6.3% | `cmp::min` |
| 5.8% + 2.8% + 2.5% | `filter_h`'s `acc += i64::from(c) * i64::from(s)` line, split across sub-instructions |
| 9.9% | `filter_v`'s equivalent accumulation line |
| 3.9% + 3.6% | more `Iterator`/`Enumerate` machinery in `filter_h` |
| 1.4% | `checked_mul` computing `d * taps` afresh every output pixel |

Roughly half of total self time was in `filter_h`'s tap loop, and almost none
of it was the actual multiply-accumulate — it was `Option`-returning bounds
checks, `Iterator::zip`/`Enumerate::next`, and a per-pixel `checked_mul`,
around a loop whose trip count (`bank.taps`) is a runtime field the optimiser
cannot see as constant. Probing the real plan (`3840x2160 yuv420p -> 1920x1080
yuv420p`, default options) showed `up.h`/`up.v` both resolve to **8-tap**
banks — this is the algorithmic/bookkeeping class of cost plan
`AGENT-CONSTRAINTS.md` asks to check first, not a missing vector instruction:
the arithmetic per pixel is trivial, the loop *scaffolding* around 8 iterations
is not.

**The fix.** `filter_h` now dispatches on `bank.taps` to a
`filter_h_fixed::<N>` body for `N` in `{2, 4, 6, 8}` — bilinear, an unscaled
cubic, an unscaled `a=3` lanczos, and this exact 2x-bicubic-downscale case —
falling back to the untouched original loop (renamed `filter_h_generic`, and
kept as the semantic reference) for any other tap count. The only change
inside the fixed body is converting the `taps`-wide coefficient and window
slices to `&[i32; N]` via `try_into()` (one bounds check) before the
accumulation loop, so the loop's trip count is carried in the type rather
than read from `bank.taps` at runtime — that is what lets it unroll. Same
round, same order of `i64` additions, so the output is identical: integer
addition without overflow (guaranteed by `abs_sum`, see §2.4) does not care
about association order. `filter_v`'s smaller share (~10% of self time,
concentrated in the width-loop rather than the tap loop) was left alone this
round.

**Measured, `cargo bench -p vaco-scale`, interleaved before/after binaries
(the "before" build from a clean `git checkout` of `exec.rs`, the "after" from
this change, both bench profiles, alternating within each of 10 independent
process launches, `--min-time 1.5`, fastest-of-sample from divan's own
output):**

| round | before | after | after/before |
|---:|---:|---:|---:|
| 1..10 | 21.2–23.9 ms | 17.0–19.4 ms | 0.73–0.90 |

**10/10 rounds favoured "after"**, mean ratio ≈0.80 (≈1.25x). The same binary
pair's full `convert` group (single launch each, for context — not the
interleaved claim above) shows the specialisation reaching every bench entry
whose tap count happens to land on 2/4/6/8:

| entry | before (fastest) | after (fastest) | ratio |
|---|---:|---:|---:|
| `yuv420p_2160p_to_1080p_bicubic` (the e2e scenario) | 21.17 ms | 16.67 ms | 0.79x |
| `yuv420p_downscale_bicubic` (1080p→720p) | 7.88 ms | 4.54 ms | 0.58x |
| `yuv420p_upscale_lanczos` (720p→1080p) | 12.06 ms | 7.44 ms | 0.62x |
| `rgb24_to_yuv420p_1080p` | 8.33 ms | 7.62 ms | 0.91x |
| `threads=1` (1080p→720p bicubic) | 7.68 ms | 4.56 ms | 0.59x |
| `yuv420p_to_rgb24_1080p` (one-tap gather, no arithmetic bank) | 5.17 ms | 5.15 ms | 1.00x (expected — untouched path) |
| `yuv420p_to_nv12_1080p` (plane copy) | 2.09 ms | 2.06 ms | 1.00x (expected — untouched path) |
| `yuv420p10le_to_yuv420p` (depth only, no resampling) | 4.57 ms | 4.58 ms | 1.00x (expected — untouched path) |

The three "expected 1.00x" rows are the important negative-space check: paths
that never call `filter_h`'s arithmetic branch (the gather fast path, plane
copy, and depth-only reduction) show no change, which is what "additive, not
different" should look like.

This revises §8's headline table's "1080p → 720p, bicubic" and "720p → 1080p,
lanczos" rows downward (measured on this pass's machine, so not directly
comparable to §8's medians from a different day — re-run both to compare on
equal footing).

**Correctness.** A standalone harness (`Scaler::scale_planes` called directly
over a deterministic synthetic 4K `yuv420p` frame — no decoder involved, to
keep the H.264 rework other agents had in flight out of the comparison)
scaled to 1080p with default options, before and after, MD5-matches
byte-for-byte: `203c30f5ab6350691a166884b4098d62` both times. `cargo test
-p vaco-scale` — including `tests/reference.rs`'s `paths_graded_exact_stay_exact`
Exact-path gate and the `a_constant_image_survives_every_kernel_and_every_ratio`
property — all pass unchanged. A new differential test
(`exec::filter_h_fixed_tests::fixed_and_generic_agree_at_every_tap_count_and_length`)
checks `filter_h_fixed::<N>` against `filter_h_generic` for `N` in `1..=9` and
several `src_len`/`dst_len` combinations including truncated and empty
outputs, so a future change to either body that disagrees fails a test rather
than shipping quietly.

### Fixed-width vertical filtering (measured 2026-09-04)

The same 3840×2160 to 1920×1080 bicubic benchmark was profiled again before
changing the remaining vertical pass. A 12-second, 4 kHz Samply capture of the
unchanged bench resolved 224 of 226 sampled addresses. By outermost emitted
function, `filter_h` accounted for 72.36% of self samples, `filter_v` for
14.41%, and `run_band` for 12.88%. By innermost inline frame, `filter_v` itself
still accounted for 12.17%. That made the vertical filter material but bounded:
even removing it entirely could improve this isolated workload by only about
17%.

`filter_v` now dispatches non-gather banks with 2, 4, 6, or 8 taps to an
output-major fixed-width dot product. It validates the coefficient slice and
collects the input-row references into a stack array before touching the
output. Each output accumulator then stays in a register for all taps instead
of being loaded from and stored to the scratch row once per tap. The original
tap-major implementation remains `filter_v_generic`, both as the independent
test oracle and as the fallback for gather banks, unusual tap counts, or an
incomplete row window. To extend the specialization, add a dispatcher arm and
include that width in the direct differential test; do not remove the fallback
or allocate row references per output row.

The performance comparison used separately preserved before/after bench and
CLI binaries from the same working tree. Every Cargo invocation used the bench
or `dist` profile, a private target directory, `CARGO_INCREMENTAL=0`, an empty
`RUSTC_WRAPPER`, and at most two build jobs. Timings are 12 rotating,
interleaved process launches after one warm-up per binary. Wall time is
`time.perf_counter()` and child CPU seconds are the delta of
`getrusage(RUSAGE_CHILDREN)` user plus system time. These are seconds, not CPU
cycles; raw cycle measurement was unavailable on this macOS host.

| 12-round median | before | after | after/before |
|---|---:|---:|---:|
| isolated bench, wall | 1.665353 s | 1.541039 s | **0.9253** |
| isolated bench, child CPU | 1.656141 s | 1.522984 s | **0.9196** |
| H.264 decode + scale, wall | 8.330905 s | 8.263187 s | **0.9919** |
| H.264 decode + scale, child CPU | 8.281395 s | 8.203405 s | **0.9906** |

The isolated commands ran `yuv420p_2160p_to_1080p_bicubic --min-time 1.5`.
The end-to-end fixture was generated fresh from `testsrc2` with libx264: 75
frames at 3840×2160, 25 fps, High-profile `yuv420p`. The three rotating
end-to-end commands used the before and after `vaco` binaries and `ffmpeg
-threads 1 -filter_threads 1`, all decoding the same fixture and scaling to
1920×1080 into a null output. The ffmpeg medians were 0.781361 s wall and
0.953031 s child CPU, making the after/ffmpeg ratios 10.58× wall and 8.61× CPU.
Load rose from about 5 to 15 during the earliest end-to-end rounds, so the
interleaved ratio and child CPU result carry the claim, not any individual wall
sample.

Correctness was checked independently at `-filter_threads 1`, 2, 4, and 8 with
decode fixed at `-threads 1`. Every before and after invocation emitted exactly
233,280,000 rawvideo bytes; all eight files had SHA-256
`48a301e83b07b9a6d03b7c9b2350973182b2cbbf0e58eaa8ebd248c4e75351b6`,
and every pair passed `cmp`. The differential unit test also exercises taps
1 through 9, empty and truncated output widths, and incomplete source windows;
for 2/4/6/8 taps it calls the fixed function directly so an accidental fallback
cannot make the test vacuous.

A post-change Samply capture could not be collected. Three invocations of the
same 4 kHz, 12-second command failed immediately, before creating an output
file, with macOS reporting `Unknown(1100)`. A privileged process check found no
other Samply, xctrace, Cargo, or rustc process, and the third attempt ran after
load returned to about 5.7. The before profile therefore establishes the hot
callee, while the post-change hotspot movement is explicitly unverified. No
Linux checkasm cycle adapter was part of that optimization commit.

#### Isolated vertical-filter cycle adapter

The default-off, documentation-hidden `checkasm` feature exposes an opaque
synthetic vertical-filter case plus two runners to the downward-dependent
`vaco-checkasm` tool. `Grid`, `filter_v_generic`, and `filter_v_fixed` remain
private production details: the adapter is a child of `exec`, so it can invoke
both shipped callees directly without making them part of the normal public
API. The feature adds no codec or format path and is not enabled by default.

The adapter covers fixed tap counts 2, 4, 6, and 8 across tail-sensitive widths
and short one-, two-, and three-row shapes. Its production benchmark is an 8-tap 1920×1080
vertical pass. Both generic and fixed runners allocate equal output and scratch
storage before entering their row loops, keeping the checkasm
`adapter-inclusive` scope symmetric while leaving the per-row comparison free
of adapter allocation. On a permitted Linux x86_64/aarch64 host, checkasm
reports direct unmultiplexed PMU readings as `backend=perf-event unit=cycles`;
macOS, unsupported targets, and restricted or multiplexed Linux counters report
the existing `backend=instant unit=ns` fallback instead. Nanoseconds are never
reported as CPU cycles.

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
