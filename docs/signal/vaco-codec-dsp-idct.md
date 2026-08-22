# `vaco-codec-dsp-idct` — inverse transforms

---

## 1. What it is

The inverse transforms codecs apply to residual coefficients, for the three
families that need one:

| Module | Standard | Sizes | Bit-exact? |
|---|---|---|---|
| [`h264`](../../crates/signal/vaco-codec-dsp-idct/src/h264.rs) | ITU-T H.264 §8.5.10, §8.5.11.1, §8.5.12.2, §8.5.13.2 | 4×4, 8×8, plus the 4×4 luma-16×16 DC and 2×2/2×4 chroma DC Hadamards | **Yes — normative** |
| [`hevc`](../../crates/signal/vaco-codec-dsp-idct/src/hevc.rs) | ITU-T H.265 §8.6.4 | 4×4, 8×8, 16×16, 32×32 DCT-II, plus the 4×4 DST-VII for intra luma | **Yes — normative** |
| [`mpeg2`](../../crates/signal/vaco-codec-dsp-idct/src/mpeg2.rs) | ISO/IEC 13818-2 Annex A / IEEE 1180 | 8×8 | Accuracy-bound, not bit-exact — the standard does not mandate one algorithm |

H.264 and HEVC specify their transforms as exact integer procedures: a
conforming decoder must reproduce the standard's arithmetic bit for bit, so
there is no fidelity trade-off here, unlike almost everywhere else in this
project. MPEG-2/JPEG-family codecs instead specify an *accuracy bound*
against the real-valued DCT (peak/mean/mean-square error over a large
statistical sample), which is why `mpeg2` is a thin wrapper over `vaco-tx`
rather than a third hand-written transform.

**Scope boundary.** Each standard splits "reconstruct a residual block" into
*scaling* (QP-dependent dequantisation — `LevelScale` tables, `qP`) and
*transformation* (the fixed procedure this crate implements). Only the
transformation half is here; a codec crate dequantises first and passes the
already-scaled coefficients in.

## 2. How it works

### 2.1 H.264 — a butterfly, not a matrix

`h264::idct4x4`/`idct8x8` implement the standard's row-then-column
add/subtract/shift butterfly (§8.5.12.2 eq. 8-338–8-345, §8.5.13.2 eq.
8-358–8-405) literally, followed by one rounding shift
(`vaco_tx::fixed::round_shift`, reused rather than reimplemented — see §5).
There is **no matrix-multiply equivalent** to cross-check against: the
butterfly's `>>1`/`>>2` interior shifts truncate, and truncation is not
linear over the integers, so no fixed coefficient matrix reproduces it. The
oracle for this module is therefore an independently-written transliteration
of the same equations (`tests/golden.rs`), not an alternative algorithm.

`h264::luma_dc_hadamard4x4`, `chroma_dc_hadamard2x2` and
`chroma_dc_hadamard2x4` implement §8.5.10/§8.5.11.1's small Hadamard
sandwiches (`f = H·c·H`, `f = A·c·A`) using the same row/column engine
(`separable`, in `h264.rs`) with the Hadamard butterfly as the 1-D operator
instead of the core transform's.

### 2.2 HEVC — one matrix, four sizes, and a transpose that mattered

`hevc::TRANS_MATRIX_32` is the standard's own `32×32` integer matrix
(§8.6.4.2 eq. 8-318–8-321), transcribed by script from the primary text, not
typed by hand. Every smaller size is that same matrix, subsampled per
eq. 8-317.

**The one thing worth understanding before touching this module**: the
naive reading of eq. 8-317 — `y[i] = Σⱼ M[i·stride][j]·x[j]`, output index
strided into the row, input index truncated into the column — reproduces
the well-known H.264/HEVC 4-point and 8-point cores *exactly*. It is also
**wrong** for the inverse transform: feed it a pure DC coefficient (the only
nonzero frequency) and the output is not spatially uniform, which it must
be, because the frequency-0 basis function is the constant function by
definition. The reading that satisfies that requirement — checked
computationally for all four sizes before being trusted, in
`hevc::tests::dc_only_32x32_gives_a_uniform_block` and its siblings — puts
the stride on the *summed* index instead: `y[i] = Σⱼ M[j·stride][i]·x[j]`
(`hevc::dct1d`). Matching a remembered table is necessary but not
sufficient; it is possible to match one exactly and still have the wrong
matrix orientation for how it is *used*. See `hevc.rs`'s `dct1d` doc comment
for the full derivation. `dst1d` (the DST-VII, eq. 8-316) takes the same
transpose, for the same reason: the standard states both transforms with the
identical formula shape.

`hevc::idct2d_dct`/`idct2d_dst4` implement the 2-D process (§8.6.4.1):
transform every column, `Clip3((e+64)>>7)`, transform every row — column
first, unlike H.264's row-first order, and with a fixed shift-and-clip
between the passes that H.264 has nowhere. The mid-transform shift is
*always* 7, regardless of size or bit depth; bit-depth dependence lives
entirely upstream, in §8.6.3's coefficient scaling.

### 2.3 MPEG-2 — reusing `vaco-tx`'s DCT-III

`mpeg2::Idct8x8<T>` wraps a `vaco_tx::Tx<T>` built for
`TxKind::Dct, Direction::Inverse, len=8`. `vaco-tx`'s DCT-III is
`y[k] = x[0]/2 + Σₙ x[n]·cos(π(2k+1)n/16)` (unnormalised); the classical
JPEG/MPEG-2 IDCT is the same sum with a `C(0) = 1/√2` weight on the DC term
and an overall `¼`. Pre-multiplying row 0 and column 0 of the input by `√2`
before running the DCT-III down each axis, then scaling the final result by
`¼`, reproduces the classical formula — checked numerically against a direct
`f64` evaluation to ~1e-13 before being trusted, and pinned in this crate's
tests against the same direct evaluation to within `vaco-tx`'s own Class C
bound.

## 3. How to change it

- **A new H.264/HEVC size or variant**: for H.264, add a new 1-D butterfly
  function (`rowN`) mirroring the standard's equations directly, then reuse
  `separable::<N>` — do not write a new row/column driver. For HEVC, if it is
  another DCT-II size, `dct1d::<N>` already handles it via `row_stride`; if
  it is a genuinely new matrix (another DST variant, say), add it next to
  `DST_MATRIX_4` and give it its own `*1d` function using the same
  transposed-access pattern `dct1d`/`dst1d` share — do not assume the naive
  `M[i][j]` reading is correct without re-deriving via a property no
  reasonable implementation could get away with skipping (DC-uniformity, or
  an equivalent).
- **Suspect the matrix orientation, not the table, first.** If a symptom
  looks like "close but subtly wrong; unit tests for one probe pass, a
  round-trip property fails" — re-read §2.2 before re-checking the numbers.
- **Never add a clip/round inside a 1-D `dct1d`/`dst1d` call.** HEVC's 1-D
  transform is pure integer matrix multiply by definition (§8.6.4.2); all
  rounding lives in the 2-D driver (`idct2d`), between the two passes only.
  Property tests (`hevc_dct8_is_linear`) assert this and will catch a
  regression.
- **Extending `mpeg2`**: this module intentionally does not implement a
  specific fast integer approximation (AAN, LLM, etc.) — the standard does
  not require one, and `vaco-tx`'s DCT-III already meets the accuracy bound.
  If a codec needs a specific fast integer IDCT for its own reasons, that
  belongs in that codec's own crate, checked against this module's direct-`f64`
  reference (`mpeg2::tests::direct_f64`) for accuracy, not duplicated here.

## 4. Configuration

None. Every function is a pure transform of an in-memory coefficient array;
there is no QP, no bit depth (beyond `hevc::ClipRange`, below), no I/O.

- **`hevc::ClipRange`** — the `CoeffMin`/`CoeffMax` clip applied between
  HEVC's two 1-D passes (§7.4.9.11 eq. 7-27/7-28). `ClipRange::non_extended()`
  (`±2^15`) covers every bit depth when
  `extended_precision_processing_flag == 0`, which is the overwhelming common
  case; a decoder that has set that flag computes its own range from
  eq. 8-304 and constructs `ClipRange` directly.

## 5. Dependencies

- **`vaco-tx`** (same layer, `signal`) — checked first per D19 before writing
  anything new. Reused directly for [`mpeg2`] (its DCT-III *is* the
  transform, not merely similar to it) and for one primitive,
  [`vaco_tx::fixed::round_shift`] (`(x + 2^(s-1)) >> s`, saturating), which
  is exactly the "add a rounding bias, shift, never panic on adversarial
  input" operation both H.264 and HEVC specify for their rounding steps.
  **Not reused**: `vaco-tx`'s own `i32` path, which is a Q31 `[-1, 1)`
  fixed-point contract for the mathematical DCT/FFT family — a different
  arithmetic contract from either standard's plain-integer butterfly/matrix,
  so forcing H.264/HEVC through it would mean re-deriving the standard's own
  tables in a rescaled basis for no shared implementation once written out.
- **`vaco-core`** — the `Result`/`Error` type `mpeg2::Idct8x8::new` returns.
- **`proptest`, `divan`** (dev-only) — property tests and benchmarks.

No `vaco-simd`: every function here operates on 4–32 element arrays with a
fixed shape known at compile time, and the specified/idiomatic scalar shape
(`.iter().zip().sum()` for HEVC's dot product, a literal transliteration of
the add/shift equations for H.264) is what this crate ships. A
divan-measured alternative (splitting the HEVC size-32 dot product across 8
manual `i64` accumulators, following plan 12's PF-0.3 recommendation) was
**~2× slower**, not faster (`benches/idct.rs`), reproducing that plan
amendment's own conclusion — "accumulator splitting must exceed the target's
vector width, not match it" — on this crate's own kernel rather than
assuming the earlier measurement transfers. No SIMD path is proposed here
until a real profile shows this crate on a hot path where the specified
scalar shape measurably loses.
