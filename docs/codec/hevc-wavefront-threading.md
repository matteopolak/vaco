# HEVC wavefront (WPP) threading — design, and current status

## What it is

`PERF-PROGRAMME.md` item B4: intra-picture parallelism for `vaco-codec-hevc`,
using the wavefront parallel processing (WPP) structure ITU-T H.265
§9.3.2.3/§7.3.1.1 already puts in the bitstream — one CTU row decoded per
worker, each row starting two CTUs behind the row above it. Controlled by
`-threads N`, opt-in, default off until proven the way H.264's own row
threading was (`docs/codec/frame-threading.md`).

**Status: design only. No code has changed.** This document is the
deliverable of this pass, per the plan's own instruction that "the executing
agent writes the design doc first, as `docs/codec/frame-threading.md` was
written for H.264." The reasoning below is grounded in this crate's actual
code (cited by file and function throughout), not a guess at its shape, and
identifies the specific correctness questions the next agent into this item
must answer before writing a line of the restructure — see "What is not yet
known" at the end.

## Why this is not H.264's row-level frame threading, reused

`docs/codec/frame-threading.md`'s "row-level frame threading" publishes rows
of picture *N* so picture *N + 1*'s own task can start consuming them early —
one writer per picture, publishing incrementally so a *different* picture's
task can read ahead. That is frame-level (cross-picture) pipeline
parallelism expressed at row granularity. It is real, tested, and it is what
`vaco_codec_core::picture`'s `PictureWriter`/`PictureRef`/`ProgressPicture`
already give any codec for free — `PictureWriter` is documented as "neither
`Sync` nor `Clone`: exactly one frame task holds it."

WPP is a different shape: **several rows of the *same* picture**, each
written by a different worker, each one row's worker also being a *reader*
of the row above's still-in-progress output. That is not a variant of
"one writer, many readers of a finished-or-finishing predecessor picture" —
it is "several concurrent writers of one picture, each also a reader of its
immediate neighbour." `vaco_codec_core::threading::SliceThreadedDecoder`
(`PictureWriter::split_bands_mut`) looks closer — "hand out disjoint band
ranges... to concurrent slice or tile jobs" — but its own doc is explicit
that "publication stays with the owning thread after the jobs join": nothing
a `split_bands_mut` job writes is visible to another job until *all* jobs
finish and the caller republishes serially afterward. That is the right
shape for genuinely independent tiles (which is what `vaco-codec-vp9`, the
only current user, has: independent tile columns with no cross-tile read
during decode). It is the wrong shape for WPP, whose whole point is that row
`r + 1` starts after row `r`'s *second* CTU, not after row `r` finishes.

**The reusable part, per D19, is not `FrameRunner` itself** (it is typed
over whole-picture `FrameTask`s) but the *pattern* `vaco_codec_core::picture`
already proves out: a band owned exclusively while being filled, then moved
into a `OnceLock` — release on write, acquire on read — so no type in the
API can observe a partially-written band, and a reader blocks on a
`Condvar` keyed to "rows published so far" rather than polling. The concrete
proposal below is to reuse `ProgressPicture`/`PictureWriter`/`PictureRef`
*as already written*, one per CTU row rather than one per picture: row
`r`'s worker holds a `PictureWriter` for a picture-shaped-but-one-CTU-row-tall
`ProgressPicture`, publishes through it via `publish_through` as its own CTU
loop advances, and row `r + 1`'s worker holds a `PictureRef` clone of row
`r`'s `ProgressPicture` and calls `wait_rows_for` on it exactly the way a
frame task waits on a reference picture today. This is literally the
existing mechanism, applied one level down — not a second one — and it costs
nothing new in `vaco-codec-core`. What is genuinely new is everything HEVC's
own decode has to learn to do with a *slice* of its state instead of the
whole thing, which is the rest of this document.

## What actually needs to become per-row

`ctu::Ctx` (`crates/codec/vaco-codec-hevc/src/ctu.rs`) is the single
`&mut` threaded through the entire CTU walk today, and it is a mix of three
very different things:

1. **Per-slice, read-only, already shareable as-is.** `log2_ctb_size`,
   `bit_depth_luma`/`_chroma`, `sign_data_hiding`, `constrained_intra_pred`,
   `cu_qp_delta_enabled`, `inter: Option<InterSliceParams<'p>>` (reference
   picture lists/weights — themselves already `Arc`-backed via the DPB), and
   every other slice-header-derived scalar. These can be `Copy`/`Clone` (or
   `Arc`) and hand out identically to every row worker; nothing here changes
   during the CTU walk.

2. **Genuinely per-row mutable state, already conceptually separated.**
   `qp_y_prev` already resets to `slice_qp` at the start of every CTB row
   when WPP is active (`decoder::decode_wpp_row_ranges`'s own comment: "a
   *different* per-row reset... §8.6.1's own rule"), and `qg_qp_pred`/
   `is_cu_qp_delta_coded`/`cu_qp_delta_val` are QG-lifetime state that never
   survives a CTU boundary in a way that would need cross-row visibility.
   The existing CABAC context handoff (`decode_wpp_row_ranges`: `if col == 1
   { saved_ctx = Some(ctx); }`, loaded by the *next* row) is exactly the
   per-row-to-per-row dependency a wavefront has to schedule, already
   isolated in one place, and already only two CTUs deep — the two-CTU lag a
   wavefront runs on is not something to invent, it is already the shape
   `decode_wpp_row_ranges` executes serially today.

3. **Whole-picture structures every CTU currently reaches through
   unconditionally**, which is the actual work:
   - `pic: &'p mut Picture` (`framebuf::Picture`/`Plane`, post-B2 `u8`
     storage, 4x4-block `ready` grid) — every reconstruction write and every
     intra/inter prediction read goes through this.
   - `cu_grid: CuGrid` — depth/mode/QP/motion/skip/cbf, at 4x4 granularity,
     read by intra MPM derivation, merge/AMVP spatial candidates, and
     `deblock`'s own `qP_P`/`qP_Q` lookups.
   - `edges: EdgeMarks` — per-4x4 vertical/horizontal edge-filter flags,
     written by every transform-unit leaf, read only by `deblock` afterward.
   - `sao_params: Vec<CtuSao>` — one entry per CTU, written as each CTU's
     `sao()` syntax is parsed, read back by a later CTU's own merge-left/
     merge-up (`sao::parse_ctu_sao`) and by `sao::filter_picture` afterward.

   Each of these needs to become "a band this row owns exclusively, plus
   read-only access to however many *already-published* rows above it the
   worst-case neighbour reach requires" — the same shape `ProgressPicture`
   already gives `Plane`, generalised to four structures instead of one.

### How far up does a row actually reach?

This is the number a wavefront's lag bound is built from, and it needs to be
exact, not a round number, per `frame-threading.md`'s own "each is exact
rather than conservative" — getting it too small corrupts output silently;
too large only costs parallelism. What this pass established by reading, not
by measuring:

- **Intra prediction** (`intra_pred::build_reference_line`, called from
  `ctu::reconstruct_luma`/`reconstruct_chroma`/`predict_chroma_only`) reads
  `y0 - 1` (one row of samples immediately above the current block) plus the
  above-right diagonal, out to `x0 + 2*size - 1` — i.e., strictly the row
  above the current CTU row, never further. The existing CTB-row-boundary
  MPM rule (`ctu.rs`'s per-PU mode loop forcing `DC_IDX` whenever `pu.y %
  ctb_size == 0`) already treats "the row above" as a hard availability
  boundary for the *mode*, which is a second, independent confirmation that
  nothing in this crate's intra path reaches two rows up.
- **Merge/AMVP spatial candidates** (`motion::derive_merge_candidates`,
  `CuGrid` neighbour queries) read the current CU's immediate above/
  above-left/above-right 4x4 neighbours — again, the row immediately above,
  never further, and `CuGrid::*_at` already returns the "unavailable"
  default for anything not yet written, so an under-published neighbour
  reads as absent rather than stale — which is *not* the same as reading a
  not-yet-final value, and is exactly the distinction a wavefront's lag
  bound has to get right (see "must be refused, not tolerated" below).
- **Deblocking is the one that is not "one row," and needs first-principles
  re-derivation, not an assumption**: `deblock::filter_picture`'s own
  comment says it runs "every vertical edge first (both planes), then every
  horizontal edge (both planes) — matching `TComLoopFilter::loopFilterPic`'s
  own two full, separate passes, since horizontal filtering must see
  vertical filtering's own output." That is a *global* two-pass ordering
  today (all vertical edges, picture-wide, before any horizontal edge,
  picture-wide), not a per-row lag the way H.264's single interleaved pass
  is. HEVC's strong filter reaches `p2`/`q2` (three samples each side of an
  edge), which bounds *how far* a horizontal edge at a CTU-row boundary can
  move samples into the row above or below, but it does **not** by itself
  prove that row `r`'s horizontal-edge pass is independent of row `r + 2`'s
  vertical-edge pass the way H.264's own one-macroblock-row lag was proven
  independent — that proof does not exist yet for this codec and has to be
  built the same way `frame-threading.md`'s was, against HM's own
  `TComLoopFilter.cpp`, before any row-lagged deblock schedule can be
  trusted. Until it exists, treat deblocking as the item's critical path,
  not a detail to fill in later.
- **SAO** already reads a picture-wide `Snapshot` of the *post-deblock*
  picture (`sao::Snapshot::capture`, `filter_picture`) specifically so a
  CTU's own edge-offset computation never reads a neighbour this same pass
  might also be about to rewrite. A wavefront needs the same guarantee at
  row granularity instead of picture granularity: row `r`'s SAO pass may
  read row `r - 1` and `r + 1`'s *already-deblocked* samples (its own
  `Eo` classes reach one row either side, per `sao.rs`'s own edge-offset
  neighbour deltas) but must not depend on those neighbours' *own* SAO
  output, which the existing whole-picture `Snapshot` already enforces by
  construction and a row-wise version has to preserve.

## Proposed staging (unchanged from the plan; restated against what is above)

**Stage 1 — serial restructure, gated on ≤1.03x, no threads yet.** Replace
`Picture`/`CuGrid`/`EdgeMarks`/`sao_params` with per-CTU-row-band structures
(one `ProgressPicture`-per-plane-per-row for `Picture`, an equivalent banded
shape for the other three — they are smaller and simpler than a pixel plane,
so a hand-rolled `OnceLock`-per-row-band following the exact same publish
discipline is likely simpler than forcing them through `vaco_codec_core::
picture`'s pixel-shaped API), still driven by one worker, one row at a time,
in the same order as today. This is the item's own gate: if turning four
whole-picture structures into per-row bands costs more than 3% serially —
plausible, since every read gains at least one more indirection and a
row-boundary check that used to be a flat array index — **stop and report
that number**, per the plan's own "restructured, measured, no faster,
reverted" allowance (D20) and this task's own explicit sanction of that as a
complete outcome.

**Stage 2 — thread the row loop**, once Stage 1 is byte-exact and within
budget. Each row's worker: waits on the row above's `PictureRef`-equivalent
for CABAC context (already a two-CTU-deep dependency, already isolated) and
for the pixel/`CuGrid`/`EdgeMarks` neighbour rows intra and merge/AMVP need;
runs its own CTU loop exactly as `decode_wpp_row_ranges` does today, one
worker's row at a time; publishes its own bands as it advances. Deblocking
and SAO become their own row-lagged passes once reconstruction publishes
correctly — their own exact lag is Stage 1's hardest open question, not
Stage 2's, since Stage 1 already has to decide it to represent state
per-row at all.

**Verification, unchanged from the plan and this item's own brief**:
byte-exact against plain `libx265` (no `-x265-params`) at every thread count
1/2/4/8/16, across 322x242, 300x500 (partial CTU row *and* column —
`ctbs_y`'s own last, short row is exactly where an off-by-one in a
row-band's height shows up), 416x240, 640x480, 854x480, 1920x1080 and
3840x2160; `tests/oracle.rs::dense_content_is_byte_exact` and
`tests/flat.rs` unchanged; a **new** `hevc_decode_threaded` fuzz target
(decode at 1 and at N threads, assert identical — `vaco-codec-h264`'s own
`h264_decode_threaded` is the template) run for minutes, not seconds, before
any default changes; a long real clip repeated 40+ times per thread count
(H.264's own pass used 47-50 runs of a 1800-frame file) — a race that
reproduces one run in twenty needs at least that many attempts to be seen at
all. **A read past what was waited for must be refused, not served** —
whatever bound Stage 1 picks for "how far a row's worker may read into the
row above," making it a hard error on violation (the way `PictureRef::
wait_rows_for` already refuses a cyclic wait) rather than "safe because
generous" is what turns a wrong bound into a loud failure instead of
content-dependent pixel corruption. Default stays `-threads 1` (off) until
all of the above passes, exactly as H.264's own rollout did — a separate
commit flips the default only once byte-exactness at every thread count is
proven, not designed.

## What is not yet known, and has to be answered before Stage 1 is written

- **The exact row-lag bound for deblocking's two-pass (vertical-then-
  horizontal) structure.** This is the one piece of the "how far up" section
  above that is a re-derivation, not a citation of existing code, and it
  gates whether deblocking can be made row-wavefront-safe at all without a
  wider change to `deblock.rs`'s own two-full-passes shape. Do this the way
  `frame-threading.md`'s own three boundary conditions were pinned: against
  HM 18.0's `TComLoopFilter.cpp` (Tier A, BSD-3-Clause — already this
  crate's own clean-room precedent), not by assumption, and write the proof
  down before trusting a schedule built on it.
- **Whether representing `CuGrid`/`EdgeMarks`/`sao_params` as per-row bands
  is cheaper hand-rolled (each is a handful of small arrays indexed at 4x4
  or CTU granularity, not a pixel plane) than forcing them through
  `vaco_codec_core::picture`'s API, which is sized for byte-stride pixel
  planes.** The pattern (owned-while-filling, moved into a `OnceLock`,
  published, waited on) is worth reusing regardless (D19); the concrete
  types are not obviously the same ones `Picture` should reuse.
- **Whether `vaco-codec-core::picture` needs any new capability at all**,
  or whether "one `ProgressPicture` per CTU row" as sketched above is
  sufficient — this pass believes the latter (no `vaco-codec-core` change),
  but has not written the code to confirm it, and this crate's own lane does
  not extend to changing shared threading infrastructure without that
  confirmation.

## Why this pass stops here

`vaco-codec-hevc` has had concurrent editors for this entire session (see
`planning/E2E-GAPS.md` §24's own account of a collision during item B1, and
the `VACO_HEVC_TRACE` debug instrumentation found live in `ctu.rs` during
B3's work). The restructure this item needs touches `framebuf.rs`, `ctu.rs`,
`deblock.rs`, `sao.rs` and `decoder.rs` at once — effectively the whole
crate — for a byte-exact video decoder, gated on a deblocking-lag proof that
does not exist yet. Landing a partial version of that restructure, unverified
at every one of the seven required fixture sizes and five thread counts,
into a crate under active concurrent editing is a materially different risk
than any of B1-B3's changes, each of which was one function's own data
shape, byte-exact-checked within the same session it was written. This
document is what the plan itself asks for first; the restructure it
describes is sized in the plan as XL (3-4 weeks) for a reason, and starting
it without the deblocking-lag proof above would be building on a foundation
this pass could not itself verify.
