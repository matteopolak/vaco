# HEVC wavefront (WPP) threading — design, and current status

## What it is

`PERF-PROGRAMME.md` item B4: intra-picture parallelism for `vaco-codec-hevc`,
using the wavefront parallel processing (WPP) structure ITU-T H.265
§9.3.2.3/§7.3.1.1 already puts in the bitstream — one CTU row decoded per
worker, each row starting two CTUs behind the row above it. Controlled by
`-threads N`, opt-in, default off until proven the way H.264's own row
threading was (`docs/codec/frame-threading.md`).

**Status: design only, now on its second revision. No production code has
changed** (a `#[cfg(test)]`-only empirical measurement has — see
`planning/E2E-GAPS.md` §31). This document was first written per the plan's
own instruction that "the executing agent writes the design doc first, as
`docs/codec/frame-threading.md` was written for H.264," then corrected by a
later pass after finding that its own central technical claim — reusing
`vaco_codec_core::picture::ProgressPicture` "one level down" — does not
survive reading that module's actual implementation. See "Correction:
chaining `ProgressPicture` per row does not give WPP its parallelism"
below for what changed and why, and "Two honest paths forward" for what
this means for the item going forward. The reasoning throughout is grounded
in this crate's (and now also `vaco-codec-core`'s) actual code, cited by
file and function, not a guess at its shape.

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
  sufficient — this pass believes the latter~~ **Corrected, not just
  resolved**: chaining one `ProgressPicture` per CTU row is not sufficient
  — see the correction above, reached by reading `picture.rs`'s actual
  `band_of`/`publish_through`/`ready` implementation rather than trusting
  its module doc's cross-picture framing to generalise. Which of the two
  paths in "Two honest paths forward" to take (build the new per-CTU-tile
  capability, in or out of this crate, vs. build the smaller two-stage
  pipeline that needs nothing new) is now the open decision, and it is a
  plan-level scoping call, not a fact this crate's own code can settle by
  itself the way the deblocking-lag question was.

## Why this pass stops here

`vaco-codec-hevc` has had concurrent editors for this entire session (see
`planning/E2E-GAPS.md` §24's own account of a collision during item B1, and
the `VACO_HEVC_TRACE` debug instrumentation found live in `ctu.rs` during
B3's work). The restructure this item needs touches `framebuf.rs`, `ctu.rs`,
`deblock.rs`, `sao.rs` and `decoder.rs` at once — effectively the whole
crate — for a byte-exact video decoder, and (per the correction above) at a
finer, more invasive granularity (per-CTU-tile, not per-CTU-row) than this
document previously believed, if path 1 (true WPP) is the one taken.

The deblocking-lag proof that used to gate even starting this is resolved
(one CTU row each side, `planning/E2E-GAPS.md` §31) and is no longer a
blocker either way. What replaced it as the actual blocker is the
correction above: this pass discovered, by reading `vaco_codec_core::
picture`'s implementation rather than assuming its cross-picture framing
generalised, that the mechanism this document previously proposed reusing
does not give WPP real parallelism at all. Writing Stage 1 against that
premise would have meant restructuring `framebuf.rs`/`ctu.rs`/`deblock.rs`/
`sao.rs`/`decoder.rs` — unverified, in a crate under active concurrent
editing — around a mechanism that, once threaded in Stage 2, could not have
delivered the scaling the item's own 1/2/4/8/16-thread verification bar
requires. Finding that out empirically now, before any of that restructure
is written, instead of after Stage 2 fails to scale, is the same discipline
this document's own deblocking-lag section used a measurement to answer
rather than an assumption. This document is what the plan itself asks for
first; it now says the item is more expensive than previously scoped (path
1) or is not quite the item as named (path 2), and picking between those is
a decision for whoever owns the plan, not a default this pass takes for
itself.
