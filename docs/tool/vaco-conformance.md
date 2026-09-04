# `vaco-conformance` — the differential conformance harness

> **THE BRIGHT-LINE RULE**
>
> **You may run the reference binary as often as you like. You may not read its
> source. When our output differs and you cannot explain why, you escalate — you
> do not go looking in the source for the answer.**

Read that before anything else in this document. It is plan 13 §1.7.2 verbatim,
it is why this design is clean-room compatible, and everything below is shaped
by it.

---

## What it is

Vaco claims that for a given input and a given argument vector its output is
identical to `ffmpeg`/`ffprobe`'s. `vaco-conformance` is the machinery that
either proves that or says precisely where it fails. Plan 11 calls it the
project's primary acceptance criterion: **nothing is correct until this says
so.**

It also does something useful before any codec exists — it holds our static
tables to the reference. `vaco-pixfmt`'s 268-format table and `vaco-core`'s
colour, frame-size and frame-rate tables are internally consistent, but internal
consistency is not correctness. The table extractors (§ *Table extractors*) are
the first external validation anything in the project has had.

---

## The clean-room argument, and what it forbids

The reference binary is an **oracle we query, never a source we read**.

Running a shipped executable and recording what it printed acquires *facts about
behaviour*. Facts are not copyrightable (*Feist*, 499 U.S. 340). Behaviour is
functionality, and functionality is filtered out of any similarity analysis
(*Altai*, 982 F.2d 693; *SAS Institute v. World Programming*, C-406/10). EU
Directive 2009/24/EC Art. 5(3) grants a lawful user the right to observe, study
and test a program to determine its underlying ideas, and Art. 8 makes that
right non-waivable. Running `ffprobe` on a file is the paradigm case.

Two design consequences, both load-bearing:

### There are no golden files

Every expected value is computed by running the reference **at test time** and
discarded when the case finishes. Nothing FFmpeg-derived ever lands in the
repository. That defeats both the *access* element and the *substantial
similarity* element rather than arguing about one of them, and it sidesteps the
compilation-copyright question about FATE's 4,936 reference digests entirely.

There is deliberately **no `--update-expected` / blessing mode**. There is
nothing to update. This also removes the most common failure mode of snapshot
testing, which is blessing a regression.

### Reading the reference's source to explain a divergence crosses the line

| Action | Verdict |
|---|---|
| Running the reference 10,000 times a night in CI | **Fine** — observation at scale is still observation |
| Storing a minimised *input* that triggers a divergence | **Fine** — it is our file, not their expression |
| Varying flags, running `-loglevel trace`, feeding it synthetic files | **Fine** — this is what the oracle is *for*, and it is unlimited |
| Reading the reference's user documentation, man pages, Trac tickets, commit **messages** | **Fine** — Tier A material |
| Storing the reference's *output* as a committed expected value | **Avoid** — the design makes it impossible by accident |
| Opening `libavformat/mov.c` because a box parses differently | **LINE CROSSED** — you are now a dirty-team member for that module and may not commit implementation code to it |
| Asking an LLM "how does FFmpeg parse the `elst` box?" | **LINE CROSSED** — source access with extra steps, and it destroys the provenance record |
| Asking an LLM "what does ISO/IEC 14496-12 §8.6.6 say the `elst` box contains?" | **Fine** — a question about a public standard; verify against the standard and cite the clause |
| Disassembling the reference binary to recover an algorithm | **LINE CROSSED** — that is reverse-engineering expression, not black-box observation |

### The divergence triage ladder

When a case fails and the cause is not obvious, work down this. Steps 1–4 keep
you clean; step 5 requires someone else; step 6 is capped at ten project-wide.

1. **Re-read the specification clause the code was written from.** About half of
   all divergences are a misread normative clause and the answer is in the
   document you already have.
2. **Minimise the input** until it explains itself.
3. **Interrogate the oracle.** `vaco-conformance explore -- <argv…>` runs it and
   shows you the output, and writes nothing to the repository. Vary flags, vary
   the input, build synthetic files, read its `-h` output. Unlimited.
4. **Read Tier-A material** — user docs, man pages, tickets, commit *messages*.
   Commit messages are prose *about* a change and are Tier A; the diffs they
   link to are not. Do not click through.
5. **Escalate.** File a `question: behaviour` issue. A designated gatekeeper for
   that module reads whatever they need to and replies with a *behavioural
   statement* — a description of what the program does, written as facts. You
   stay clean; they become dirty for that module.
6. **Record it as `unexplained`** in the divergence register. Two approvals,
   90-day expiry, and it blocks the owning module from being marked done.

---

## How it works

### The model

```
Case = (Media, Argv, Comparison) → Verdict
```

Both sides run under a fixed environment: `TZ=UTC`, `LC_ALL=C`,
`SOURCE_DATE_EPOCH=0`, a scratch `HOME`, a scratch working directory, stdin
closed, a wall-clock timeout, and an output size cap. `PATH` and the dynamic
loader variables are inherited — a shared-library reference build cannot start
without them — and that is the one deliberate hole in hermeticity. Both sides
inherit the same values, so it cannot skew a comparison.

### Probe acceptance slices

`tests/probe_confusion.rs` contains small black-box slices alongside the
manifest-driven suites. Each creates its fixture with the reference `ffmpeg`
binary, queries both `ffprobe` and `vaco-probe`, and discards the fixture when
the test ends. The detector sweep checks the selected format and packet count
across container and raw-format families; the raw-audio metadata slice checks
`codec_name`, `sample_rate`, `channels`, and `time_base` for ADTS AAC, AC-3,
E-AC-3, MP3, and FLAC at 48 kHz stereo.

These tests intentionally compare the reported field values, not merely a zero
exit code. They exclude container duration from this slice: duration precision
has a separate acceptance path, so a metadata regression cannot be hidden by
or attributed to timestamp rounding. Extend the case table with another
independent format family when adding a probe field; keep a fixture's encoding
arguments and the compared fields in the test so the oracle measurement stays
reproducible.

Output is drained on dedicated threads while the main thread waits for exit. The
obvious implementation (wait, then read) deadlocks the moment a child fills a
pipe buffer, and finding that out from a hung nightly is expensive; `run.rs` has
a regression test for it. On Unix, timeout cleanup signals the child's entire
process group through the external `kill` command. Keep the `--` separator
before its negative process-group id: procps otherwise parses that id as an
option and leaves grandchildren holding the output pipes open.

### The comparison modes

| Mode | Name | State | Applies to |
|---|---|---|---|
| C0 | `exact-bytes` | **implemented** | every probe writer, deterministic remuxes, listing commands, exit codes |
| C1 | `exact-bytes-normalised` | **implemented** | C0 with a declared normalisation chain |
| C4 | `raw-exact` | **implemented** (`compare::raw::compare`) | decoders we claim are bit-exact (lossless codecs), and every filter's pixel output |
| C5 | `raw-tolerant` | **implemented** (`compare::raw::compare_tolerant`) | codecs whose *spec* defines conformance as a bounded error |
| C6 | `structured-diff` | **implemented** (`default` writer) | the metadata surface where a few divergences are expected |
| C7 | `behavioural` | **implemented** | malformed input, unsupported paths, the differential fuzzer |
| C10 | `quality-band` | seam — the metrics are not written | lossy encoders |

C2 `container-structure`, C3 `frame-hash`, C8 `cross-decode` and C9
`three-way` are not modes in this crate. They were typed seams with no
manifest ever declaring them and no backing machinery (a container walker, a
frame-digest pipeline, an interoperability matrix, the native/external/
reference lattice) — removed rather than left unreachable. The design stays
recorded in plan 13 §1.2 for whoever builds that machinery to re-add the mode
alongside it.

**An unimplemented mode skips; it never passes.** That distinction is why C10
stays typed rather than left as `todo!()`. A suite declaring `quality-band`
today shows up as "not implemented" in the run summary, and the tier skip
budget makes that visible instead of silently green.

**C10 is a seam on purpose.** Byte comparison applies to every operation whose
output is fully determined by its input and its declared options; quality
comparison applies to every operation involving a lossy encoder's
rate–distortion decisions. The dividing question is *not* "is the codec lossy?"
— lossy *decoding* is fully determined and is held to C4 with zero tolerance.
Lossy *encoding* is a search result, and no two searchers agree. But the metrics
themselves carry a constraint: `tiny_ssim.c` and `tiny_psnr.c` are GPL and are
on the do-not-reuse list, so SSIM must come from Wang, Bovik, Sheikh &
Simoncelli, *IEEE TIP* 13(4), 2004 — citing the paper, never the file. That work
belongs in the crate that owns image metrics. `compare/quality.rs` defines the
`Metric` trait, the `Registry` extension point and the band arithmetic (which is
unit-tested without needing an encoder), and returns an honest skip until a
metric is registered.

### The `filter` tool (FT-6.1, #500)

Every other tool (`probe`, `transcode`, `play-headless`) compares two
**subprocesses** run against the same argv. `filter` cannot: there is no
`vaco -vf` CLI yet (a separate, larger piece of work — FT-2.x territory,
not this harness's), so "what `vaco` does with this filter" can only be
observed by calling the filter crate's own `FilterRegistry` directly,
through a real `vaco_filter_core::Graph`. `src/filterexec.rs` does exactly
that, in-process, and hands back a [`crate::run::Observation`] shaped the
same way a subprocess's would have been — `stdout` carries the raw output
frame, plane by plane, row-major, no padding, exactly what `ffmpeg -f
rawvideo` writes for `gray8`/`yuv444p`/`gbrp` — so `raw-exact` (C4) diffs
the two streams with no format-specific knowledge at all.

A `filter`-tool case's `argv`, after `{media}`/`{media:<id>}` substitution,
is nine positional tokens for input 0 (not CLI flags — there is no CLI to
hand them to), plus one four-token group per *additional* input pad a
multi-input filter declares:

```text
[0] path to input 0's generated raw file
[1] filter name, e.g. "histogram"
[2] filter args string, e.g. "level_height=50:scale_height=0:components=1"
[3] input 0's pixel format: "gray8" | "yuv444p" | "gbrp"
[4] input 0's width    [5] input 0's height
[6] output pixel format
[7] output width       [8] output height
[9..] zero or more groups of four (media_path, pixfmt, width, height),
      one per additional input pad, in the filter's own pad-declaration
      order
```

A single-input case is untouched by this: still exactly nine tokens, so
every case written before multi-input support existed keeps working
unmodified. `FilterArgs::parse` accepts any number of trailing groups
(including zero); `filterexec::run` separately checks the count it parsed
against the filter's own declared `FilterDesc::inputs` length once the
filter is instantiated, so a case naming too few or too many inputs for
that specific filter fails loudly, naming both numbers, rather than
silently connecting the wrong pad.

A suite names an extra input by declaring it `extra_media` on the
`[[axis]].values[]` entry that needs it, resolved by id against the
suite's full `[[media]]` list (not iterated — declaring three inputs does
not multiply the case count into three cases), and referencing it from
`argv` as `{media:<id>}`. An input with no natural "vary this, rerun
everything else" role (a fixed `base`/`overlay`/`mask`, say) marks its own
`[[media]]` entry `fixed = true` so `Suite::expand`'s per-case iteration
skips it entirely, leaving it available only by explicit name. See
`tests/conformance/filter/vaco-filter-key-multi.toml` (`maskedmerge`,
`maskedmax`, `maskedmin` — three inputs each, two different multi-input
adapter shapes) for a worked example, and `filterexec.rs`'s own doc for
the full convention and what is still out of scope (a real `framesync`
timeline needing more than one frame per input before producing output).

Output geometry is declared, not derived — every filter in the first
corpus (`tests/conformance/filter/`) already has a fixed, filter-specific
output shape, and deriving it generically would mean re-implementing each
filter's own `configure` logic a second time in the harness, exactly the
"looks measured, is not" risk this project has already paid for once. A
case with the wrong declared geometry fails loudly, not silently — see
`filterexec::extract_output`.

`filterexec::REGISTRIES` is the explicit, short list of `FilterRegistry`s
this tool tries. There is no aggregate registry combining every filter
crate in the tree yet; adding a filter
crate to the corpus means adding its registry to that list, a genuine
reviewable change, not an oversight to fix later.

**The permanent D7 font-table ceiling gets a declared downgrade, not a
silent omission or a case left failing forever.** `datascope`,
`graphmonitor`, `agraphmonitor` and `pixscope` draw text with an
independently-sourced font (D7 forbids transcribing the reference's own
glyph table), so no frame they draw text into can ever be `raw-exact`
against the reference. `manifest.rs`'s rule 3 makes this the same
declared move `structured-diff` already makes relative to C0: a
`filter`-tool suite using `behavioural` (C7 — "did both sides produce a
frame at all", not "was it the same frame") must carry a
`downgrade_reason` in its `[compare]` block, or the suite fails to load.
See `tests/conformance/filter/vaco-filter-scope-text-ceiling.toml` for
the shape.

### Normalisation

Normalisers are named, individually enabled per case, and listed in the
manifest. **There is no implicit normalisation.** The review question for every
proposed one is: *"name a bug this would conceal."* If the answer is non-empty
it is not a normaliser, it is a divergence-allowlist entry with a scope, and
that carries much heavier governance. Each variant in `normalise.rs` carries its
answer in its doc comment.

Prefer an *invocation* normaliser to an *output* one whenever both would work.
Deleting a difference is worse than never creating it.

### The divergence allowlist

`crates/tool/vaco-conformance/divergences.toml`. It is a **governed register,
not a suppression file**, and it ships empty on purpose — every entry is a place
the harness has been told to stop proving something.

Seven categories: `identification`, `wallclock`, `encoder-nondeterminism`,
`float-lastbit`, `upstream-bug`, `unimplemented`, `unexplained`.

Seven anti-rot mechanisms, five of them machine-enforced at load time:

1. **No wildcards in scope.** `field = "*"` or `suite = "*"` is a load error. A
   divergence you cannot localise is a bug you have not understood.
2. **Category caps with a ratchet.** A category with entries and no cap is also
   a load error — an uncapped category is an uncapped dumping ground.
3. **Expiry.** `review_by` in the past fails the load. Renewal is a PR with a
   fresh justification, which forces someone to re-argue the case annually.
4. **Dead-entry detection.** Every entry counts its hits per run; entries that
   suppressed nothing are printed as deletion candidates.
5. **CODEOWNERS + approvals.** Process, plus a machine check that
   `unexplained` and `upstream-bug` carry two approvers.
6. **Blast-radius report.** An entry suppressing more than 2% of a run is
   flagged regardless of its expiry — the scope is wrong or the divergence is
   systemic.
7. **Publication.** The table ships in the release notes. Nobody wants to
   explain a sprawling suppression list to users.

A justification shorter than 40 characters is rejected: §1.4.2 expects an
argument a reviewer can accept, not a note to self.

### Reference pinning (QA-03)

`crates/tool/vaco-conformance/refspec.toml`, compiled into the binary so the pin
travels with the harness.

| Channel | Version | Role |
|---|---|---|
| `stable` | **8.1** | gates; failures block |
| `next` | 8.2 | advisory, nightly; drift is triaged weeks before the bump |
| `previous` | 8.0 | the multi-version assertion of §1.6.2 |

**Why 8.1 and not 8.0**, against plan 13 §1.6.1's worked example: version skew
is not hypothetical in this repository. `planning/research/05-fftools-cli.md`
was compiled from an **8.0.git** source snapshot and carries a corrections block
listing five statements that black-box runs against an **8.1** binary proved
wrong — root child order, `-show_entries 'format='`, `-byte_binary_prefix`,
`packets_and_frames` in `-sections`, and `-stats elapsed=`.
`planning/14-cli.md`, which is the document our CLI is actually implemented
from, states that every "OBSERVED" line comes from 8.1 and resolves every
conflict in favour of it. **The behaviour we implement is 8.1 behaviour**, so
8.1 is what must gate. Pinning 8.0 would fail CI on five behaviours we
deliberately match, and 8.0.git is a git snapshot, which §1.6.1 itself forbids
as an unreproducible oracle.

Those five deltas are recorded as `[[known_drift]]` entries so a contributor
running against 8.0 gets an explanation instead of a mystery.

A version that is neither pin is **advisory**: suites run, findings are printed,
and nothing blocks. `VACO_CONFORMANCE_STRICT=1` turns that into a hard error for
CI.

### Graceful absence

No reference installed means every case is `Skipped` with a message naming what
to install and which environment variables would point at it. `cargo test`
passes. This is a requirement, not a courtesy — a harness that a contributor
cannot run is a harness that gets ignored.

Skips are counted and reported. Coverage that erodes quietly is worse than
coverage that was never claimed.

---

## Table extractors

The part that is useful today. Each asks the oracle a question whose answer is a
fact about observable behaviour, parses it, and diffs it against our table.

| Extractor | Oracle | Direction |
|---|---|---|
| `pixfmt` | `ffprobe -show_pixel_formats` | both ways |
| `pixfmt-cross` | `ffmpeg -pix_fmts` | both ways |
| `pixfmt-geometry` | one raw frame's byte count vs `PixFmt::plane_layout` | ours → oracle (`--deep`) |
| `colors` | `ffmpeg -colors` | both ways |
| `frame-sizes` | `ffprobe -f lavfi -i color=s=<name>` | **ours → oracle only** (`--deep`) |
| `frame-rates` | `ffprobe -f lavfi -i color=r=<name>` | **ours → oracle only** (`--deep`) |

Two limitations, stated rather than papered over:

- **Neither pixel-format listing exposes a plane count.** `nb_components` is not
  it — `yuyv422` has three components in one plane. Plane count is checked only
  indirectly, by the geometry probe: ask the reference to write one 64×64 frame
  as `rawvideo` and compare the byte count against `PixFmt::plane_layout`. That
  is the only check that exercises our *arithmetic* rather than our copy of the
  metadata, and it is the one that would catch a table that is self-consistently
  wrong.
- **The frame-size and frame-rate checks are one-directional.** The reference
  has no listing command for those abbreviations, so the harness can prove that
  every name we accept means what the reference thinks it means, but it cannot
  enumerate names the reference knows and we lack. Recovering those would mean
  reading the parser, which the bright-line rule forbids. Mitigation: a `SUSPECTED`
  candidate list of names we do not have, probed on every deep run so that any
  the reference accepts is reported as a gap. A contributor who learns of a name
  from the reference's *user documentation* (Tier A) adds it there.

**Extractors report precisely and decide nothing.** They never edit a table and
never suppress a difference; suppression is the register's job and it is
governed. A finding is raised with the table's owner, who decides which side is
wrong.

---

## Findings — the first run, against FFmpeg 8.1

Recorded here because they are the point of the exercise, not because they are
settled. **The divergence register is empty and stays empty until a table's
owner decides which side is wrong.** `vaco-conformance tables --deep` reproduces
every line below.

### The result that matters most

`pixfmt-geometry` compared, for **205 formats**, the byte count of one 64×64
`rawvideo` frame written by the reference against `PixFmt::plane_layout`.
**204 of 205 agree.** That is an independent check of plane count, `step`,
subsampling and stride — our *arithmetic*, not our copy of the metadata — and it
is the strongest statement anyone can currently make about `vaco-pixfmt`. Forty-six
formats have no `rawvideo` conversion path in the reference and were not probed;
that is a coverage gap, not a divergence.

The judgement calls `docs/model/vaco-pixfmt.md` flags as unvalidated:
`nv20le`/`nv20be` and `v30xle` **agree** on both listings and on geometry
(16384 bytes at 64×64, matching our model). `v30xbe`/`xv30be` cannot be written
as `rawvideo` at all, so their geometry remains unchecked. Note that the probe
cannot see `shift`, so the specific `nv20` question — right- versus left-aligned
— is still open.

### `vaco-pixfmt` — name-set divergences (3 + 2)

| Ours | Reference 8.1 | Note |
|---|---|---|
| `amf_surface` | `amf` | a CLI-visible name; `-pix_fmt amf` would be rejected by us |
| `videotoolbox` | `videotoolbox_vld` | same |
| `cuarray` | *(absent)* | we have a format 8.1's listing does not carry |

All three are hardware-surface formats, so nothing decodes differently — but the
names are an interface surface (D9) and a script that passes one of them gets a
different answer from each program.

### `vaco-pixfmt` — field divergences

| Formats | Field | Ours | Reference | Reading |
|---|---|---|---|---|
| all 12 `bayer_*` | `nb_components` / `bit_depths` | `1` / full sample depth | `3` / `2-4-2` (8-bit), `4-8-4` (16-bit) | **deliberate.** `docs/model/vaco-pixfmt.md` records the choice: a colour-filter-array mosaic is one sample per pixel and demosaicing is a filter's job. `bits_per_pixel` agrees either way. Needs an allowlist entry, not a fix. |
| `bgr8` | `bit_depths` | `2-3-3` | `3-3-2` | **likely ours.** `rgb8` agrees at `3-3-2`, so the two members of the pair are modelled inconsistently. Components are indexed by logical channel (0=R, 1=G, 2=B) in both models, so a 3:3:2 packing should read `3-3-2` whichever byte order it is stored in. |
| `pal8` | `alpha` | `0` | `1` | **likely ours** — see the geometry line below; the palette carries alpha. |
| `pal8` | `frame_bytes` (geometry) | 4096 | 5120 | **likely ours.** The 1024-byte difference is a 256-entry × 4-byte palette. We model `pal8` as one 4096-byte plane; the reference emits the palette alongside it. Together with the `alpha` line this is one finding, not two. |
| `xyz12le`, `xyz12be` | `rgb` | `1` | `0` | **arguable.** We set both `XYZ` and `RGB`; the reference sets neither. Since we have a distinct `XYZ` flag, the question is whether `RGB` should mean "stored like RGB" or "is RGB". A caller branching on `is_rgb()` gets a different answer from each. |
| `v30xbe`, `xv30be` | `bitstream` | `0` | `1` | **suspect the oracle, do not guess.** The reference marks only the **big-endian** members as bitstream formats; `v30xle` and `xv30le` are `0` on both sides. An endianness-dependent bitstream flag has no obvious meaning, and both formats also fail to write as `rawvideo` while their LE siblings succeed. This is a §1.7.3 step-5 escalation, not something to match by inspection. |

`-pix_fmts` and `-show_pixel_formats` agree with each other on every format they
both describe, once the zero-component rendering artifact is handled (the column
listing prints a lone `0` where the section listing prints nothing).

### `vaco-core` colours

| Name | Ours | Reference 8.1 | Reading |
|---|---|---|---|
| `mediumpurple` | `#9370db` | `#9370d8` | **the reference disagrees with the standard.** SVG 1.1 / CSS Color 3 define MediumPurple as `#9370DB`. |
| `palevioletred` | `#db7093` | `#d87093` | same; the standard is `#DB7093`. |

Both are one hex digit apart in the same position, which is the signature of a
transcription slip somewhere upstream. This is the `upstream-bug` category if we
keep our values, and a compatibility decision either way: `-fill_color
mediumpurple` currently produces different pixels in the two programs.

Seven names we accept and the reference does not: `grey`, `darkgrey`,
`dimgrey`, `slategrey`, `darkslategrey`, `lightslategrey`, `lightgray`.
**Confirmed behaviourally**, not merely inferred from the listing — the
extractor asked the parser directly and all seven were rejected at the CLI. Our
set is a strict superset of theirs (147 ⊃ 140), so every reference-valid command
means the same thing in both. Worth knowing: the reference's own spelling choice
is inconsistent — it takes `Gray` for six of the pairs but `LightGrey` for the
seventh.

### `vaco-core` frame sizes and rates

**Clean.** All 53 size abbreviations and all 8 rate abbreviations resolve to
exactly the same values in both programs, confirmed by asking the reference to
build a source with each one. None of the 18 `SUSPECTED` candidate names were
accepted by the reference, so no gap in our tables was found — subject to the
one-directional limitation above.

---

## How to change it

### Adding a test case

Cases are **data, not code**. Add a stanza to a suite manifest under
`crates/tool/vaco-conformance/suites/` (or `tests/conformance/` at the
repository root, which wins when it exists):

```toml
schema = 1
suite  = "probe-isobmff"
tool   = "probe"          # probe | transcode | play-headless
tier   = "core"           # smoke | core | full | exhaustive | manual
owner  = "@isobmff-owner"

[[media]]
id     = "h264-aac-30f"
source = "corpus://vaco/mp4/h264-aac-30f.mp4"
tags   = ["video", "audio"]

[[axis]]
name   = "writer"
values = [
  { id = "json", argv = ["-of", "json"] },
  { id = "xml",  argv = ["-of", "xml"], tier = "full" },
]

[[exclude]]
when   = { writer = ["xml"] }
reason = "documented as incompatible with -sexagesimal"

[compare]
mode    = "exact-bytes"
capture = ["stdout", "exit-code"]
timeout = "20s"

[normalise]
invocation = ["bitexact", "hide-banner"]
output     = ["line-endings"]
```

Cases are the cartesian product of the axes, minus the exclusions. Case ids come
out as `suite/media/axis=value,axis=value`, in declaration order, which is what
makes them stable enough to paste into a bug report.

The inner loop is `vaco-conformance explore -- <argv…>`: it runs the oracle and
shows you the output, and it writes nothing to the repository. Copy the stanza
in by hand — that keeps a human in the loop on every case that lands.

**Three rules the loader enforces**, and all exist to keep the harness honest:

1. A non-empty normalisation chain requires mode `exact-bytes-normalised`, not
   `exact-bytes`. Keeping the modes distinct is what makes the permitted
   blindness visible in review.
2. `structured-diff` requires a `downgrade_reason`. C6 is weaker than C0 by
   construction and must never be used to launder a failing C0 case.
3. A `filter`-tool suite using `behavioural` requires a `downgrade_reason`
   too — the same move rule 2 makes, for the same reason: filter output is
   pixel data with an exact-or-not answer, so settling for outcome-class-only
   needs a declared justification, not a bare `mode = "behavioural"` a future
   reader has to reverse-engineer.

### Adding a filter test case

`filter`-tool cases do not use CLI-flag argv — see "The `filter` tool"
above for the nine-token (plus extra-input groups, for a multi-input
filter) convention `filterexec.rs` expects. A minimal, single-input case:

```toml
schema = 1
suite  = "filter-my-crate-exact"
tool   = "filter"
tier   = "core"
owner  = "@my-crate-owner"

[[media]]
id       = "gray8-64"
source   = "generated://gray8-64.raw"
tags     = ["video"]
generate = ["-f", "lavfi", "-i", "color=c=gray:s=64x64:d=1:r=25",
            "-frames:v", "1", "-pix_fmt", "gray8", "-f", "rawvideo"]

[[axis]]
name   = "filter"
values = [
  { id = "myfilter", argv = ["{media}", "myfilter", "opt=value", "gray8", "64", "64", "gray8", "64", "64"] },
]

[compare]
mode    = "raw-exact"
timeout = "10s"

[normalise]
invocation = ["bitexact", "hide-banner"]
```

If your filter can never be byte-identical for a structural, permanent
reason (the same D7 font-table ceiling `datascope`/`pixscope` carry, or
something new), use `mode = "behavioural"` with a `downgrade_reason`
explaining *why* and *what* the divergence is — see
`tests/conformance/filter/vaco-filter-scope-text-ceiling.toml`. Do not
reach for `behavioural` to avoid a `raw-exact` failure you have not
diagnosed yet; that is the "a failure nobody looks at again" failure mode
this rule exists to prevent, not a shortcut it offers.

If your filter crate is not yet in `filterexec::REGISTRIES`, add it —
one line, reviewed like any other code change.

### Transcode suites and the `{output}` token

`tests/conformance/transcode/` (XF-03, issue #211) drives the `transcode`
tool — `vaco` against `ffmpeg`, not `vaco-probe` against `ffprobe` — and
became possible only once the CLI resolved muxers through the registry
instead of always building a null muxer.

A transcode case needs a place to write its output that the harness can then
read back and diff, and `{media}` is read-only. `{output}` (bare, meaning
`out.bin`) or `{output:<name>}` (e.g. `{output:out.mkv}`) resolves to a path
inside **a subdirectory private to that side of the comparison** —
`Runner::run_case` gives `ours` and `theirs` separate directories before
substitution, because both binaries run the identical argv, and if both
wrote `out.mkv` into the same directory the second run would silently
overwrite the first one's file before anything compared them. Declare
`capture = ["output-file", "exit-code"]` under `[compare]` to have
`exact-bytes` diff that file's raw bytes instead of (or alongside) stdout —
this is `Capture::OutputFile`, wired through `Pair::ours_output_file` /
`theirs_output_file` in `compare::exact`.

**The bitexact flags are positional for this tool, and the manifest-level
name for it matters.** `-fflags`/`-flags` are *per-file* options: prepended
before `-i` (where a naive normaliser puts them) they configure the *input*,
not the output, and a Matroska mux keeps writing a random Segment UID —
measured directly, two runs of the identical reference command line
differing by ~60 bytes. `Chain::positional_suffix` inserts them immediately
before the output path instead (every transcode suite here ends its own argv
with the `{output...}` token for exactly this reason). Use invocation
normaliser `bitexact-copy`, not `bitexact`, for any `-c copy` case: `vaco`
does not currently parse a bare `-flags` option at all (`Unrecognized option
'flags'`, exit 8), and `bitexact-copy` emits only `-fflags +bitexact` —
confirmed sufficient for two-run byte determinism on every `-c copy` remux in
this directory, because `-flags` selects encoder/decoder bitexact behaviour
and a stream copy invokes neither. Reach for plain `bitexact` only for a case
that actually encodes.

**The `media` pseudo-axis.** A remux matrix is naturally "one axis of input
containers, one axis of output containers", and several `[[media]]` entries
already give a suite an input dimension the same way `[[axis]]` gives it an
output one — `Suite::expand` loops over declared media exactly like it loops
over an axis's values. `[[exclude]]` can bind `when = { media = [...], ... }`
to say a given input cannot reach a given output, which real muxers disagree
with the reference about constantly: MPEG-TS auto-converts a length-prefixed
H.264 stream to Annex-B when muxing, AVI does not and rejects it outright;
AVI's demuxer sets no PTS on its packets, and Matroska/MPEG-TS refuse to
write a packet with none. Every exclusion in `remux-bitexact.toml` quotes the
actual `ffmpeg 8.1` stderr line it is built from.

**`behavioural` (C7) compares outcome class, not literal exit code, and the
harness's own pre-check used to make that impossible.** `compare::evaluate`
checks `ours.exit != theirs.exit` before dispatching to any mode — correct
for the byte-comparing modes, where §1.2 C0 calls exit-code equality a
co-assertion "on every mode". But two independent codebases essentially never
choose the same integer for "I rejected this input" (measured: `vaco` and
`ffmpeg` produced 183, 218, 234 and 0 across one ten-case suite, no two
*failing* codes matching except by coincidence), so a literal pre-check
before `Compare::Behavioural` ever got to classify accepted/rejected/
signalled made that classification dead code exactly where C7 is supposed to
apply. `evaluate` now exempts `Behavioural` from the literal check; the class
comparison inside `behavioural()` still sees both exit codes and still
diverges across the accept/reject boundary, just not on every mismatched
integer within "both rejected".

### Corpus-backed suites (JVT/JCT-VC, QA-04/QA-09, #427/#442)

`tests/conformance/transcode/h264-jvt-conformance.toml` and
`hevc-jctvc-conformance.toml` are the first suites whose media comes from
`vaco-corpus` rather than from `generate`. Two new pieces make that work:

- **`source = "corpus://<name>"`** on a `[[media]]` entry resolves `<name>`
  against `vaco-corpus`'s embedded `vaco-media.lock` and fetches it through
  the shared content-addressed `Store` (`MediaCache::corpus_path` in
  `runner.rs`) — a cache hit never touches the network; a miss does only
  under `VACO_CORPUS_NETWORK=1` (`vaco_corpus::NetworkPolicy::from_env`).
  When the lock entry names an archive `member` (every JVT/JCT-VC
  conformance ZIP does — the archives also ship a decoder trace log and/or a
  reference YUV this project has no use for), only that one file is
  extracted (`vaco_corpus::zip`, this crate's own from-scratch ZIP/DEFLATE
  reader — see that module's doc for why it isn't a `miniz_oxide` dependency:
  D11 already gives that crate one owner, `vaco-demux-matroska`).
- **`{corpus:<name>}`** is a token usable *inside a `generate` command*,
  resolved by `MediaCache::resolve_corpus_tokens` before the reference
  synthesises the media. This exists because every JVT/JCT-VC bitstream is a
  raw Annex-B elementary stream with no container-level timestamps, and
  `vaco`'s transcode pipeline currently requires every packet reaching its
  filtering stage to carry one — a direct `-i foo.264` run of `vaco` fails
  with `"this container needs timestamps and the packet has none"` before a
  single frame reaches the codec under test. That is a demux/pipeline gap
  (`vaco-demux-raw`'s raw-elementary-stream demuxer is *correct* here — its
  own module doc records that the reference's own `h264` demuxer produces
  the identical `pts=N/A, dts=N/A` for every packet; the gap is that nothing
  downstream synthesises a PTS from a packet's own duration when *neither*
  timestamp is ever set, only from an existing DTS under `+genpts` — see
  `vaco-format-core::time::TimestampFixer`'s R20), outside this crate and
  not a codec defect, so both manifests route their `generate` through the
  reference's own `-c copy` into an MP4 first:
  ```toml
  generate = ["-fflags", "+genpts", "-i", "{corpus:jvt-h264-canl1-sva-b}",
              "-c", "copy", "-f", "mp4"]
  ```
  `-c copy` changes no NAL unit, only container framing, so this sidesteps
  the gap without weakening what is under test — confirmed by hand:
  `jvt-h264-canl1-sva-b`'s direct-elementary-stream reference decode and its
  MP4-wrapped `vaco` decode are byte-identical.

Both suites are `tier = "full"` (nightly), not `core`, because a corpus
fetch needs `VACO_CORPUS_NETWORK=1` explicitly and `smoke`/`core` runs do
not set it. Both are a **curated subset**, not a full mirror of either
suite — see `vaco-media.lock`'s `jvt-h264`/`jctvc` section headers for
exactly what was left out and why (disk/bandwidth, chosen to maximise
construct coverage per byte rather than to approach either suite's full
size).

### Adding a comparison mode

A `Compare` variant in `case.rs`, its kebab-case name in
`Compare::from_manifest`, an arm in `compare::evaluate`, and a submodule.
Do not implement a mode by downgrading it.

### Adding a normaliser

A variant in `normalise.rs`, a name in `parse`, the behaviour, a unit test, and
an answer to *"name a bug this would conceal"* in its doc comment.

### Adding an allowlist entry

Argue for it in `divergences.toml` with a concrete scope, a category, a rule, a
justification a reviewer can accept, an owner, an issue, and a `review_by` date.
Get the approvals the category requires. Expect to re-argue it in a year.

### Bumping the reference

`refspec.toml` plus the drift triage: run the full tier against both pins, and
sort every line whose *reference output* changed into `follow`, `regression`,
`intentional-change`, or `harness-artifact`. Never edit `stable` by hand without
that.

---

## Configuration

| Variable | Effect |
|---|---|
| `VACO_REF_FFMPEG`, `VACO_REF_FFPROBE` | point at a specific reference build instead of searching `PATH` |
| `VACO_REFSPEC` | use a different pin file (for testing a bump) |
| `VACO_DIVERGENCES` | use a different divergence register |
| `VACO_CONFORMANCE_SUITES` | use a different suite directory |
| `VACO_CONFORMANCE_STRICT` | treat an unpinned reference as a hard error |
| `VACO_CONFORMANCE_DEEP` | run the per-format and per-abbreviation probes in `cargo test` |
| `VACO_BIN_PROBE`, `VACO_BIN_VACO`, `VACO_BIN_PLAY` | point at our binaries instead of searching `target/` |

Commands:

```
vaco-conformance tables [--deep] [--strict]            differential checks on our static tables
vaco-conformance refbin                                what is installed, and does it gate
vaco-conformance run [--suite S] [--tier T] [--case ID] run declared suites
vaco-conformance divergences                           the register and its health
vaco-conformance explore -- <argv…>                    interrogate the oracle
```

`--case <id>` reproduces exactly one case — the id printed with every failure,
and what `just conformance-run '<id>'` actually invokes — and bypasses the
tier filter entirely rather than requiring `--tier exhaustive` too, so a case
declared `tier = "manual"` is still reproducible by pasting the one line a
failure gave you.

Exit codes: `0` clean (or advisory, or reference absent), `1` unexplained
findings against the gating pin, `2` a usage or load error.

---

## Dependencies

- `vaco-core` — `parse::color_names`, `parse::image_size`, `parse::video_rate`,
  `Rational`; the tables the extractors check.
- `vaco-pixfmt` — `PixFmt`, `PixFmtDescriptor`, `PixFmtFlags`,
  `PixFmt::plane_layout`; the table the extractors check.
- `tempfile` — a scratch directory per case.
- The reference binary itself, at run time only. It is a **test dependency**: it
  is never linked, never vendored, and never placed in a release artifact.

### Why there is a hand-written TOML reader

`src/toml.rs` implements the subset of TOML that the manifests, the pin file and
the register use. The workspace dependency list (D10) declares no TOML crate,
and adding one is a reviewed decision that is not this crate's to make. The
subset is deliberately small and rejects everything outside it — hex integers,
dotted assignment keys, date-times — so a manifest using an unsupported
construct fails loudly the first time anyone writes it rather than being
silently mis-parsed. If the manifests ever need more than this, the right move
is to request a workspace dependency, not to keep extending a bespoke parser.

---

## Deviations from plan 13 §1, and why

| Plan says | We do | Why |
|---|---|---|
| `stable = 8.0`, `next = 8.1` (§1.6.1 example) | `stable = 8.1`, `next = 8.2`, `previous = 8.0` | plan 14 implements 8.1's observed behaviour; pinning 8.0 would fail CI on five behaviours we deliberately match, and 8.0.git is a snapshot, which §1.6.1 forbids |
| `tools/refbin/refspec.toml` | `crates/tool/vaco-conformance/refspec.toml` | the crate owns its pin; `tools/refbin/` is outside this crate's scope, and `include_str!` means the pin cannot go missing |
| `tests/conformance/divergences.toml` | `crates/tool/vaco-conformance/divergences.toml` | same; the repository-root location is still honoured for *suites* when it exists |
| §1.2 says "eight modes … C0–C8" then lists ten | ten modes, C0–C10 | an editing artifact in the plan; §1.10 and §1.11.2 add C9 and C10 explicitly |
| §1.4.2 calls them "the six categories" and lists seven | seven | same kind of artifact; the table itself has seven rows |
| Divergence rules use regexes (`ours_pattern`) | substring rules (`ours_contains`) | no regex crate in the workspace dependency list; a substring is enough for every rule shape the plan's own examples use |
| Every tool compares two subprocesses (implied by §1.9's `Runner` design, written before FT-6.1 existed) | `filter` compares one subprocess (the reference) against one in-process `vaco_filter_core::Graph` run | there is no `vaco -vf` CLI yet to be the second subprocess (a separate, larger FT-2.x task); `filterexec.rs` gets a real signal today instead of a permanent skip, and `Runner::run_filter_case` still ends at the same shared `compare::evaluate` every other tool uses |
