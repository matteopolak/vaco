# `vaco-codec-prores`

Layer 4. Apple ProRes video decode, native, from the freely-published SMPTE
RDD 36:2022 bitstream specification.

## What it is

A decode-only implementation of Apple ProRes (4:2:2 profiles and the 4:4:4/
4:4:4 XQ profiles including their optional lossless alpha channel), built
directly from `Vaco-Spec-Ref: smpte-rdd36-2022` — a SMPTE **Registered
Disclosure Document**, which (unlike a full SMPTE Standard) is freely
published at <https://pub.smpte.org/doc/rdd36/20220909-pub/rdd36-2022.pdf>.
Encode is out of scope: `planning/research/07-legal-patents-licensing.md`
places ProRes *encode* at legal RED (Apple's own support page names
FFmpeg-derivative encoders as unauthorised), while decode is unconditionally
GREEN in the project's default distributable build.

## How it works

- `src/header.rs` — `frame_header()`, `picture_header()`, `slice_table()`,
  `slice_header()` (RDD 36 SS5.1/5.2, SS6.1/6.2): geometry, chroma format,
  interlace mode, alpha channel type, the two quantization weight matrices,
  and the `slice_size_in_mb` layout algorithm.
- `src/golomb.rs` — the Golomb-Rice/exponential-Golomb combination codes
  (SS7.1.1.1/7.1.1.2) every entropy-coded syntax element in this bitstream
  uses, parameterized by `(lastRiceQ, kRice, kExp)`.
- `src/coeff.rs` — `scanned_coefficients()` (SS7.1.1: adaptive DC-difference
  and AC run/level decode, Tables 9–11) and `scanned_alpha()` (SS7.1.2:
  run-length-coded alpha differences, Tables 12–14).
- `src/scan.rs` — the progressive/interlaced block scan patterns (Figures 4
  and 5) and the slice inverse-scan formula (SS7.2.1) that redistributes a
  flat `scannedCoeffs[]` array back into per-block 8x8 arrays.
- `src/decoder.rs` — ties it together: parses `frame()`, dequantizes
  (SS7.3), inverse-transforms via `vaco_codec_dsp_idct::mpeg2::Idct8x8`
  (SS7.4 is the same IEEE-1180-accuracy classical IDCT MPEG-2 uses — reused
  rather than duplicated, per D19), and writes samples (SS7.5), including
  the progressive/interlaced field-row interleave and the two- vs. four-block
  chroma layouts for 4:2:2 vs. 4:4:4.

### Bit depth is inferred, not signaled

RDD 36 does not carry a bit-depth syntax element (SS1 explicitly excludes
the container format that would normally signal it). Measured against real
`ffmpeg -c:v prores_ks` output across every documented FourCC profile:
`chroma_format == 2` (4:2:2) always pairs with 10-bit samples, and
`chroma_format == 3` (4:4:4) always pairs with 12-bit samples — no
counterexample among Apple's shipped profiles. `decoder::pix_fmt_for`
implements exactly that rule.

## Verified against a real file, per plane

No genuine Apple-encoded ProRes sample was available in this environment.
`ffmpeg 8.1`'s own `prores_ks` software encoder was used instead to produce
four real `.mov` fixtures — 4:2:2 (profile 2), 4:4:4 (profile 4), a
multi-slice 256x144 frame (`-mbs_per_slice 2`, eight slices per macroblock
row), and a 4:4:4 frame with an `alphamerge`-encoded alpha channel — each
compared **Y, U, V (and alpha) separately** against `ffmpeg`'s own decode of
the same file (`tests/oracle.rs`).

Result: alpha is **byte-exact** (RDD 36 states it is coded losslessly, and
the measurement confirms it). Color planes show a small, unstructured
scatter — max difference 1–4 codes out of 1023/4095, mean difference under
0.2 — from IDCT rounding: RDD 36 SS7.4/Annex A permits any
IEEE-1180-accurate implementation, not one mandated integer core, so a
handful of ±1-code differences against one *particular* reference decoder's
own rounding is the expected outcome per the owner's byte-exactness ruling
(`705779d`), not a defect. `tests/oracle.rs` asserts a bound loose enough to
allow that scatter but tight enough (≤ 8, mean < 2.0) to fail hard on a
structured error — wrong geometry, a channel swap, a scan-table
transposition.

## How to change it

A new profile variant (alpha bit depth, chroma layout) starts in
`header::ChromaFormat`/`FrameHeader::bit_depth` and
`decoder::pix_fmt_for`/the block-offset tables in `decoder.rs`. The entropy
codebook adaptation tables (`coeff::dc_diff_codebook`/`run_codebook`/
`level_codebook`) are transcribed directly from RDD 36 Tables 9–11; check
against the primary PDF text before changing any of them; `golomb.rs`'s own
round-trip tests exercise the underlying combination-code arithmetic broadly
but cannot catch a wrong table-to-codebook mapping on their own.

## Configuration

No feature flags beyond the registry's standard `codec-prores`. `vaco_limits`
bounds are applied the same way every decoder in this tree applies them:
`Budget::check_frame` before any per-macroblock table is sized from the
frame header's (attacker-controlled) width/height.

## Dependencies

`vaco-codec-dsp-idct` (the classical IEEE-1180 IDCT, SS7.4), `vaco-bitstream`
(the MSB-first bit reader RDD 36's bitpacking convention matches exactly),
`vaco-frame`/`vaco-pixfmt` (the decoded picture), `vaco-packet`,
`vaco-limits`.

## Fuzzing

`fuzz/fuzz_targets/prores_decode.rs` feeds arbitrary bytes directly as a
packet payload (RDD 36's `frame()` syntax is entirely in-band — no separate
extradata to build). 10.7M executions in 30s, clean, no artifacts.
