# vaco-filter-deinterlace

T2/T3 field-order and deinterlace filters (plan `16-filters.md` §4.3, the
FT-4.12 long tail — GitHub #480 is the catch-all, no dedicated issue):
`yadif`, `bwdif`, `w3fdif`, `estdif`, `separatefields`, `weave`,
`doubleweave`, `fieldorder`, `fieldmatch`, `fieldhint`, `detelecine`,
`telecine`, `idet`, `vfrdet`, `interlace`, `tinterlace`, `kerndeint`,
`pullup`, `repeatfields`, `phase`.

## Group membership, checked rather than assumed

Every one of the row's twenty names was checked against
`ffmpeg -hide_banner -filters` and `ffmpeg -h filter=<name>` (ffmpeg 8.1,
2026-08-23). All twenty exist in the reference, and — this is the finding
that shaped the crate — nineteen of them are plain `V->V` (one input pad,
one output pad); only `fieldmatch` is `N->V` (dynamic: 1 input normally, 2
when `ppsrc=true`). The row is exact in both directions: nothing to add,
nothing to drop.

## A finding worth stating plainly: this row does not need `Paired` or `Fanout`

The dispatch brief flagged `vaco-filter-core::adapt`'s new `Paired`/
`Fanout` adapters (interface gap 10) as likely necessary here, because
`separatefields` emits two frames per input, `weave`/`doubleweave` consume
two and emit one, and `tinterlace`/`telecine`/`detelecine` change the frame
rate. Measured against the reference, none of the `V->V` filters need
either adapter: every frame-count or rate change happens **inside a single
pad**, via `FrameOut::Many`/`FrameOut::None` and internal buffering —
exactly the shape `vaco-filter-temporal`'s `tmix`/`decimate` already use.
`Simple` (from `vaco-filter-core::adapt`) is sufficient for all nineteen.
`fieldmatch`'s `ppsrc=true` path is the one genuine two-input shape in the
row, and it is declined rather than wired onto `Paired` — see below.

## What it is

One module per filter, each exposing `pub const DESC: FilterDesc` and a
crate-private `fn create`, aggregated by
[`registry::DeinterlaceRegistry`](../../crates/filter/vaco-filter-deinterlace/src/registry.rs)
— the same shape `vaco-filter-temporal` and `vaco-filter-denoise` use.
`src/video.rs` holds the shared byte-level plane/row helpers
(`extract_field`, `weave_fields`, `copy_row`, `is_tff`) almost everything
here is built on: unlike `vaco-filter-temporal::video::PlaneBuf` (decode to
`f32`, run arithmetic, encode back), most of this crate's work is *row
selection and rearrangement* with no per-sample math, so operating on raw
row bytes directly is both simpler and exact for any sample depth.
`src/mad.rs` holds a second shared core — an original motion-adaptive
deinterlace kernel — used by `yadif`, `bwdif`, `w3fdif`, `estdif` and
`kerndeint`.

## `vaco-filter-vdsp`: extended, not duplicated

The row's dependency column calls for `vdsp`. `idet` and `fieldmatch` both
need a per-frame "how combed is this" metric, which is a different question
from `plane_sad`'s "how different are these two whole planes" (combing is a
property of *one* frame's own vertical structure). Per that crate's own
invitation to extend rather than duplicate,
[`vaco_filter_vdsp::comb_score`](../../crates/filter/vaco-filter-vdsp/src/lib.rs)
was added there: the sum of absolute vertical second differences,
`|row[y-1] - 2*row[y] + row[y+1]|`. Its own doc states the two algebraic
identities that anchor it (zero on any linear ramp; maximal on strict row
alternation) and that it is an **original** metric, not a transcription of
the reference's own (GPL, unread) interlace-detection formula. This crate
also has its own uncommitted concurrent additions from another agent
(`plane_sse`, an `identical_count` helper) landed in the same file during
this pass; both sets of additions compile and test together with no
conflict.

## Correctness discipline: independent oracles, per filter

### Byte-exact against the reference (measured, not guessed)

| Filter | Invariant checked |
|---|---|
| `separatefields` | Field order and row selection measured directly on a frame-identifiable ramp (`ffmpeg -f rawvideo`); `setfield=tff` gives even rows first, `bff`/unmarked gives odd rows first — the unmarked case is *not* the same as defaulting to top. |
| `weave` / `doubleweave` | `separatefields` then `weave` reproduces the original frame byte for byte (the invariant the row's brief names explicitly). Role assignment is by continuous field-index parity, not pair position — measured with `doubleweave`, where field 1 plays "top" in one output pair and "bottom" in the next. |
| `interlace` | Two identical frames with `lowpass=off` reproduce that frame back. `scan=tff`/`bff` row selection measured on a distinguishable pair. `lowpass=linear`'s exact `[1,2,1]/4` edge-clamped kernel measured via single-impulse probes at interior rows and at row 0 (edge clamp confirmed: `75`, not the unclamped `50`). |
| `fieldorder` | `tff` on an already-`tff` frame is a no-op (the invariant the brief names explicitly). Converting the other way shifts every row by one line, reflect-101 at the exposed edge (measured: `bff->tff` reflects `orig[rows]` to `orig[rows-2]`; `tff->bff` reflects `orig[-1]` to `orig[1]` — not a plain edge-duplicate). |
| `telecine` / `detelecine` | Telecine's continuous-field-stream algorithm was derived from a full byte-level readout of 30 output frames against 24 frame-identifiable inputs and matches exactly. `detelecine` is its algorithmic inverse; round-tripping a synthetic 24 fps source through both recovers the original frame count and every row's content exactly (the invariant the brief names explicitly). |
| `phase` (`mode=t`/`b`) | Measured directly: `mode=t` keeps the current frame's own top field and delays the bottom field by one frame (`weave(top=current, bottom=held previous)`). `mode=p` is an unconditional, exact passthrough. |
| `tinterlace` (`merge`, `drop_even`, `drop_odd`, `interleave_top`) | Frame counts and geometry measured for all eight modes; content measured and matched for these four (`drop_even`/`drop_odd` keep the frame unmodified at stride 2; `merge` doubles height via `weave_fields`; `interleave_top` matches `interlace`'s row selection at unchanged height). |
| `repeatfields` | Passthrough by construction (see the real gap below) — trivially exact because it does nothing. |

### Documented structural approximations (not byte-exact, and said so)

- **`yadif`, `bwdif`, `w3fdif`, `estdif`, `kerndeint`** (`src/mad.rs`): an
  **original** motion-adaptive interpolator — not a transcription of any of
  the reference's published kernels. The public descriptions this pass
  could reach (deinterlacing forums, AviSynth wiki pages, doxygen struct
  listings) describe the algorithms' *shape* (spatial+temporal check,
  edge-directed interpolation) but not exact coefficients, and several are
  themselves close paraphrases of the GPL source this project will not read
  even indirectly (D7). What **is** guaranteed, by construction rather than
  special-casing: on a genuinely static/progressive sequence (`prev == cur
  == next`), the temporal candidate at every non-kept row equals that row's
  true value exactly, so the output reproduces the input exactly except at
  a frame's own top/bottom edge row (one-sided spatial estimate, a real
  bounded limitation — see `mad::tests::a_static_sequence_reproduces_exactly`).
  `kerndeint` additionally reuses this temporal core even though the
  reference's own `kerndeint` is purely spatial, the single largest
  documented simplification in the crate. `bwdif`'s and `w3fdif`'s/
  `estdif`'s reference default (`mode`/`filter`=field-rate, two outputs per
  input) is not implemented; every mode here behaves like the reference's
  own frame-rate mode.

  **A real bug was found and fixed (2026-08-29) by measuring against real
  `ffmpeg` rather than only the static-sequence invariant above.** The
  original `blend()` read `prev`/`next` at the *same row* as the missing
  sample as its "temporal" candidate. On real (or realistically
  synthesised) interlaced content that row is always the *other*, discarded
  field's own genuine sample at three different times — so averaging it
  reconstructs `cur`'s own already-known, wrong-field-time value, almost
  exactly, whenever motion is smooth. Measured on a fixture built so a
  correct deinterlace has zero vertical variation (`mad::oracle`, a
  horizontally-scrolling flat ramp through `ffmpeg -vf tinterlace=4`): the
  old code's comb score barely moved (input 730112, output 746224 — no
  better). The fix, `kept_field_estimate`, asks the same "what would the
  *kept* field show here" question of `prev`/`cur`/`next` alike, so
  temporal averaging combines three readings of one signal instead of
  reproducing the artefact. Verified two ways, both now in `src/mad.rs`'s
  own test suite and reproducible any time `ffmpeg` is on `PATH`:
  - On the discriminating ramp fixture: comb score collapses to interior-
    frame-exact (residual confined to the two edge frames with no
    symmetric temporal partner, a separate documented limitation), and
    Y/U/V PSNR against real `yadif`/`bwdif`/`w3fdif`/`estdif` (each pinned
    to its own frame-rate mode option for a fair comparison) is **exactly
    infinite** — byte-identical to all four real filters on unambiguous
    linear motion.
  - On busy, realistic content (`testsrc2`): comb score 689384 -> 251126
    (63.6% reduction) and Y/U/V PSNR against real `yadif` of 24.01/27.83/
    28.14 dB — consistent with "two reasonable, differently-designed
    deinterlacers disagreeing on ambiguous detail", not with either side
    being broken, and never claimed as byte-exact.
  Per the repository owner's 2026-08-28 ruling, byte-exactness is not the
  acceptance bar; this measurement is offered as the "not broken, and here
  is the residual" evidence that ruling asks for instead.
- **`pullup`, `fieldmatch`**: original `comb_score`-based heuristics (queue
  fields, weave the front two, accept if the result scores below a fixed
  threshold, otherwise drop the front field as a likely duplicate/orphan
  and retry). Structurally correct on a genuinely progressive source
  (degrades to plain `weave`, checked in `pullup`'s tests) and recovers a
  plausible frame count on real telecined content, but not claimed
  byte-exact against the reference's own combing analysis.
  `fieldmatch`'s `ppsrc=true` (2-input clean-source) mode is refused at
  creation with a clear error rather than silently ignoring the second
  input.
- **`fieldhint`**: like `vaco-filter-temporal::fsync`, the reference's
  per-line grammar for its three `mode`s was not reverse-engineered from
  probing (confirmed the option genuinely opens a file; every line format
  tried against an existing file failed to disambiguate the grammar). This
  crate defines its own contract instead (`top,bottom` field-index pairs,
  one per output frame) rather than guessing the reference's.
- **`tinterlace`**: `pad`, `interlacex2` and `mergex2` have their *frame
  counts and geometry* measured exactly (`N-1` sliding pairs for the first
  two, `2(N-1)` for the third) but their per-sample content is a documented
  structural approximation, not fully reverse-engineered.

### A real, structural gap: `repeatfields`

The reference's `repeatfields` reads `AVFrame::repeat_pict`, set by an
MPEG-1/2 decoder from the picture header's `repeat_first_field` bit.
`vaco_frame::Frame`/`FrameFlags` (owned by another crate, out of this
brief's single-writer scope) has no equivalent field — only `INTERLACED`
and `TOP_FIELD_FIRST`, both booleans. This filter is registered and never
panics, but behaves as a pure passthrough on every input, because nothing
it can observe ever carries a repeat signal. Not a placeholder that will
start working quietly later: closing it needs a field this crate cannot
add.

### Interface gap 11, closed mid-flight: `idet` now publishes real metadata

`INTERFACE-GAPS.md` gap 11 (`vaco_frame::Frame` had no per-frame metadata
dictionary) was still open when this crate started and closed additively
(`Frame::set_metadata`/`metadata_get`, a new `FrameSideData::Metadata`
variant) before it finished — checked again immediately before `idet` was
written, per that gap's own note that `idet` was expected to need it. So
`idet` **does** write real `lavfi.idet.single.current_frame` and
`lavfi.idet.repeated.current_frame` keys, under the reference's own names
(measured via `ffprobe -show_frames -f lavfi -i testsrc2,idet`), onto every
output frame. What is not reproduced is the reference's four-way
vocabulary (`tff`/`bff`/`progressive`/`undetermined`, plus cumulative
`.multiple.*` fractions) — this classifier only distinguishes progressive
from interlaced, so it publishes `progressive`/`interlaced` under the
correct key rather than fabricating a parity split it cannot support.

`vfrdet` is unaffected by the gap closing: measured directly, the
reference's own `vfrdet` publishes **no** per-frame metadata at all, only a
final summary log line at filter destruction. There was never a dictionary
entry for gap 11 to unblock here.

## Every fix falsified

The `extract_field`/`is_tff` interaction that made `pullup`'s progressive
round-trip test fail (`extract_field` did not tag the field it produced
with its own top/bottom role, so a caller asking `is_tff` of an extracted
field read the *source frame's* flag instead) was falsified by reverting
the one-line fix, confirming the test failed exactly the same way it had
before, and restoring it.

The `kept_field_estimate` fix above (2026-08-29) was found the same way in
reverse: `mad::oracle::measured_against_real_ffmpeg_deinterlacers` was
written *before* the fix, observed to fail with the exact numbers quoted
above (comb score 730112 -> 746224, no reduction), the fix applied, and
the same test re-run to confirm it now passes with comb score collapsing
and PSNR going to infinity on the same fixture. The failure and the fix
are both reproducible from `src/mad.rs`'s own git history.

## Configuration

No crate-level configuration. Per-filter options are documented in each
module's own doc comment, matching `ffmpeg -h filter=<name>`'s option table
(ffmpeg 8.1, 2026-08-23) for every option name, default and range —
including options this crate parses but does not act on, which are called
out explicitly rather than silently accepted.

## How to change it

- New row members go in their own `src/<name>.rs`, registered in
  `src/registry.rs` and `vaco-component.toml`.
- Extend `src/video.rs` for any new byte-level row operation shared across
  filters; extend `src/mad.rs` only if a new filter genuinely shares the
  motion-adaptive shape (do not bolt an unrelated algorithm onto it).
- If the real reference algorithm for `yadif`/`bwdif`/`w3fdif`/`estdif`/
  `kerndeint`/`pullup`/`fieldmatch` is ever reached through a genuinely
  clean-room, precise-enough public specification, replace `src/mad.rs`'s
  core (or the relevant heuristic) and update `Vaco-Provenance` on that
  commit — the current `original` provenance for those filters reflects
  what could be verified this pass, not a permanent design choice.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-vdsp` (extended by this crate, see
above).

## Fuzzing

`fuzz/fuzz_targets/filter_deinterlace_options.rs` (option parsing, every
registered name, through the real filtergraph parser, including
`fieldhint`'s file-open path with fuzzer-controlled paths): 31s,
241,937 execs, 0 crashes, `fuzz/artifacts/` empty for this target.
