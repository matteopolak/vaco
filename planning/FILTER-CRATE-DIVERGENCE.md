# The filter crate partition drifted from plan 16 §4.2–4.4, and it was the orchestrator's doing

`planning/16-filters.md` §4.2, §4.3 and §4.4 carry an authoritative table: one row per
`vaco-filter-*` crate, with the exact filter list, the shared kernels it leans
on, and its tier. It is a deliberate partition — crates are grouped by the
kernels they share (`vdsp`, `adsp`, `draw`, `framesync`), not by topic — and it
is the single source of truth D19 asks for.

Several dispatch briefs invented crate names and memberships instead of reading
it. That is an orchestrator error, not an agent one: every agent built exactly
what its brief asked for, and three of them checked the *reference* carefully
enough to correct my filter lists while having no reason to question the crate
name.

## What diverged

| Built | Plan says | Note |
|---|---|---|
| ~~`vaco-filter-audio-eq`~~ `vaco-filter-aeq` | `vaco-filter-aeq` | **Done**: renamed, membership unchanged. Landed alongside the `vaco-filter-adsp::biquad` consolidation (its `engine` module moved there, D19), so the rename commit is separate from that move. |
| `vaco-filter-audio-dynamics` | `vaco-filter-adynamics` | membership matches; name only |
| `vaco-filter-ameasure` | `vaco-filter-aanalysis` | membership matches exactly; name only |
| ~~`vaco-filter-achannel`~~ `vaco-filter-aeffects` | `vaco-filter-aeffects` | **Done** (FT-4.13d, GitHub #484): renamed, and the crate now implements 22 of the row's 25 filters — `surround`/`headphone` remain deferred as disproportionately large (flagged by this crate's original author), and `hdcd`'s proprietary bit-level decode is out of reach for black-box probing at this project's clean-room standard. |
| `vaco-filter-video-geometry` | `vaco-filter-geometry` | holds the T1 subset of one plan row |
| `vaco-filter-video-format` | `vaco-filter-scale` | |
| `vaco-filter-video-source` | `vaco-filter-source` | |
| `vaco-filter-component` | *(no such row)* | its filters belong to `geometry`, `color`, `key` and `lut` |
| `vaco-filter-audio` | *(no such row)* | spans `aformat`, `amix`, `adynamics` |
| ~~`vaco-filter-plumbing`~~ `vaco-filter-mm` | `vaco-filter-mm` | **Done** (FT-4.12f, GitHub #479): renamed, and the crate now implements 37 of the row's 41 filters — `avsynctest` deferred as a synthetic A/V generator disproportionate to the row (the `aeffects` precedent for `surround`/`headphone`), `cmdsocket`/`acmdsocket` need a real listening socket, and `aeval` was deferred for time (the reference's own docs call it "slow"). `sendcmd`/`asendcmd` parse the command grammar, detect enter/leave edges, and now dispatch through `FilterContext::send_command` after the graph-level command API landed. `color`/`nullsrc`/`nullsink`/`anullsrc` are carried in this crate rather than `vaco-filter-source`/`vaco-filter-asource` (the row those four actually belong to, per §4.2/§4.3): both crates exist but do not yet register these four names, and deleting working filters with nothing to replace them would regress the CLI with no gain — left as a recorded, not silent, divergence for whoever owns those two crates to resolve, with `dup-check` as the day-they-collide safety net. |
| `vaco-filter-blur` | `vaco-filter-blur` | name right; brief added the `convolve` row's filters to it |
| `vaco-filter-denoise` | `vaco-filter-denoise` | correct, both name and membership |

## The concrete cost

`axcorrelate` was listed in two briefs at once (#482 and #483). The second agent
implemented it in full, tested it, and then deleted it when `gen-registry`'s
duplicate-name check surfaced the collision. Wasted work that the plan's table
would have prevented, because `axcorrelate` appears in exactly one row.

## The rule going forward

**A dispatch brief for a filter crate must quote its row from
`planning/16-filters.md` §4.2–4.4 and must not name a crate the table does not
have.** If the reference has a filter the table places nowhere, that is a change
to the table — proposed in a commit that says why — not a new crate invented at
dispatch time.

## Reconciliation, still to do

Renaming is mechanical but touches registry fragments, `docs/`, fuzz targets and
`provenance/` for each crate, so it wants one sweep with no filter agents in
flight rather than six concurrent renames. The rows above are the work list.

Two of them are more than a rename and need a decision first:

- `vaco-filter-video-geometry` holds the T1 subset (`crop`, `pad`, `transpose`,
  the flips) of the plan's single `vaco-filter-geometry` row, whose T2 remainder
  is being written separately. Either they merge, or the plan grows an explicit
  T1/T2 split. Merging is truer to §4.2.
- `vaco-filter-audio` spans three plan rows and predates all of this. Splitting
  it is the largest single piece of the reconciliation.

## `vaco-filter-mm`'s row: membership right, one dependency wrong

Every one of the row's 41 filter names was checked against `ffmpeg -filters`
and `ffmpeg -h filter=<name>` while building `vaco-filter-mm` (GitHub #479);
all 41 exist under exactly those names, so the row's membership needs no
correction. Its "extra deps" column does, in one place: it lists `framesync
(streamselect)`, and `-h filter=streamselect` shows neither `eof_action` nor
`shortest`/`repeatlast`/`ts_sync_mode` — the option surface
`planning/AGENT-CONSTRAINTS.md`'s "two inputs does not mean framesync" rule
says to check before reaching for it. `streamselect` does not use
`vaco-filter-framesync`; it is implemented as plain per-pad lockstep
passthrough. A small correction to the row's own text, recorded rather than
silently diverged from.

## Resolved during the wave

`vaco-filter-component` never landed. The agent building it was redirected
mid-flight and shipped `vaco-filter-color`, `vaco-filter-key` and
`vaco-filter-lut` instead — three real rows from §4.2. `vaco-filter-blur`'s
author likewise split the convolution and morphology family out into
`vaco-filter-convolve`, its own row, rather than leaving it misfiled.

So the table above is now three rows shorter in the "built a crate the plan
does not have" category, and the remaining work is renames plus the two
structural decisions.

## What the redirect cost, recorded honestly

The `component` agent had working, tested implementations of `extractplanes`,
`mergeplanes`, `alphamerge` and `maskedmerge` and deleted them rather than push
into crates it did not own — which was the right call under a single-writer
rule, and was still four filters of wasted work caused by the brief, not by the
agent. `maskedmerge` survived in `vaco-filter-key`; the other three did not.

Their shape is worth keeping even though the code is gone, because it is
independent evidence about `INTERFACE-GAPS.md` gap 10: `extractplanes` is
dynamic-output V→N, `mergeplanes` is **dynamic-input N→V**, and `alphamerge`
runs through `framesync` directly. The gap-10 entry originally named only a
two-input adapter and a dynamic-output one; the N-input case is a third shape
and neither covers it.

## The section numbers, since three briefs got them wrong

`planning/16-filters.md` §4 splits as:

| §   | What                                                     |
|-----|----------------------------------------------------------|
| 4.1 | Infrastructure (layer 5a) — `-core`, `-graph`, `-framesync`, `-draw`, `-vdsp`, `-adsp` |
| 4.2 | **Video** filter crates                                  |
| 4.3 | **Audio** filter crates                                  |
| 4.4 | Multimedia crates — `-mm`, `-movie`, `-avvis`            |
| 4.5 | GPU and hardware                                         |

Several dispatch briefs cited §4.3 for *video* rows. The `vaco-filter-color`
agent caught it and said so; the rows it needed were in §4.2. Cite the right
one, or just say "the table in §4" and let the agent find its row.

## The same D19 problem the biquads had, in three more shared kernels

The biquad consolidation (`vaco-filter-adsp` gaining `Coeffs`, `normalise`,
`response_db` and the RBJ design family, so five crates stop carrying their
own) fixed one instance of a pattern. Auditing plan 16 §4.1's other
shared-kernel rows finds three more, all live right now:

| Kernel | §4.1 says it lives in | Where it actually is |
|---|---|---|
| EBU R128 loudness core | `vaco-filter-adsp` | `vaco-filter-aanalysis` (`kweight.rs`, `loudness.rs`, `ebur128.rs`, `replaygain.rs`) **and** `vaco-filter-adynamics/loudnorm.rs` |
| Window functions | `vaco-filter-adsp` | `vaco-filter-aanalysis/aspectralstats.rs` (`hann`), `vaco-filter-asource/window.rs` (five funcs), `vaco-resample/design.rs` (`kaiser`, `blackman_nuttall`) |
| Box-blur core | `vaco-filter-vdsp` | `vaco-filter-blur/common.rs`, reached again from `vaco-filter-convolve` |

**`dup-check` cannot see any of these**, and that is the important part. It
compares *type names* across crates, and these are free functions with
different names for the same mathematics — `hann` here, `value(WinFunc::Hann,
…)` there, `blackman_nuttall` somewhere else. The gate's own module docs already
say "it cannot see two types that mean the same thing under different names.
That needs a person." This is what that sentence looks like in practice.

Two of the three are worth consolidating and one may not be:

- **Windows** are the clearest case: three implementations of the same closed
  forms, and a window function is exactly the kind of thing that is subtly
  wrong in one place and right in two. `vaco-resample`'s pair is at a different
  layer (`crates/signal`, not `crates/filter`) so the merge target needs
  thought — possibly `vaco-tx` or a new shared crate rather than `adsp`.
- **EBU R128** is one core used by `ebur128`, `replaygain` and `loudnorm`
  across two crates. `loudnorm` reaching into `vaco-filter-aanalysis` is the
  wrong direction; the core moving down to `adsp` is the plan's answer.
- **Box blur** is used by `boxblur`, `avgblur` and `unsharp` in one crate and
  by `convolve` in another. Smallest of the three, and the one where the two
  callers may genuinely want different edge handling — `vaco-filter-blur`'s own
  docs record that this family has two incompatible border conventions.

Recorded rather than done: four of these five crates had a live owner when the
audit ran.


## A fourth instance, GitHub #478/"FT-4.12e": `vaco-filter-effect` was never a row

A dispatch brief for issue #478 asked for a `vaco-filter-effect` crate
covering roughly two dozen "stylisation" filters (`sobel`, `prewitt`,
`roberts`, `kirsch`, `edgedetect`, `morpho`, `erosion`, `dilation`,
`deflate`, `inflate`, `shuffleframes`, `shufflepixels`, `shuffleplanes`,
`swaprect`, `swapuv`, `tmix`, `lagfun`, `random`, `photosensitivity`,
`noise`, `vignette`, `pixelize`, among others). `planning/16-filters.md`
§4.2 has no such row. Checking `planning/ASSIGNMENTS.md` before writing
anything found that essentially the whole named list already has a home and
is already built: `vaco-filter-convolve` (#468, done), `vaco-filter-geometry`
(#470, done) and `vaco-filter-temporal` (#475, done) between them cover all
but four of the names, and `photosensitivity` belongs to `vaco-filter-analysis`
(#477), which had a live owner (`agent:analysis2`) mid-commit at the time —
correctly left untouched under the single-writer rule.

What was actually left, unclaimed: `noise`, `vignette`, plus the rest of
§4.2's real `vaco-filter-artistic` row (`pixelize` — already a documented
duplicate in `vaco-filter-geometry`, `epx`, `xbr`, `hqx`, `super2xsai`,
`amplify`, `delogo`, `removelogo`, `cover_rect`, `find_rect`). Built
`vaco-filter-artistic` — the row's real name — implementing `noise` and
`vignette`; the rest of that row is still open. See
`docs/filter/vaco-filter-artistic.md` for the full reconciliation and the
framecrc-verification table.

The concrete cost this time was smaller than `axcorrelate`'s: no code was
implemented against the wrong crate name before the table was checked, so
nothing was thrown away. The cost was entirely in verification time — five
crates' worth of `ls`/`git log`/`ASSIGNMENTS.md` reading before writing a
line of filter code — which is exactly what this document exists to save
the next brief from repeating.
