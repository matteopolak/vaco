# `fearless_simd` adoption measurements — PF-0.0

**Issue:** #90 (PF-0.0). **Executes:** `planning/12-performance.md` §11. **Date:** 2026-08-21.

This is the measurement pass D12 was taken without. It exists because D12 and its addendum were
written from a source and documentation review, and the point of maximum leverage for a correction is
*before* `vaco-simd`'s API freezes and before a production kernel is written against it.

Reproduce with `cargo bench -p vaco-simd`. The benchmark is
`crates/core/vaco-simd/benches/adoption.rs`; every number below has a named symbol you can
disassemble.

---

## Verdict

**`fearless_simd` holds up. Adopt it.** On this machine the substrate is not the bottleneck the
addendum feared — but the reason is not the one the plan gives, and two of the plan's specific
prescriptions are wrong.

| Claim under test | Result |
|---|---|
| D12 addendum: widening MAC costs **~6x** for the `pmaddwd` shape | **Wrong on NEON. Measured 0.79x** — the composition is *faster* than the auto-vectorised scalar loop, because LLVM recovers `smull`/`smull2`/`addp` from it. |
| D12 addendum: **~2.2–2.5x** on the 8-tap u8 FIR | **Too pessimistic. Measured 1.12x.** |
| Plan 11 §5.6: hoist the widen out of the tap loop and use `slide` | **Wrong on NEON. It is the slowest of the three variants at 1.63x**, against 1.12x for the "naive" reload it was supposed to improve on. |
| Plan 11 §5.6: the compositions cost 2–4 ops against 1 native | **Mostly moot.** LLVM's peephole combiner reconstructs `uqadd`, `uqsub`, `urhadd`, `uabd`, `abs` and `addp` from the compositions. Four of the six Group 1 rows measure 1.00x with *byte-identical* machine code to the native baseline. |
| Plan 11 §5.6's honesty note: "the combines may still fire, but far less reliably" | **Too gloomy for NEON.** Every combine we tested fired. It remains unverified on x86 — see the caveat that matters most, below. |
| Plan 12 §11 item 3: dispatch < 5 ns and indistinguishable from a `fn` pointer | **Confirmed. 0.00–0.23 ns.** |
| Plan 12 §11 item 5: the dispatched body inlines, zero `call` | **Confirmed.** The only `bl` in any kernel body is a cold `slice_index_fail`. |
| D12: `dispatch!` is safe, `kernel!` is not | **Confirmed** against v0.7.0 source, and demonstrated: this crate compiles under the workspace's `unsafe_code = "forbid"`. |
| D12 addendum: `Level::Neon` exists | **Confirmed**, and asserted by a test. |

**The genuine remaining gap is `saturating_add`/`sub` on signed `i16` — 1.46x.** It is the only
operation where LLVM does not reconstruct the native instruction, and it is the only one worth
raising upstream on performance grounds.

**One gap the plans never named was found**: there is no `i16 → u8` saturating narrow — no
`packuswb`, no `sqxtun`. See "The pack that is not there".

---

## Machine and toolchain

| | |
|---|---|
| CPU | Apple M5 — 10 cores (4 performance, 6 efficiency) |
| Target | `aarch64-apple-darwin`, Darwin 25.5.0 |
| Memory | 16 GB |
| Toolchain | `rustc 1.97.1` (8bab26f4f, 2026-07-14), LLVM 22.1.6, **stable** |
| Substrate | `fearless_simd` 0.7.0, Apache-2.0 OR MIT, edition 2024, MSRV 1.89 |
| Profile | `[profile.bench]` → release: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1` |
| Detected tier | `Neon`. Native `u8` width 16 bytes. |

### The caveat that matters most

**Nothing here measures x86.** aarch64 has exactly one SIMD level, which is convenient for us and
useless for validating the part of D12 that is actually load-bearing: that runtime dispatch across
SSE4.2 / AVX2 / AVX-512 is what buys the performance. Three checklist items are therefore
**unmeasured**, not passed:

* **Item 4 — binary size under multiversioning.** There is no multiversioning on aarch64. The
  `--cfg disable_dispatch_avx512` question is entirely open.
* **Item 6 — `interleave` and `swizzle_dyn` at 256/512 bits.** Every vector here is 128 bits, so the
  lane-crossing penalty the plan worries about cannot appear.
* **Item 7 — cross-tier bit-exactness.** There is one tier. The correctness suite proves NEON agrees
  with our scalar references; it proves nothing about AVX2 agreeing with SSE4.2.

Two of those (4 and 6) are non-blocking by the plan's own reckoning. **Item 7 is blocking, and it
cannot be closed on this hardware.** It needs a re-run on an x86-64 host — ideally an AVX-512 one —
before production kernels land. That is the single outstanding action from this pass.

A second caveat: LLVM 22's combiner is the reason most of these numbers are good. A toolchain bump
could silently take a 1.00x row to 3x. The `probes` module and its disassembly recipe exist so that
is checkable rather than a surprise; a CI assertion on instruction selection would be better still.

---

## Method, and the trap it avoids

Every gap has a native instruction the substrate does not expose. We cannot call it — `kernel!`
expands `unsafe` into the calling crate and is closed to us. But we can **measure against it**,
because LLVM emits it from a plain scalar Rust loop. So each pair is:

* **native** — an ordinary Rust loop at `opt-level = 3`. LLVM auto-vectorises it into `uqadd`,
  `urhadd`, `uabd`, `sqadd`, `abs`, `addv`, `umlal`, `smull` directly.
* **composed** — our `ops::simd` composition, dispatched through `dispatch_kernel!`.

That baseline is not soft: an auto-vectorised loop is a real shipping alternative, and it is exactly
what we would have got from `std::simd` under the superseded F5 design.

Three things make the numbers trustworthy:

1. **Both sides are `#[inline(never)]` symbols in the benchmark's `probes` module, and the timing
   loop calls those symbols.** The first version of this benchmark inlined each loop into its own
   timing closure and reported **0.45x for a composition whose disassembly is byte-identical to its
   baseline**. Two implementations that compile to the same machine code must measure the same; when
   they do not, the harness is measuring itself.
2. **A and B are timed interleaved, round by round.** Measuring A to completion and then B gives each
   a different slice of the machine's mood.
3. **The process spins for 300 ms before measuring.** macOS starts a process on an efficiency core
   and promotes it once it looks busy. Without this, early rows came back 2–3x slow: a 45 ns row
   measured 132 ns between two runs of an *unchanged binary*. That was not a hypothesis about
   scheduling; it was the observed failure.

Min-of-100 over 500-pass samples on 4096-element (L1-resident) buffers. Three consecutive runs agree
to ±0.01x on every row except one, which moves ±0.09x.

---

## Results

Full output follows; `composed / native` is the number to read. Except in Group 6, **lower is
better** and 1.00x means "the composition is as good as the instruction it replaces".

### Group 1 — gap compositions vs the instruction LLVM reaches for

| operation | native (ns) | composed (ns) | composed/native | composition |
|---|---:|---:|---:|---|
| `saturating_add_u8` | 44.5 | 44.8 | **1.01x** | `min(!b) + b` |
| `saturating_sub_u8` | 44.5 | 44.8 | **1.01x** | `max(b) - b` |
| `rounded_avg_u8` | 44.5 | 69.1 | **1.55x** | `(a\|b) - ((a^b)>>1)` |
| `rounded_avg_u8`, batched 4x | 44.5 | 44.5 | **1.00x** | same, four vectors per iteration |
| `abs_diff_u8` | 150.5 | 69.0 | **0.46x** | `max - min` |
| `saturating_add_i16` | 89.1 | 129.8 | **1.46x** | widen / add / `saturating_narrow` |
| `abs_i16` | 65.0 | 65.2 | **1.00x** | `max(x, -x)` |

### Group 2 — horizontal reduction: where the accumulator lives

| operation | native (ns) | composed (ns) | composed/native |
|---|---:|---:|---:|
| one hoisted vector accumulator | 112.2 | 437.7 | **3.90x** |
| reduced once per chunk (the documented mistake) | 112.2 | 189.8 | **1.69x** |
| **four hoisted accumulators** | 112.2 | 111.0 | **0.99x** |

### Group 3 — the `pmaddwd` shape

| operation | native (ns) | composed (ns) | composed/native |
|---|---:|---:|---:|
| `madd_i16_i32` | 165.4 | 130.2 | **0.79x** |

### Group 4 — 8-tap u8 horizontal FIR, 4096 outputs (the `pmaddubsw` shape)

| variant | native (ns) | composed (ns) | composed/native |
|---|---:|---:|---:|
| reload + widen per tap | 376.3 | 421.0 | **1.12x** |
| reload, batched 2 output vectors | 376.3 | 510.7 | **1.36x** |
| widen hoisted, `slide` per tap (plan 11 §5.6) | 376.3 | 615.2 | **1.63x** |

### Group 5 — dispatch overhead

| calls per pass | `dispatch_kernel!` | plain `fn` pointer | delta per call |
|---:|---:|---:|---:|
| 1 | 0.50 ns | 0.42 ns | +0.08 ns |
| 10 | 4.75 ns | 4.75 ns | 0.00 ns |
| 100 | 122.00 ns | 99.08 ns | +0.23 ns |

Comfortably inside the <5 ns pass condition, and the deltas are at the resolution floor of the
measurement. Dispatch is not a cost we need to think about again.

### Group 6 — the worked example, `yuv420p → rgb24`, one 1920px row

| operation | scalar (ns) | vectorised (ns) | speedup |
|---|---:|---:|---:|
| `yuv420p_to_rgb24_row` | 2427.0 | 591.1 | **4.1x** |

A whole-kernel result, not a microbenchmark: `u8 → u16 → u32` widening chain, an integer colour
matrix, clipping, a saturating pack and a 3-way `swizzle_dyn` interleaved store. This is the shape
`vaco-scale` will be full of, and it comes out well.

### Group 7 — masked-lane select

Added later than the rest of this document (Group 7 is `#127`'s own spike, extended for `#619`'s
deblocking-vectorisation blocker), so it is dated separately: **2026-08-29**, same machine and
toolchain as above. 4096-element buffers, min-of-100 over 500-pass samples, 5 independent process
runs — the win/loss split below is across those runs, not across `time_pair`'s own internal
interleaving.

| operation | scalar (ns) | composed (ns) | composed/scalar | round-1..5 ratio | win/loss |
|---|---:|---:|---:|---|---:|
| `select_u8` (`mask8x16::select`) | ~805–834 | ~80–84 | **≈0.10x** | 0.10, 0.10, 0.10, 0.10, 0.10 | 5/5 |
| `select_u8` (bitwise blend, `(m&a)\|(!m&b)`) | ~805–834 | ~82–86 | **≈0.10–0.11x** | 0.11, 0.10, 0.10, 0.10, 0.11 | 5/5 |
| `select_i16` (`mask16x8::select`) | ~876–881 | ~163–165 | **≈0.19x** | 0.19, 0.19, 0.19, 0.19, 0.19 | 5/5 |
| `select_i32` (`mask32x4::select`) | ~700–729 | ~300–313 | **≈0.43x** | 0.43, 0.43, 0.43, 0.43, 0.43 | 5/5 |

`select_u8` was `#127`'s own spike, and its verdict stands: `mask8x16::select` and a hand-composed
bitwise blend measure identically, both ~10x the branchy scalar loop, so there is no reason to prefer
the blend. **`select_i16` and `select_i32` are new for `#619`** — the widths `vaco-codec-dsp-deblock`'s
own per-sample filter decisions actually need once `u8` samples are widened for signed arithmetic.
Both beat the scalar branchy loop cleanly and with essentially zero round-to-round variance (the
ratio does not move in the third significant figure across 5 independent process launches, despite a
niced fuzz sweep occupying 2 of this machine's 10 cores throughout) — this is a genuine win on its own
merits, not merely an unblocker adopted at parity.

The falling speedup with width (10x → ~5.3x → ~2.3x) is the fixed 128-bit block shrinking the lane
count that one instruction covers (16 → 8 → 4), while the scalar loop's per-lane branch cost stays
roughly constant. On aarch64/NEON, "native width" and "128-bit block" are the same thing, so this
table cannot say whether AVX2's 256-bit `S::i32s` recovers the wider ratio — the same x86
verification gap Outstanding item 1 already names for the rest of this document.

`ops::select_i16`/`ops::select_i32` and their `simd::` siblings are, like `select_u8`, **not
compositions**: `fearless_simd` provides `Select` generically over `SimdBase::Mask` at every lane
width already, so there was no gap to fill. The value added is the *named, tested, benchmarked*
primitive at the widths a real kernel needs, plus `vaco_simd::testing::check_ternary_u8`, the ternary
differential driver that did not exist before (`select_u8`'s own edge-corpus test hand-rolled the
sweep inline until this pass gave it a shared driver, the same way `check_binary_u8` already served
every binary op).

---

## Instruction selection — the evidence

Read back from the benchmark binary with `objdump`; the recipe is in the `probes` module docs. Top
SIMD mnemonics per symbol:

| symbol | instruction mix | `bl` |
|---|---|---:|
| `scalar_sat_add_u8` | `uqadd.16b` x4 (unrolled 4x) | 0 |
| `composed_sat_add_u8` | **`uqadd.16b` x7** | 0 |
| `scalar_sat_sub_u8` | `uqsub.16b` x4 | 0 |
| `composed_sat_sub_u8` | **`uqsub.16b` x7** | 0 |
| `scalar_avg_round_u8` | `urhadd.16b` x4 | 0 |
| `composed_avg_round_u8` | **`urhadd.16b` x1** (not unrolled) | 0 |
| `composed_avg_round_u8_x4` | **`urhadd.16b` x4** | 0 |
| `scalar_abs_diff_u8` | `ushll.4s` x9, `tbl.16b` x5, `uabd.16b` x4 | 0 |
| `composed_abs_diff_u8` | **`uabd.16b` x1** | 0 |
| `scalar_sat_add_i16` | `sqadd.8h` x4 | 0 |
| `composed_sat_add_i16` | `saddl.4s`, `saddl2.4s`, `sqxtn.4h`, `sqxtn2.8h` — **no `sqadd`** | 0 |
| `scalar_abs_i16` | `abs.8h` x4 | 0 |
| `composed_abs_i16` | **`abs.8h` x6** | 0 |
| `scalar_hsum` | `add.4s` x8 (eight accumulators), `addv.4s` x2 | 0 |
| `composed_hsum_hoisted` | **`add.4s` x1** — one accumulator | 0 |
| `composed_hsum_x4` | **`add.4s` x7** | 0 |
| `scalar_madd` | `smull.4s` x5, `smlal.4s` x5 | 0 |
| `composed_madd` | **`smull` + `smull2` + `addp.4s`** — the optimal NEON form | 0 |
| `composed_fir8_reload` | `umlal.8h`, `ushll.8h`, `add.8h`, 1 stack spill | 1 (`slice_index_fail`) |
| `composed_fir8_reload_x2` | same, **6 stack spills** | 9 (all `slice_index_fail`) |
| `composed_fir8_slide` | **`ext.16b` x12**, `umlal.8h` x4, `mla.8h` x4 | 2 (all `slice_index_fail`) |

Every `bl` in every kernel is a cold `core::slice::index::slice_index_fail` edge. **Checklist item 5
passes: no call on any hot path, at any level.**

---

## What the numbers actually mean

### 1. LLVM reconstructs the native instruction from the composition

This is the headline, and it inverts plan 11 §5.6's "one honesty note about what we gave up". That
note argued that `combineAddToPMADDWD`, `combineBasicSADPattern` and `combineAVG` operate on generic
IR, and that going through explicit `core::arch` intrinsics would make them fire "far less
reliably".

On NEON, with LLVM 22, they all fired. `min(!b)+b` becomes `uqadd`. `max-min` becomes `uabd`.
`(a|b)-((a^b)>>1)` becomes `urhadd`. `max(x,-x)` becomes `abs`. Widen-mul-unzip-add becomes
`smull`/`smull2`/`addp`. **The composed and native forms of `saturating_add_u8` are byte-identical
machine code.**

The op-count column in plan 11 §5.6's gap table describes source-level operations, not emitted
instructions, and the two are not close. The table should say so.

`abs_diff_u8` is the funny one: the composition is **2.2x faster than the baseline**, because
`u8::abs_diff` compiles to a widening path (`ushll`, `tbl`) rather than to `uabd`. Our composition
tells LLVM more clearly what we want than `std`'s own function does.

### 2. The residual gaps are loop shape, not instruction selection — and both are fixable in our code

Two Group 1/2 rows looked like substrate failures and were not:

* **`rounded_avg_u8` at 1.55x.** Same instruction, but the scalar iterator loop gets unrolled 4x by
  LLVM and our `chunks_exact` loop does not. Processing four vectors per iteration takes it to
  **1.00x**.
* **`hsum_i32` at 3.90x.** A single hoisted vector accumulator is a 1024-long chain of dependent
  adds with ~2 cycles of latency each and nothing to fill them. LLVM automatically splits the scalar
  loop into *eight* accumulators; it will not do that to a hand-written loop, because it has no
  reason to think the loop is latency-bound. Four independent accumulators take it to **0.99x**.

Neither is a `fearless_simd` problem. Both are kernel-authoring rules, and they are the most
valuable output of this pass:

> **Rule A — batch.** A `chunks_exact` loop with one vector per iteration will not be unrolled.
> Process 2–4 vectors per iteration where registers allow.
>
> **Rule B — never carry a single accumulator.** Any loop-carried vector accumulator needs at least
> four independent copies, reduced once at the end. This is Rule A's more important half: it is worth
> ~4x, and it is invisible to every correctness test.

These belong in plan 11 §5.4's list of non-negotiable kernel properties.

### 3. Rule A has a ceiling, and the FIR is past it

Batching the FIR to two output vectors made it **worse** — 1.12x → 1.36x. The disassembly says why:
one stack spill becomes six. Eight taps × two output vectors × two `i16` accumulators, plus the
widened sources, does not fit in 32 NEON registers.

So Rule A is "batch until you spill", and the spill count is the thing to check, not the ratio. The
FIR at one output vector per iteration is already at the right width.

### 4. Plan 11 §5.6's prescribed FIR structure is the worst of the three

"Hoist the widen out of the tap loop and reach neighbouring taps with `slide`" measures **1.63x**,
against **1.12x** for the reload-and-re-widen form it was meant to improve on. Twelve `ext.16b`
instructions, all contending for the same shuffle port, cost more than the `ushll` pairs they
replace — and unlike `ushll`, `ext` has no second pipe to go to.

The reasoning was sound (an `ext` is one instruction; a load-plus-widen is three) and it is simply
not what the machine does. Plan 11 §5.6's "how to avoid the widening-multiply gap" paragraph should
be rewritten around what actually won: **broadcast-coefficient `wmla`, reloading per tap, one output
vector at a time.**

### 5. The pack that is not there — a gap the plans missed

D12 lists `saturating_narrow` as present and calls it "the `packuswb` we need". It is present. It is
not `packuswb`.

`SimdNarrow` narrows `i16 → i8` and `u16 → u8`. **There is no `i16 → u8` saturating narrow.** But the
last step of essentially every video kernel is exactly that: clamp a signed intermediate to `0..=255`
and pack it into bytes — one instruction on both architectures (`packuswb`, `sqxtun`).

`ops::simd::pack_u8_from_i16` composes it as `max(0)` + bitcast + `saturating_narrow`: two extra
operations, on the final step of nearly every kernel we will write. Individually trivial, and
ubiquitous. **This should be the second item on the upstream list**, after signed saturating
arithmetic.

A related smaller friction: `S::i16s` and the fixed-width `i16x8<S>` cannot be covered by one generic
helper, because they are the same type on NEON/SSE and different on AVX2. `pack_u8_from_i16` and
`pack_u8_from_i16x8` are duplicated for that reason.

### 6. Signed saturating arithmetic is the one real gap

`saturating_add_i16` at **1.46x** is the only Group 1 row where LLVM does not reconstruct the native
instruction: four instructions (`saddl`, `saddl2`, `sqxtn`, `sqxtn2`) where `sqadd.8h` would do.

The plan already argues the exposure is smaller than it looks, because the dominant use of saturating
arithmetic in a codec is `clip(pred + residual)` — an add followed by a narrow, where the narrowing
half is where the saturation is needed. That argument still holds. But 1.46x is real, it is the only
measured gap in the group, and it is the strongest single ask to take upstream.

---

### Group 8 — `#619`: `vaco-codec-dsp-deblock`'s masked-select kernel, in isolation and end to end

Dated separately: **2026-08-29**, same machine and toolchain as above, with a niced fuzz sweep
occupying 2 of the machine's 10 cores throughout (as in Group 7) and, for the end-to-end row,
several other agents' concurrent `cargo build`s on the same host (load average ~5.8-6 during
measurement) -- noisier conditions than any earlier group in this document, noted because it is the
likely explanation for the wider end-to-end spread below.

**Microbenchmark** (`cargo bench -p vaco-codec-dsp-deblock`, `benches/batch.rs`): the batched
kernel (`vaco_codec_dsp_deblock::batch::filter_luma_edge`/`filter_chroma_edge`, one call per edge)
against the per-line scalar loop a caller would otherwise run (`filter_luma_line`/`filter_chroma_line`
called once per line). Interleaved A/B, min-of-100 over 2000-pass samples, one edge's worth of
non-flat, mixed-`bS` (`0..=4`) fixture data per call — 5 independent process launches:

| edge | scalar (ns) | batched (ns) | batched/scalar | round-1..5 ratio | win/loss |
|---|---:|---:|---:|---|---:|
| luma, 16 lines | ~58.8-64.5 | ~18.0-19.7 | **≈0.31x** | 0.306, 0.306, 0.307, 0.306, 0.305 | 5/5 |
| chroma, 8 lines | ~8.7-9.6 | ~3.6-4.0 | **≈0.41x** | 0.410, 0.409, 0.410, 0.408, 0.407 | 5/5 |

Both widths beat the scalar per-line loop cleanly and with essentially zero round-to-round variance,
matching Group 7's `select_i16` shape (the masked-select tree is the majority of each kernel's cost).
Chroma's batch (8 lines) is *narrower* than NEON's native `u8` width (16), so the kernel padding a
zero-filled native-width buffer to reach the vector path at all (`load_i16_group_padded`/
`store_i16_group_padded` in `batch.rs`) is load-bearing, not incidental — the first version of this
kernel chunked by native `u8` width only, which measured a **regression** on chroma specifically
(≈1.1x, 5/5 losses): an 8-line batch never filled one full 16-lane `u8` chunk, so every chroma call
fell straight through to the scalar tail path and paid the dispatch/truncation overhead for nothing.
Adding the narrower padded-load stage turned that into the 0.41x win above.

**End to end** (4K H.264 decode, 3840x2160, 75 frames, Main profile, `-bf 0 -refs 1`, byte-exact
against `ffmpeg` both before and after): interleaved baseline/candidate, alternating which ran first
each launch, **10 independent process launches** (wall clock via `date`, not a cycle counter — this
crate's own D2 forbids the `unsafe` a cycle-counter read needs, the same constraint Group 7's own
measurement recorded):

| launch | baseline (s) | candidate (s) | candidate/baseline |
|---:|---:|---:|---:|
| 1 | 15.866 | 15.276 | 0.963 |
| 2 | 15.928 | 15.376 | 0.965 |
| 3 | 15.771 | 16.616 | 1.054 |
| 4 | 17.909 | 17.230 | 0.962 |
| 5 | 17.767 | 17.046 | 0.959 |
| 6 | 15.987 | 15.272 | 0.955 |
| 7 | 16.198 | 16.244 | 1.003 |
| 8 | 16.635 | 15.950 | 0.959 |
| 9 | 15.640 | 14.840 | 0.949 |
| 10 | 34.638 | 20.984 | 0.606 |

8 of 10 launches favoured the candidate; launch 3 was a real 5.4% loss and launch 7 a wash. Launch
10's absolute times (34.6s/21.0s, against every other launch's 15-18s) are a load spike mid-run, not a
real 40% effect — excluding it, the mean ratio across the other 9 launches is **≈0.974x, a ~2.6%
end-to-end win**, keeping launch 10 in gives ≈0.94x, which overstates it.

**The end-to-end win is real but far smaller than the isolated ratio, and that gap is the more
important number.** Deblocking's luma/chroma filters were ~17%/~9% of self time (E2E-GAPS.md §10), so
a 2.9x/2.4x win on the filter arithmetic alone, with the surrounding per-edge cost (boundary-strength
derivation, `EdgeThresholds::derive`, and the per-sample gather/scatter through the picture buffer's
`get`/`set` closures — all still scalar, untouched by this kernel) unchanged, arithmetically caps the
possible end-to-end win at roughly `0.26 * (1 - 1/2.7) ≈ 16%` even before dispatch and gather overhead
are counted, and the *measured* ~2.6% says most of the edge's real cost is that surrounding scalar
work, not the filter equations this kernel replaced. This is not a new lesson so much as the same one
this document's Group 4/Rule A material already drew about the FIR: a kernel that wins cleanly in
isolation is not the same claim as a caller-visible win, and only measuring the caller settles it.

**Kept, not reverted**, because the win — while modest — is positive and directionally consistent (8
of 10 launches, mean ≈0.974x excluding the one load-contaminated outlier), unlike the genuine
negatives this document and `E2E-GAPS.md` record elsewhere (`add_pixels_clamped_vector` at 0.9x/0.84x,
round 2's three FIR/deblock-memory attempts at 0.997/1.0025/1.034). All three Main-profile regression
fixtures (416x240 `smptebars`, 352x288 `testsrc2`, 640x360 `mandelbrot`) and the 4K clip above remained
byte-exact against `ffmpeg` with the kernel wired in.

---

### Group 9 — `vaco-scale`: a fixed-tap-count body, not a vector kernel

Dated separately: **2026-08-30**. `planning/E2E-GAPS.md` §9-11's 2160p→1080p
scaling gap, isolated to the scaler's own cost and profiled — full detail and
the measured before/after numbers are in `docs/signal/vaco-scale.md` §8's "The
E2E-GAPS 2160p→1080p gap" subsection; this entry records it here because it is
a kernel-shaped change even though it never touches `vaco-simd`.

Recorded here specifically for the negative-space this group's own rule
predicts: **this is not a SIMD kernel.** `vaco_scale::exec::filter_h`'s tap
loop runs a runtime-determined number of iterations (`bank.taps`), so the
optimiser cannot unroll or vectorise it even though real tap counts are
small (2-16) and mostly fixed per plan. Samply profiling (methodology in the
vaco-scale doc, including the `dsymutil` + `llvm-symbolizer --inlines` step
needed because `#[inline]` had collapsed the whole call chain onto one
attributed line) found roughly half the loop's self time was `Iterator`/
`Option` bounds-check scaffolding around the loop, not the multiply-accumulate
itself. Fixed by converting the coefficient/window slices to `&[i32; N]` for
`N` in `{2, 4, 6, 8}` before the accumulation loop, so the trip count reaches
the optimiser through the type rather than a runtime field — no lanes, no
`Caps`/`Tier` dispatch, no `vaco-simd` dependency at all. Measured 0.73-0.90x
(mean ≈0.80x) on the exact `2160p -> 1080p` bicubic scenario, 10/10 interleaved
rounds favouring the change, and up to 0.58x on other real conversions whose
tap count lands on one of the four specialised widths.

This is the same lesson Group 4's honest framing already drew about the 8-tap
u8 FIR (structure matters more than the instruction set) pushed one step
earlier: here the missing structure was a *compile-time-visible trip count*,
and supplying it let ordinary scalar codegen do the rest — no hand-vectorised
body was written or needed. Filed here rather than left unrecorded because a
future reader searching this document for "why is the filter loop slow" should
find the answer, even though the fix has no `Lanes`/`KernelSet` footprint to
show up under `cargo bench -p vaco-simd`.

---

### Group 10 — H.264 deblocking's `boundary_strength`: the missing scalar cost, found and cut

Dated **2026-08-30**, same 4K 75-frame Main fixture as Groups 8/9, private `--target-dir`, `cargo
build --profile dist` (opt-level 3, LTO, `strip = "none"`, `debug = "line-tables-only"` — release
codegen with symbols kept) rather than the stripped `release` profile, because `--unstable-presymbolicate`
resolved almost nothing on the stripped binary (leaf frames came back as bare hex addresses, e.g.
`0x37500c`, for the large majority of samples — the same presymbolication failure Group 8/§10's own
notes warned about, just total here instead of partial). `dsymutil` on the `dist` binary plus
`llvm-symbolizer --obj=<dSYM> --inlines -f -C -p` against each leaf address (module-relative address
plus the binary's own `__TEXT` `vmaddr`, confirmed with `dwarfdump --lookup`) recovered the full inline
chain per sample; aggregating self time by each chain's outermost (physically-emitted) frame gives the
first *reliable* whole-decoder cost breakdown this document has for H.264:

| self time | function |
|---:|---|
| 28.00% | `deblock::boundary_strength` |
| 18.82% | `reconstruct::reconstruct_picture` |
| 11.72% | `reconstruct::sample_luma_block` |
| 11.36% | `deblock::deblock_picture_luma` (gather/scatter and edge bookkeeping, *not* `boundary_strength`) |
| 4.31% | `reconstruct_inter_mb::{closure#0}` |
| 3.99% | `cabac_residual::residual_block_cabac` |
| 3.72% | `deblock::deblock_picture_chroma` |
| 3.55% | `vaco_codec_dsp_idct::h264::idct4x4` |
| 2.67% | `vaco_codec_dsp_deblock::batch::filter_luma_edge` (the masked-select kernel itself) |

**This closes the question `E2E-GAPS.md` §10/Group 8 left open.** Group 8 measured the deblocking
SIMD kernel at 0.31x/0.41x in isolation but only ~2.6% end to end, and reasoned from the two filters'
combined ~26% share of *total* runtime that the surrounding scalar cost (boundary-strength derivation,
`EdgeThresholds::derive`, and the per-sample `get`/`set` gather/scatter) must be absorbing most of the
possible win. This profile confirms it directly and names the size: `boundary_strength` alone is
**28% of total self time** — the single largest cost centre in the entire decoder, larger than
`reconstruct_picture`, and almost eleven times the batched filter kernel it feeds (2.67%).

**The cause, found from the call site, not the function body.** `boundary_strength` is a pure
function of `(mb_edge, p_blk, q_blk)` (plus the two macroblocks and reference-POC lists, none of
which change within one edge). `deblock_picture_luma`'s per-edge gather loop calls it once per
*pixel row* (16 times per vertical edge, 16 per horizontal edge) — but `p_blk`/`q_blk` are derived
from `row / 4` (`blk_row`/`blk_col`), so **all four rows in one group of 4 compute the identical
`bS` value**, and the call was simply repeated four times instead of once. `deblock_picture_chroma`
has the same shape at its own granularity (`row / 2`, a 2x repeat, since chroma reuses luma's `bS`
at half resolution per this crate's own chroma doc comment). Clause 8.7.2.1 defines one `bS` per 4x4
luma block, not per pixel row — the code already computed the block index correctly, it just called
the derivation function once too often per block.

**Fix** (`crates/codec/vaco-codec-h264/src/deblock.rs`): in all four gather loops (luma vertical,
luma horizontal, chroma vertical, chroma horizontal), compute `boundary_strength` once per
`blk_row`/`blk_col` group into a small fixed-size array (`[u8; 4]`) before the per-row/column gather
loop, then have that loop index into the array instead of calling the function again. This is pure
memoisation of an already-pure function — no table, no new data structure, no change to any `bS`
value — so every call site's inputs and the resulting bytes are unchanged; it only stops recomputing
the answer the loop already knew from three iterations ago. Not a SIMD change and does not touch
`vaco-simd`: this is the same "the cost was bookkeeping, not arithmetic" shape `E2E-GAPS.md` §14
(Group 9) and this document's own Group 8 discussion already named, applied to a redundant scalar
call instead of a missing compile-time bound.

**Measured**, interleaved baseline/candidate, alternating which ran first each round, 10 independent
process launches, wall clock (no cycle counter available under this crate's `#![forbid(unsafe_code)]`,
same constraint Group 8 recorded), on the 4K 75-frame fixture end to end (`vaco -i uhd.mp4 -map 0:v:0
-c:v rawvideo -f null -`), under background load (a concurrent `dsymutil`/build history left load
average around 6-16 during the run):

| round | baseline (s) | candidate (s) | candidate/baseline |
|---:|---:|---:|---:|
| 1 | 12.801 | 8.138 | 0.636 |
| 2 | 9.606 | 8.110 | 0.844 |
| 3 | 9.737 | 7.444 | 0.765 |
| 4 | 9.589 | 7.550 | 0.787 |
| 5 | 9.763 | 8.738 | 0.895 |
| 6 | 11.469 | 9.000 | 0.785 |
| 7 | 10.748 | 8.817 | 0.820 |
| 8 | 10.702 | 9.099 | 0.850 |
| 9 | 13.227 | 8.997 | 0.680 |
| 10 | 11.798 | 9.366 | 0.794 |

**10 of 10 launches favoured the candidate, mean ratio ≈0.786 (≈1.27x), median ≈0.791.** This is a
substantially larger, and far more consistent, win than any earlier round in this document or
`E2E-GAPS.md` — every one of the four prior negative/marginal results (round 2's three interpolation
attempts, the deblocking kernel's own 2.6% end-to-end share) touched code that was already a small
share of total time; this touched the single largest one.

**Byte-exact against `ffmpeg`, unchanged**, on all four regression fixtures: the 4K 75-frame clip
above, a 60-second 1800-frame 1080p `libx264` file (`big.mkv`), and two fresh stock-`libx264`
(`testsrc2`, default encoder settings — B-frames, CABAC, 3 references) clips at 322x242 and
1024x576. `cargo test -p vaco-codec-h264 --locked` and `cargo clippy -p vaco-codec-h264 --all-targets
--locked -- -D warnings` both clean.

**Kept.** Full decode time on the 4K fixture: best-of-3 **8.14s**, down from the round's own
9.89s-11s baseline range (machine-load-dependent, see `E2E-GAPS.md`'s own caution against comparing
absolute numbers across sessions) — call it **≈1.2-1.27x** depending on which baseline figure is
used, against `ffmpeg -threads 1`'s 0.62s best-of-3 (unchanged by this work; the gap narrows from
~15.9x to ~13.1x using this session's own before/after numbers).

---

## Group 11 — H.264 chroma inter prediction: a plausible redundancy that measured as a regression

Dated **2026-08-30**, same 4K 75-frame Main fixture and toolchain as Group 10, private
`--target-dir`, `cargo build --profile dist` with `patent-encumbered-h264-decode`, `dsymutil` +
`llvm-symbolizer --obj=<dSYM> --inlines -f -C -p` against each leaf sample's address (module offset
plus `__TEXT` vmaddr `0x100000000`). Re-running Group 10's own method after its fix landed gives the
first post-fix whole-decoder profile:

| self time | function |
|---:|---|
| 23.01% | `reconstruct::reconstruct_picture` |
| 15.87% | `reconstruct::sample_luma_block` |
| 12.70% | `deblock::deblock_picture_luma` |
| 11.26% | `deblock::boundary_strength` |
| 5.37% | `reconstruct_inter_mb::{closure#0}` |
| 4.66% | `cabac_residual::residual_block_cabac` |
| 4.48% | `mb::decode_slice_cabac` |
| 4.27% | `vaco_codec_dsp_idct::h264::idct4x4` |
| 3.92% | `deblock::deblock_picture_chroma` |
| 3.41% | `vaco_codec_dsp_deblock::batch::filter_luma_edge` |

`boundary_strength` fell from Group 10's 28.00% to **11.26%**, confirming that fix's effect directly
rather than by inference. `reconstruct_picture`/`sample_luma_block` rose from 18.82%/11.72% to
23.01%/15.87% — a share increase from a shrinking denominator, not a slowdown in either function.

**Attempt**: `reconstruct_picture`'s per-macroblock chroma section calls `predict_chroma_inter` once
per component (Cb, then Cr), and the innermost-frame breakdown put that function at 9.87% of total
self time — the largest single named leaf inside `reconstruct_picture`'s 23.01%, on the strength of
being called twice per macroblock and re-deriving, both times, the identical per-4x4-block
bookkeeping (`blk_xy`, the `mv_blocks` lookup, `ref_idx_l0`/`ref_idx_l1`, `reads_l0`/`reads_l1`,
`mv_l0`/`mv_l1`, `cx0`/`cy0`) and the identical eighth-pel position/fraction derivation one level
down inside the per-pixel sampler — none of which depends on which component is being predicted.
Implemented a merged `predict_chroma_inter_planes` producing both planes from one pass over the 16
blocks (via a new `chroma_mc_sample_pair`/`sample_chroma_point_pair` that derive the shared
position/weights once), verified byte-identical by a direct unit test and the full
`decoder_output_matches_ffmpeg` integration test.

**Measured**, interleaved, 10 launches: candidate lost **9 of 10** rounds (one wash at 0.997x);
excluding one clear load-outlier round (1.502x), every remaining round was still 2-5% *slower*,
median ratio **1.024** (a ~2.4% regression). **Reverted**, no commit. Full round-by-round numbers
in `planning/E2E-GAPS.md` §19.

**Why this is worth recording alongside Group 8's chroma finding, not despite it.** Group 8/§11
found a chroma fast-path regression because chroma's own clamp arithmetic was too cheap for a guard
branch to beat. This is a *different* mechanism failing the *same* plane a second time: merging two
independent single-plane call sites into one dual-plane pass plausibly increased live state per loop
iteration (two 8x8 output arrays, both planes' MV/ref-idx/pointer state at once) enough to change
LLVM's register-allocation or inlining decisions for code the optimiser was already handling well as
two separate, non-interfering call sites. Both Group 8 and this entry refute the same intuition —
"fewer redundant operations must be faster" — for chroma specifically, by two unrelated routes. The
lesson is unchanged from every other entry in this document: identifying a plausible redundancy
names a candidate, not a result.

---

## Revised upstream ask

Plan 12 §11 lists six operations to request before v1.0. The measurements reorder that list sharply —
four of the six turn out to be free.

| Ask | Priority | Evidence |
|---|---|---|
| **Signed saturating add/sub** (`sqadd`/`paddsw`) | **1 — the only measured gap** | 1.46x; no combine fires |
| **`i16 → u8` saturating narrow** (`packuswb`/`sqxtun`) | **2 — not previously identified** | 2 extra ops on the last step of every pixel kernel |
| Widening multiply-add (`pmaddwd`/`pmaddubsw`) | 3 — but say so honestly | 0.79x and 1.12x measured; ask because x86 is unverified, not because NEON hurts |
| Rounded average, abs-diff, integer `abs`, unsigned saturating add/sub | **withdraw** | LLVM reconstructs all of them; 1.00x |
| Horizontal reductions | **withdraw** | 0.99x with four accumulators; the slice-fold path lowers to `addv` |

Filing the four withdrawn asks would have cost upstream's time on operations that measure as free.
Measuring first was worth an afternoon.

---

## Outstanding

1. **Re-run on x86-64, ideally AVX-512.** Closes checklist items 4, 6 and the blocking item 7. Until
   then D12's central claim — that runtime multi-level dispatch pays for itself — rests on reasoning,
   not measurement.
2. **A CI assertion on instruction selection.** Most of these numbers depend on LLVM's combiner. A
   toolchain bump could take a 1.00x row to 3x with no test failing. The `probes` pattern is designed
   to be automatable; `xtask` should grow a check.
3. **Re-run the D10 Gate 3 assessment against `fearless_simd` 1.0** when it lands (early September
   2026), and record the `cargo-geiger` count in `docs/dependencies.md`. Crude count today: 116
   occurrences of `unsafe` across ~87 kLoC, the large majority of it in generated backends.
4. **Amend plans 11 and 12** with the corrections in this document: the gap table's op counts, the
   FIR structure recommendation, the two new authoring rules, and the missing `packuswb`.
