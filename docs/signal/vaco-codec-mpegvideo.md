# `vaco-codec-mpegvideo`

Layer 3. Shared decode-side machinery for the MPEG-heritage macroblock
family — H.261, H.263, MPEG-1/2, MPEG-4 Part 2, MSMPEG4, WMV1/2, FLV1,
RV10/20 (D-22, epic #25).

## What it is

Six modules, each a self-contained piece of the macroblock-layer shape every
member of this family shares, extracted and generalised from
`vaco-codec-mpeg12`'s working, `ffmpeg`-checked decoder rather than written
fresh:

- `refpic` — `PictureReorderBuffer<T>`, the decode-order-to-display-order
  fix-up every member with B-pictures needs (hold the most recently decoded
  reference until the *next* one is decoded).
- `sequential_mv` — `SequentialMvPredictor`, MPEG-1/2's own motion-vector
  prediction: carry the previous coded macroblock's own vector forward, not
  a spatial median.
- `motioncomp` — `Sampler`, `form_prediction`, `average_predictions`: half-pel
  motion compensation by bilinear averaging, generalised over frame/field
  addressing via a `row_scale`/`row_parity` pair.
- `mbtype` — `MbTypeFlags`, `MbTypeEntry`, `decode_mb_type`: the
  macroblock-type flags vocabulary (intra / pattern / quant / forward /
  backward) every family's own table maps its codewords onto, plus a thin
  wrapper over `vaco-codec-vlc::VlcTable` — no table *data* lives here.
- `coeff` — `ZIGZAG_SCAN`/`ALTERNATE_SCAN`, `inverse_scan`,
  `dequantise_mpeg` + `mismatch_control_mpeg1`/`_mpeg2`, `flat_step_dequantise`,
  `run_idct`: the residual pipeline from entropy-decoded coefficients to a
  spatial-domain block.
- `resync` — `find_bit_pattern`: bounded, panic-free search for a
  fixed-length bit pattern that need not be byte-aligned (H.261/H.263's own
  GOB/slice start codes, unlike MPEG's byte-aligned Annex-B ones).

## How it works

### What "shared" means, concretely

D-22's own brief is to factor the macroblock-layer *shape* out without
forcing every family's header syntax, VLC tables or quantisation constants
into one API. Each module above states its own confidence level in its doc
comment: `refpic`/`sequential_mv`/`motioncomp`/the MPEG half of `coeff` are
generalised from already-`ffmpeg`-checked code in `vaco-codec-mpeg12`;
`mbtype` and `resync` are new but purely structural (no spec-derived
constant); H.263's own dequantisation formula and every family's spatial
(median-of-neighbours) motion-vector predictor are **not** included, because
neither has been measured against a real decoder in this codebase yet, and
this crate's own rule (matching the rest of the project) is that a
spec-derived numeric formula recalled but unverified is worse than not
having it — see `coeff`'s and `sequential_mv`'s module docs.

### No unifying trait

`mbtype::MbTypeFlags` is a concrete struct, not a trait object or a generic
macroblock-loop parameter. A family's own decode loop wants to match on
which flags came back and branch directly (read a quantiser-scale code if
`quant`, read forward vectors if `motion_forward`, ...); a trait would only
add an indirection layer for no caller. The two-different-table
demonstration this design choice needs to justify itself lives in this
crate's own `tests::two_families_use_the_shared_pipeline_differently`: an
MPEG-style table (with a `quant` bit) and an H.263-style table (without one)
both decode correctly through the exact same `decode_mb_type` call.

### Motion compensation is cross-checked against an independent oracle

`motioncomp::avg2`'s rounding (`(a + b + 1) >> 1`, i.e. H.262 §4.1's `//`
operator) is checked in `avg2_agrees_with_dsp_mc_bilinear_tap_set` against
`vaco-codec-dsp-mc::fir::taps::BILINEAR` — a tap set written for an entirely
different purpose (a SIMD-dispatched two-pass FIR engine) that happens to
encode the identical rounding rule. Per this project's own "an oracle you
wrote shares your misreading" lesson, two transcriptions of the same
arithmetic from the same head would not be worth much; two independently
motivated implementations agreeing is.

## How to change it

- Adding a family (H.263, MPEG-4 Part 2 once unblocked, ...): supply its own
  header parsing and its own `MbTypeEntry` table data, call into this
  crate's shared functions for the pieces above, and add its own
  dequantisation formula either as a new function here (once measured
  against a real decoder) or in the family's own crate via
  `flat_step_dequantise`.
- `vaco-codec-mpeg12` has **not** been refactored onto this crate — it is a
  different agent's actively-owned crate this session, and its brief only
  allows the swap if its own differential fixtures keep passing unchanged.
  Its owner should migrate module by module (`motion.rs` onto `motioncomp`
  first, being the most direct 1:1 mapping) and verify each swap against its
  existing fixtures before committing it.
- If a future family's motion-vector prediction is the H.263-style spatial
  median rather than MPEG's sequential carry-forward, that is a new module
  (`median_mv`, say) alongside `sequential_mv`, not a generalisation of it —
  the two are different algorithms operating on different state (a single
  per-direction predictor vs. a row of already-decoded neighbour vectors),
  and forcing one trait over both would be the "lowest common denominator
  API" this crate's own brief warns against.

## Configuration

None. No features, no environment variables.

## Dependencies

`vaco-bitstream` (`BitReader`), `vaco-codec-vlc` (`VlcTable`/`VlcEntry`,
inside `mbtype`), `vaco-codec-dsp-idct` + `vaco-tx` (`Idct8x8<f32>`, inside
`coeff::run_idct`). `vaco-codec-dsp-mc` is a dev-dependency only, used by
`motioncomp`'s own cross-check test.

## Verification

35 unit/property tests across the six modules (`cargo test -p
vaco-codec-mpegvideo`): `refpic`'s I/P/B/B reorder sequence reconstructed
from `vaco-codec-mpeg12`'s own decode-order example; `motioncomp`'s
integer/half-pel-horizontal/half-pel-diagonal/field-addressing cases plus the
`vaco-codec-dsp-mc` cross-check; `coeff`'s scan-table permutation check,
MPEG dequantisation/mismatch-control cases ported from `vaco-codec-mpeg12`'s
own tests, and a DC-only-block-is-uniform property check on `run_idct` (an
oracle-diversity check, not a second transcription of the IDCT itself);
`mbtype` and `resync` each have a `proptest` property (arbitrary bytes
against an arbitrary table/pattern never panics) in addition to their unit
tests, since both take a caller-supplied `BitReader` over data this crate
does not itself validate is well-formed.

No fixture-based (real-stream) verification: this crate has no `Decoder`
of its own to run a real bitstream through — that only exists once a family
crate calls into it, which has not happened yet (`vaco-codec-mpeg12` stays
on its own, independently-verified implementation this session; see "How to
change it" above). `vaco-codec-mpeg12`'s own 39 tests were re-run unchanged
alongside this crate's own suite to confirm nothing here disturbed it.
