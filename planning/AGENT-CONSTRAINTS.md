# Agent constraints — the one page

**Read this instead of the plans.** Your brief names any deeper section you
actually need.

This exists because the briefs used to say "read `00-decisions.md`,
`10-architecture.md`, `13-correctness.md`, `18-formats.md`,
`19-parallel-execution.md`" — about **9,100 lines and 110k tokens** — before a
line of code. Every one of the ~220 turns that followed then carried all of it.
Measured 2026-08-23: the gates are sub-second to seven seconds and builds are
cached, so prerequisite reading and the context it leaves behind were the
dominant cost of a ~50-minute agent run, not research.

If something here is not enough to decide, the plans are still there and your
brief will usually point at the section. Guessing when this page is silent is
the one thing that is worse than reading.

## Scope

- **You own the crates your brief names, and write nowhere else.** Read
  anything. If you need a change in a crate you do not own, **stop and report**
  — do not work around it. That rule is what keeps six concurrent agents from
  corrupting one shared tree.
- **Commit your own work.** Never `git add -A`, never a bare `git commit`, never a directory
  pathspec, and never `stash`, `checkout`, `reset --hard` or `rebase` — those
  reach other agents' uncommitted work.

  The recipe lives under "`git add` then `git commit` will commit other agents'
  staged files" below.

  This rule used to read "run no git commands; the orchestrator commits". It
  was true when one orchestrator served a handful of agents and it stopped
  being true at six: the orchestrator became a bottleneck, and worse, committing
  on an agent's behalf is what caused every one of the four work-absorption
  incidents recorded below. You know which files are yours; a commit made for
  you by someone reading `git status` does not.

  If a brief you are given still says "run no git commands", the brief is
  stale — this page wins, and say so in your report.
- Do **not** run `cargo fmt --all` — it reformats other agents' uncommitted
  work. `cargo fmt -p <your-crate>`.

## Code rules (enforced, not advisory)

- `#![forbid(unsafe_code)]` workspace-wide (**D2**). No exceptions outside
  `vaco-hw-*`.
- `unwrap_used`, `expect_used`, `panic`, `indexing_slicing` are **DENIED**.
  `#[allow]` needs a `reason = "..."`.
- Overflow must not panic under the fuzz profile, which enables the checks
  deliberately.
- Benchmarks use **`divan`**, never criterion.
- Every public item gets a doc comment saying *why*, not restating the name.

## The manifest trap — the single most disruptive mistake available to you

Three ways to break manifest parsing, which is **workspace-wide**: every cargo
command fails for **every** agent until you fix it, not just yours.

1. A `[[bench]]`/`[[test]]`/`[[bin]]` declared before its file exists.
2. A `Cargo.toml` with no `src/lib.rs`.
3. A crate directory the `crates/*/*` glob matches with no `Cargo.toml`.

There is no ordering that avoids all three, which is why the repair script
exists rather than a rule. This has blocked the workspace **nine times**, once
for 25 minutes with five agents idle.

**If any cargo command fails with a manifest error, run
`python3 scripts/unblock-manifests.py`** — it fixes all three without needing
cargo to work.

## Export your descriptor **before** writing `vaco-component.toml`

The generated registry contains a resolution check for every `ctor`, so a
fragment naming a const your crate has not exported yet does not merely fail for
you — it breaks **`vaco-registry`**, which almost everything depends on. That
takes down `wasm-check`, `patent-gate` and all fuzzing for **every** agent in
the tree.

It has happened: two crates each shipped a fragment naming a `MUXER` const
before exporting one, and the registry stayed broken for most of an unrelated
agent's session.

`gen-registry` now refuses the row and tells you, rather than emitting code that
breaks everyone. Same failure, local instead of global.

## Generated files — never edit these by hand

| file | regenerate with |
|---|---|
| the component registry | write a `vaco-component.toml` fragment, then `cargo xtask gen-registry` |
| `fuzz/Cargo.toml` | add `//! fuzz-crate: <crate>` to your target, then `cargo xtask gen-fuzz` |
| `docs/README.md` | `cargo xtask gen-docs-index` |
| `vaco-pixfmt`'s tables | `cargo xtask gen-pixfmt` |

## Layering (**D14.1**, enforced by `cargo xtask layer-check`)

A `vaco-format-*` or `vaco-demux-*` crate may **not** depend on a
`vaco-parse-*` crate. Reach a parser through `vaco-registry`'s
`ParserProvider`, which is what it is for. `vaco-demux-mp4` is the worked
example.

Three agents have independently refused briefs of mine that offered such a
dependency. They were right to.

## Measure, do not recall (**D17**)

The reference binary is a black box you probe; its source is never consulted
(**D7**). **Recording observed behaviour of a shipped binary is not copying
expression** — that is what makes probing clean-room, and it licenses more than
people assume: `-pix_fmts`' entire row order was captured that way.

Things that were "obviously" true here and were not:

- `coded_width` is the **display** size for H.264 and the **coded** size for
  HEVC. AV1 has no such split at all, and no `yuvj` family.
- `bits_per_sample` follows the **codec**, not the container: AAC's MP4 sample
  entry says 16 and the reference prints **0**.
- `codec_tag`'s minimum width is **four**, not eight — and a unit test had
  pinned the wrong value, so the test was holding the bug in place.
- The default-stream-selection bonus for `default` is exactly **5,000,000
  pixels**, which only fell out of bisecting two probes that appeared to
  contradict each other.
- **`-colorspaces` does not exist.** A brief of mine said it did.

**Two probes that disagree are not noise. They are a bound on a constant nobody
wrote down.**

### Two traps that produce confidently wrong measurements

- **A pipe swallows the exit code.** The usual `${PIPESTATUS[0]}` repair is
  *bash*; zsh spells it `$pipestatus` and 1-indexes it, so in zsh the bash form
  expands to empty and the comparison **silently succeeds**. Probe with
  `bash -c`, or no pipe.
- **An option's position decides which object it lands on.** `-fflags +bitexact`
  before `-i` sets it on the *input*, so Matroska keeps writing random UIDs and
  two runs differ by ~60 bytes — which reads as "the muxer is nondeterministic".
  When a probe implies the reference is doing something surprising, check the
  flag's position before believing it.
- **`-bitexact` and `-fflags +bitexact` are different flags** and it is easy to
  write one meaning the other, as this document previously did. `-fflags
  +bitexact` is marked `E` in the reference's own flag columns — **encoding
  only, no effect on demuxing at all**. The flag that suppresses every
  `*_long_name` in `ffprobe` output, and that every `exact-bytes` conformance
  case depends on, is the *top-level* `-bitexact`. Measured while wiring the
  generic format options, after a brief asked what `-fflags +bitexact` changes
  on the demux side. The answer is nothing.

## An oracle you wrote shares your misreading

`vaco-codec-dsp-idct` checked HEVC's inverse transform against a separately
written Python transliteration *and* against the well-known H.264/HEVC integer
cores. Both passed. Both were wrong — the literal reading of equation 8-317
reproduces those famous cores and still computes the wrong transform.

**A second implementation is only an oracle if it can be wrong differently.**
Two transcriptions of one sentence cannot. When the specification is the only
source, the independent check must be **a property the output must have** — a
DC-only block must produce a uniform output — not another route to the same
numbers.

## Never pin the absence of something the project is building

Five tests have now failed **on success**: `vaco-cli` asserted "this build has
zero muxers" (twice), "`-h muxer=matroska` is unknown", "`-h protocol=file` is
unknown", and `vaco-probe` asserted a specific format was demux-only. Every one
of them was true when written and became false the day the gap it described was
closed — which is the least useful day for a test to fail, because it costs the
agent who *fixed* something a debugging session.

The fix is always the same shape: assert the **mapping**, not the emptiness. A
format shows `D` exactly when it demuxes and `E` exactly when it muxes; a name
the registry has is described and one it does not have is unknown; and where a
test genuinely needs an example of a gap, pick one out of the registry at run
time and skip cleanly when none is left:

```rust
let Some(name) = vaco_registry::demuxers().iter().map(|d| d.name)
    .find(|n| vaco_registry::muxer_by_name(n).is_none()) else { return };
```

## Two citations this project keeps getting wrong

**F1–F9 are in `planning/11-foundations.md`, not `16-filters.md`.** Three briefs
have now sent an agent to the wrong file for them, and they are not filter rules
at all: they are cross-cutting foundations decisions — `fuzz/` as a separate
workspace, integer-cast discipline, `Option` instead of sentinels, runtime SIMD
selection, layer-0 never naming a layer-1 type. Worth reading; just not where
the briefs said.

**There is no `14-io.md` and no `16-cli.md`.** I/O and protocols are in
`18-formats.md`; the CLI is `14-cli.md`. Both have been cited twice.

The plan index is in `AGENT-BRIEF-TEMPLATE.md`. Check a citation before writing
it into a brief — every one of these cost an agent a detour.

## `git stash` in a shared tree is a loaded gun

An agent ran `git stash` once to A/B-test a behaviour, recognised immediately
that it violated the rule, and restored with `git stash pop` in the same turn.
Nothing was lost, and self-reporting it was exactly right.

But it is worth being concrete about what was at stake: **four other agents had
uncommitted work in that tree at that moment** — three whole new crates among
it — and `git stash` would have swept every file of it away, in a directory none
of them would think to look in. They would have found their work simply gone,
mid-run, with no error.

That is why the rule is "no git commands" rather than "no destructive git
commands". `stash` does not read as destructive. In a shared tree it is.

To A/B-test a change, copy the file aside and copy it back.

## An empty collection at construction is not an answer

Twice in one week, in unrelated crates:

- **FLV reported zero streams for every file.** Streams are created by the first
  tag that needs one — right, since FLV declares nothing until then — but
  `Demuxer::streams()` is asked before any packet is read, so it answered with
  the empty list it had been built with. `ffprobe` printed `nb_streams=0` on
  files the reference reads perfectly.
- **Fragmented MP4 over a pipe produced zero packets, always.** A reader was
  marked `finished` at construction whenever its entry list started empty — and
  on a non-seekable source, empty means *"I have not looked yet"*, not
  *"there is nothing here"*.

Both crates' own tests passed, because both read packets before asking. The
differential harness found the first; a test written for something else found
the second.

**If a field starts empty because you have not populated it yet, do not let
anything read it as a result.** Either populate it before the first observer can
ask — bounded, since the input is untrusted — or make "not yet known"
distinguishable from "known to be nothing" in the type.

## Detection and demuxing ask different questions

A demuxer is *deliberately* forgiving. `vaco-demux-raw`'s `obu::temporal_units`
reports the whole buffer as a single span when nothing parses, so a caller
reading a damaged AV1 file still gets a packet instead of silence. That is the
right call — for demuxing.

The AV1 probe then asked `!temporal_units(buf).is_empty()` as its detection
test, and that fallback makes it true for **any** non-empty input. A plain text
file scored 51 as `av1`, claimed the input, and `vaco -i notmedia -f null -`
exited 8 where the reference exits 183 — the run got as far as *failing to find
an encoder* instead of failing to open the input, which is a much more confusing
way to be wrong.

**If your crate both detects and demuxes, the two paths need separate
functions.** Detection is strict and answers "is this plausibly mine?"; demuxing
is lenient and answers "what can I still recover?". Reusing the lenient one as
the strict one silently claims every file on the system. Test detection against
a file that is definitely *not* your format — prose is the cheapest one to hand.

## A name in the reference is not a specification

`framecrc` is not CRC-32. Measured 2026-08-23, while building the muxer the
project's whole differential story depends on: it is **Adler-32**, and the
per-packet variant seeds it `(a=0, b=0)` rather than the standard `(a=1, b=0)`.
The whole-file `crc` muxer is Adler-32 too, standard-seeded. Real CRC-32 appears
only when you ask for it by name, via `-hash crc32`.

The agent got there by trying every catalogued CRC-32 variant against the actual
bytes — all failed — then solving Adler-32's seed algebraically from the
discrepancy, then confirming it independently with `framehash -hash adler32`.
That is the shape of a real measurement: the hypothesis was refuted first, and
the confirmation came from a different direction than the derivation.

Two more from the same crate, in the same spirit: `-hash sha1` is **rejected** —
the accepted spelling is `sha160`; and a missing pts prints as the literal
`-9223372036854775808`, not `N/A`.

The general rule this is an instance of: **the reference's own vocabulary is
evidence about what it calls things, not about what it does.** When a name and a
behaviour disagree, the behaviour is the fact. Measure it.

## Performance

**Six confident performance predictions on this project have measured
backwards**, including a threading design 45–60× *slower* than serial and a
branchless CABAC decision 1.76× slower than the spec's literal shape.

Write the obvious version, benchmark alternatives side by side with `divan`,
and **report ratios, not verdicts**.

## Before you report

```
cargo check -p <crate>                 # no --locked the first time
cargo test -p <crate> --locked
cargo clippy -p <crate> --all-targets --locked -- -D warnings
cargo xtask dup-check                  # sub-second; catches name collisions early
```

The orchestrator runs the full gate sweep (`layer-check`, `wasm-check`,
`time-gate`, `patent-gate`, `owner-gate`, `unsafe-audit`, `dep-gate`, and the
four `--check` generators) at the wave boundary. **Do not run them all
yourself** unless your brief says to — they are cheap individually and the
round-trips are not.

## Fuzzing (**D6**)

A crate that parses untrusted input and has no fuzz target is not done.

Report the **exit code and the exec count**, and check
`find fuzz/artifacts -type f` is empty. Do **not** grep the log for `panicked`
— that has produced two false "clean" reports. A `slow-unit-` and an `oom-`
both exit 0, which is why the artifact check is separate.

30 seconds per target is enough during the breadth phase.

**Found an artifact? Diagnose it, do not delete it.** If you fix the bug, move
the input to `fuzz/seeds/<target>/` — that directory is committed, while
`fuzz/corpus/` is gitignored, so it is the only place a regression seed
survives. A *stale* artifact is not harmless: `find fuzz/artifacts -type f` is
in every agent's report, so it fails everyone's check until someone clears it.

## Closing issues

You are authorised to close the GitHub issues your work completed — the
repository owner asked for this directly.

**Implemented counts as done.** The shape is right, the obvious cases work,
your tests pass. Byte-identity on every field is not the bar; a comprehensive
differential and fuzzing pass is scheduled for the end, and that is where edge
cases get found.

**Judge per issue, not per crate**, and be honest in the comment. "Structure
complete, timestamps unverified against the reference" is useful. A comment
implying more coverage than it has is worse than leaving the issue open.

## A new crate's `Cargo.toml` blocks every other agent until `src/lib.rs` exists

The workspace globs `crates/*/*`. The moment you write
`crates/<area>/<new-crate>/Cargo.toml`, cargo starts trying to load it — and
until `src/lib.rs` is on disk beside it, *every* cargo command in the tree
fails for *everyone*:

```
error: failed to load manifest for workspace member `…/vaco-filter-component`
Caused by: no targets specified in the manifest
```

That is not a warning on your own build. It is a hard stop for all five other
agents and the orchestrator, for as long as the gap lasts. Measured
2026-08-23, when one agent's manifest-first ordering blocked the whole tree.

**Write `src/lib.rs` first, or write both in the same step.** A one-line
`//! TODO` stub is enough to keep the workspace loadable. The same applies in
reverse when you delete a crate: remove the directory in one move, not the
`src/` first.

## `let _ =` on a fallible seed is where bugs go to live

Two separate faults today were invisible for the same reason: something
failed, and the failure was discarded rather than reported.

`vaco-format-core`'s `build_parser` does

```rust
let _ = parser.set_extradata(&extra);
```

which is a defensible policy — extradata a parser cannot use should not kill
the stream. But it meant that when `H264Parser::set_extradata` refused every
Annex-B extradata it was ever handed, ASF reported `profile=unknown`,
`level=-99` and `pix_fmt=unknown` for *months* while holding a complete
sequence parameter set, and nothing anywhere said a word.

The rule is not "never discard an error". It is: **if you discard one, make
the discard countable.** A counter on the discovery report, a `DemuxStats`
field, a `log` line at trace level — anything that turns "silently wrong" into
"visibly degraded". When you write `let _ =` on a fallible call, ask what the
symptom would look like if that call *always* failed, and whether anyone would
notice.

The same shape appeared in `xtask`'s `wasm-check`, which reported a crate name
taken from the first `--> ` line in stderr — a line that belonged to a
*warning* from an unrelated crate. Not a discarded error, but the same class:
a plausible-looking wrong answer where a missing one would have been caught.

## The plan already partitions the filters; do not invent a crate

`planning/16-filters.md` §4.2, §4.3 and §4.4 are a table, one row per
`vaco-filter-*` crate, with the exact filter list for each. The grouping is by
*shared kernel* — `vdsp`, `adsp`, `draw`, `framesync` — which is why
`convolution` and `sobel` sit in `vaco-filter-convolve` rather than next to
`gblur` in `vaco-filter-blur`, and why the audio analysis filters sit in
`vaco-filter-aanalysis` rather than being split by what they measure.

If you are handed a filter group, find its row before you write anything. The
row is the answer to both questions a brief can get wrong — what the crate is
called, and which filters are in it — and a brief that disagrees with the row
is wrong, including one written by the orchestrator.

Measured cost of not doing this: `axcorrelate` reached two agents at once,
and the second implemented it, tested it and deleted it. See
`planning/FILTER-CRATE-DIVERGENCE.md`.

If the reference has a filter the table places nowhere, that is a change to
the table, proposed in a commit that says why — not a new crate.

## `git add` then `git commit` will commit other agents' staged files too

The index is **shared**. Every agent in this tree stages into the same
`.git/index`. So this, which looks careful:

```sh
git add crates/mine/a.rs crates/mine/b.rs
git commit -F msg.txt
```

commits *everything currently staged*, including the twenty-seven files
another agent staged thirty seconds ago and was about to commit under its own
message.

That is not hypothetical. On 2026-08-23 the orchestrator staged one planning
file, committed, and swept an entire filter agent's work — three crates, three
fuzz targets, docs and registry regeneration — into `df3a742`, a commit whose
message says "chore(planning)" and whose trailer says
`Vaco-Provenance: original`. Nothing was lost and nothing was corrupted, but
the provenance record for 3,700 lines of code is now attached to the wrong
message and the wrong provenance kind. In a project whose clean-room defence
*is* the commit trail, that is the expensive kind of harmless.

**Use a pathspec-limited commit. Never `git add` followed by a bare
`git commit`.**

```sh
git commit -F msg.txt -- crates/mine/a.rs crates/mine/b.rs
```

`git commit -- <paths>` commits the working-tree content of exactly those
paths and ignores the index for everything else, so a concurrent stage cannot
ride along. It also means you never have to remember what you staged.

Two caveats: pass the paths for files you are *deleting* too, and note that
this form takes the working tree rather than what you staged — which is what
you wanted anyway.

If you find your work has already been absorbed into someone else's commit,
**report it and do not fix it**. Rewriting shared history while five agents
hold uncommitted work in the same tree is far worse than a wrong commit
message. The orchestrator will add a correcting note.

### …and a *directory* pathspec is barely narrower than none at all

The pathspec rule above has a second half, learned the same day by breaking it:

```sh
git commit -F msg.txt -- crates/registry/vaco-registry   # still wrong
```

That commits every change anyone has made anywhere under that directory. In
`b90268c` it swept an agent's uncommitted `BsfProvider` implementation in
`crates/registry/vaco-registry/src/lib.rs` into a commit about MIME types —
the same failure the pathspec rule exists to prevent, one directory level up.

**Name files, not directories**, unless you are certain you are the only writer
in the tree. `crates/registry/vaco-registry/src/generated.rs` is the specific
trap: it is regenerated by `gen-registry`, so *every* agent touches it, and it
sits next to hand-written source in the same directory.

The generated files — `crates/registry/vaco-registry/src/generated.rs` and
`docs/README.md` — are best left out of an agent's commit entirely. Regenerate
them to check your work, then let the orchestrator sweep them in one commit
when the tree is quiet. Two agents worked this out independently today and both
were right.

### The real rule underneath all of this: two writers in one file cannot both commit

The pathspec advice above has a floor, and the orchestrator hit it twice in one
hour. `git commit -- <file>` takes the **working-tree** content of that file. If
another agent has edits to the same file, they go in — even if they carefully
staged only their own hunks with `git apply --cached`, because naming the file
overrides the index for that path. There is no incantation that fixes this.

Both incidents were the same underlying mistake, and it was not a git mistake:

* `b90268c` swept an agent's `BsfProvider` implementation out of
  `crates/registry/vaco-registry/src/lib.rs` — a file the orchestrator had no
  business touching while a bitstream-filter agent owned that work.
* `3b405a3` swept the same agent's `check_bitstream` out of
  `crates/format/vaco-mux-avi/src/mux.rs` — a file the orchestrator had
  *explicitly assigned to that agent* an hour earlier, and then edited anyway
  for an unrelated `default_video` fix.

So: **the single-writer rule is the mechanism; pathspec commits are only a
guard rail.** A sweep that touches thirty crates is exactly the kind of change
that quietly violates single-writer, because it is defined by a *property*
("every muxer's default codec") rather than by a set of files, and the property
does not care who owns what.

Before a broad sweep, list the files it will touch and check every one against
`planning/ASSIGNMENTS.md`. If any is assigned, either skip it and hand that
part to its owner, or wait. Skipping is nearly always right — the two muxers
left for the Matroska agent cost one message and no conflict at all.

And if your work does get absorbed: report it, do not fix it. Both agents did
exactly that, and both were right to.

### The commit-msg hook no longer blocks when the workspace is merely unloadable

Both of the above — a manifest with no `src/lib.rs`, and a rename before
`gen-registry` — used to block *everyone's commits* as well as everyone's
builds, because the `commit-msg` hook ran `cargo run -p xtask -- check-message`
and treated any non-zero exit as "your trailers are wrong".

It now tells the two apart. A workspace that does not load prints a loud
warning, says your trailers were **not** checked, and lets the commit through;
CI checks them later against a tree that loads. A genuinely bad trailer still
blocks, which is the whole point of the hook.

This does not make the manifest trap harmless — it still stops every build. It
just stops one agent's half-created directory from also freezing five other
agents' commits.

### Renaming a crate breaks the tree until the *registry* is regenerated

The manifest trap above has a sharper form. When you rename a crate, the stale
references are not only in your own files — `crates/registry/vaco-registry/`'s
`Cargo.toml` and `generated.rs` are **generated** from every crate's
`vaco-component.toml`, and they still name the old crate until you run
`cargo run -p xtask -- gen-registry`. Until then:

```
failed to load manifest for dependency `vaco-filter-achannel`
  No such file or directory
```

and every cargo command in the tree fails for every agent, exactly as with a
missing `src/lib.rs`, but for longer — the window lasts until you regenerate,
not until you finish moving files.

**Do the move and the regeneration as one step**, and fix the `ctor` paths in
your fragment *before* regenerating, since those are what the generator reads.

This is also the one case where an agent should commit
`vaco-registry/Cargo.toml`: the rename genuinely requires it and nobody else is
renaming your crate. `generated.rs` still stays out — the orchestrator sweeps
it.

### When you genuinely share a file: commit through a private index

Everything above says to avoid sharing a file. Sometimes you cannot — a
shared-kernel crate like `vaco-filter-vdsp` exists precisely so two callers
can add to it, and two agents will occasionally need it in the same hour.

The trap is subtle and it bit on 2026-08-23. `vaco-filter-deinterlace`
committed cleanly and left its `comb_score` addition to `vaco-filter-vdsp`
*out* of the commit, because another agent had `plane_sse` and
`identical_count` in the same file. Correct instinct — and it left **HEAD
unbuildable**, because the committed crate called a function that existed only
in the working tree. Caution about the shared file produced a broken tree.

The tool for this is a private index:

```sh
export GIT_INDEX_FILE="$SCRATCH/my-idx"        # $SCRATCH, never /tmp — see below
git read-tree HEAD                              # start from HEAD, not the shared index
blob=$(git hash-object -w "$SCRATCH/my-half-of-the-file.rs")
git update-index --cacheinfo 100644,$blob,path/to/shared.rs
git commit -F msg.txt
unset GIT_INDEX_FILE
git reset -q HEAD -- path/to/shared.rs          # settle the shared index against the new HEAD
```

**`/tmp/my-half-of-the-file.rs` is not decorative.** `git hash-object -w` takes
a *path*, and if you give it the working-tree path you hash whatever is in the
working tree — including the other agent's uncommitted edits. That defeats the
entire recipe while looking exactly like using it. It has now happened twice in
one day on `planning/TECH-DEBT.md`, once by the orchestrator, and both times the
commit message described only the committer's own work while the diff carried
somebody else's.

The append-only shared documents are `planning/CONFORMANCE-FINDINGS.md`,
`planning/TECH-DEBT.md`, `planning/ASSIGNMENTS.md` and
`provenance/sources.toml`. All four have been absorbed at least once. For any
of them, build your half from `HEAD`, never from the working tree:

```sh
git show HEAD:planning/TECH-DEBT.md > "$SCRATCH/mine.md"   # NOT the working-tree file
cat "$SCRATCH/my-append.md" >> "$SCRATCH/mine.md"
export GIT_INDEX_FILE="$SCRATCH/my-idx"
git read-tree HEAD
blob=$(git hash-object -w "$SCRATCH/mine.md")
git update-index --cacheinfo 100644,$blob,planning/TECH-DEBT.md
git commit -F msg.txt
unset GIT_INDEX_FILE
git reset -q HEAD -- planning/TECH-DEBT.md
```

The working-tree file keeps everyone's appends, including yours and theirs;
`HEAD` gains only yours. The next agent to commit repeats this and gains only
its own.

**Read `HEAD` late, and check after you commit.** Rebuilding from `HEAD` stops
you absorbing other people's work, and introduces the opposite failure: if
`HEAD` moved between your `git show` and your `git commit`, your blob no longer
contains whatever landed in that window, and committing it **silently reverts
them**. That has happened — one agent's `TECH-DEBT.md` row was undone by
another's commit built from a snapshot taken minutes earlier, and it was caught
only because the second agent grepped the resulting `HEAD` for its own content
instead of assuming the commit worked.

So: do the `git show HEAD:<path>` immediately before the commit, not at the
start of your work, and afterwards run

```sh
git show HEAD:planning/TECH-DEBT.md | grep -q "a distinctive phrase from your row"   || echo "your content is not in HEAD — rebuild from the new HEAD and commit again"
```

If it is missing, someone landed between your read and your write. Rebuild from
the *new* `HEAD` and commit again; do not rewrite their commit.

**Use your own scratch directory, never `/tmp`.** `$SCRATCH` here means the
session scratchpad path your environment gives you. This recipe used to say
`/tmp/my-idx` and `/tmp/mine.md` literally, which meant every agent running it
used the *same two paths at the same time*. One commit landed carrying the
right diff under another agent's commit message, because two agents' message
files collided in `/tmp`. Nothing was lost and the diff was correct — it was
simply labelled as somebody else's work, which is the same way the record
stops being trustworthy.

Neither failure loses work permanently — an absorbed append is misattributed and
a reverted one is still in your working tree — but both make the record lie, and
the record is the point.

Your half lands, the other agent's half stays in the working tree untouched,
and the shared index is never written. The `git reset -q HEAD -- <path>` at the
end is the one legitimate use of `git reset` in this tree: it unstages only,
never touches the working tree, and without it the shared index still holds the
*pre-commit* content for that path — so the next bare `git commit` by anyone
would silently revert you.

**Never leave HEAD unbuildable to avoid a misattribution.** A wrong commit
message is a paperwork problem; a HEAD that does not compile blocks everyone.
If you cannot split the file safely, say so and hand it to the orchestrator
rather than committing half a change.

## Two inputs does not mean `framesync` — check the option surface

Three agents in one day were told by a brief that their two-input filters
should ride on `vaco-filter-framesync`, and all three measured that they
should not. The tally so far:

| Filter | Uses `framesync`? | How it was determined |
|---|---|---|
| `overlay`, `alphamerge` | **yes** | `-h filter=` shows `eof_action`, `shortest`, `repeatlast`, `ts_sync_mode` |
| `framepack`, `mergeplanes` | no | none of those options exist |
| `maskedclamp`/`maskedmax`/`maskedmin`/`maskedthreshold` | no | measured, none expose the surface |
| `psnr`, `ssim`, `identity`, `msad` | no | measured, none expose the surface |

The rule is not arity. `framesync` exists to reconcile two inputs with
*independent timelines* — different frame rates, different end times — and a
filter that does that has the four options to prove it. A filter that simply
needs one frame from each pad per step is strict lockstep, which is what
`vaco-filter-core`'s `Paired` adapter is for.

So: **run `ffmpeg -h filter=<name>` and look for `eof_action` before reaching
for `framesync`.** It is one command and it has been the right answer four
times out of five.

## Allocate after the limits, not before them

`vaco-filter-source`'s `cellauto`, `life` and `sierpinski` each sized a working
buffer from their *options* and allocated it before any frame was requested:

```
cellauto=size=911111x91111   ->  Vec<bool> of 83 GB
```

`FramePool` enforces `vaco-limits`, but it never saw this — the allocation
happened upstream of it, in the filter's own setup. Found by fuzzing, on a
filter whose inputs are entirely attacker-controlled in any `-filter_complex`
string, and the crashing input is kept as a regression seed.

The shape generalises to every filter with a size, count or duration option:

- **A `Vec::with_capacity(n)` where `n` comes from an option is an
  attacker-controlled allocation.** So is `vec![x; n]`, `resize`, and
  collecting an iterator whose length an option decides.
- Route it through the pool or a `vaco_limits::Budget`, or clamp it explicitly
  and say what the clamp is.
- "The frame allocation is bounded" is not enough. Ask what your filter
  allocates *before* the first frame exists.

Grep your own crate for `with_capacity`, `vec![`, and `resize` and check where
each length comes from. This is the one class of bug in filter option parsing
that fuzzing reliably finds and review reliably misses.

**The workspace already denies the three obvious ones.** `clippy.toml` has
carried this since wave 0:

```toml
disallowed-methods = [
  { path = "std::vec::Vec::with_capacity", reason = "size an allocation through vaco_limits::Budget::alloc instead" },
  { path = "std::vec::Vec::reserve",       reason = "..." },
  { path = "std::vec::Vec::reserve_exact", reason = "..." },
]
```

So `with_capacity` is a clippy **error**, not a warning, and not one to
`#[allow]` past — the reason field tells you what to do instead. Two agents hit
it on the same afternoon and it blocked clippy *transitively* for everyone
downstream of their crate, because most filter crates are dependencies of
`vaco-registry`. If you trip it, fix it in an early separate commit rather than
at the end of your work.

Note what the rule does **not** cover: `vec![x; n]`, `resize`, and collecting an
iterator whose length an option decides are all still yours to check by hand.
The lint catches the spelling, not the class.


## An API with no caller is invisible to every test you will write

`cargo xtask dead-code`'s "orphans" category found 143 public items across 24
crates that nothing outside their own crate uses. Most are harmless. One of them
was this:

```text
$ vaco -i in.mp4 -c copy -f mpegts out.ts
Error while filtering: unsupported: this muxer needs a bitstream filter and no
BsfProvider was supplied
```

`vaco_registry::Bsfs` was written when gap 8 closed, and had no caller.
`PipelineSpec::set_output_bsfs` was written to receive it, and had no caller
either. Both halves of the M6 stage existed, were unit-tested, and were never
connected — so `-c copy` from an MP4 to **anything Annex-B** failed outright,
and MP4 → AVI wrote a 224-byte stub, while the whole test suite stayed green.

The shape is worth recognising because it recurs. A test written for a new API
exercises the API. It cannot notice that the *only* thing missing is somebody
calling it, because you write the caller and the test in the same head, and if
you had thought of the caller you would have written it. The gap only becomes
visible when you run the binary against a real file and compare it to the
reference.

So: **after landing an interface, run the command a user would run.** Not the
unit test — the command. Both bugs above cost one `vaco -i x.mp4 -c copy -f
mpegts out.ts` to find, and neither was findable any other way.

And when you add a public item, either call it in the same change or say in the
commit message who is going to. `dead-code`'s orphan list is a list of promises
nobody has kept yet.

## `Vaco-Spec-Ref` starts with a registered id, not with prose

The gate reads the **first whitespace-delimited token** of the trailer and
requires it to be an `id` declared by some `[[source]]` in `provenance/`. So

```text
Vaco-Spec-Ref: ffprobe -bitexact -show_entries stream=profile on H.264/AAC/VP9/AV1
```

fails on `ffprobe`, and the failure is tree-wide: `provenance-check` walks
every commit since the baseline, so one bad trailer turns the gate red for
every agent and fails CI until it is dealt with.

The form is `Vaco-Spec-Ref: <id> <free text saying what you measured>`. The free
text is encouraged — it is the part a reader actually wants — but the id has to
come first. `git log -1` on any recent commit shows a working example.

If the source you measured has no id yet, **register it in
`provenance/sources.toml` in the same commit**. Do not describe it inline
instead. The AV1 specification had been cited in prose in `vaco-demux-raw`'s
module docs for weeks and was never declared, which the gate could not see until
a commit finally referenced it by id.

If you find you have already pushed a bad trailer, do **not** rebase to fix it.
Add a row to `provenance/corrections.toml` naming the commit, the citation it
carries and the id you meant. Rewriting history in a tree this many agents share
moves HEAD under all of them mid-edit, and moving `provenance/baseline` forward
would exempt every commit in between.

## `CONFORMANCE-FINDINGS.md` is a multi-writer hotspot — commit it with a private index

Three collisions in one day, all in this one file, all the same shape: two
agents append a finding, one commits with a plain `-- planning/CONFORMANCE-FINDINGS.md`
pathspec, and the other's working-tree edit goes in under the first one's
provenance trailers. Nothing is lost and nothing is silently wrong, but the
record says the wrong person measured it.

Appending "at the very end" does not help, because everyone appends at the very
end. **Commit this file with the private `GIT_INDEX_FILE` recipe**, not with a
pathspec: read the file, stage only your own version of it through a private
index, commit, then `git reset -q HEAD -- planning/CONFORMANCE-FINDINGS.md`. The
recipe is above.

Two consequences worth knowing:

- **Numbers collide.** Two findings were numbered 41 and two 44 on the same
  afternoon, because each author read the file before the other appended. Check
  the highest number *at the moment you commit*, not when you started, and
  expect to renumber.
- **Cite findings by title, not only by number**, wherever the citation has to
  survive — issue bodies especially. A number can be renumbered out from under
  you; a title cannot.

## Comments: fewer, shorter, and about the code in front of you

Comments are 23% of this tree — 80,000 lines across 349,000. Individual files
run to 54% comment, and 263 files carry a single unbroken block of more than
forty comment lines. That is too much, and most of it was written this month.

`cargo xtask comment-check` enforces the mechanical part:

- **No comment may cite a planning document.** `CONFORMANCE-FINDINGS`,
  `INTERFACE-GAPS`, `TECH-DEBT`, `AGENT-CONSTRAINTS`, or any `planning/` path.
  Those documents get renumbered — two findings were renumbered the same day
  they were written — and a comment pointing into one is wrong from then on.
  The measurement belongs in the planning doc; the code says what it does.
- **No comment may cite an issue number.** Say what the code does, not which
  ticket asked for it.
- **No unbroken run of more than forty comment lines.**

What the gate cannot check, and matters more:

- **A doc comment says what a function is for and what its contract is.** That
  is what `///` is for and it earns its place.
- **A history lesson does not belong in the source.** "This used to do X, which
  was wrong because Y, and the reference actually does Z, measured across six
  inputs" is a *commit message*, and git already has it. Keep the one sentence a
  reader needs — "A-law stores one byte per sample and decodes to `s16`" — and
  delete the rest.
- **A comment restating the code is worse than none**, because it is one edit
  away from lying. Three bugs this month were in code whose comment described
  the correct behaviour while the code did something else, and the comment is
  why nobody looked.
- **Prefer making the code say it.** A named constant, a named function, an
  early return with a clear condition. If a block needs a paragraph, that is
  usually a sign it wants a name.

Where a genuinely surprising fact has to be recorded — a specification clause
the code would otherwise look wrong against, a measured reference behaviour that
contradicts the obvious reading — keep it, and keep it to a sentence or two.

## The primary specifications are reachable — fetch them

An agent implementing AC-3 reported that the dominant source of decode error
was a set of bit-allocation constants it "could not verify against the primary
ATSC text (no network access)". The network is reachable from this tree, and
that document is free:

```sh
curl -sSL -o /tmp/a52.pdf https://www.atsc.org/wp-content/uploads/2015/03/A52-201212-17.pdf
# 200, application/pdf, 1.77 MB
```

ITU-T, AOMedia, IETF, W3C and ATSC all publish freely, and `provenance/sources.toml`
already records a `where` URL for every source precisely because fetching them is
the expected workflow. **Before recording "could not verify against the
specification", try.**

This matters more than one codec. D7's clean-room rule says the specification is
the right input and the reference implementation's source is not; an agent that
cannot reach the specification is left with black-box probing alone, which is
enough for interfaces and framing and not enough for a masking curve or a
quantisation table. A whole class of "measured but unverified" outcomes is
avoidable.

Two things that stay true when you do fetch one:

- **Register it in `provenance/sources.toml`** with the `where` URL and the date,
  in the same commit as the first code citing it, and cite it as
  `Vaco-Spec-Ref: <id> <what you took from it>`.
- **Do not paste specification text into the tree.** Constants and structure
  implemented from a document are exactly what D7 asks for; transcribed prose is
  not, and neither is a table copied wholesale where the specification's own
  licence does not allow it. Cite the clause, write the code.
