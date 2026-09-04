# Performance programme, planned 2026-09-01

The optimisation plan that `planning/PERF-BASELINE.md` was measured for. It is
written to be executed by several agents in parallel without them invalidating
each other's work or measurements, and it is deliberately specific about
evidence: every item says whether it rests on a **measured share** (data) or on
a **belief about what a change would do** (hypothesis), because this repository
has six recorded optimisations that reasoned correctly and measured slower.

Binding context, in order: `planning/00-decisions.md` **D2** (no `unsafe`),
**D6/D7/D15** (the `ffmpeg` binary is a black box; its source and FFmpeg-family
sources stay unopened), **D19** (one definition per concept), **D20** (no
design is load-bearing for its own sake; byte-exactness and `forbid(unsafe)` do
not move). `planning/AGENT-CONSTRAINTS.md` "Profile the callee before you
optimise the caller", "Under background load, measure cycles and interleave",
"A benchmark where both paths tie exactly is measuring the optimiser", and
"'I'll resume automatically when it finishes' — you will not".

Nothing in this document was implemented. Section 9 lists the additional
measurements taken while writing it (all on the baseline session's own `dist`
binary and fixtures, which survive in this session's scratchpad), so that the
plan's corrections to the baseline are themselves data rather than opinion.

---

## 0. Summary

| | today (baseline §1) | realistic after this programme | ceiling if every item hits its bound |
|---|---:|---:|---:|
| H.264 4K decode vs `ffmpeg -threads 1` | 11.2x | **~5–5.7x** | ~3.5x |
| H.264 4K decode vs ffmpeg default | 15.5x | **~5–6x** | ~4x |
| HEVC 4K decode vs `ffmpeg -threads 1` | 7.7x | **~3–3.5x** | ~2.5x |
| HEVC 4K decode vs ffmpeg default | 26.5x | **~4–5x** (with WPP threading) | ~3x |
| AAC decode | 217x | **~20–25x** | ~15x |
| transcode H.264→FFV1 1080p, default threads | 13.7x | **~3–4x** (encoder-bound today) | ~2.5x |
| decode+scale 2160p→1080p, default | 14.1x | tracks H.264 decode | |
| H.264 4K peak RSS, 1 thread | 3.9 GiB | **< 0.5 GiB** | |

**Parity with ffmpeg is not reachable under D2** for the video decoders, and
this document does not pretend otherwise. ffmpeg's inner loops are hand-written
NEON/AVX assembly with per-partition block kernels, and its CABAC engine is
hand-scheduled; ours must come from autovectorised safe Rust and
`fearless_simd`. The measured evidence in this repository (Group 1–4 of
`docs/core/simd-adoption-measurements.md`: compositions reach 0.8–1.5x of the
native instruction; the `boundary_strength` and `filter_h` rounds: the cost was
scaffolding, not arithmetic) supports a **serial gap of roughly 3–5x at the
end of the programme, not 1x**. What *is* reachable, and is most of the value:
removing the per-pixel scaffolding that currently makes our arithmetic a rounding
error in our own profile, and giving HEVC the threading it has none of.

The three highest-value items, in the order they should start:

1. **C1 — AAC IMDCT through `vaco-tx`'s `Plan`** (a day; ~8x on AAC decode).
2. **B1+B2+B3 — HEVC data movement, `Plane` representation, PU-level MC**
   (~3 weeks in sequence; ~2.4x serial on HEVC, and they are the prerequisite
   for B4).
3. **A1 — H.264 partition-level motion compensation** (~2 weeks; ~1.4x on
   H.264 serial, the largest single H.264 item, and independent of the frame
   model so it survives A6).

---

## 1. Corrections to the baseline's ranking

The baseline (`§7`) is a good document. Checking it against the code and
against three extra measurements changes the picture in six places.

1. **Its candidate 3 (HEVC MC is bookkeeping-bound) is no longer a
   hypothesis.** An innermost-inline-frame breakdown of the baseline's own HEVC
   profile (§9.2) puts `predict_block_intermediate`'s 26.76% at: `Plane::index`
   9.11%, `clamp` 7.05%, the function's own body 5.02%, `copied<u16>` 2.66%,
   `try_from` 0.64% — and the tap multiply-accumulate (`fold`) at **0.28%**.
   Fewer than one sample in fifty inside HEVC's largest cost centre is
   arithmetic. The same holds for H.264: `sample_luma_block`'s 21.15% contains
   `tap6` at 1.98% and `clip_u8` at 1.17%; the rest is closures, iterator
   `next`, and bounds-checked `get`.

2. **Its candidate 4 (SAO `Snapshot::capture` "copies more than SAO reads") is
   half right.** `Snapshot::capture` (`sao.rs:251`) copies every sample of every
   plane through `plane.get(x, y)` one at a time — it is a full-plane copy
   *and* a per-sample one. Narrowing the region is not the fix (SAO's
   edge-offset classes read the deblocked neighbour on every side, so a
   pre-SAO copy of the whole plane, or of every CTU border, is genuinely
   needed); making the copy row-wise is, and the same per-sample shape accounts
   for `write_inter_cu_no_residual` (9.32%), `emit_pocs`' `blit` (5.11%),
   `offset_block` (5.35%) and `build_cu_prediction`'s `blit` (3.48%). Together
   that is **31.3% of HEVC decode spent moving samples one bounds-checked
   `u16` at a time** — item B1, and it is data, not a name-based guess.

3. **Its candidate 7 (heap profile of H.264 4K) is done, and the answer is
   not retention.** `vmmap` during a 1-thread 4K decode (§9.3) shows the live
   large allocations total ~150 MiB — one 59 MB `Vec<MbSummary>`, a few 11.9 MB
   frames and 8 MB planes — while **3.4 GB sits in `MALLOC_LARGE (empty)`**:
   freed per-picture buffers the allocator has cached, 2.3 GB of them still
   resident and 1.1 GB swapped. Peak RSS scales with frame count (1.85 GiB at
   25 frames, 3.87 GiB at 75) because every picture allocates and frees its
   `Vec<MbSummary>` (grown by `push` from empty, 59 MB at 4K), its working
   `PictureBuffer`, its DPB entry and its output `Frame` afresh. HEVC, which
   reuses less but allocates far less per picture, sits at 318 MiB on the same
   clip. This is item M1, it is cheap, and it matters for *measurement*: a
   process that swaps 1.1 GB on a 16 GiB machine shared with six agents is
   noise for everyone.

4. **Its candidate 2 (HEVC threading) should not be sized as "port H.264's
   frame threading".** The HEVC decoder is structured differently — entropy
   decoding and reconstruction are interleaved per CTU in `ctu::decode_ctu`,
   there is no intermediate per-CU syntax representation — and the fixture
   already carries what the cheaper design needs: `libx265`'s default is
   WPP (the fixture's x265 options SEI says `wpp`; a disabled flag is
   spelled `no-wpp`), which writes one entry point per CTU row;
   `decoder.rs:361` already branches on the PPS's
   `entropy_coding_sync_enabled`, `decode_wpp_row_ranges` (`decoder.rs:639`)
   already splits such a slice into per-row CABAC substreams and performs
   §9.3.2.3's context hand-off after each row's second CTU, and
   `wpp_row_ranges` **refuses** a WPP stream whose entry-point count does not
   match the row count. Row-parallel (wavefront)
   decoding is therefore a restructuring of the row loop plus a per-CTU-row
   picture representation, not a second serial/parallel split. Item B4.

5. **The FFV1 encoder is missing from the ranking and dominates the transcode
   row.** Isolated (§9.4): on the 1080p fixture at `-threads 1`, decode alone
   is 3.6–4.7 s and decode+FFV1 is 7.0–7.8 s, so the encoder is **~3.3 s
   serial** for 125 frames; ffmpeg's adds 0.05 s to its decode. At vaco's
   default threads the row is 4.27 s wall for 7.70 CPU-seconds with decode
   at ~1.06 s — **the encoder is ~74% of the wall time and runs unthreaded on
   the caller's thread**. It has never been profiled. Item D1.

6. **Candidate 1 (AAC) is right, and slightly undersold.** The 80.3% libm
   share plus `vaco_tx::reference::imp::imdct`'s own 40.88% of the in-library
   samples (7.7% of total) makes the IMDCT ~88% of AAC decode, an Amdahl
   ceiling of ~8x rather than 5x; and `kbd_window::<2048>` is recomputed
   per frame (1.76% of in-library time — the Bessel series every 1024
   samples). Item C1/C2.

What the baseline gets right and this document keeps: the phase attribution
(§3), the HEVC-has-no-threading finding (§4, direct CPU% measurement), the
scaling shape and the reason the default is four threads, the remux/probe
rows as "not where the gap is", and the do-not-re-propose list (§6), which is
reproduced and extended in §7 below.

---

## 2. Measurement protocol (binding for every item)

Every item's win/loss is decided by this protocol and nothing else. Two agents
measuring the same binary in this repository have differed by 30% in absolute
time and agreed exactly on the ffmpeg-relative ratio; treat that as the rule.

**Build.** `cargo build --profile dist -p vaco-cli --features
vaco-registry/patent-encumbered-h264-decode,vaco-registry/patent-encumbered-hevc-decode,vaco-registry/patent-encumbered-aac-decode
--target-dir <private dir under your scratchpad>`. Never the shared
`target/`. `dsymutil` the binary before profiling. Check disk first
(`df -h`); an agent has hit ENOSPC here.

**A/B.** On a Linux host with PMU access, use `scripts/perf-hwcycles.py` with a
spec naming baseline and candidate binaries as two commands of one job:
interleaved, alternating start order, **≥10 rounds**, report the per-round cycle
ratio list, median ratio, instruction ratio, win count, percentage-running, and
CPU migrations. Pin both commands to the same core class on heterogeneous
machines. Retain `scripts/perf-baseline-bench.py` wall and `user+sys` figures as
latency/context, not as substitutes for cycles. On a host without usable PMU
counters, say so and use `scripts/perf-icount.py` for deterministic work counts;
never calculate or label a time-derived estimate as cycles.

**Same-session ffmpeg ratio.** Every report of an absolute time or hardware
counter also reports `ffmpeg -threads 1` on the same fixture, interleaved in the
same run. Counts or times from another session are not comparable and must not
be quoted as a regression or an improvement.

**Fixtures.** The baseline's generated corpus (fixture table in
`PERF-BASELINE.md`), regenerated from its recipe if the scratchpad is gone.
For H.264 also `big.mkv` (1500 frames, the race detector) and
`bpyramid_1080p.mp4`. For a change that could depend on content shape, at
least one all-P and one B-pyramid fixture.

**Byte-exactness (video).** For every fixture in the set and every `N` in
`{1, 2, 4, 8}`:

```
ffmpeg -v error -i F -map 0:v:0 -f rawvideo -pix_fmt yuv420p - | shasum -a 256
vaco -threads N -i F -map 0:v:0 -c:v rawvideo -f rawvideo -   | shasum -a 256
```

All hashes equal, before and after. For any change that touches the threaded
path, additionally `big.mkv` ≥ 12 runs per thread count and the
`h264_decode_threaded` determinism fuzz target for ≥ 5 minutes. The crate
integration tests (`decoder_output_matches_ffmpeg`, `frame_threading.rs`)
stay green.

**Byte-exactness (audio and encoders)** is item-specific and stated per item —
AAC is not byte-exact against ffmpeg today and never claimed to be; an
encoder's bitstream is not a conformance target, its decoded output is.

**Profiles.** `samply record --rate 4000 --save-only` on the `dist` binary,
`llvm-symbolizer --obj=<dSYM> --inlines`, aggregate by outermost
physically-emitted frame (`scripts/perf-baseline-symbolicate.py`) **and**, for
any function you are about to optimise, by innermost frame using the script's
`--innermost` flag. `--unstable-presymbolicate` resolves
almost nothing on this toolchain; do not use it.

**Concurrency between agents.** Builds may overlap freely. **Measurements may
not**: before a timing run, `mkdir /tmp/vaco-perf-measure.lock` (atomic; fails
if it exists), run, `rmdir` it; if it exists, wait and poll — do not start a
second measurement alongside. Record the 1-minute load average in every report.
A report whose load average exceeded ~8 during the run says so and quotes
CPU-seconds as its primary number. This is a convention, not a mechanism; it
is cheap and the alternative is what the baseline's HEVC SD/720p rows look
like.

**Stop conditions are stated before work starts** and are quoted verbatim in
the item's final report, met or not. "Restructured, measured, no faster,
reverted" is a complete and valuable result (D20).

---

## 3. Items

Item IDs: **A** H.264 decode, **B** HEVC decode, **C** audio, **D** encoders
and filters, **M** memory, **R** reachability, **T** threading efficiency.
Sizes: **S** ≈ a day or two, **M** ≈ a week, **L** ≈ two to four weeks,
**XL** more. "Local" fits the current structure; "architectural" changes a
representation, an API or a crate's seam (D20).

Ceilings are Amdahl on the measured share: if a phase is `s` of runtime and
becomes `k` times faster, the whole is `1 / (1 − s + s/k)` faster. "Realistic"
is the author's estimate of `k`; "ceiling" is the best case the profile
allows (all scaffolding gone, arithmetic left).

**This section's own item prose describes the state of things when each item
was written, not now.** Several items below have landed since (A0, A1, C1,
D1's profile stage — each carries a `**Status:**` line where it has). Read
those lines before starting anything; do not infer "unstarted" from the
absence of a status line elsewhere in this document, and do not infer it
from any older summary claiming nothing here has been implemented — that
claim aged out within about a day of heavy parallel work and nobody updated
it.

### Track A — H.264 decode (`vaco-codec-h264`, `vaco-codec-dsp-mc`)

All A items touch `reconstruct.rs`, `deblock.rs`, `mb.rs` or `frame_task.rs`;
**one agent at a time in this crate**, in the order given. Note that as of
this writing another agent has uncommitted edits in `decoder.rs` and `mb.rs`;
coordinate before starting A0.

#### A0 (= M1) — reuse per-picture buffers instead of allocating them per picture

- **Evidence (data).** §9.3: 3.4 GB of freed large allocations cached by the
  allocator; live set ~150 MiB; RSS grows with frame count; `sys` time is
  0.30–0.37 s of 8.2 s (page-faulting ~100 MB of fresh pages per picture);
  `write<MbSummary>` is 2.48% of decode (`decode_slice_cabac` copying
  1,888-byte structs into a `Vec` grown from empty by `push`).
- **Change (local).** Keep one `Vec<MbSummary>` per in-flight slot and
  `clear()` it rather than dropping it; allocate it to `mbs_wide * mbs_high`
  once via `Budget` (the charge already exists at `decoder.rs:796`). Reuse the
  `PictureBuffer` and `ReadScratch` across tasks (a small free list keyed by
  geometry, like `vaco-pool`'s). Route `build_frame`'s `Frame::alloc_video`
  through a `vaco_frame::FramePool` owned by the decoder (the pool type exists
  in `vaco-frame/src/pool.rs` and is used only by that crate's own tests;
  no decoder or scheduler in the tree constructs one).
- **Ceiling.** Memory: ≥ 5x lower peak RSS at 1 thread (3.9 GiB → well under
  0.5 GiB). Time: `sys` 3.7% + growth copies; **≤ ~5% wall**. Threading: the
  `threads + 1` window of 59 MB arrays stops being fresh allocations.
- **Size / risk.** S. Local. Risk: a reused `MbSummary` slot that is not
  fully overwritten — `MbSummary` has no partial-write path today, but the
  invariance tests must run at 1/2/4/8 threads.
- **Verify.** Byte-exactness protocol; `/usr/bin/time -l` peak RSS at 1 and 4
  threads on `h264_4k.mp4` before/after; `vmmap --summary` mid-run shows
  `MALLOC_LARGE (empty)` no longer growing.
- **Stop.** If peak RSS does not fall below 1 GiB at 1 thread on the 4K
  fixture, the cache is being fed by something this item did not find —
  stop, re-run `vmmap` per region size, and report which sizes remain.

**Status: landed** (`6312d9e`, `7fbef08`, `ba10a40`) — 13-14x lower peak RSS
at 1 thread, verified; see `planning/E2E-GAPS.md` §26. This document's own
prose elsewhere still describes A0 as unstarted; that prose is stale, not
this line — see the note at the top of §3.

#### A1 — partition-level motion compensation (luma and chroma)

- **Evidence (data).** `sample_luma_block` 21.15% + `sample_chroma_2x2` 9.04% +
  `reconstruct_inter_mb::{closure#0}` 7.90% = **38.1%** of serial 4K decode
  (baseline §2.1). Innermost breakdown (§9.1): inside `sample_luma_block`,
  `tap6` 1.98%, `clip_u8` 1.17%; `next<[u8;4]>` 3.00%, `get<u8>` 2.11%,
  closures 3.5%, own body 7.79%. The MC arithmetic is ~4% of decode; the
  remaining ~34% is per-pixel scaffolding. Structurally
  (`reconstruct.rs:756–830`, `interp.rs:78–160`): every 4x4 block is predicted
  independently through `luma_qpel_sample`, which evaluates up to six 6-tap
  sums *per output pixel* through a `Fn(i32,i32)->u8` fetch closure; a 16x16
  partition is sixteen 4x4 predictions each re-fetching a 9x9 window (1,296
  fetches for 256 outputs against 441 needed); the bi-prediction combine
  matches `Option`s per pixel.
- **Evidence (hypothesis).** That a block-level separable implementation over
  strided slices autovectorises well enough to reach 4–6x on this share. Support:
  `vaco-scale`'s `filter_h` (1.25x end to end from a fixed trip count alone),
  and `vaco-codec-dsp-mc`'s measured `fir_row` (1.12x vs autovectorised scalar
  on an 8-tap row, i.e. the substrate is not the bottleneck).
- **Change (local to the codec, new API in `vaco-codec-dsp-mc`).** Predict per
  *partition* (16x16, 16x8, 8x16, 8x8 and the sub-8x8 shapes) not per 4x4:
  one block request (`PlaneView::block` on the banded arm, a strided sub-slice
  on the flat arm) sized to the partition plus the filter reach; horizontal
  6-tap pass over `h + 5` rows into an `i16` scratch, vertical pass, then the
  position-specific average with the clipped integer/half sample — the
  clause 8.4.2.2.1 semantics `interp.rs` already documents, including the
  unclipped intermediate for `j` and the clipped half-pels for the quarter
  positions. Chroma: 8x8/8x4/4x8/4x4 bilinear blocks over rows. Weighted and
  bi-prediction combine over whole rows of `u8`/`i16`, not per-pixel `Option`.
  Edge emulation once per partition (`vaco-codec-dsp-mc::edge::extend_edges`)
  when the reach leaves the picture, else direct strided reads. Keep the
  current per-pixel path as the scalar oracle in tests (every fractional
  position × every partition shape × in/out-of-picture, compared bit for bit),
  the same way `deblock_picture_luma` stayed as the row schedule's oracle.
- **Ceiling.** Share 38.1%: k=4 → 1.40x; k=6 → 1.46x; all scaffolding gone
  (k≈8) → 1.49x serial. On the banded (threaded) arm the win should be
  larger, because one block request per partition replaces sixteen.
- **Size / risk.** L (1–2 weeks). Local to the codec plus a small DSP API.
  Risks: the chroma-specific negative results (§7 items 5–6) — chroma
  clamps are cheap and merging Cb/Cr into one loop body measured slower; write
  chroma as two independent single-plane block kernels and measure luma and
  chroma as *separate commits*. Register pressure on the bi-pred combine.
- **Verify.** Protocol §2, all fixtures at 1/2/4/8 threads; `vaco-checkasm`
  differential for the new kernels against the retained per-pixel oracle;
  innermost profile after, to confirm `tap`-class arithmetic is now the
  majority of the function.
- **Stop.** Luma partition kernel measured against the current path on the
  4K fixture at `-threads 1`: if the median ratio is not ≤ 0.85 (≥ 1.18x
  end to end) with ≥ 8/10 rounds, the design is not reaching the optimiser
  and the item stops for a disassembly check before any chroma work begins.

**Status: landed, partial win, stop condition not met** (`4d75fe4`,
`ecf93f5`, `3e3a271`) — measured ~6-8%, below the stated 1.18x/0.85-ratio
bar; kept per the honest-partial-result convention (D20), documented in
full in `planning/E2E-GAPS.md` §28. Chroma work was not started.

#### A2 — deblocking: per-macroblock edge record, slice-based gather/scatter

- **Evidence (data).** `deblock_row` 10.73% (`luma_mb_row` 5.72% + its
  `get`/`set` closure 3.84%) + `boundary_strength` 10.41% + `chroma_mb_row`
  3.16% + `filter_luma_edge` 2.77% + `filter_chroma_edge` 0.63% = **27.7%**.
  Innermost (§9.1): `boundary_strength`'s 10.41% is `eq<i32>` 1.54%,
  `as_ref<CabacResidual>` 1.41% (`has_luma_coeffs` inspecting
  `Option<CabacResidual>` per call), `copied<MvInfo>` 1.15% (copying a ~40-byte
  `MvInfo` twice per call), `abs_diff` 1.02%, `is_intra` 0.75%, own body
  1.69%. The filter kernels themselves (3.4%) are already the vectorised
  masked-select kernel and are **not** the target (§7 item 7).
- **Evidence (hypothesis).** That a precomputed per-macroblock record — a
  16-bit "has coefficients" mask per 4x4, intra flag, transform-8x8 flag, and
  the per-4x4 `(ref_poc_l0, ref_poc_l1, mv_l0, mv_l1)` already resolved to
  POCs — turns `boundary_strength` into a few integer compares with no
  `Option` walking; and that gathering a 16-line edge with `chunks_exact` row
  slices (or a 4-row transpose) instead of 64 closure calls per edge removes
  most of the 9.6% gather/scatter.
- **Change (local).** Build the record once per macroblock as part of A3's
  residual path (or from `MbSummary` until A6 lands); rewrite the four gather
  loops over row slices. Chroma `bS` reuses luma's per 4x4 (it already does).
- **Ceiling.** 27.7% → realistic ~11% (k≈2.5): **1.20x**; ceiling ~8%
  (k≈3.5): 1.25x.
- **Size / risk.** M. Local. Risk: §7 item 2 — "batching deblocking's
  per-pixel reads and writes into contiguous slice operations" measured a
  wash-to-loss in round 1. That attempt batched *inside the existing
  per-pixel-`get` structure*; this one removes the per-pixel `get`. Measure
  the `bS` half and the gather half as separate commits so the round-1 result
  can be reproduced or refuted on its own.
- **Verify.** Protocol §2; the `deblock.rs` watermark tests (both sides) and
  the row-schedule-equals-whole-picture test stay green; the `no-deblock=1`
  and `deblock=-3,2` fixtures from E2E-GAPS §21 are re-encoded and checked.
- **Stop.** If the `bS` record alone does not reach ≤ 0.95 median (≥ 1.05x),
  stop and profile innermost before touching the gather loops.

#### A3 — residual and intra path: dense coefficient blocks, in-place add

- **Evidence (data).** `reconstruct_mb` 12.74% + `idct4x4` 2.53% = 15.3%.
  Innermost: `predict_chroma_inter` 1.85%, `clamp` 1.56%, `copy_nonoverlapping`
  1.01%, `saturating_add` 1.00%, `overflowing_mul` 0.63%, the rest spread. The
  residual is stored sparse (`CabacResidual { positions, levels }` as two heap
  `Vec`s per coded block, `Option` per block), scanned back into a dense
  block per reconstruction (`inverse_scan_luma_dc`, `build_luma_ac_block`),
  dequantised, IDCT'd, then added through nested `get`/`clamp` loops
  (`reconstruct.rs:120–135`, `581–589`).
- **Evidence (hypothesis).** That decoding coefficients straight into a
  dense `[i16; 16]`/`[i16; 64]` per block (zero-filled, no heap), with the
  "block is all-zero" case a single flag, lets dequant+IDCT+add run as
  straight-line array code the optimiser vectorises — `vaco-codec-dsp-idct`'s
  `idct4x4` already takes a dense input.
- **Change (local, prepares A6).** `MbResidual` becomes dense inline
  storage (worst case 384 luma + 128 chroma `i16` = 1 KiB, with a `coded`
  bitmask so uncoded blocks cost a flag); the DC/AC split for `Intra_16x16`
  and chroma DC keeps its own small arrays; `residual_block_cabac` writes
  positions directly. `add_pixels_clamped`-shaped loops are written as plain
  scalar loops over rows (the hand-vectorised version measured 0.84–0.9x,
  §7 item 1).
- **Ceiling.** 15.3% → ~8% (k≈2): **1.08x**; ceiling ~5%: 1.11x. Plus the
  A0-adjacent effect of removing every per-block heap `Vec` (allocator
  samples ~1% today).
- **Size / risk.** M. Local. Risk: `MbSummary` grows if the dense residual is
  larger than the current sparse average — measure `size_of` and the 4K
  `Vec<MbSummary>` footprint before and after; A0's reuse makes the absolute
  size less important than its churn.
- **Verify.** Protocol §2; the crate's residual unit tests; an I-only fixture
  (`-g 1` encode) added to the byte-exactness set so intra paths are exercised
  at full weight.
- **Stop.** Median ratio > 0.97 (< 1.03x) on the 4K fixture after both the
  dense representation and the in-place add are in: revert the add, keep the
  representation only if A6 needs it, and record the result.

#### A4 — output path: stop copying every picture three times

- **Evidence (data).** `build_frame` 3.03% (blit from `ReconstructedPicture`
  into a freshly allocated `Frame`), `send_packet` 2.19%, and
  `copy_nonoverlapping<u8>` 1.08% innermost (the `RowPublisher` band copy
  into the DPB entry, `frame_task.rs:241`). Every 4K picture is written to
  the working `PictureBuffer`, copied into the `ProgressPicture` DPB entry,
  and copied again into the output `Frame` — ~25 MB of extra traffic per
  picture.
- **Evidence (hypothesis).** That decoding directly into the DPB entry's
  bands (the `PictureWriter` already owns them exclusively while filling) and
  building the output `Frame` from the same storage removes both copies.
  `docs/model/vaco-frame.md` anticipated this: "if we later adopt banded
  planes ... `PlaneRef` grows a banded representation".
- **Change (architectural: `vaco-frame` grows a banded plane; the DPB entry
  becomes the frame).** A `Frame` plane that is a `Vec<Buffer>` of bands
  with a stride, readable through the existing `PlaneRef::row(y)` API (a row
  lookup becomes a band index — a shift when band height is a power of two,
  as `ProgressPlane::band_of` already does). Consumers that need one
  contiguous plane (`vaco-scale`, encoders) get a `contiguous()` that copies
  only when the plane is actually banded, i.e. never at `-threads 1`.
- **Ceiling.** ~6% share: **1.04–1.06x** serial; a larger share of the
  *threaded* CPU-seconds overhead (T1).
- **Size / risk.** L. Architectural across `vaco-frame`, `vaco-codec-core`,
  `vaco-codec-h264` and every consumer of `PlaneRef`. Risk: touching a model
  crate that every filter compiles against; a copy-on-first-contiguous-read
  that silently reintroduces the copy for every consumer.
- **Verify.** Protocol §2; `vaco-frame`'s own tests; a filter-graph e2e
  (`-vf scale`) byte-exact before/after.
- **Sequencing.** After A1–A3 and after T1's measurement says the band copy
  is a material part of the threaded overhead; otherwise defer indefinitely —
  6% serial is not worth a model change on its own.

#### A5 — CABAC engine and the serial half

- **Evidence (data).** `residual_block_cabac` 4.36% + `decode_slice_cabac`
  3.84% = 8.2%; innermost `decode_decision` 1.80% + `renorm` 0.59%. The
  engine (`vaco-codec-cabac`) has already been measured through four variants
  and the spec-literal branchy shape won by 1.76x (`docs/signal/vaco-codec-cabac.md`);
  `write<MbSummary>` (A0) is 2.48% of the 3.84%.
- **Assessment.** No item. After A0 the engine is ~5% of serial time and its
  shape is already the measured winner. It matters later as the Amdahl limit
  of the serial half (A6), not as a kernel.

#### A6 — frame model: the picture task does entropy decoding too (architectural)

- **Evidence (data).** The decoder's seam puts all entropy decoding on the
  caller's thread (`docs/codec/frame-threading.md`). Today that is 8.2% of
  serial time, so the ideal 4-thread bound is 1/(0.082 + 0.918/4) = 3.2x
  against 2.61x measured (baseline §4). **After A1–A3 the same serial work is
  ~19% of a ~2.3x smaller total**, and the bound falls to 2.7x at four
  threads and 3.6x at eight. The split also forces every picture's full
  syntax (`MbSummary`, 59 MB at 4K, `threads + 1` of them) to exist as a
  data structure — the root of A0's churn and of the 1,888-byte struct copy.
- **Evidence (hypothesis).** That the ffmpeg-class model — each task decodes
  *its whole picture*, entropy and reconstruction interleaved per macroblock,
  keeping only a compact per-macroblock record (motion field, `bS` inputs,
  QP; ~100 bytes) — is expressible with the existing determinism machinery:
  the serial half still parses headers, builds reference lists and does
  marking in decode order; a task's entropy decode of a B picture waits on
  the *colocated picture's motion field* (published once per picture through
  a `OnceLock`, exactly as bands are) instead of on its samples; results are
  still collected in dispatch order. `RefPicture::motion` (`decoder.rs:166`)
  is already that published field.
- **Ceiling.** Serial: removes `write<MbSummary>`/alloc/drop (~3.5% → 1.04x)
  and A0's memory root cause outright. Threaded: raises the Amdahl bound at
  four threads from ~2.7x (post-A3) to ~3.8x, and at eight from ~3.6x to
  ~6x, on B-content where picture-level parallelism exists; on all-P content
  the row-progress mechanism is unchanged.
- **Size / risk.** XL (3–4 weeks). Architectural: it moves the seam the
  byte-exact, race-tested threading design is built around. Determinism
  argument stays (dispatch-order collection, publish-once bands); a *new*
  dependency (motion field of the colocated picture) needs its own
  refusal-not-wrong-pixels guard and its own fuzz coverage in
  `h264_decode_threaded`. Byte-exactness is not at risk in principle — no
  arithmetic changes — but every stage must be committed byte-exact on its
  own.
- **Verify.** Protocol §2 with the extended `big.mkv`/fuzz runs; CPU% and
  scaling table at 1/2/4/8/16 on the 4K all-P and the B-pyramid fixtures
  before and after; peak RSS.
- **Stop / gate.** Do not start until A1–A3 have landed and the 4K scaling
  has been re-measured. **Start only if** the measured 4-thread speedup is
  below 2.4x or the 8-thread speedup is below 3.2x on the B-pyramid fixture
  (i.e. the serial half is now the limiter). If scaling is still above those
  numbers, the model is not the bottleneck and this item is deferred with
  that measurement recorded.

### Track B — HEVC decode (`vaco-codec-hevc`)

One agent at a time in this crate, in the order B1 → B2 → B3 → B4. B1 and
B2 could be one agent's first fortnight.

**B4 is parked as of 2026-09-02 — coordinator decision, not a technical
blocker.** A sweep of what the README advertises, measured against the ffmpeg
binary, found FFV1 decoding to wrong pixels on 99.6% of bytes (a lossless
codec) and encoding files ffmpeg misreads; H.264 encode writing Annex-B into
MP4/Matroska, so every encoded file is malformed; MP3 and AAC offset by an
untrimmed encoder delay; AC-3 wrong on 99.5% of samples; nine of thirteen
image formats reporting `0x0, pix_fmt=unknown`; PCM in MP4 undecodable; and
pipe input broken. Three to four weeks making an *already byte-exact* HEVC
decoder faster is the wrong trade against that — "performance only matters
when the output is useful".

Picking it back up is cheap: the design correction in
`docs/codec/hevc-wavefront-threading.md` records that `CuGrid` and
`SaoParamsGrid` publish only at whole-row granularity, which caps adjacent-row
overlap at near zero under real dispatch, and that `RowPublish` needs a
blocking `wait` modelled on `PictureRef::wait_tile`. Both were unknown before
and are the substance of what B4 costs. `Pool`/`FrameRunner` was ruled out
deliberately — its `'static` bound is session-lifetime and does not compose
with `std::thread::scope`'s lexical join — so dispatch belongs local to the
crate.

#### B1 — data movement: row-wise copies everywhere a sample is moved one at a time

- **Evidence (data).** `write_inter_cu_no_residual` 9.32% (per-sample
  `plane.set` with `clamp`/`try_from`), `Snapshot::capture` 8.08% (per-sample
  `get` into a `Vec<u16>` copy of each plane), `offset_block` 5.35%
  (per-sample `snapshot.get`/`plane.set_i32` with `try_from` 1.57%),
  `emit_pocs`/`blit` 5.11% (per-sample `u16 → u8`), `build_cu_prediction`'s
  `blit` 3.48% (PU `Vec<i32>` → CU `Vec<i32>`, then written again): **31.3%**,
  every one an innermost-frame-confirmed per-sample loop (§9.2). The same
  shape won 3.5% on H.264 (E2E-GAPS §10, row `copy_from_slice`) when it was a
  much smaller share.
- **Change (local).** `Plane` grows `row(y) -> &[u16]` / `row_mut(y)`;
  `Snapshot::capture` becomes a row copy (or, better, a `Plane::clone` of
  the data vector — one `memcpy`); `write_pred_block` writes rows with a
  `clamp` over a slice; `offset_block` (band offset and each edge class)
  runs per row over three row slices; `pic_to_frame` converts rows;
  `build_cu_prediction` predicts straight into the CU buffer (no per-PU
  `Vec<i32>` + blit). All of it is the "move whole rows" shape, no SIMD.
- **Ceiling.** 31.3% → ~5% (memcpy-class): **1.36x**; realistic ~7%: 1.32x.
- **Size / risk.** S–M (2–4 days). Local. Risk: none of the recorded negative
  results are of this shape; the recorded *positive* one is.
- **Verify.** Protocol §2 on `hevc_{sd,720p,1080p,4k}.mp4` (HEVC has no
  threading, so `N=1` only until B4); the crate's SAO/deblock tests.
- **Stop.** If the five changes together do not reach ≤ 0.85 median (≥ 1.18x)
  on `hevc_4k.mp4`, the profile attribution is wrong somewhere — stop and
  re-profile innermost before B2.

#### B2 — `Plane` representation: `u8` storage for 8-bit, no per-sample `ready` bitmap

- **Evidence (data).** `framebuf.rs:37`: `Plane { data: Vec<u16>, ready:
  Vec<bool> }` — three bytes per sample for a crate whose `check_scope`
  refuses anything but 8-bit; `Plane::index`'s bounds arithmetic is 9.11%
  inside `predict_block_intermediate`, 1.78% inside `predict_block`, 1.04%
  inside `emit_pocs`, 0.88% inside `write_inter_cu_no_residual` — **~13% of
  decode is the accessor**. Reference pictures in the DPB are read through
  the same accessor by MC, at twice the memory traffic of `u8`.
- **Change (architectural within the crate).** `Plane` stores `u8` with a
  stride (padding to the CTU grid removes the picture-edge branch from the
  interior); availability becomes a per-minimum-TB (4x4) grid, as H.264's
  `decoded_4x4` already is (`intra_pred.rs` is the only `is_ready` caller,
  two sites); `get`/`set` survive as thin wrappers over `row`. The DPB holds
  the same type; `pic_to_frame` becomes a row `copy_from_slice`. Keep a
  `u16` path out of scope — the crate's scope is 8-bit and a generic `Plane<T>`
  is the next agent's problem when 10-bit lands.
- **Ceiling.** Direct: the accessor share (~13%) mostly folds into B3's
  numbers; on its own, realistic **1.10x** (accessor cost halved, traffic
  halved). It is primarily the prerequisite that makes B3 and B4 writeable.
- **Size / risk.** M. Risk: intra availability semantics — the `ready`
  bitmap "substitutes for z-scan availability" and the module doc argues it is
  exact for the single-slice, no-tiles scope; the 4x4-grid replacement must
  be argued the same way (it is: every write is at least a 4x4 TB and
  availability is only ever queried at TB granularity) and pinned by the
  intra fixtures (an I-only `libx265` encode with `--tu-intra-depth 4`).
- **Verify.** Protocol §2, HEVC fixtures plus an I-only fixture.
- **Stop.** This item can legitimately measure ~1.0x on its own; its stop
  condition is correctness only. Revert if any fixture's hash changes.

#### B3 — PU-level separable motion compensation over row slices

- **Evidence (data).** `predict_block_intermediate` 26.76% + `predict_block`
  6.22% + `build_cu_prediction` 10.43% + `predict_component` closures 2.15% =
  **45.6%**, with the arithmetic at well under 1% (§9.2). `mc.rs:242–322`:
  every tap of every output sample goes through `clamped_sample` (dims,
  two `clamp`s, two `try_from`s, `index`); the two-pass case allocates a
  `vec![0i32; ...]` per PU per plane; every PU returns a `Vec<i32>`.
- **Evidence (hypothesis).** Same as A1: separable 8-tap/4-tap passes over
  strided `u8` rows into an `i16` intermediate, edge emulation once per PU,
  weighted/bi-pred combine over rows, no per-PU heap. HEVC's PU sizes (4x8
  to 64x64) are a better fit for row kernels than H.264's 4x4 was — E2E-GAPS
  §11 ruled out `vaco-codec-dsp-mc::fir_row` for H.264 *because* 4x4 was too
  narrow; here it is not.
- **Change (local after B2).** `predict_block`/`predict_block_intermediate`
  rewritten on `&[u8]` rows with the filter tables as `TapSet<8>`/`TapSet<4>`
  (the `vaco-codec-dsp-mc` API, adding the HEVC tables with the DC and
  impulse checks that crate's doc requires); intermediate precision and the
  `shift1/shift2/shift3` conventions unchanged. Keep the current per-sample
  path as the oracle in tests across every fractional position, PU size and
  edge condition.
- **Ceiling.** 45.6% → ~10% (k≈4.5): **1.55x**; ceiling ~7%: 1.63x.
- **Size / risk.** L (1–2 weeks). Same chroma cautions as A1; same
  separate-commit rule for luma, chroma, uni, bi and weighted.
- **Verify.** Protocol §2; `vaco-checkasm` for the new tap sets.
- **Stop.** Luma uni-prediction alone must reach ≤ 0.80 median on
  `hevc_4k.mp4` (the share is large enough that 1.25x is the *minimum* a
  working rewrite shows); if not, stop for disassembly before chroma.

#### B4 — wavefront (WPP) row-parallel decoding (architectural, HEVC's first threading)

- **Evidence (data).** Baseline §4: CPU% flat at 93–99% from 1 to 16 threads;
  no HEVC threading exists. `decode_wpp_row_ranges` already decodes each CTU
  row from its own substream with the §9.3.2.3 context hand-off; the fixture
  set (stock `libx265`, `wpp=1`, 34 CTU rows at 4K, 17 at 1080p) is the
  content shape this parallelism was designed for. ffmpeg's default-thread
  win on this fixture is 3.4x (§1).
- **Design (hypothesis, sketched — the executing agent writes the design
  doc first, as `docs/codec/frame-threading.md` was written for H.264).**
  Each CTU row is decoded by one worker owning that row's band of the picture
  exclusively (a `Vec<u8>` per plane of CTU height, plus its share of the
  motion/mode grids). Everything a row reads from the row above is published
  per CTU as an owned, immutable record — the bottom sample row and the
  above-right reach for intra prediction, the per-4x4 motion and mode entries
  for merge/AMVP and `bS`, the CABAC context snapshot after CTU 1 — through a
  `Vec<OnceLock<CtuBorder>>` per row, so a reader can only observe a finished
  CTU (the same "publish by move" argument `ProgressPicture` makes). A row
  starts once the row above has published CTU 1 and waits per CTU on the
  above-right CTU (the standard two-CTU lag). Deblocking and SAO run per CTU
  row, one row behind and lagged the way H.264's Stage 1 lags the filter:
  the row above hands its bottom rows (three luma rows for the horizontal
  edge filter, one more for SAO's neighbour reads) down by *moving* them, so
  ownership stays exclusive and no sample is read before it is final. The
  whole-picture `deblock::filter_picture`/`sao::filter_picture` stay as the
  order-independence oracle — the row schedule must equal them byte for byte,
  as H.264's did.
- **Ceiling.** At 4 threads on 4K: WPP's ramp costs ~2 CTUs per row per
  thread against 60 CTUs per row; expect **~3x at 4 threads, ~4.5x at 8**
  on the 4K fixture, less at SD (10 rows). Combined with B1–B3's ~2.4x
  serial: HEVC 4K default from 26.5x to **~4–5x** behind ffmpeg default.
- **Size / risk.** XL (3–4 weeks). Architectural: the picture, `CuGrid`,
  `EdgeMarks` and SAO parameter storage all become per-row; `Ctx` splits into
  per-row state and shared read-only slice state. Risks: a lag bound that is
  "safe" rather than exact costs speed; one that is wrong produces
  content-dependent corruption — so, as in H.264, every bound is pinned by a
  two-sided test, and a read past what was waited for is **refused**, never
  served. Memory: one band per in-flight row, not a picture per thread.
- **Verify.** Protocol §2 at 1/2/4/8 threads on every HEVC fixture; a
  `hevc_decode_threaded` determinism fuzz target (decode at 1 and at N,
  assert identical) **before** any default is enabled; a 1000+-frame 1080p
  `libx265` fixture (the HEVC `big.mkv`) run ≥ 12 times per thread count.
  Default stays `-threads 1` for HEVC until those three exist.
- **Stop.** After the per-row picture representation lands (stage 1, still
  serial), the serial ratio must be ≤ 1.03 (no more than 3% slower) — the
  same "the restructure is free" gate H.264's Stage 1 passed at 1.0013. If
  it is not, stop and find the cost before adding threads.

#### B5 — HEVC frame threading through `vaco-codec-core::FrameRunner`

- **Evidence.** The mechanism exists and is codec-agnostic; `libx265`'s
  default GOP (`bframes=4`, B-pyramid) has ~2x picture-level parallelism.
- **Assessment.** Deferred. B4 gives intra-picture parallelism on every
  content shape and fits the decoder's interleaved structure; frame threading
  would need either an intermediate syntax representation (the model A6 is
  moving H.264 *away* from) or whole-picture tasks doing their own entropy
  decode — which is A6's model and should be designed once, after A6 proves
  it. Revisit after A6 and B4 are both measured.

### Track C — audio (`vaco-codec-aac`, later `vaco-codec-mpegaudio`)

#### C1 — AAC IMDCT through `vaco-tx::Plan` (`TxKind::Mdct`, inverse, `FULL_IMDCT`)

- **Evidence (data).** Baseline §2.3: 80.3% of AAC decode samples are leaves
  in `libsystem_m.dylib`, every caller `vaco_tx::reference::imp::imdct` — an
  O(n²) direct evaluation with a `cos` per `(j, k)` pair, called from
  `reconstruct.rs:447` and `:460` in the production path; plus the function's
  own 40.88% of in-library samples (7.7% of total). `vaco-tx`'s
  `tests/oracle.rs:176–192` already asserts that `Plan::<f64>::new(Mdct,
  Inverse, n, 1.0, FULL_IMDCT)` matches `reference::imdct` to `rms_rel <
  1e-12` for `n` up to 960, and `golden_i32.rs:106` covers 2048.
- **Change (local).** Hold two `Tx<f64>` (lengths 2048 and 256, scale `1.0`,
  `FULL_IMDCT`) in the decoder state; feed the `n/2` coefficients, take `n`
  samples, keep the existing `2/N` and window multiply. Use `f64` so the
  result is numerically the reference's to ~1e-12 relative — this keeps the
  change verifiable against the *current* output rather than against a
  tolerance. (An `f32` plan is a later, separately measured step.)
- **Ceiling.** IMDCT-attributable share ≈ 88% → **~8x** on AAC decode
  (217x → ~27x). Remaining time: VLC decode, `ics_stream::read`, per-frame
  `Vec` churn, the per-frame `kbd_window` recomputation.
- **Size / risk.** S (a day). Local. Risk: the short-window path's
  `first_left` window selection and the overlap-add must be untouched; only
  the transform call moves.
- **Verify.** AAC is not byte-exact against ffmpeg (baseline §1, crate doc).
  Criteria: (1) decoded `f32` output vs the pre-change binary's output on
  `audio_aac.m4a` and the crate's existing fixtures: **max |Δ| ≤ 4 × f32 ulp
  at the sample's magnitude, and identical for ≥ 99.9% of samples**; (2) the
  `correlation/max_abs/rms` table in `docs/codec/vaco-codec-aac.md` against
  `ffmpeg -bitexact` unchanged to the printed precision; (3) `cargo test -p
  vaco-codec-aac`. Timing per §2 (the `audio_decode_aac` job).
- **Stop.** If (1) fails, the plan's convention differs from the reference's
  in a way the oracle test does not cover at 2048 — stop, add the 2048/256
  cases to `oracle.rs`, and report the discrepancy rather than widening the
  tolerance.

**Status: landed** — AAC decode moved 217x behind ffmpeg to 2-5x; see
`planning/E2E-GAPS.md` §23 for the full before/after.

#### C2 — AAC per-frame allocation and window caching

- **Evidence (data).** `kbd_window::<2048>` 1.76% and `::<256>` 0.31% of
  in-library time, recomputed per frame (`reconstruct.rs:436–445`, `build_window`);
  `finalize_channel` allocates `Vec<Vec<f32>>`, `Vec<f64>` coefficient copies,
  and `vec![0.0f32; LONG_LEN]` per window per channel per frame. Small today
  (~0.5% of total) but ~5% once C1 removes the IMDCT.
- **Change (local).** Window tables computed once per decoder (they depend
  only on shape); coefficient and output scratch reused; `Tx<f32>` plan if
  (1) of C1's criterion still holds at f32 — measured, not assumed.
- **Ceiling.** ~1.3–1.5x on post-C1 AAC. Diminishing; do after C1 only if
  the post-C1 profile confirms the shares.
- **Size.** S. **Stop.** Post-C1 profile shows these under 5% combined.
- **Measured f32 candidate (2026-09-04).** The Linux Cachegrind C1 baseline
  measured 212,444,621 Ir; `kbd_window::<2048>` + `::<256>` accounted for
  13.3% and allocation/free for 12.9%, so the candidate gate passed. A
  `Tx<f32>` reconstruction candidate passed all 76 focused AAC tests but was
  rejected before timing because native `f32le` PCM differed from the f64
  parent on sine, noise, and stereo fixtures (first byte differed in each;
  outputs had equal lengths). No candidate Ir or speedup is claimed. Do not
  re-propose the f32 plan without a new, byte-exact design.

#### C3 — MP3 (6.5x, 0.24 s for 30 s)

Not profiled; 0.24 s absolute. Listed for completeness; no item until AAC and
the video tracks are done. A profile is the first step if it is ever picked up.

### Track D — encoders and filters

#### D1 — FFV1 encoder: profile, then the coder and the per-sample path

- **Evidence (data).** §9.4: ~3.3 s serial for 125 1080p frames against
  ffmpeg's ~0.05 s on the same input; 74% of the default-thread transcode
  row; runs entirely on the caller's thread (`codec.rs:690 encode_frame`:
  one whole-frame slice, `RangeEncoder`, `load_planes` copy). Never profiled.
- **Evidence (hypothesis).** Two candidates the code makes visible: (a) this
  encoder always uses the range coder (`params.rs:100`, "version 3, range
  coder"), whereas the reference's default for 8-bit content is Golomb-Rice
  (`-coder` default `rice`), whose run mode makes flat regions — most of a
  `testsrc2` frame — nearly free; the 60x gap is content-dependent and a
  natural-content fixture will show a smaller one. (b) The range coder's
  per-bit `put_rac` with state lookups, and `load_planes`' per-plane copy,
  are per-sample scaffolding of the shape this whole document is about.
- **Change.** Profile first (innermost). Then, in separate measured commits:
  the per-sample path over rows; an encoder-side Rice coder (the crate already
  decodes Rice) selectable by `-coder`; whole-frame → multiple slices, which
  are independent by construction in FFV1 and are how the reference threads
  this encoder (`slices`/`threads` in `ffmpeg -h encoder=ffv1`).
- **Ceiling.** Transcode row at default threads: 4.27 s → decode-bound at
  ~1.1 s if the encoder drops below the decoder: **~3.5x on the row**.
  Encoder alone: unknown until profiled.
- **Size / risk.** M for the profile and row path; M for Rice; M for slices
  with threading. **Behavioural note:** changing the *default* coder or slice
  layout changes the bitstream `ffprobe` reports (coder type, slice count) —
  the decoded output is what must not change. Do it behind the option first;
  moving the default is a separate decision recorded against the reference's
  own defaults per D17.
- **Verify.** Lossless round trip: `ffmpeg -i out.mkv -f rawvideo` must equal
  the input frames byte for byte, on every fixture and every coder/slice
  setting; vaco's own decoder round-trips too; timing per §2.
- **Stop.** If the profile puts more than half the time in the range coder's
  own arithmetic (as opposed to per-sample scaffolding around it), the
  per-sample item stops and only the coder/slice items proceed.

**Status: profile stage landed, one fix landed, work continuing** (profile
+ `.ok_or_else` fix: `3bf2732`; D21/D22 inlining/cold-path/branch-hint
follow-up: `a2e6706`) — see `planning/E2E-GAPS.md` §25 and §27. As of this
writing another agent has further uncommitted work in this crate; the
per-sample/Rice-coder/slice items below this one are not yet started.

#### D2 — `vaco-scale`: default threading and the fused-kernel gap

- **Evidence (data).** Baseline §3: the scaler is 1.33 s of 9.74 s at
  `-threads 1` (13.7%) and ~0.43 s of residual at default threads (it runs on
  the caller's thread while decoder workers run). `docs/signal/vaco-scale.md`
  §8: slice threading exists and measured 3.02x at 8 threads, but
  `threads = 0` means serial by deliberate choice; the same doc records the
  generic path as 5.9–9.1x slower than the reference because it materialises
  `i32` planes and makes up to four passes, and names fused kernels in
  `fast.rs` as the fix.
- **Change.** Have the CLI pass `-filter_threads` (reference default: auto)
  into the scaler's `threads` rather than leaving the library default; that
  is a CLI-layer change, not a library one, so the doc's reason for `0 =
  serial` stands. The fused kernels are that crate's own documented next
  step; not scheduled here — the scale share tracks the decoder's, and the
  decoder is the larger item.
- **Ceiling.** Decode+scale row at default threads: the ~0.43 s residual
  mostly disappears (≤ 1.1x on the row). **Size.** S. **Stop.** None
  needed; it is an option-plumbing change verified by the existing
  `thread_count_never_changes_the_output` property.

### Track M — memory

M1 is A0. Nothing else is sized: HEVC's 318 MiB is unremarkable, and A6
removes the H.264 structure that A0 papers over.

### Track R — reachability (not performance; included, with reasons)

The baseline asks whether FLAC-misdetected-as-CDG and Opus-never-registered
belong here. **They do, as one systemic item and two test cases, not as two
point fixes** — because the programme's own coverage claim depends on it
(two of the eleven baseline rows are "no data" for reachability reasons), and
because this is at least the eighth instance of the class: H.264 decode
unreachable from the binary (E2E-GAPS §1), the four in E2E-GAPS "The
pattern", `#655`'s eleven image formats registered but mapped to no
`CodecId` (TECH-DEBT), TGA with no `pipe` row, `VobSubDemuxer::open_pair`
unreachable through the registry (INTERFACE-GAPS), and now FLAC and Opus.
Every one passed every internal test.

#### R1 — an `xtask` gate: a descriptor without a fragment is an error

- **Evidence.** `vaco-codec-opus` exports `DECODER_OPUS: DecoderDesc`
  (`lib.rs:63`) and has no `vaco-component.toml`; `gen-registry` discovers
  fragments by walking crate directories, so a crate with none is silently
  absent. `vaco-format-audio-simple/src/flac.rs` exports only a `MuxerDesc`;
  no crate exports a FLAC `DemuxerDesc` at all.
- **Change (local, `xtask/src/registry.rs` or a sibling).** Scan every crate
  under `crates/codec`, `crates/format`, `crates/filter`, `crates/io` for
  `pub const|static NAME: (Decoder|Encoder|Demuxer|Muxer|Parser|Filter|Protocol)Desc`
  and require each to be named by some fragment's `ctor`, with an explicit
  allow-list (with reasons) for descriptors that are deliberately internal.
  Run it in `gen-registry --check` so CI catches it.
- **Size.** S. **Verify.** The gate fails on today's tree (Opus) and passes
  once R3 lands.

#### R2 — an end-to-end reachability suite driven by the reference

- **Evidence.** Every incident above was invisible to unit tests and visible
  to the binary. `vaco-conformance` already runs the reference at test time
  and keeps no golden files (its clean-room argument).
- **Change (local, `vaco-conformance`).** For every registered demuxer whose
  format the reference can mux, and every registered decoder whose codec the
  reference can encode: generate a one-second fixture with `ffmpeg -f lavfi`,
  assert `vaco-probe` detects the container as the reference does, and
  assert `vaco -i fixture -f null -` exits 0 with the expected frame count.
  Skip-with-reason for what the reference cannot produce. This is a coverage
  table, not a byte-exactness claim.
- **Size.** M. **Stop.** None — it is a test.

#### R3 — the two cases in hand: FLAC and Opus

- **FLAC.** Two defects: no FLAC demuxer is registered (the prober falls
  through to `cdg`, whose `probe` is content-scored); and once FLAC reaches
  the decoder inside Matroska, the pipeline dies with `progress limit
  exceeded: requested 65, cap 65` — that string is `vaco-limits`'
  `LimitError::NoProgress { ticks: 65 }` from `ProgressGuard`, the
  scheduler's stall watchdog, so the FLAC decode node is making no progress
  for 65 steps (a decoder/scheduler hand-off that never emits, **hypothesis**:
  the `claxon`-backed decoder's need-more-data path), not a cap that is set
  too low. Both are R2's first two red rows.
- **Opus.** Add the fragment (GREEN under D9, so `default = true`), regenerate
  with `cargo xtask gen-registry`, commit the generated files through a
  private index (the shared-file rule in AGENT-CONSTRAINTS), and run the
  crate's own tests against `ffmpeg -c:a libopus` fixtures.
- **Size.** S each. Not performance items; they unblock two measurement rows.

### Track T — threading efficiency (after Track A)

#### T1 — where the extra CPU-seconds go at 4 and 8 threads

- **Evidence (data).** E2E-GAPS §21: 4K all-P, 6.96 CPU-s at one thread →
  9.97 at four → 11.72 at eight (+43% / +68%), against ffmpeg's +37% at its
  default. Baseline §4: 2.61x at four threads for 396% CPU. Named suspects,
  unmeasured: `RowPublisher` band copies, `PlaneView::block` guard-row copies
  (a read that straddles a seam is copied), park/unpark in `wait_rows`, and
  E-core scheduling on this 4P+6E machine.
- **Change.** Measure first: per-thread profile at 4 threads, innermost;
  count copied vs borrowed block reads. Then whichever of A4 (no band copy),
  a larger guard, or a different wait primitive the numbers pick.
- **Ceiling.** If the overhead halves, ~1.15x at four threads. Small; last.
- **Stop.** If the four-thread CPU-seconds overhead is under 20% after
  A1–A3 (kernels faster, waits unchanged → overhead share rises, so this is
  not automatic), close the item.

---

## 4. Sequencing, dependencies and parallelism

### What must land first, and what would be invalidated

| item | needs | invalidated / wasted if done first |
|---|---|---|
| A0 | nothing | — (A6 later removes the structure it pools; the `FramePool` wiring survives) |
| A1 | A0 (measurement noise) | nothing — kernels take `(plane rows, mv, ref, rect)` and are model-independent |
| A2 | A1 landed (share re-measured) | its `bS` record must be the one A6 keeps; write it as its own type now |
| A3 | A1, A2 | its dense residual is what A6's interleaved task consumes |
| A6 | A1–A3 landed **and** the scaling gate in A6 | doing it before A1–A3 would rewrite loops that A1–A3 are about to replace |
| A4 | T1's measurement | a model change for 6% serial is not justified on its own |
| B1 | nothing | nothing (its row API survives B2; only the element type changes) |
| B2 | B1 | doing B3 on `u16 + ready` would be redone |
| B3 | B2 | — |
| B4 | B2 (per-row bands of a plain `u8` plane), preferably B3 | threading a picture representation B2 is about to replace |
| C1 | nothing | — |
| C2 | C1's post-profile | — |
| D1 | nothing (own crate) | — |
| D2 | nothing | — |
| R1–R3 | nothing (xtask, registry, format crates) | — |
| T1 | A1–A3 | measuring overhead against kernels that are about to shrink |

### Who can run in parallel without corrupting each other

By crate ownership, so no two agents edit one file:

| lane | crates | items, in order |
|---|---|---|
| H.264 | `vaco-codec-h264`, `vaco-codec-dsp-mc` (new API only) | A0 → A1 → A2 → A3 → (gate) → A6 |
| HEVC | `vaco-codec-hevc` (+ HEVC tap sets in `vaco-codec-dsp-mc`, a separate file) | B1 → B2 → B3 → B4 |
| audio | `vaco-codec-aac` | C1 → C2 |
| encoders/filters | `vaco-codec-ffv1`, then `vaco-cli` (D2) | D1 → D2 |
| reachability | `xtask`, `vaco-registry` (generated files via private index), `vaco-format-audio-simple`, `vaco-conformance` | R1 → R3 → R2 |

Five lanes can run concurrently. **Measurement is the shared resource, not
the tree**: the `mkdir` lock in §2, one timing run at a time, and
CPU-seconds reported beside wall clock. Two lanes share
`vaco-codec-dsp-mc`; H.264 adds a partition API, HEVC adds tap sets — put
them in separate files and commit with a pathspec.

### Recommended waves

- **Wave 1 (start now, all parallel):** A0, B1, C1, D1 (profile stage), R1.
  Every one is S–M, every one banks a measured result or a decision inside a
  week, and A0 removes a swap-inducing 3 GB from everyone's measurement
  environment.
- **Wave 2:** A1, B2 → B3, C2, D1 (fix stage), R3, D2.
- **Wave 3:** A2 → A3, B4 (design doc first, then stage 1 serial restructure,
  then threads), R2.
- **Wave 4:** A6 (only if its gate says so), T1, A4 (only if T1 says so).

Total, if every lane is staffed: roughly seven to nine calendar weeks, of
which B4 and A6 are the long poles. Serial agent-time is nearer four months.

---

## 5. Verification summary

| item | correctness check | performance check | win criterion |
|---|---|---|---|
| A0 | video protocol, 1/2/4/8 | RSS at 1 and 4 threads; wall/CPU-s | RSS < 1 GiB at 1 thread; wall ≤ 1.00 |
| A1 | video protocol; checkasm vs retained per-pixel oracle | 4K, `-threads 1` and 4 | median ≤ 0.85 luma-only (gate), ≤ 0.75 complete |
| A2 | video protocol; watermark tests; deblock fixtures | 4K | `bS` half ≤ 0.95; complete ≤ 0.85 |
| A3 | video protocol + I-only fixture | 4K | ≤ 0.95 |
| A6 | video protocol; ≥ 12× `big.mkv` per N; threaded fuzz ≥ 5 min | scaling table 1–16 | 4-thread speedup ≥ 3.0x on B-pyramid |
| B1 | HEVC fixtures, N=1 | 4K | ≤ 0.85 |
| B2 | HEVC fixtures + I-only | 4K | hashes unchanged (perf may be ~1.0) |
| B3 | HEVC fixtures; checkasm | 4K | luma-uni ≤ 0.80 (gate); complete ≤ 0.65 |
| B4 | HEVC fixtures at 1/2/4/8; new determinism fuzz target; long fixture ≥ 12× per N | scaling + CPU% table | stage 1 serial ≤ 1.03; ≥ 2.5x at 4 threads |
| C1 | max \|Δ\| vs previous output ≤ 4 ulp, ≥ 99.9% identical; doc table unchanged | `audio_decode_aac` job | ≤ 0.20 |
| D1 | lossless round trip via `ffmpeg` decode, every coder/slice setting | transcode job | encoder < decoder time at default threads |
| D2 | scaler property test | decode+scale job at default | ≤ 0.95 on the row |
| R1–R3 | gate red today, green after; R2 table | — | — |
| T1 | video protocol | CPU-seconds at 4/8 | overhead < 20% |

---

## 6. Realistic end state, and its cost

Applying the realistic (not ceiling) estimates multiplicatively to the
baseline's 4K numbers:

**H.264, 4K.** Serial: A0 1.03 × A1 1.40 × A2 1.20 × A3 1.08 × (A6 1.04) ≈
1.95x multiplicatively, or **~2.0–2.25x** computed directly from the
remaining shares (MC 9.5 + deblock 11 + reconstruction 8 + CABAC 5.7 + glue
5 + other 5 ≈ 44% of today) → 8.4 s → ~3.9–4.3 s, i.e. **~5–5.7x behind
`ffmpeg -threads 1`** (ceiling ~3.2x → ~3.5x behind). Default threads: 3.03 s → ~1.5 s at four
threads; with A6 raising the threading bound and an eight-thread default
reconsidered on a bounded-memory basis, ~1.0 s — **~5x behind ffmpeg
default** (ceiling ~4x). The remaining gap is arithmetic vs hand-written
NEON, CABAC, and per-thread overhead; none of it is reachable without the
kinds of code D2 forbids, and this document does not recommend revisiting D2.

**HEVC, 4K.** Serial: B1 1.32 × B2 1.05 × B3 1.55 ≈ **2.1–2.4x** → 6.6 s →
~2.9 s, **~3.4x behind `ffmpeg -threads 1`** (ceiling ~2.6x). With B4 at
four threads (~3x): ~1.0 s, **~4x behind ffmpeg default** (ceiling ~3x).

**AAC.** C1 ~8x, C2 ~1.4x → 7.7 s → ~0.7 s for 30 s of audio: **~20x behind
ffmpeg**, ~40x real time. Adequate; closing further means rewriting the
spectral path and is not scheduled.

**Transcode H.264→FFV1.** From 13.7x to whatever the decoder is once the
encoder is below it: **~3–4x** at default threads, if D1's profile is as the
hypothesis says; unknown otherwise.

**Memory.** H.264 4K from 3.9 GiB to under 0.5 GiB at one thread (A0), and
structurally small after A6.

Cost: five lanes for seven to nine weeks; two of them (A6, B4) are
architectural and each carries an explicit "restructured, measured, not
faster, reverted" outcome as an acceptable result. Everything else is local
and reversible per commit.

What this programme will **not** achieve, stated so nobody spends a round
chasing it: parity with ffmpeg on any video decode path; byte-exact AAC
against ffmpeg (never claimed; not a goal); a faster CABAC engine (already
measured to its shape's optimum); a remux or probe win (already ahead).

---

## 7. Do not do (measured failures; do not re-propose without new evidence)

Each entry names the source of the measurement.

1. **Hand-written SIMD for `add_pixels_clamped`.** 0.9x/0.84x — lost to
   autovectorisation; gated to scalar. (AGENT-CONSTRAINTS "A benchmark where
   both paths tie exactly"; SIMD doc.)
2. **Batching deblocking's per-pixel reads/writes into slice ops inside the
   existing per-pixel-`get` gather.** Wash-to-loss, slower 6/8 rounds, no
   drop in `deblock_picture_luma` self time. (E2E-GAPS §10.) A2 removes the
   per-pixel `get` rather than batching around it, and must reproduce or
   refute this on its own commit.
3. **Lazy two-axis `j` derivation in `luma_qpel_sample`.** 0.997, 4/8 —
   wash. (E2E-GAPS §11.)
4. **Windowed 9x9 gather in `fetch_pred_4x4`.** 1.0025, 6/10 — wash.
   (E2E-GAPS §11.) A1 supersedes the 4x4 structure entirely; do not retry
   this at 4x4 granularity.
5. **Chroma bilinear in-bounds fast path** mirroring luma's. **1.034 —
   regression**, 2/10. (E2E-GAPS §11.) Chroma's clamp is two cheap ops.
6. **Merging Cb/Cr chroma inter prediction into one pass.** **1.024 —
   regression**, lost 9/10. (E2E-GAPS §19; SIMD doc Group 11.) Predict the
   two chroma planes as two independent single-plane calls.
7. **Vectorising the deblock filter arithmetic further.** The kernel is
   2.67–3.41% of runtime and its isolated 0.31x/0.41x bought 2.6% end to
   end. (SIMD doc Group 8; E2E-GAPS §18.)
8. **Branchless CABAC decision / whole-width renormalisation.** 1.35–1.85x
   *slower* than the spec-literal branchy per-bit engine, measured across
   four variants. (`docs/signal/vaco-codec-cabac.md`.)
9. **`std::thread::scope` per wave in the scheduler.** 45–60x slower than
   serial; the replacement pool breaks even at ~20 µs per job.
   (`docs/app/vaco-sched.md` "Measurements".)
10. **`vaco-sched` pipeline threading as the answer to a decode-bound job.**
    Stage parallelism "buys almost nothing"; the win came from threading
    inside the decoder. (E2E-GAPS §10.) `Driver::serial()` is what the CLI
    runs and that is fine.
11. **Hoisting the widen out of a FIR tap loop and reaching taps with
    `slide`; batching two output vectors per iteration in `fir_row`.** 1.64x
    and 1.36x *worse* than the naive reload. (SIMD doc Group 4;
    `docs/signal/vaco-codec-dsp-mc.md` §2.1.)
12. **Reusing `vaco-codec-dsp-mc::fir_row` at 4x4 granularity.** Ruled out
    in E2E-GAPS §11 because 4x4 cannot fill lanes; it is *not* ruled out at
    partition/PU granularity, which is what A1/B3 do.
13. **`samply --unstable-presymbolicate`** on this toolchain, and any profile
    aggregated without inline chains resolved. It hid `boundary_strength`
    at 28% for two rounds. (E2E-GAPS §18.)
14. **Quoting an absolute time from another session as a regression or a
    win.** Two agents differed by 30% on the same binary. (E2E-GAPS §15/§18/§19.)
15. **Widening the reference-plane guard rows beyond eight, or shrinking
    below.** Eight is exact for the 9-row six-tap read; seven pushes reads
    onto the copy path, nine costs memory for nothing. (E2E-GAPS §21.)
16. **Making the default thread count `available_parallelism`.** The curve
    is flat past four for roughly double the memory, and the memory ceiling
    must not be machine-dependent. (E2E-GAPS §22.) A6 may re-open the
    *number*, on a bounded-memory argument, not the principle.

---

## 8. Open questions this plan does not settle

- **x86-64.** Every SIMD and autovectorisation number in this repository is
  NEON. D12's central claim (multi-level dispatch pays for itself) is
  unmeasured on x86. Every kernel in A1/B3 keeps a scalar reference and goes
  through `vaco-checkasm`, so an x86 re-measurement later is a benchmark
  run, not a rewrite — but no item here should claim an x86 number.
- **Cycle counters.** None available under D2; CPU-seconds is the proxy.
- **`fearless_simd` 1.0** (due early September 2026): re-run Group 1–7 and
  record; the `i16` saturating add/sub and `i16→u8` narrow gaps are the ones
  that would change A1/B3's combine steps.
- **Whether the default `-threads` should move to eight after A6** is a
  memory-policy decision for the owner, not a performance one.

---

## 9. Measurements taken for this plan (2026-09-01, same binary and fixtures as the baseline)

Load average 4–6 throughout unless stated. Binary: the baseline session's
`dist` build (`cargo build --profile dist`, private target dir) with the three
encumbered decode features; dSYM from the same session. Fixtures: the
baseline's `e2e/` corpus.

### 9.1 H.264 4K, `-threads 1` — innermost inlined frame (same profile as baseline §2.1)

Aggregation by the *first* line of each `llvm-symbolizer --inlines` chain,
i.e. the deepest inlined function containing the sampled instruction;
percentages are of the 35,152 in-library samples. Breakdown inside the top
outermost frames:

| outermost (share) | innermost contributors |
|---|---|
| `sample_luma_block` (21.15%) | own body 7.79, `next<[u8;4]>` 3.00, `get<u8>` 2.11, `tap6` **1.98**, closure#2 1.93, `luma_qpel_sample` 1.58, `copied<u8>` 0.90, `IterMut::next` 0.88 |
| `reconstruct_mb` (12.74%) | `predict_chroma_inter` 1.85, `clamp` 1.56, `copy_nonoverlapping` 1.01, `saturating_add` 1.00, own 0.94, `overflowing_mul` 0.63 |
| `deblock_row` (10.73%) | `luma_mb_row` 5.72, closure#1 (`get`/`set`) 3.84, `copied<u8>` 0.34, `get_mut<u8>` 0.32, `EdgeThresholds::derive` 0.11 |
| `boundary_strength` (10.41%) | own 1.69, `eq<i32>` 1.54, `as_ref<CabacResidual>` 1.41, `copied<MvInfo>` 1.15, `abs_diff` 1.02, `copied<i32>` 0.83, `is_intra` 0.75, `mv_differs` 0.62 |
| `sample_chroma_2x2` (9.04%) | own 2.62, closure#1 1.63, `copied<u8>` 1.61, `clamp` 1.10, `get<u8>` 0.94, `clip_u8` 0.63, `chroma_mc_sample` 0.49 |
| `reconstruct_inter_mb::{closure#0}` (7.90%) | own 1.56, `saturating_add` 1.39, `overflowing_mul` 1.12, `clamp` 0.56, weight closures ~1.2 |
| `residual_block_cabac` (4.36%) | `decode_decision` 1.80, own 0.75, `renorm` 0.59 |
| `decode_slice_cabac` (3.84%) | **`write<MbSummary>` 2.48**, `write<MvInfo>` 0.19 |

### 9.2 HEVC 4K, `-threads 1` — innermost inlined frame (same profile as baseline §2.2)

| outermost (share) | innermost contributors |
|---|---|
| `predict_block_intermediate` (26.76%) | `Plane::index` **9.11**, `clamp` **7.05**, own 5.02, `copied<u16>` 2.66, `try_from` 0.64, `spec_next` 0.62, `get<u16>` 0.32, tap `fold` **0.28** |
| `build_cu_prediction` (10.43%) | `blit` 3.48, `saturating_add` 2.04, `clamp` 1.27, `predict_component` closures ~2.0, `unchecked_add` 0.62 |
| `write_inter_cu_no_residual` (9.32%) | `write_pred_block` 2.24, `clamp` 1.52, `lt` 1.42, `Plane::set` 1.24, `index` 0.88, `get<i32>` 0.85 |
| `Snapshot::capture` (8.08%) | own 4.86, `get_mut<u16>` 1.59, `get<u16>` 1.50 |
| `residual_coding` (6.83%) | `decode_decision` 1.77, own 1.61, `renorm` 0.73, `sig_ctx_inc` 0.35 |
| `predict_block` (6.22%) | `clamp` 1.83, `index` 1.78, own 0.95, `get_mut<i32>` 0.46 |
| `offset_block` (5.35%) | `try_from` 1.57, `get` 0.78, `index` 0.72, `clamp` 0.66 |
| `emit_pocs` (5.11%) | `try_from` 1.48, `blit` 1.39, `index` 1.04, `get<u16>` 1.03 |

Whole-profile innermost: `Plane::index`-class bounds arithmetic ≈ 13%,
`clamp` ≈ 11%, `try_from` ≈ 4%; the two CABAC `decode_decision` sites ≈ 3.6%.

### 9.3 H.264 4K memory, `-threads 1`

| run | peak RSS |
|---|---:|
| 25 frames (stream-copied `-t 1` cut of the same file) | 1,853 MiB |
| 75 frames | 3,870 MiB |
| 75 frames, transcoding to FFV1 instead of `-f null` | 3,421 MiB |
| HEVC, 75 frames, same clip | 318 MiB |

`vmmap --summary` seven seconds into the 75-frame decode: `MALLOC_LARGE`
160 MB virtual / 159 MB resident (live); **`MALLOC_LARGE (empty)` 3.4 GB
virtual, 2.3 GB resident, 1.1 GB swapped, 61 regions**; physical footprint
3.5 GB. Individual live large regions at that moment: one 59.0 MB
(`Vec<MbSummary>`, 32,400 × 1,888 B), three 11.9 MB (4K `yuv420p` frames),
three 8.1 MB (luma planes), three 10.9 MB; total live large ≈ 151 MB.

### 9.4 FFV1 encoder isolation, 1080p, 125 frames

| | round 1 | round 2 |
|---|---:|---:|
| vaco `-threads 1` decode only | 4.66 s | 3.61 s |
| vaco `-threads 1` decode + `-c:v ffv1 -f matroska` | 7.75 s | 6.98 s |
| ffmpeg `-threads 1` decode only | 0.38 s | 0.35 s |
| ffmpeg `-threads 1` decode + ffv1 | 0.43 s | 0.40 s |

vaco default threads, decode + ffv1: 4.27 s wall, 7.70 user + 0.18 sys,
1,041 MiB peak RSS. Reference encoder defaults (`ffmpeg -h encoder=ffv1`):
`coder` default `rice`, `context` 0, `slicecrc` −1. Two rounds under load
average 5–6; directional, not a protocol-grade measurement — D1 starts by
making one.

### 9.5 Encoder settings recorded in the fixtures

`hevc_4k.mp4` (x265 options SEI): `wpp` (enabled — x265 spells the
disabled flag `no-wpp`; the decoder takes its `entropy_coding_sync_enabled`
path for this file), `ctu=64`, `bframes=4`, `sao=4`, `deblock=0`,
`frame-threads=3`.
`h264_4k.mp4`: `cabac=1`, `ref=3`, `bframes=3`, `b_pyramid=2`, `weightp=2`,
`8x8dct=1`, `deblock=1:0:0`, `sliced_threads=0`.
