# HEVC wavefront (WPP) threading — design, and current status

## What it is

`PERF-PROGRAMME.md` item B4: intra-picture parallelism for `vaco-codec-hevc`,
using the wavefront parallel processing (WPP) structure ITU-T H.265
§9.3.2.3/§7.3.1.1 already puts in the bitstream — one CTU row decoded per
worker, each row starting two CTUs behind the row above it. Controlled by
`-threads N`, opt-in, default off until proven the way H.264's own row
threading was (`docs/codec/frame-threading.md`).

**Status: Stage 1 in progress, single-threaded — `-threads N` does not exist
yet.** This document was first written per the plan's own instruction that
"the executing agent writes the design doc first, as `docs/codec/
frame-threading.md` was written for H.264," then corrected by a later pass
after finding that its own central technical claim — reusing
`vaco_codec_core::picture::ProgressPicture` "one level down" — does not
survive reading that module's actual implementation (see "Correction:
chaining `ProgressPicture` per row does not give WPP its parallelism"
below). `vaco-codec-core` then gained the per-CTU-tile capability path 1
actually needs, and `vaco-codec-hevc`'s own `framebuf::Plane` now
reconstructs through it (row-banded, not yet column-tiled — see "Concrete
Stage 1 plan" below for exactly what has landed and what has not). The
reasoning throughout is grounded in this crate's (and `vaco-codec-core`'s)
actual code, cited by file and function, not a guess at its shape.

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
`Condvar` keyed to "rows published so far" rather than polling. An earlier
version of this document proposed reusing `ProgressPicture`/`PictureWriter`/
`PictureRef` *as already written*, one instance per CTU row instead of one
per picture, with `publish_through` called as each row's own CTU loop
advances. **That proposal does not actually give WPP its parallelism — see
the correction immediately below.** What is genuinely reusable is only the
*pattern* (own-while-filling, then move into something read-only and
waited-on) — not the concrete `ProgressPicture` type, whose own publish axis
turns out to run the wrong way for this problem.

## Correction: chaining `ProgressPicture` per row does not give WPP its parallelism

This is a correction to this document's own earlier reasoning, found by
reading `vaco_codec_core::picture`'s actual band/publish implementation
(`crates/signal/vaco-codec-core/src/picture.rs`) rather than its module
doc's gloss, before writing any code against it — the same discipline this
item's own brief asked for on the deblocking question above, applied here
because the premise turned out to need it too.

**`ProgressPicture` publishes progress along exactly one axis: height, over
the plane's *full* width, every time.** `PlaneSpec` (`picture.rs:64`) is
`width_bytes`/`height`/`stride` — a literal 2-D byte buffer. A "band" is
`band_h` rows of that *full width* (`ProgressPicture::allocate`,
`picture.rs:273`, allocates each band as `body_rows * stride` bytes).
`ProgressPlane::band_of` (`picture.rs:218`) maps a row number to a band
index — rows only, never a column. `PictureWriter::publish_through`
(`picture.rs:538`) publishes bands `0..=k` and then does exactly one thing
to make them visible: `geom.ready.store(rows, Ordering::Release)` — `rows`
is a *row count*, the same `u32` `PictureRef::ready_rows`/`wait_rows_for`
read back. There is no way, at any `band_h`, to publish "the left half of
row `y`" while the right half is still being written: a band's own
allocation is `stride` bytes wide by construction, and `publish_through`
moves whole bands into `OnceLock`s one release-store at a time. This is
exactly right for cross-picture pipelining (`docs/codec/
frame-threading.md`): picture *N* is decoded in strict raster order, so by
the time it has processed CTU row `r`'s rightmost CTU, row `r` genuinely is
100% complete across its full width, and "rows published so far" is a
correct, useful signal for picture *N + 1*'s task to wait on.

**WPP's real dependency does not run along that axis.** Row `r + 1`'s
worker needs row `r`'s CTU `c` (directly above) and CTU `c + 1`
(above-right) to be done *while row `r`'s own worker is still only two CTUs
into that row* — i.e., while the overwhelming majority of row `r`'s width
is still unwritten. That is a dependency on *partial width at a fixed
height*, the transpose of what `ProgressPicture` tracks. Chaining one
`ProgressPicture` per CTU row the way this document previously proposed —
row `r + 1`'s worker holding a `PictureRef` to row `r`'s `ProgressPicture`
and calling `wait_rows_for` — cannot express "wait for CTU column `c + 1`
only": the *only* signal available is "how many of this one-CTU-row-tall
picture's own (height-wise) rows are done", which for a normal raster CTU
write only becomes true across the *full* row-picture width once **every**
CTU in that row has been written. Row `r + 1` would therefore have to wait
for row `r` to finish *entirely* before starting at all — at which point
row `r`'s own worker has nothing left to do in that row, so at most one
row's worth of reconstruction is ever in flight at once. That is not a
smaller version of WPP's 2-CTU-lag wavefront; it is zero overlap between
adjacent rows' reconstruction, dressed up in wavefront-shaped machinery.
(One real, narrower thing *does* fall out of full-row publish for free —
see "A real, smaller alternative" below.)

**A tile-grid workaround exists, but it is not a small change.** `stride`
and `width_bytes` do not have to mean "one real image row" — nothing stops
treating one flattened, whole CTU (a `ctb_size * ctb_size`-byte blob) as one
"row" of a transposed, one-real-CTU-row-tall `ProgressPicture` (`band_h =
1`, `width_bytes = stride = ctb_size²`), publishing band `c` the instant CTU
`c` finishes. That would give real per-CTU publish granularity along the
correct axis. But it requires every reconstruction write and every
neighbour read in `ctu.rs` (intra reference-line construction, MC
edge-extension, deblocking, SAO) to address samples as *(tile index, local
offset within that flattened tile)* instead of the single contiguous,
globally-addressed, full-width-row `Plane` every one of those call sites
assumes today (`Plane::row`/`row_mut` return one contiguous full-width
slice — B1/B2's whole design). A reference read that crosses a CTU boundary
(the common case: "above-right" reaches into the next CTU column; deblocking
and SAO read across a CTU's own left/right/top/bottom edge by definition)
would need to look up a *different* tile, wait on its own publish flag, and
compute a fresh local offset, at every such boundary. This is not "the same
mechanism, one level down" (D19's cheap reuse); it is a different memory
layout for the crate's single hottest data structure, touching effectively
every read/write site B1–B3 already tuned. Whether it is worth building is
a real question, not a foregone one — see "Two honest paths forward" below.

**A real, smaller alternative falls out of `ProgressPicture` exactly as it
is.** Reconstruction (one worker, serial, exactly as today) can publish
each finished CTU row as one `ProgressPicture` band, full width, the moment
that row's last CTU is written — this is the *correct* use of the
primitive, needing no new capability and no coordinate rewrite, because
reconstruction genuinely does complete a row's full width before moving on.
A second worker running deblocking + SAO can then trail it by one row,
reading via `wait_rows_for` (`DEFAULT_GUARD = 8` already covers deblocking's
now-measured ≤3-sample reach and SAO's ≤1-row edge-offset reach with margin
to spare) while reconstruction moves on to the next row. That is a genuine,
two-stage pipeline — reconstruction and deblock+SAO overlapping across
rows — bounded at roughly 2x by Amdahl's law regardless of thread count
(a third or fourth thread has no further stage to run), so it does **not**
meet this item's own verification bar (byte-exact scaling checked at every
thread count 1/2/4/8/16 — a bound this design cannot even in principle
exceed past 2 effective workers). It is a different, smaller feature than
"WPP," not a scoped-down version of it, and would need its own name and its
own (much smaller) sizing if pursued instead.

### Two honest paths forward

1. **Build true WPP**, which needs the tile-grid mechanism above (or an
   equivalent new capability, possibly belonging in `vaco-codec-core`
   rather than hand-rolled here, since "publish a fixed-size tile,
   addressed by 2-D grid position, read by any later tile" is a shape other
   codecs with tile/superblock structure — VP9, AV1 — could plausibly want
   too). This is the item as named and as sized in the plan, but the sizing
   (XL, 3–4 weeks) was set believing no new capability was needed; that
   belief is now known to be false, and the real cost is higher than
   previously stated.
2. **Build the two-stage reconstruction/deblock+SAO pipeline instead**,
   using `ProgressPicture` exactly as it exists, at low implementation risk
   and low cost (no coordinate rewrite, no new primitive), for a bounded
   real speedup well under what N-way WPP promises.

This document does not pick between them — that is a plan-level call about
what this item is actually asking for, not something to decide unilaterally
mid-implementation. Whoever picks this item back up should get an explicit
answer to that question before writing Stage 1 code either way, the same
way the deblocking-lag question needed answering before this document could
be trusted at all.

**Decided: path 1.** HEVC has no threading of any kind today (26.5x behind
default-threaded `ffmpeg`, most of that spread attributable to threading
alone) and path 2 caps near 2x regardless of thread count — building it
would not clear the item's own 1/2/4/8/16-thread verification bar, so it
would be a different, smaller feature built instead of this one, not a step
toward it. `vaco-codec-core::picture` gained the per-CTU-tile publish
capability path 1 needs
(`crates/signal/vaco-codec-core/src/picture.rs`, commit `0af678e`):
`PlaneSpec::with_bands(band_w, band_h)` splits a plane into a 2-D grid of
independently publish-and-wait-able tiles instead of one column of
full-width row bands, using the identical own-while-filling-then-move-into-
`OnceLock` discipline the row axis already had —
`PictureWriter::tile_mut`/`publish_tile` and `PictureRef::wait_tile`/
`wait_tile_for`/`try_tile`/`ready_cols` are the tile-addressed counterparts
of `band_mut`/`publish_through`/`wait_rows`/`wait_rows_for`/`try_rows`/
`ready_rows`. `PictureSpec::new` without `PlaneSpec::with_bands` reproduces
every existing row-banded caller's behaviour exactly, byte for byte — all
15 pre-existing `picture.rs` tests plus `vaco-codec-h264`/`vp8`/`vp9`'s full
suites pass unchanged, and 11 new tests (26 total) cover the tile axis
itself, including one that proves the actual point directly: a "row 1"
worker reading through `wait_tile_for` can proceed past its own first tile
while a "row 0" writer still has three-quarters of its own row left to
publish — the schedule a row-banded plane cannot express at any band
height, and the reason this document's earlier premise (chaining
`ProgressPicture` per row) was wrong.

**What this does not yet solve, found while building it — the read-side
split.** `vaco_codec_core::picture::PlaneView::row`/`block` promise one
contiguous borrow per row; that promise cannot survive a plane whose rows
are split across independently-allocated column tiles, so column-banded
planes read through `BlockRef`-per-tile instead, and `PlaneView::block`
itself now refuses a column-banded plane outright (see the crate's own
module doc for why: a `&mut` a writer thread still holds over anything, even
a disjoint sub-region of a shared allocation, cannot coexist with any other
thread's `&` into that same allocation under Rust's aliasing rules without
`unsafe` — this is not a design preference, it is why `Band`/tiles are
separate heap allocations moved by ownership transfer rather than slices of
one shared buffer). This has a direct, unavoidable consequence for HEVC's
own `framebuf::Plane`: `Plane::row`/`row_mut` currently return one
contiguous full-width slice — the exact shape B1/B2 tuned every hot copy
loop in `ctu.rs`/`deblock.rs`/`sao.rs`/`decoder.rs` around — and that shape
cannot survive the move to tile storage either, for the identical reason.
Stage 1 is therefore not "swap `Plane`'s internal `Vec<u8>` for something
tile-shaped behind the same `get`/`set`/`row`/`row_mut` API" (B2's own
template): it is "give up the contiguous-row read/write API at every call
site that currently uses it, in favour of a tile-addressed one," which is a
larger, more invasive change than B2 was, in the crate's single hottest
data structure.

**Concrete Stage 1 plan, now that the primitive exists:**

1. ~~`framebuf::Plane` gains a `PictureWriter`... every *write* call site
   maps onto exactly one `tile_mut`.~~ **Landed** (`ReconPlane`/
   `ReconPicture`, commit `1ba192d`) — with one correction found in the
   doing: Stage 1 uses the *row-banded* 1-D API
   (`band_mut`/`publish_through`/`band_ref`, `PlaneSpec::with_bands` never
   called) rather than the 2-D per-CTU tile grid this step originally
   sketched. Column tiling is what Stage 2's real wavefront overlap needs
   (see the correction earlier in this document — full-width row bands
   cannot express "row r+1 starts after row r's second CTU"), but Stage 1
   is still single-threaded, so there is no overlap to enable yet and no
   reason to pay a cost only Stage 2 needs. Staying row-banded also kept
   `row_mut` returning one contiguous, full-width slice exactly as
   `Plane::row_mut` always did (a full-width row never spans more than one
   row band either way), which is the whole reason B1/B2's row-wise copy
   loops in `write_pred_block`/`write_block` needed zero changes, only the
   type they write through.
2. ~~Every *read* call site... splits into two shapes...~~ **Landed as
   part of the same commit**, not deferred: `ReconPlane::get`/`is_ready`
   dispatch on whether the requested row is the one currently being
   written (`band_ref`, no publish/wait — the row-banded shape of "reading
   the CTU currently being written") or an earlier, already-published one
   (`PictureRef::try_rows`). This could not be deferred to its own step:
   same-CU intra reference-line reads need both cases *today*, correctly,
   for anything to byte-exact-check at all. What is genuinely still ahead
   here is moving from `band_ref`/`try_rows` to `tile_ref`/`wait_tile` once
   Stage 2 needs column granularity — a different primitive call, not a
   different shape of dispatch.
3. `CuGrid`/`EdgeMarks`/`sao_params` need the equivalent treatment at their
   own (finer, 4x4 or per-CTU) granularity. **In progress: `EdgeMarks`
   landed, `CuGrid`/`sao_params` not started.**

   The "deciding in the doing" this bullet used to defer resolved simpler
   than expected, and in a way that applies to all three structures, not
   just `EdgeMarks`: unlike `ReconPlane`, nothing ever needs *partial,
   sub-row* visibility into one of these three structures' still-being-
   written row. Every same-row read targets an already-decoded (hence
   already-written) earlier position in z-scan/raster order, and every
   cross-row read targets a neighbour row that WPP's own 2-CTU CABAC-
   context lag (and the 1-CTU-row deblock lag this doc's own "How far up
   does a row actually reach?" section measured) guarantees is *already
   fully finished* by the time anything reads it — never a row still being
   filled by another thread. `ReconPlane` needed `vaco_codec_core::picture`
   specifically because deblocking and intra reference-line reads need to
   see a row still in progress, sample by sample; these three structures
   never do, so a coarser once-per-row-band freeze is enough, and a small
   hand-rolled "current owned/mutable band, `Vec` of frozen `published`
   bands" type (mirroring `ReconPlane`'s own `current`/`try_rows` split,
   sized in blocks rather than pixels) is simpler than routing four bools'
   worth of per-4x4-block flags through a pixel-plane-shaped API built for
   byte samples. `EdgeMarks` (`crates/codec/vaco-codec-hevc/src/framebuf.rs`)
   is the first of the three built this way: `EdgeBand` bundles its four
   `Vec<bool>` grids per CTU row, `EdgeMarks::begin_row`/`finish` mirror
   `ReconPlane::begin_row`/`finish` exactly (including advancing
   `current_band` one past the last real band on `finish`, for the same
   reason — a stale, freshly-emptied `current` must not still answer reads
   for the row `finish` just moved into `published`), and every
   `mark_*`/`*_at` method keeps its exact existing signature, so every call
   site in `ctu.rs`/`deblock.rs` needed zero changes beyond `EdgeMarks::new`
   gaining a `ctb_size` parameter. `CuGrid` and `sao_params` are next, in
   that order (per the standing increasing-complexity/lowest-risk-first
   staging) — `CuGrid`'s nine heterogeneous typed arrays are the largest
   remaining piece of step 3.
4. Gate exactly as originally planned: byte-exact against every fixture
   this item's brief names, single-threaded, before touching `Stage 2`'s
   actual thread dispatch; ≤1.03x versus the pre-Stage-1 baseline or stop
   and report the number (D20). **Gate cleared** for steps 1-2's own
   scope: byte-exact against real `libx265` I/P/B-slice content (a fully
   stock 25-frame GOP, a 40-frame deep hierarchical-B GOP with weighted
   bi-prediction, 300x500's partial CTU row and column, and both the WPP
   and non-WPP CABAC paths), plus the existing `tests/oracle.rs`/
   `tests/flat.rs`; serial cost measured at 0.998x mean / 0.992x median
   over 10 interleaved, CPU-time-measured rounds (a private-worktree
   baseline against the working tree, both release builds) — see
   `planning/E2E-GAPS.md` §34 for the full methodology and per-round
   numbers. `CuGrid`/`EdgeMarks`/`sao_params` (step 3) have not been
   measured and are not covered by this gate.

Steps 1 and 2 are done; step 3 (`EdgeMarks` landed, `CuGrid`/`sao_params`
still ahead) and step 4's own gate for that step's own scope are what the
next pass into this item starts with.

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

   Each of these needs to become "a unit this row owns exclusively while
   filling, plus read-only access to however much of the row above it the
   worst-case neighbour reach requires, published at CTU granularity rather
   than full-row granularity" — the *pattern* `ProgressPicture` proves out
   (own-while-filling, then move into something read-only and waited-on),
   but not literally `ProgressPicture` itself; see the correction above for
   why its own publish axis does not fit this shape unmodified.

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
- **Deblocking's row-lag is now measured, not assumed: one CTU row each
  side, the same shape as H.264's own one-macroblock-row lag.**
  `deblock::filter_picture`'s own comment says it runs "every vertical edge
  first (both planes), then every horizontal edge (both planes) — matching
  `TComLoopFilter::loopFilterPic`'s own two full, separate passes, since
  horizontal filtering must see vertical filtering's own output" — a
  *global* two-pass ordering that reads as if it could reach arbitrarily
  far. It doesn't. `decoder.rs`'s `deblock_lag_tests` module (added to pin
  this down) corrupts every sample more than `lag` CTU rows away from a
  target row, one direction at a time, re-runs `deblock::filter_picture`,
  and diffs the target row's own output against a pristine reference,
  across CTU rows 1, 2 and 3 of a 4x5-CTU `libx265` fixture
  (`tests/fixtures/deblock_lag_256x320.hevc`). At every interior row, in
  both directions: `lag = 0` does **not** match (the immediately adjacent
  CTU row's own pixels do move the target row's output — the filter is not
  vacuous) and `lag = 1` **does** match (nothing two or more CTU rows away
  moves it). Cross-checked against clause 8.7.2 itself: boundary-strength
  decisions are derived once from already-decoded CU/edge data before
  either pass runs (not recomputed mid-pass from filtered samples), and
  per-edge sample modification reaches at most three samples (`p2..p0`/
  `q0..q2`) across an 8-sample-grid edge — nothing in the two-pass
  structure gives that reach a way to propagate past one adjacent CTU row.
  This clears Stage 1's deblocking question: a row-wavefront schedule can
  treat deblocking the same as intra/merge-AMVP above — each row waits on
  its own CTU row plus one neighbour on each side — without needing a
  separate whole-picture post-pass.
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

## Proposed staging (path 1, true WPP — see "Two honest paths forward" above)

The staging below is unchanged from the earlier version of this document
*in shape*, but its Stage 1 now explicitly means "per-CTU-tile bands," not
"per-CTU-row `ProgressPicture`s" — the correction above changes what has to
be built, not whether staging it serial-then-threaded is still the right
order to build it in. If path 2 (the two-stage pipeline) is chosen instead,
this staging does not apply; that path is small enough not to need it.

**Stage 1 — serial restructure, gated on ≤1.03x, no threads yet.** Replace
`Picture`/`CuGrid`/`EdgeMarks`/`sao_params` with structures banded at CTU
(not CTU-row) granularity — a 2-D grid of per-CTU units, each owned
exclusively while that CTU is being reconstructed, then moved into
something read-only and waited-on (a hand-rolled `OnceLock`-per-tile grid,
or a new `vaco-codec-core` capability if one gets built — see path 1's own
note above about whether this belongs there instead) — still driven by one
worker, one row at a time, in the same order as today, with every
cross-tile neighbour read (the common case for intra reference lines,
deblocking and SAO, all of which read across CTU boundaries) going through
the new lookup instead of a flat array index. This is the item's own gate:
if this costs more than 3% serially — very plausible now that "the same
shape `ProgressPicture` already gives `Plane`" turned out not to be
available for free — **stop and report that number**, per the plan's own
"restructured, measured, no faster, reverted" allowance (D20) and this
task's own explicit sanction of that as a complete outcome.

**Stage 2 — thread the row loop**, once Stage 1 is byte-exact and within
budget. Each row's worker: waits on the row above's own per-CTU-tile
publish for CABAC context (already a two-CTU-deep dependency, already
isolated) and for the pixel/`CuGrid`/`EdgeMarks` neighbour tiles intra and
merge/AMVP need; runs its own CTU loop exactly as `decode_wpp_row_ranges`
does today, one worker's row at a time; publishes its own tiles as it
advances. Deblocking and SAO become their own row-lagged passes once
reconstruction publishes correctly, at the now-measured one-CTU-row lag
from "How far up does a row actually reach?" above.

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

- ~~The exact row-lag bound for deblocking's two-pass (vertical-then-
  horizontal) structure.~~ **Resolved**: one CTU row each side, measured
  empirically and cross-checked against clause 8.7.2 — see "How far up does
  a row actually reach?" above and `decoder.rs`'s `deblock_lag_tests`
  module (`deblocking_depends_on_exactly_one_ctu_row_each_side`,
  `deblocking_bound_holds_at_every_interior_row`). Stage 1 can proceed on a
  row-wavefront deblocking schedule; it does not need a whole-picture
  post-pass.
- ~~Whether representing `CuGrid`/`EdgeMarks`/`sao_params` as per-row bands
  is cheaper hand-rolled... than forcing them through `vaco_codec_core::
  picture`'s API~~ **Superseded**: per the correction above, none of the
  four structures (`Picture` included) can use `vaco_codec_core::picture`'s
  API unmodified at all — its own publish axis is the wrong one for WPP's
  real dependency shape — so this is no longer "which API is cheaper," it
  is "hand-rolled per-CTU-tile grid, in this crate, or a new capability
  in `vaco-codec-core`."
- ~~Whether `vaco-codec-core::picture` needs any new capability at all, or
  whether "one `ProgressPicture` per CTU row" as sketched above is
  sufficient~~ **Resolved, and built**: it needed a new capability —
  `PlaneSpec::with_bands`, `PictureWriter::tile_mut`/`publish_tile`,
  `PictureRef::wait_tile`/`wait_tile_for`/`try_tile`/`ready_cols`
  (`vaco-codec-core` commit `0af678e`) — and it now exists, additively,
  with every pre-existing row-banded caller (`vaco-codec-h264`/`vp8`/`vp9`)
  unaffected. Path 1 was chosen over path 2 (see "Two honest paths
  forward" above): HEVC has no threading at all today and path 2's ~2x
  ceiling would not clear this item's own thread-count verification bar.
- **Whether the tile-addressed read path — every `ctu.rs`/`deblock.rs`/
  `sao.rs` call site that currently reads a contiguous full-width `Plane`
  row and now has to determine which tile a coordinate falls in and look it
  up, sometimes across a not-yet-guaranteed-published neighbour — costs
  more than Stage 1's own 3% serial gate.** This is genuinely unmeasured:
  building the primitive answered "can this exist at all in safe Rust"
  (yes), not "is it fast enough once wired into the hottest data structure
  in a byte-exact decoder." See "Concrete Stage 1 plan" above for exactly
  what has to be rewritten to find out.

## Where this stands

`vaco-codec-hevc` has had concurrent editors for this entire session (see
`planning/E2E-GAPS.md` §24's own account of a collision during item B1, and
the `VACO_HEVC_TRACE` debug instrumentation found live in `ctu.rs` during
B3's work) — the reason each of this item's own increments lands and is
verified separately rather than as one large, hard-to-bisect drop.

All three prior blockers are resolved: the deblocking-lag proof (one CTU
row each side, `planning/E2E-GAPS.md` §31), the mechanism question (this
document's own earlier proposal to chain `ProgressPicture` per row did not
give WPP real parallelism; `vaco-codec-core` now has the per-CTU-tile
capability that does, `planning/E2E-GAPS.md` §33 and commit `0af678e`), and
Stage 1's own first landable piece — `framebuf::Plane` reconstructing
through a row-banded `PictureWriter` instead of a flat `Vec<u8>`, byte-exact
across real I/P/B-slice `libx265` content and measured at 0.998x mean
serial cost, `planning/E2E-GAPS.md` §34 and commit `1ba192d`. "Concrete
Stage 1 plan" above records the one correction found landing it: Stage 1
uses the existing row-banded API, not the 2-D per-CTU tile grid this
document originally sketched for it, since single-threaded Stage 1 has no
overlap to enable and no reason to pay the tile grid's cost before Stage 2
actually needs it.

Stage 1 step 3 is also done: `EdgeMarks`, `CuGrid` and `sao_params` all
carry the row-banded current/published split (`planning/E2E-GAPS.md`
§§36-38, commits landing "row-band EdgeMarks"/"row-band CuGrid"/"row-band
sao_params"). That closed out row-banded Stage 1 in full, still
single-threaded, still gated at <=1.03x, with room to spare on every
piece measured.

The next piece — `ReconPlane`'s own move from row-banded to the 2-D
per-CTU tile grid this document's "Concrete Stage 1 plan" describes,
needed because deblocking and intra reference-line reads must see a row
still in progress, sample by sample, which `EdgeMarks`/`CuGrid`/
`sao_params` never need (`planning/E2E-GAPS.md` §36's own finding) —
landed (`planning/E2E-GAPS.md` §39, commit `3ac859f`), byte-exact, but
**missed this stage's own gate**: six clean interleaved rounds (after an
earlier, disk-exhaustion-confounded attempt was correctly discarded
rather than trusted) measured CPU-seconds at median 1.047x, mean 1.041x
against the previous commit — above the <=1.03x line, with the
per-round spread (0.965x-1.115x) still wide enough that the exact number
is not yet tightly bound. `CuGrid`/`EdgeMarks`/`sao_params` do not need
this same move (§36's finding again: nothing about them needs per-sample
mid-row visibility), so this gate applies to `ReconPlane` alone, not to
redoing all four structures.

**The gate is conditionally waived, not silently ignored.** The
<=1.03x line was written for Stage 1's row-banded pieces, where the
representation change bought nothing on its own -- a 5%-slower
`EdgeMarks` would have been pure loss, so refusing it was correct. The
tile move is different in kind: it is the prerequisite for the only thing
that closes HEVC's real gap (7.7x behind serial ffmpeg, 26.5x behind
default-threaded ffmpeg at 4K, essentially all of the second number being
the absence of any HEVC threading). Paying ~4.7% serial to unlock Stage
2's 3-4x threaded ceiling is judged, by the coordinator, a good trade;
applying a gate written for a no-upside restructure to a load-bearing
prerequisite would follow the rule against its own purpose. So `3ac859f`
stays, and Stage 2b proceeds without reverting or re-optimising it first.

**The condition, so this waiver cannot become permanent by default: if
Stage 2b does not deliver a genuine multi-thread speedup, the tile
representation has bought nothing and must be reverted along with it.**
Whoever evaluates Stage 2b's own verification bar (byte-exact at
1/2/4/8/16 threads, the determinism fuzz target, the race-detector
fixture, speedup measured as a same-session ratio against `ffmpeg` at
matching thread counts) is the one who checks this condition -- a Stage
2b that lands but does not actually speed decoding up is not a partial
win with a 4.7% tax attached, it is grounds to undo this piece too. Do
not spend time optimising the 4.7% itself before then: it is bookkeeping
in `begin_ctu`/`publish_ctu` plus the `single_column` branch, it will
look different once real concurrent access patterns replace today's
single-threaded walk, and optimising against the serial-only shape now
risks being thrown away. Revisit it after Stage 2b lands, with a profile,
if it is still worth revisiting at all.

Stage 2 itself and its full 1/2/4/8/16-thread verification bar are what
remain, for the same reason the item has been staged this way throughout:
a byte-exact decoder's core representation is not something to rewrite in
one drop in a crate under active concurrent editing. See
`planning/E2E-GAPS.md` for each increment's own record as it lands.
