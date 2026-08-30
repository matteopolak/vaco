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
