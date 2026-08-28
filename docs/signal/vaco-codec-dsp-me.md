# `vaco-codec-dsp-me` — motion-estimation search patterns

---

## 1. What it is

D-13 (#260): given a block in the current frame and a candidate reference
plane, find the displacement (a [`Displacement`]) that minimises a
[`vaco-codec-dsp-mecmp`](vaco-codec-dsp-mecmp.md) (D-12) cost. Three
patterns, all on [`Searcher`]:

| Pattern | Cost | Role |
|---|---|---|
| `full_search` | O(range²) | ground truth — what the other two are measured against |
| `diamond_search` | small multiple of the descent path length | the general-purpose fast search |
| `three_step_search` | O(log range) rounds of 8 | cheaper, coarser, and — see §2.2 — provably not always optimal |

None of the three is normative. An encoder may pick any displacement it
likes; there is no bitstream syntax any of these numbers ever land in
directly (an encoder writes the *vector*, and the cost was only how it got
chosen). Judged on: does it improve on where it started, and how close does
it get to what full search finds.

## 2. How it works

### 2.1 `BlockOrigin`, `candidate_cost`, and where bounds-checking happens

Every pattern reduces to the same primitive: `Searcher::candidate_cost`
converts a `(BlockOrigin, Displacement)` pair into two
`vaco_codec_dsp_mecmp::Plane` sub-views (via `Plane::sub`, which is `None`
on any out-of-bounds or overflowing offset) and runs the configured
`Metric`'s cost function on them. Every one of the three patterns' inner
loops calls `Searcher::consider`, which wraps `candidate_cost` and updates
a running best — so bounds-checking, cost dispatch and the tie-break rule
(prefer the smaller-magnitude vector on an exact cost tie, since it is
cheaper to code) live in exactly one place.

### 2.2 Diamond search vs. three-step search — a real, not cosmetic, difference

Diamond search (LDSP/SDSP: Tham/Ranganath 1998) repeatedly probes an
8-point radius-2 diamond around the current best and recentres on any
improvement, finishing with a 4-point radius-1 diamond. Because it moves in
small steps and recentres every round, it reliably descends to the true
optimum on a convex cost surface from an arbitrary start.

Three-step search (Koga et al. 1981) instead probes a 3×3 grid at a
*halving power-of-two* step size. This is measurably faster (see the
`divan` numbers below) but has a known, real failure mode: once a round
commits to one coarse grid point over another, a true optimum that does not
lie near any point that coarse grid ever visits can become unreachable —
finer steps later only refine *around* the committed point, they do not
revisit the alternative. `tests::three_step_search_finds_shifts_that_land_on_its_coarse_grid`
demonstrates this is real, not a bug in this crate's implementation: TSS
finds shifts aligned to its own grid exactly, and
`tests::full_search_and_diamond_search_find_the_true_shift` shows the same
fixture shape defeats TSS at an off-grid shift ((1, −7) under range 8)
while diamond search still finds it. This is why D-13 ships as a family of
patterns rather than one: pick diamond for correctness-sensitive work, TSS
when its speed and known limitation are an acceptable trade.

### 2.3 The test fixtures: why a quadratic bowl, not a random or linear texture

Every correctness test builds a reference plane as an anisotropic quadratic
bowl (`255 - (3·dx² + 5·dy²)/40` around the plane's centre) rather than
noise or a linear ramp. Two earlier attempts failed for instructive
reasons, kept here because the failure mode recurs:

- **High-frequency/chaotic texture** (`(x*37 + y*91 + (x^y)*13) % 256`)
  produced a cost surface with no exploitable local gradient near the true
  shift, so diamond search got stuck in a spurious local minimum a few
  steps from the start — not because diamond search is broken, but because
  real video content has spatial correlation this texture does not.
- **A linear ramp / L1 pyramid** is locally a tilted plane in each quadrant,
  which is constant along the direction *orthogonal* to its gradient — so
  many distinct vectors tie at the same minimal cost, and a search "finding
  a zero-cost vector" that is not the *intended* true shift proves nothing.

The quadratic bowl has curvature in both axes (no flat direction to
plateau along) and unequal `3`/`5` axis weights (no circular symmetry to
produce accidental ties), which is what makes "the search recovered
*exactly* the true shift" a meaningful assertion rather than a coincidence
of degenerate input — see `AGENT-CONSTRAINTS.md`'s point about an input
that cannot separate two candidate answers.

## 3. How to change it

- **A new search pattern**: add a method on `Searcher` that calls
  `consider` for each candidate it wants to try, in whatever order/shape
  the pattern needs; reuse `Displacement::within` to respect
  `SearchConfig::range`.
- **Sub-pixel refinement**: out of scope for this pass. `vaco-codec-dsp-mc`
  (D-08a, separable FIR) is the interpolation primitive a half/quarter-pel
  refinement stage would build on; it is not wired in here.
- **A lambda-weighted cost** (rate-distortion bias toward cheaper-to-code
  vectors, beyond the exact-tie rule `consider` already applies): would
  replace `consider`'s comparison with `cost + lambda * mv_bits(mv)`; not
  implemented, since D-14 (rate control) is where a lambda would come from
  and the two were assigned to be built independently.
- **A predictor-seeded search**: every method already takes a `start`
  vector rather than assuming `(0, 0)` — a caller (the VP8/VP9 encoders
  this crate unblocks) supplies a neighbour's MV or zero as appropriate;
  nothing here needs to change to support that.

## 4. Configuration

None — no env vars or feature flags. `SearchConfig` is the per-call
parameterisation (`metric`, `range`).

## 5. Dependencies

`vaco-core`, `vaco-simd` (only for the `KernelSet` trait, to call
`MecmpKernels::select`), `vaco-codec-dsp-mecmp` (the cost functions and
`Plane`). No `provenance/` table: diamond search and three-step search are
named, general algorithms from the motion-estimation literature (see §2.2's
citations), not transcribed from any specification or reference
implementation.

## Measured (this machine, `cargo bench -p vaco-codec-dsp-me`)

8×8 block, ±16 range, SAD metric:

| Pattern | Mean | vs. full search |
|---|---|---|
| `full_search` | 20.4 µs | 1× (reference) |
| `diamond_search` | 733 ns | ~28× faster |
| `three_step_search` | 604 ns | ~34× faster |

Ratios, not verdicts, per `AGENT-CONSTRAINTS.md`'s performance section —
re-run `cargo bench -p vaco-codec-dsp-me` on the machine that matters
before relying on these numbers.
