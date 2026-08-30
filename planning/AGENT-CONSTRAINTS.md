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

## Byte-exactness is a check, not the bar (**owner ruling, 2026-08-28**)

The repository owner has ruled directly:

> byte-exact isn't a hard requirement, as long as we know it isn't broken it's
> fine. the ffmpeg oracle is just to keep it in check and not wrong. if we have
> a couple differences here and there it's fine — even better if we make
> differences to improve perf without sacrificing quality.

**This supersedes every "bit-exact" or "framemd5-identical" acceptance
criterion written into an issue.** Those were written before this ruling and
are not binding where they conflict with it.

### What to ship on

Ship when the thing is **demonstrably not broken**:

- it decodes/encodes/filters real input without error, panic, or hang;
- its output is *structurally* right — no wrong geometry, no channel swap, no
  drift accumulating over a sequence, no artefacts a viewer would see;
- the deviation from the reference is **small and unstructured** — a scatter of
  ±1s from rounding is fine, and a measured mean error is enough evidence;
- round-trips hold where the format is lossless;
- fuzzing finds no panic.

That is a complete, closeable result. Say what the deviation is and move on.

### The distinction that still matters

**Small and unstructured is fine. Structured is a bug.** The shape of the error
carries the diagnosis, not its size:

- max deviation 1–2 spread across a frame → rounding. Ship it.
- error concentrated in specific blocks, or on every row but the first, or on
  every macroblock of one type → **a real defect**, however small the average.
  A structured error means something is *wrong*, not imprecise.

This project has both cases on record. MPEG-1/2's float-IDCT ceiling is a
measured, permanent ±1–2 scatter — fine, ship, done. But the error that
correlated with coefficient count, and the one that hit every row except row 0,
were both genuine bugs that a "close enough on average" reading would have
shipped.

### What does not change

Deliberate divergence for performance is welcome, provided quality is not worse
— measure both sides and say so. **Clean-room rules are untouched**: run the
reference binary and read its output; never read its source. Provenance
trailers, the ownership table and the commit discipline all stand.


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

## `BitReader::get` pads with zeros — a parse loop must use `try_get`

`get` never fails. Past the end of the buffer it returns zeros, which is the
right behaviour for a reader that flags its own error state and lets a caller
check once at the end — and a trap for any loop whose *termination* depends on
reading something.

An AAC `raw_data_block()` element loop looked for the `ID_END` marker. A
truncated stream never produced one, `get` kept handing back zeros, and the
loop allocated until it died: a real libFuzzer OOM from an **11-byte input**.

`try_get` fails immediately on truncation and consumes nothing. Its own doc
names length prefixes about to size an allocation, which is narrower than the
hazard — **any read whose value decides whether the loop continues** wants
`try_get`, not just one that sizes a buffer.

This is a shared primitive under every parser in the tree, so the shape is
worth checking wherever a loop reads a sentinel, a marker, or an element type.

## `Vaco-Spec-Ref: none` is accepted, and means the trailer is absent

Write it when there is genuinely nothing to cite, or omit the line — they are
the same thing to the gate. Two commits failed permanently before that was
true, because `none` resolved as an unregistered source id.

It opens no hole: omitting the trailer is already allowed, so `none` grants
nothing a author did not already have. And a `Vaco-Provenance` kind that
*requires* a citation still fails, now with the accurate message that it needs
one — three tests pin that, including the case where `none` must not satisfy a
`spec` provenance.

## Measure the thing that can be wrong, not the thing that is convenient

Four defects this project shipped past, or nearly shipped past, were invisible
to the metric being used — not because the metric was computed wrongly, but
because it was too narrow to contain the failure.

- **AAC 5.1**: per-channel correlation ~0.98, whole-frame correlation ~0. Every
  channel individually right; the frame collectively wrong, because the
  bitstream's channel order is not the output's. A per-channel metric calls
  that a pass.
- **AAC's QMF banks**: single tones round-trip at >0.99, white noise at ~0.04
  — and **there was no defect**. The verification searched a lag window that
  never contained the real delay of 289 samples; a sustained tone's
  self-correlation is periodic in the lag, so every tone found a >0.99 alias
  inside the wrong window while noise, having no period, could not. An impulse
  round trip settled it in one measurement. The lesson survives its own
  correction: the *disagreement* between tones and noise was the real signal,
  and reading it as "noise exposes a defect tones hide" was as wrong as
  ignoring it would have been.
- **VP9's `inv_remap_prob`**: invisible on trivial content because probability
  updates rarely fire, catastrophic on real content.
- **MPEG-2's `CODED_BLOCK_PATTERN`**: small quiet fixtures at max deviation 2,
  busy ones at 234 of 255.
- **An H.264 fuzz target's own input encoding**: a one-byte selector cannot
  address 5 context categories crossed with 4 initialisation sets — `>> 6`
  leaves two bits — so the category where the outstanding bug lived was
  **unreachable**, while the target reported millions of clean executions. Found
  by writing up its coverage, not by running it.
- **`vlc-scan`'s target list**: hand-maintained, two known table shapes, so a
  crate whose tables use a third shape is reported clean without being looked
  at.

The pattern is one thing: **a corpus or a metric that cannot reach the failing
path reports success indistinguishable from real success.** So before trusting
a green number, ask what a failure would have to look like to survive it — and
then build the case that would not.

Broadband where you tested tones. Whole-signal where you tested parts. Busy
content where you tested flat. Two tones where one passed. And when a test
cannot fail, say so: a test that buys confidence it has not earned is worse
than no test, because it stops the next person looking.

**Check that your harness can reach what it claims to cover.** A fuzz target's
input decoding, a gate's target list, a corpus's parameter ranges — each is a
door, and a door too narrow for the state space behind it produces the most
convincing false clean there is, because the execution count is real. Count the
cases you intend to cover, then prove the harness can address every one.

**When two measurements of the same thing disagree, suspect the measurement
before the code.** The QMF case above cost a dispatch because the disagreement
was read as a diagnosis rather than as a question. A signal with no period —
an impulse — or a second parameter varied independently will usually tell you
which of the two is lying, faster than reasoning about either will.

### A source that cannot separate two rules validates neither

Three divergences this campaign hid behind a test input that collapsed the
competing hypotheses into one answer. In each case the measurement was run
carefully and the conclusion drawn from it was wrong.

- **`vectorscope`'s intensity**: frame size was varied to vary hit count, so
  the two were never independent. Holding hit count fixed gave an exact rule
  at 35 points.
- **`waveform`'s accumulation**: probed only at `intensity=1`, where the
  per-hit step is already the integer 255 — so "sum then truncate" and
  "truncate then sum" produce identical output. The filter's own default,
  `0.04`, separates them, and the shipped rule was wrong.
- **`sobel`'s border rule**: checked against a source varying in one axis
  only, which cannot distinguish "force out-of-bounds taps to zero" from
  "compute normally, and the border is legitimately zero because a
  one-axis-invariant image has no gradient in the other axis." A genuinely
  two-axis source shows the reference's border is mostly nonzero.

The failure is not carelessness — each probe was real and its arithmetic was
right. It is that **the input was chosen to be valid rather than to be
discriminating.** A flat source saturates any accumulator. A single frame
hides temporal state. An axis-aligned source hides an axis.

So before trusting a measured rule, ask: **what other rule would produce this
same output on this same input?** If you can name one, the input has not
tested what you think, and the fix is a different input, not more points of
the same kind. Prefer values where candidate rules visibly disagree — which
is usually *not* the default, and usually not the roundest number.

### A test that asserts well-formedness does not assert correctness

Two full dispatch rounds on H.264 CABAC reported divergence points that were
not there. The harness asserted `!malformed()` and a matching macroblock count
— both of which hold when every decoded value is wrong, because a decoder fed
plausible bits produces a plausible *count* of plausible-looking macroblocks.
Adding one assertion that the arithmetic engine ends exactly at
`rbsp_trailing_bits()` moved the reported divergence from "late, in one
corpus" to "slice 0, in all three". The earlier reports were honest; their
locations were artifacts of a measurement with no teeth.

The tell is generic. **If an assertion would still pass against a decoder that
got the right shape from the wrong values, it measures shape, not values.**
Counts, "did not error", "produced the expected number of frames", and "the
output re-parses" are all shape. The assertions with teeth are the ones only
an exactly-correct decode can satisfy: a stream consumed to precisely its
documented end, a checksum, a byte comparison, a first-divergence locate.

This compounds worse than an ordinary weak test, because the shape assertions
usually *do* fail while the decoder is badly broken. They pass at exactly the
point the work gets hard — which reads as progress.

## A wrapper swallows what it does not forward — five instances so far

Every one was found reactively, because something downstream was silently
wrong days later. Two shapes:

**A delegating wrapper over a trait** silently returns a defaulted method's
default instead of forwarding it. `Box<dyn Muxer>` swallowed
`add_stream_with`; `MappedFilter` swallowed a `BitstreamFilter` option one
layer down; `AsDecoder` swallowed `set_extradata`; `Box<dyn Decoder>` needed
its own blanket impl. Four of five.

**A snapshotting wrapper that holds derived state** copies something once and
never sees a later change. `Discovery<D>` snapshots streams at construction, so
a `start_time` set inside the wrapped demuxer is invisible to it. Harder to
spot, because there is no missing `fn` — the tell is a field initialised from
the inner value in a constructor.

So when you add a method to a trait, or set state on something that may be
wrapped: **find every wrapper first** and write a test that fails without the
forward. The gap-19 agent wrote two, and they fail differently — one does not
compile without the blanket impl, the other passes compilation and fails at
runtime without the explicit override. Both were needed.

## You cannot identify your own commits by author — only by hash

Every agent in this tree commits under the same git identity. `git log
--author=...` therefore selects *everyone's* work, and an agent auditing
"which of these gate failures are mine?" by author will claim or disclaim
commits it never made. One agent nearly attributed another's malformed
trailer to itself this way, and caught it only by checking the hash directly.

**Record the SHA of every commit you create, and audit against that list.**
`git diff HEAD~1 HEAD --name-only` after each commit gives you the SHA and the
file list in one step; keep both.

The same applies in reverse: when a report says "these failures are not
yours," that claim was also made by someone who cannot see author identity.
Verify against your own SHAs before repeating it.

### `commit-msg` enforces provenance trailers only under some directories

`crates/codec`, `crates/format`, `crates/filter` and `crates/signal` are
checked areas: a commit touching them is **rejected outright** without
`Vaco-Provenance` / `Vaco-Clean-Room` / `Vaco-AI-Assisted`. `fuzz/` and
`crates/io/` are not checked, which is why the same agent can commit happily
for several rounds and then be blocked the first time it edits a scaler. The
rejection is clean and loses nothing — add the trailers and retry. For an
original (non-spec-derived) fix, `Vaco-Provenance: original` is the value the
repository's own history uses.

## Run `git diff HEAD~1 HEAD` after every commit

It is the only check that reliably catches a commit carrying somebody else's
work, and it costs one command. Three commits did it today. One swept in nine
unrelated files and went unnoticed for hours — because nobody ran this. Another
silently reverted a VP9 fix that had landed minutes earlier, and **was caught
immediately**, because its author did.

Read the file list, not just the summary. If a path you did not touch appears,
stop and repair it forward — never with `checkout`, and never by trusting the
working tree, which is where the stale content came from. Restore from the
blobs of the commit that introduced it:

```sh
git show <their-commit>:<path> > <path>
```

Then verify: `git diff <their-commit> HEAD -- <their-paths>` must be empty.

Repair forward rather than rewriting; a second commit that says what it is
restoring and why is a better record than a rewritten first one.

## `cargo fmt -p <pkg> -- <file>` reformats the whole package

The trailing path looks like it narrows the command to one file. It does not.
An agent ran it mid-round and pulled several other agents' concurrent
uncommitted edits into its own working copy — caught only because it ran
`git diff --stat` before staging, and recovered by reconstructing each
intended file as `git show HEAD:<path>` plus its own edit, verified hunk by
hunk. Nothing of anyone else's was committed or discarded, but that was the
check, not the tool.

In a shared tree every whole-package command is a whole-package command:
`cargo fmt -p`, `cargo fix`, `cargo clippy --fix`, and anything else that
rewrites files rather than reading them. **Formatting is not exempt from the
rule that you write only your own paths.** To format one file, run
`rustfmt <path>` directly, or format inside a worktree.

**`rustfmt <path>` is narrower but still not narrow.** It follows `mod`
declarations out of the file you named and reformats what it finds, so
pointing it at a `lib.rs` reformats the crate. Two agents hit this. The
reliable check is not the command, it is the diff: **`git diff --stat` before
staging, and the file list must be exactly what you meant to touch.**

And narrower still, since the same principle applies inside one file: if
`rustfmt` on a file you legitimately own would rewrite hunks that predate you
— formatting drift is common in this tree — leave them, format only your own
added lines by hand, and confirm with `git diff` that no pre-existing line
moved. Two agents have made that call correctly; it is the rule's purpose
rather than its literal text.

The same reasoning covers `Cargo.lock`: if regenerating it sweeps in entries
for another agent's not-yet-locked crates, commit your own change without it.

The general form: before staging, `git diff --stat` and confirm the file list
is *exactly* what you meant to touch. A tool that edited more than you asked
is indistinguishable from your own mistake once it is committed.

## Isolate with a worktree, never `git stash`

`stash` acts on the whole shared tree, not on your files, so it removes every
other agent's uncommitted work for as long as it is stashed and restores it in
one lump — a window in which anyone else's commit captures the wrong state.
`git worktree add --detach <scratch-path> HEAD` gives you a clean checkout to
build, bisect or compare in, at no risk to anyone, and ordinary git commands
are safe *inside* it. Remove it when you are done.

## Check a recorded blocker before you accept it

A doc comment or a `TECH-DEBT.md` row saying something is not tractable is
evidence, not a verdict. It records what one agent measured on one pass, and
it is wrong often enough to be worth ten minutes.

Three times in one day, a blocker turned out not to be one. A crate's own doc
said MXF D-10 could not be exercised because "every quantiser refused"; the
working `ffmpeg` recipe simply needed options the original attempt had not
tried, and once found it produced a real reference fixture where hand-building
had been assumed necessary. An `xwma` duration anomaly recorded as
decoder-dependent and needing real bitstream data fell to a wider parameter
sweep with no bitstream data at all. And a filter family recorded as blocked on
this tree's missing text renderer needed no text renderer, only a fixed
bitmap glyph table.

Every one was overturned by **widening the sweep**, not by new information.

So when a recorded blocker sits between you and your package: spend a bounded
amount of time trying to reproduce it before routing around it. If it holds,
say so and move on — a confirmed blocker is worth more than an inherited one.
If it does not, correct the record where you found it, and say plainly that
your own or a predecessor's earlier judgement was wrong. That correction is
what keeps "I checked and it is not tractable" believable the next time
anybody writes it.

The same applies to a blocker you recorded yourself an hour ago.

## How confident should a transcribed table be — three tiers, not two

There are three levels of checking a hand-transcribed variable-length code
table can receive, and this section used to present the middle one as the
answer. It is not. The tiers, weakest to strongest:

1. **Prefix-free and complete.** Cheap, structural, catches nothing about
   individual values — see the `CODED_BLOCK_PATTERN` story below for exactly
   what it misses.
2. **Prefix-free, complete, and exact per-entry bit length.** Catches the
   `CODED_BLOCK_PATTERN` class of bug (a code transcribed one bit shorter or
   longer than the specification states). It does **not** catch a
   *transposed pair of equal-length codes* — swap two codewords' values
   between two rows of the same length, and the table is still prefix-free,
   still complete, and every length is still exactly right. Only the meaning
   is wrong.
3. **Checked line-by-line against primary specification text.** The only
   tier that is actually sufficient. Nothing weaker can be, because tiers 1
   and 2 are both properties of the table considered as a set — they cannot
   see that entry number 14 and entry number 31 have swapped meanings.

This was corrected after tier 2 was shipped as "the mitigation" and then
found to not be one: `vaco-codec-h264`'s CAVLC tables were transcribed from
recollection, checked against both tier-1 and tier-2 tests, and still had
real errors — several `COEFF_TOKEN_NC2` rows and over half of `RUN_BEFORE`'s
highest-risk row were **wrong but still prefix-free and correctly
lengthed**, passing both checks cleanly. Only re-checking every entry
against a primary edition of the ITU-T text found them.

The `CODED_BLOCK_PATTERN` story that originally motivated tier 2, kept
because it is real and still worth knowing: MPEG-2's `CODED_BLOCK_PATTERN`
(H.262 Table B.9) had its last three codes transcribed at 10 bits where the
specification has them at 9 — one shorter than the four rows above them, an
easy miscount. The existing prefix-free-and-coverage test (tier 1) passed.
Average per-frame deviation on a CIF fixture was **234 out of 255**, and the
cause was found only by hand-tracing a real encoder's bits to one
macroblock and reading the nine bits the stream actually contained.

**What this means in practice**: tier 1 (`cargo xtask vlc-scan`, workspace-
wide — see below) and tier 2 (per-entry length assertions, written where a
table is tested at all) are both worth doing, because they are cheap and
each catches a real class of error tier 3 alone would also catch but more
expensively. Neither is a substitute for tier 3, and a table's own
documentation — or a commit message reporting it as "verified" — must say
which tier it actually received, not imply the stronger one from having
passed the weaker one. `provenance/*.toml`'s `method = "transcribed"` rows
do not currently distinguish tiers; a table verified against primary text
line-by-line (H.264's CAVLC tables, now) is not distinguished there from one
that has only ever passed tier 1, which is worth fixing separately from this
correction.

`cargo xtask vlc-scan` runs tier 1 across every hand-transcribed
`(bit-length, code)`-shaped table in the tree it knows about (see that
module's own doc for exactly which tables and why some codecs — AC-3 has no
VLC tables at all; VP8/VP9's mode/coefficient tables are binary-tree
traversal structures, not independently-transcribed bit-strings that could
collide) — cheap enough to run on every change, and it is a *gate*: a real
conflict fails the build. It does not import `vaco-codec-vlc`'s own
`is_prefix_free` (`xtask` is deliberately dependency-free, so that a
transient compile break in a codec crate cannot take every gate down with
it) — the same ~15-line algorithm is re-derived there instead. A crate whose
tables are already shaped as `vaco-codec-vlc::VlcEntry` (`vaco-codec-aac`'s
spectral codebooks, for instance) should call `is_prefix_free`/
`kraft_numerator` directly in its own test suite instead — that is tier 1
too, just run where the table already lives rather than swept externally.

## Fuzzing (**D6**)

A crate that parses untrusted input and has no fuzz target is not done.

**Run one target in isolation, always:**

```sh
cargo +nightly fuzz run <target> --no-default-features --features <feature> \
  -- -max_total_time=30
```

`--no-default-features` is not decoration and `--features` alone is not enough.
Every path dependency in `fuzz/Cargo.toml` is now `optional`, so this pair is
what builds *only* the crates your target names. Without it you build every
crate in the workspace, and any one of them failing takes your fuzz run with
it — in a tree where eight agents are mid-write, that is the normal state, not
an edge case. Two crates being transiently uncompilable blocked everybody's
fuzzing this way before the dependencies were gated. `just fuzz <target>`
reads the target's `required-features` and passes both flags for you.

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

**Staging one file first does not protect you.** `git add <one-file>` followed
by a plain `git commit` still sweeps in everything another agent had staged
before you. Two agents hit exactly this on 2026-08-28, an hour apart, each
reverting a different concurrent edit — one of them a 20-line addendum written
minutes earlier. Both caught it via `git diff HEAD~1 HEAD --name-only` and
restored it, which is the only reason it cost nothing.

So: **`git status --porcelain` before any commit, and the staged set must be
empty or yours.** A dirty index that is not yours means someone else is
mid-commit — use the pathspec form and do not touch the index at all.

**The pathspec form itself is sound — verified, not assumed.** In a scratch
repo: stage `b.txt`, then `git commit -m ... -- a.txt`. The commit contains
`a.txt` alone and `b.txt` remains staged and uncommitted. A concurrent stage
cannot ride along.

**But the pre-commit hook can still reject you for someone else's files.**
The hook inspects the *index*, which still holds whatever another agent
staged — so it may refuse your correctly-scoped commit over a provenance
problem in a file you never touched. That is a spurious rejection, not
evidence that your pathspec leaked.

**When that happens, do not `git reset`.** It unstages the other agent's
work mid-commit, which is a different way of damaging them. Use the private
index recipe instead: it builds a tree from `HEAD` plus your own blobs and
never touches the shared index at all, so a dirty index cannot reject you
and you cannot disturb it.

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
rm -f "$GIT_INDEX_FILE"
BASE=$(git rev-parse HEAD)                     # ONE sha, used three times below
git read-tree "$BASE"                          # start from BASE, not the shared index
git show "$BASE:path/to/shared.rs" > "$SCRATCH/mine.rs"   # NOT the working-tree file
#   ...make your edit to "$SCRATCH/mine.rs"...
blob=$(git hash-object -w "$SCRATCH/mine.rs")
git update-index --add --cacheinfo 100644,$blob,path/to/shared.rs
tree=$(git write-tree)
commit=$(git commit-tree "$tree" -p "$BASE" -F "$SCRATCH/msg.txt")
git update-ref refs/heads/main "$commit" "$BASE"   # FAILS if HEAD moved — see below
unset GIT_INDEX_FILE
cp "$SCRATCH/mine.rs" path/to/shared.rs        # write back, or the tree falls behind HEAD
git reset -q HEAD -- path/to/shared.rs         # settle the shared index against the new HEAD
git diff HEAD~1 HEAD --name-only               # must list only your own paths
```

**`git commit` is not usable here, and that is the whole reason this recipe
looks the way it does.** With `GIT_INDEX_FILE` set, `git commit` resolves the
parent at commit time while your *tree* was built by `read-tree` earlier. If
anything landed in between, the parent is new, the tree is old, and the commit
**silently reverts every file changed in that window** — files you never
touched and will not see in your own diff summary. The compare-and-swap in
`update-ref` does not save you either, because the ref move itself is
legitimate: the parent really is current. That is what corrupted a VP9 crate
this session, twice, through commits that were individually valid.

So: one `BASE`, used by `read-tree`, by `commit-tree -p`, and by `update-ref`'s
old-value guard. **If `update-ref` fails, start again from `git rev-parse HEAD`
— including the `read-tree`.** Re-running only the commit is the bug.

`GIT_INDEX_FILE` does not survive between tool calls, so do the whole block in
one invocation. An agent that split it landed an empty commit.

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
rm -f "$GIT_INDEX_FILE"
BASE=$(git rev-parse HEAD)
git read-tree "$BASE"
blob=$(git hash-object -w "$SCRATCH/mine.md")
git update-index --add --cacheinfo 100644,$blob,planning/TECH-DEBT.md
tree=$(git write-tree)
commit=$(git commit-tree "$tree" -p "$BASE" -F "$SCRATCH/msg.txt")
git update-ref refs/heads/main "$commit" "$BASE"
unset GIT_INDEX_FILE
cp "$SCRATCH/mine.md" planning/TECH-DEBT.md
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

**If your change is an edit and not an append, write it back to the working
tree as well.** The recipe above is safe for appends because the working-tree
file ends up a superset: it keeps everyone's additions, `HEAD` gains only
yours. An *edit* -- replacing a row, marking a package done, correcting a
number -- breaks that. Change the scratch copy alone and the working-tree file
is now **behind** `HEAD`, and the next agent that edits it and commits will
silently revert you, having done nothing wrong.

That has now happened. `ASSIGNMENTS.md` sat 24 lines behind `HEAD` for a full
wave, and a separate agent independently reported finding five planning files
reverted to stale content in its working tree -- committed work it would have
destroyed had it staged them.

So after the commit, either `cp "$SCRATCH/mine.md" <path>` or, equivalently,

```sh
git show HEAD:planning/ASSIGNMENTS.md > planning/ASSIGNMENTS.md
```

and then confirm the only thing left is other people's pending work:

```sh
git diff HEAD -- planning/ASSIGNMENTS.md
```

Lines your own commit added must not appear there as deletions. If they do, the
working tree is stale and the next writer will undo you.

**The commit-msg hook reads `provenance/sources.toml` from disk, not from
`HEAD`.** So the usual two-step — register a source in one commit, cite it with
`Vaco-Spec-Ref` in the next — fails at the second commit if you built the first
one purely through a private index, because the hook looks at a working-tree
file your private-index commit never touched. This cost one agent two commits
before it worked out what was happening.

The fix is the write-back step above, applied to `sources.toml` specifically:
after the registration commit, append your entry to the working-tree file as
well. That is not a workaround for the hook, it is the same rule this page
already states — a private-index commit that does not write back leaves the
working tree behind `HEAD` — and the hook is simply the first thing to notice.

**Never skip the closing `git reset -q HEAD -- <path>`. This is the step that
has done the most damage of anything on this page.** A private-index commit
does not touch the main index, so afterwards the main index still holds the
**pre-commit** blob. `git status` shows that as `MM` — staged changes, for a
file that is already committed and byte-identical to `HEAD`.

That is not cosmetic. The next agent to stage broadly commits those stale
blobs, and they land as **deletions of everybody else's work**. It has
happened, at a scale nothing else here has reached: one commit carrying two
genuine AC-3 fixes also reverted nine unrelated files — a `PixFmt` method and
its tests, the CLI that called it, a lint fix, both halves of a muxer's
extension list, its generated registry row, and two planning documents — four
of them byte-identical to the blob that preceded the work, which is how a
reversion is told apart from an edit. The agent that committed it did nothing
wrong. The orchestrator had skipped this line on five commits in a row, seen
the `MM`, checked that the working tree matched `HEAD`, and concluded it was
cosmetic.

Run it after every private-index commit, for every path in that commit. If you
ever see `MM` on a file you did not just edit, the index is carrying somebody's
stale blob and it is loaded — reset that path before you commit anything.

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

## The clean-room rule is about FFmpeg, not about every reference implementation

Twice in one day an agent treated a **permissively-licensed** reference
implementation as forbidden. One built ALAC from scratch as a self-invented,
self-interoperable-only design rather than open Apple's reference. Another wrote
a working Opus decoder from libopus and then reported it in terms that read like
a confession. The orchestrator briefly mistook the second for a clean-room
violation. All three of us were wrong, in the same direction.

`planning/research/07-legal-patents-licensing.md` §1.6.1 is explicit. **Tier A —
open freely, no record needed** includes:

- Published standards (ITU-T, ISO/IEC, IETF RFCs, SMPTE, AES, ETSI/3GPP, W3C, Xiph)
- Academic papers and textbooks
- **Permissively-licensed reference implementations** already cleared in the
  register — libopus (BSD), libvpx (BSD), dav1d (BSD), HM/JM (BSD-3),
  libjxl (Apache-2.0), Apple's ALAC (Apache-2.0)
- FFmpeg's *user documentation*, man pages, and `--help` output
- The `ffmpeg`/`ffprobe`/`ffplay` **binaries**, for black-box testing

**Tier B — off-limits** is FFmpeg / libav / VLC / GStreamer / mpv C source, and
GPL codec source such as x264/x265. That is what D7/D15 protect against, and the
distinction is *licence*, not a general principle that reading any implementation
is dirty.

The cost of getting this backwards is not caution, it is defects. A codec written
without the reference it was permitted to read is how G.722, G.726 and the first
ALAC all shipped as plausible-looking stand-ins that no other decoder could read
— and all three had to be pulled back. Refusing a permitted source does not make
the work cleaner; it makes it wrong in a way that takes another agent a full
session to find.

Two duties survive when you do use a Tier A implementation:

- **Record it.** `Vaco-Spec-Ref` trailer plus an entry in `provenance/`, naming
  the licence you checked and the date you checked it. If an earlier provenance
  file claims you never read something you have now read, **retract the claim in
  the same commit** — a stale disclaimer is worse than none.
- **Attribution is a real obligation, not a nicety.** BSD, MIT, ISC, Apache and
  FTL all require it in redistributed binaries. That duty is discharged
  project-wide through the generated `NOTICE` / `THIRD_PARTY_LICENSES` file
  tracked in #182, not per-crate — so use the source, record it, and let the
  release-engineering issue collect it.

If you are unsure whether a specific source is Tier A, ask the orchestrator. Do
not resolve the doubt by inventing an algorithm.

## Under concurrent load, verify commits by content — `git log` is not enough

An existing rule says you cannot identify your own commits by author, only by
hash. There is a stronger version of that, found while landing `vaco-checkasm`
with eight agents writing to one tree: under heavy concurrent write load,
`git log -- <path>` and `git ls-tree` can return **attribution that looks
inconsistent** — a path appearing to belong to a commit that did not touch it,
or a recent write not showing where you expect.

Do not resolve that by staring at more `git log`. Resolve it against content:

- `git diff HEAD~1 HEAD --name-only` after every commit — only your files, every time.
- If anything looks wrong, `git show HEAD:<path>` against your working tree, and
  diff against a known-good earlier baseline.
- Then run the tests and clippy against the **actual committed state**, not
  against your working tree, which may differ.

The same session produced a related artifact worth recognising: a second
private-index attempt landed on top of an already-successful first one, because
the first one's shell output was read as a failure. The result was a harmless
empty no-op commit. That is the benign outcome. The malign version of the same
confusion — retrying a step that already succeeded, against a base captured
before it — is how 106 lines of another agent's work were silently reverted
earlier. **Capture BASE once, and when a git step's output is ambiguous, check
whether it already worked before running it again.**

An empty commit in the history is not a problem. A retry against a stale base is.

## `dup-check` catches colliding names, not duplicated logic

`cargo xtask dup-check` compares **public type names**. That is a genuine gate —
it caught `vaco-fuzz-support` redefining `ProgressGuard` when `vaco-limits`
already had one, and it caught a filter crate's first-draft colour type
duplicating `vaco-core::Rgba`. Both became re-exports instead of second
implementations.

It cannot catch the more common case: the same *algorithm* written twice under
two different names, or written as free functions. Two agents hit exactly that
today. One drafted `edge_common`, a box-blur core, morphology and LUT sampling
before finding, by reading rather than by gate, that `vaco-filter-convolve`,
`vaco-filter-blur` and `vaco-filter-lut` already had real, tested versions of
all four. Another found the same for EBU R128 and window functions. In both
cases a green `dup-check` would have signed off on the duplicate.

So before writing any primitive, **grep for the concept, not the name**. If
something plausibly already exists, it probably does — this tree now has ~200
crates and no one holds all of them in their head.

The specific reason this matters more than tidiness: the existing
implementations have absorbed corrections a fresh copy will not have. The
box-blur in `vaco-filter-blur` carries a `reflect-101` versus `replicate`
border distinction that was measured, not guessed. A clean-room rewrite of it
would look correct, pass its own tests, and be subtly wrong at every edge — the
same failure that produced a VP8 encoder missing a bitstream partition, an Opus
decoder at 0.03 correlation, and a self-interoperable-only ALAC, all in one day.

A second implementation is not neutral. It is a place for a fixed bug to come
back.

## A benchmark where both paths tie exactly is measuring the optimiser

An agent benchmarking `dc_predict` measured its dispatched path at ~3.2x the
scalar one, and noticed the fastest times of the two paths were **exactly**
tied. Equal-to-the-nanosecond is not a plausible result for two different
implementations. The inputs were compile-time constants, so LLVM had folded
both loops away and the benchmark was timing an empty function.

Wrapping the inputs in `divan::black_box` gave the honest number: **1.28x**.
Still a win, and a quarter of the claimed one.

The tell is worth memorising, because a fabricated speedup is more dangerous
than a missing one — it gets quoted, and it justifies keeping code that earns
nothing:

- Two paths whose fastest times match exactly, or to an implausible number of
  digits.
- A speedup far larger than the arithmetic could explain.
- Timings that do not move when the input size changes.

`black_box` every input a kernel reads and every value it returns. Then report
the ratio, not a verdict.

The same round produced the reason this matters. Of five kernels measured
honestly, **two lost to plain autovectorisation** (~0.65x and ~0.84x), one was
a wash, and two won (2.8x on a 4096-element reduction, 1.28x on `dc_predict`).
LLVM already vectorises a trivial widen-convert-store loop as well as a
hand-composed version does. Hand-written SIMD is not free speed — it is a
hypothesis, and it needs measuring before and after, per D17.

If a dispatched path loses, say so plainly and do not route callers through it.
A pessimisation behind a dispatch layer is worse than no dispatch layer: it
costs performance and reads as an optimisation to everyone who follows.

## Measuring one plane is not measuring the output

A VP8 encoder used luma-scale macroblock coordinates for intra **chroma**
prediction — a 16px grid where chroma needs 8px — silently corrupting every
chroma macroblock after the first.

Two tests passed cleanly over that bug:

- the crate's **own decoder round-trip**, because both sides made the same
  mistake and agreed with each other;
- a **luma-only MSE** check, because luma was genuinely fine.

It was found by measuring **chroma** PSNR against `ffmpeg`-decoded output:
7 dB before the fix, 29 dB after. Nothing internal could have found it.

Two rules follow, and the second is the one that gets skipped:

1. **Verify against a decoder you did not write.** Self-round-trip proves the
   encoder and decoder share a convention; it says nothing about whether that
   convention is the format's. This is the same failure as the VP8 encoder that
   omitted an entire token partition, the Opus decoder at 0.03 correlation, and
   the first ALAC — all pulled back in one day.
2. **Measure every plane, every channel, every field — not the one that is
   easiest to eyeball.** A metric averaged over the whole frame, or taken over
   luma alone, will hide a defect confined to a subsampled plane. Report Y, U
   and V separately. The same applies to audio channels, to alpha, and to
   interlaced fields.

A single aggregate number is a summary, and summaries are where structured
error goes to hide. Per `705779d`, structured deviation is a bug however small
its average.

## You are the agent. There is no one else working for you.

Three agents in one session stalled the same way: each reported it was waiting
on sub-agents to finish, having spawned none. One waited on a "survey agent"
that never existed. One waited on sub-agents that were in fact alive but simply
had not written yet. One returned, as its entire result, *"Standing by for the
audit agents to finish; I'll act on their findings as soon as they land."*

Nothing was coming. Each cost a full dispatch.

Unless you have called the Agent tool yourself in this session and hold the
resulting id, **no one is working on your behalf.** A brief that describes work
in the third person, mentions other agents, or says what "should" be produced is
describing *your* task, not someone else's. Do it yourself, directly.

If you genuinely need something another agent owns, say so and stop — that is
the documented escalation, and it is correct. But "waiting" is not a state this
system has. There is no queue, no inbox, and nothing arrives while you idle.
Either act, or report and end your turn.

## A test that skips on error is indistinguishable from a test that passes

An agent wrote a differential test invoking `ffmpeg -f webp`. There is no `webp`
demuxer — the registered name is `webp_pipe` — so ffmpeg failed every time. The
test treated a non-zero exit as "reference unavailable, skip", printed nothing
under captured output, and reported green. It had never once compared anything.

It was caught only by rerunning with `--nocapture`.

Skip-on-error is a reasonable pattern when a tool genuinely may be absent. It
becomes a lie the moment it also swallows *the tool being present and rejecting
your command*. Distinguish the two:

- Probe for the tool once, explicitly, and skip on that.
- After that, a non-zero exit is a **failure**, not a skip. Assert on it.
- Print the command and its stderr when it fails. A test that cannot tell you
  what it ran cannot be debugged.
- If a test can skip, make it say so loudly enough to notice in captured output.

The general form, and it has now bitten this project several ways: **a test that
cannot fail proves nothing, and reports the same green as one that can.** Before
trusting a passing differential test, break it on purpose — feed it the wrong
file, or corrupt one side — and confirm it goes red. If it does not, it was
never testing anything.

This is `763a374`'s rule one level up: a test that asserts well-formedness does
not assert correctness, and a test that asserts nothing at all does not even
assert well-formedness.

## The pathspec form protects against staged files, not against a shared working tree

The rule that `git commit -F <msg> -- <paths>` is safe has a limit that was
found the hard way: it commits **whatever is in the working tree at those
paths**, including another agent's *uncommitted* edits to the same file.

That is exactly what happened. An agent held an in-progress edit to
`vaco-codec-core`'s `Encoder` trait; a second agent committed that same path by
pathspec, and the first agent's unfinished work was absorbed into a commit that
knew nothing about it. It re-applied on top of the new `HEAD` rather than
rewriting history, which was the correct recovery.

So the earlier finding stands but is narrower than it reads. The pathspec form
protects you from sweeping up files you never touched. It does **not** give you
exclusive claim to a file, and nothing does.

Two consequences:

- **Do not hold a large uncommitted edit to a widely-shared file.** Land it in
  small commits as you go. The window in which someone else can absorb your work
  is exactly the window in which it sits uncommitted.
- **If your work is absorbed, re-apply on top of the new `HEAD`.** Do not
  rewrite history, and do not `git reset` — both destroy someone else's landed
  work to recover your own.

`git diff HEAD~1 HEAD --name-only` after every commit remains the check, and it
catches this from the *other* side: if a file you did not edit appears in your
commit, you have just absorbed someone. Say so rather than moving on.

## Extract large tables programmatically; a prefix-free test catches the rest

Building VC-1, an agent hand-typed a CBPCY VLC table and copy-duplicated one
entry from a neighbouring row. Its own `is_prefix_free` test caught it before
anything shipped. It then extracted the ~340-entry AC tables **programmatically
from the specification PDF's own table cells** rather than typing them,
specifically to avoid repeating that.

Both halves are worth copying.

**Extract, do not transcribe.** A table you typed is a table you introduced
errors into. Pull the cells out of the source document mechanically wherever the
document allows it. This is not a shortcut — it is the more accurate method, and
it scales to tables no one would check by eye.

**Then test a structural property, not the values.** A VLC table that is not
prefix-free is provably wrong, whatever the values are, and that check costs
nothing and needs no reference. The equivalent exists for most table kinds:
codebooks must be prefix-free, scan orders must be permutations, quantiser
tables must be monotonic, inverse transforms must round-trip. **Find the
invariant and assert it** — it catches the transcription errors that a
value-by-value review reliably misses, because the reviewer reads what they
meant to type.

This matters more here than it sounds. Every codec pulled back this session --
G.722, G.726, ALAC, DFPWM -- failed on a table or a formula, not on
architecture. The structure was always right and the numbers were always wrong.

## The default configuration is where bugs hide

Every one of these was found today, and every one was invisible in the simplest
case and wrong everywhere else:

- VP9's header reset every frame without its own `color_config()` to a
  hardcoded 4:2:0/8-bit literal. **Invisible on profile 0**, because the literal
  happened to match. Catastrophic on profiles 1/2/3 from the second frame on.
- VP9's `left_nz` was not reset per superblock row. **Invisible on a
  one-row frame.** Wrong from the second row on.
- VP9's `decode_tile` ignored its own tile-column range and decoded every tile
  as the full frame width. **Invisible on a single-tile frame.** A two-column
  key frame was 99.98% wrong.
- An H.264 sub-macroblock bug survived eleven rounds because row 0 looked
  perfect — that fixture's row-0 macroblocks never used the affected split.
- A VP8 encoder corrupted every chroma macroblock. **Invisible to a luma-only
  metric** and to its own decoder round-trip.
- A v360 `roll` formula passed a 90° probe. **90° is symmetric**; a 20° probe
  ruled out that formula and all six compositions with yaw and pitch.

The shape is always the same: the simplest configuration is degenerate in some
way that makes a wrong implementation indistinguishable from a right one.
Profile 0, one row, one tile, one frame, luma only, a right angle, a flat
fixture.

So **the first fixture is never enough, and passing it is not evidence.** Before
believing a decoder or a filter works, test at least one case that is:

- not the default profile or the default pixel format,
- more than one row, tile, slice, partition or frame,
- not axis-aligned, not symmetric, not a special angle,
- not flat — real texture, and every plane checked separately.

When something passes immediately, that is the moment to get suspicious, not
the moment to stop. Ask what the fixture cannot distinguish, then build the
fixture that can.

## Closing a refusal tells you nothing about what was behind it

`vaco-codec-hevc` refused stock `libx265` files with "per-CU QP delta is not
supported". That was true, and closing it was real work: `cu_qp_delta` now
decodes byte-exact at three frame sizes across three CRFs, and it forced a
latent bug out of `deblock.rs`, whose `qP_P == qP_Q` shortcut had been correct
only because no per-CU QP existed to invalidate it.

But "the last blocker" was the orchestrator's phrase, not a measurement. Behind
that refusal sat a second one — `only I-slices are decoded` — and a stock file
is `I B B B B P B B B P ...`: twenty-four of its twenty-five frames are inter.
The decoder emitted 115,200 bytes for a 320x240 clip. Exactly one frame.

The error you can see is the first one the decoder reaches, not the smallest
one remaining. A decoder that bails at the first unsupported construct reports
its refusals in bitstream order, which has nothing to do with their size. Every
one you close can reveal another, and the one behind may be an order of
magnitude larger — as it was here, where `cu_qp_delta` is a few hundred lines
and inter prediction is reference-list management, merge/AMVP derivation, and
sub-pel interpolation.

So: **measure the capability, not the error.** Before claiming a format works,
run the real file and check what came out — frame count, byte count, every
plane of every frame. `115200 == 320*240*1.5` was the whole diagnosis, and it
took one `ls -l`. Announcing "one blocker left" cost nothing to say and was
wrong the moment it was said.

A refusal is a floor on what is missing, never a ceiling.

## A path that names an architecture is not evidence of an architecture

An agent implementing HEVC inter prediction stopped after one of four stages
and reported that no usable oracle existed: the HM it found was "an x86-64
Linux ELF binary that cannot execute on this arm64 Darwin machine", confirmed
by an `exec format error`. That was all true. It was also not the only HM
present.

A previous pass had already built HM 18.0 from source here, natively, with
`-msse4.1` guarded to x86 targets — the procedure `docs/codec/vaco-codec-hevc.md`
describes in two separate places. That binary lives at:

    .../scratchpad/hm/HM-HM-18.0/bin/umake/clang-21.0/x86_64/release/TAppDecoder

`file` reports it as `Mach-O 64-bit executable arm64`, and it runs. The
`x86_64` in the path is HM's own umake build-directory naming, not the target
architecture. The agent tested a different binary, got a true negative, and
generalised it into "there is no working HM", which was false and cost it three
stages of work.

Two habits would have caught it:

- **Test the artefact, not its name.** `file` and `--help` take one second
  each. A directory component is a claim made by a build system, not a fact
  about the file.
- **When a tool you need appears to be missing, search for it before concluding
  it is absent** — especially in a repo whose docs describe having built it.
  One `find -name TAppDecoder` returned the working binary immediately.

The wider point: "I could not find X" and "X does not exist" are different
claims, and only the first one is ever supported by having looked in one place.
Report the first; earn the second.

## Under background load, measure cycles and interleave; wall clock is noise

Round 2 of the H.264 profiling loop ran while a niced fuzz sweep occupied the
machine. Measured on the same changes, **wall-clock timing carried roughly 300%
noise; cycles-elapsed carried about 2%.** A change that looks like a 40% win in
wall clock under that load is telling you about the scheduler, not your code.

Two habits follow, and both are now proven here:

- **Measure in cycles, not wall clock**, whenever anything else is running.
- **Interleave the A/B**: alternate baseline and candidate *within* each round
  and report the win/loss split across rounds, rather than timing all of A then
  all of B. Round 1 established a real 1.11x win this way (8 of 8 rounds) while
  a sweep ran; round 2 correctly rejected three changes at ratios of 0.997,
  1.0025 and 1.034.

Report the ratio and the round count. A single number from a single sequential
run is not a measurement, and on a busy machine it is not even evidence.

A corollary worth keeping: **the same optimisation can win on one data path and
lose on a neighbouring one.** An in-bounds fast path that skipped edge clamping
won on H.264 luma and *regressed chroma by 3.4%*, because chroma's clamp is two
cheap ops and the guard branch cost more than it saved. Symmetry between two
code paths is not a reason to skip measuring the second one.

## "Not ready to enable" and "not ready to commit" are different decisions

An agent finished a large HEVC P-slice implementation, found two real bugs with
it (an inferred-vs-parsed `cbf_luma` desync, and an inverted MER-exclusion test
that made spatial merge candidates unavailable for essentially every PU), took a
fixture from 64.8% to 98.2% byte-exact — and then **committed none of it**,
roughly 1,700 lines, on the grounds that the bar was "land P-slices byte-exact"
and the bar was not met.

The instinct was correct: never make a decode path reachable while it produces
silently wrong pixels. Registered-but-wrong is worse than absent, and that
discipline is why several codecs here could be registered at all.

The remedy was wrong. In a shared tree with other agents committing, uncommitted
work is the *least* safe place to put it. Someone else's `git stash` — which has
already happened here once — or a commit made with a directory pathspec rather
than explicit file paths will take it. Nothing about leaving it in the working
tree protects users from wrong pixels; only `check_scope` does that.

**Commit the code with the refusal still in place.** The implementation is
preserved and bisectable, the bug fixes land as their own commits, and
user-visible behaviour is unchanged because nothing became reachable. Lift the
refusal in a later, separate commit when the output is actually byte-exact.

Gating reachability and gating version control are independent decisions. Use
`check_scope` for the first. Never use "don't commit" for it.

### Postscript: the loss actually happened, and not the way it was predicted

The entry above was written as a caution about *other* agents. Within the hour,
the agent holding those ~1,700 uncommitted lines destroyed them itself: it ran
`git checkout -- decoder.rs` meaning to undo a single edit, and wiped the entire
uncommitted Stage 2 rewrite of that file. Nothing had ever been staged or
committed, so no git recovery existed.

It got the work back — reconstructed from the crate's own intact APIs plus the
design record, then validated by temporarily bypassing the refusal and
reproducing the exact known-good byte count, 2,829,043 of 2,880,000. That
number depends on precise DPB, POC and context-bank wiring, so reproducing it
is strong evidence the reconstruction was faithful. But that recovery was
luck and effort, not a guarantee, and it was only possible because a specific
measured number happened to be on record.

Two corrections to draw:

- **The danger to uncommitted work is not primarily other agents.** It is the
  single-file `git checkout --`, `git restore`, and `git reset` you run on
  yourself while iterating. Those are the commands that make "I'll commit it
  when it's finished" expensive.
- **Commit early and often behind a refusal.** A commit is not a claim that the
  work is done or correct — `check_scope` makes that claim, and it is what
  users actually see. A commit is only a claim that the work exists and can be
  found again.

## A fuzz assertion names a class, not an instance — audit the siblings

`registry_discovery` asserted `bits_per_raw_sample` lies in `1..=64` and caught
JPEG reporting **164**: `Jpeg::parse` read the SOF precision byte with no
validation and handed it straight to a field `vaco-probe` prints verbatim. Not
an internal inconsistency — fabricated metadata reaching the user, and
reachable from any corrupted or hand-crafted JPEG, not only from fuzzed input.
Confirmed by patching one byte of a real file: before the fix it printed 164,
after it prints `N/A`, which is what `ffprobe` prints for the same file.

The fix took one line. The value came from what happened next: auditing the
harness's *sibling* assertions for the same class turned up **four more**, none
of which the corpus had reached —

- `vaco-demux-mp4` computed `nal_length_size` from a reserved 2-bit encoding,
  fabricating `3`, which ISO/IEC 14496-15 does not define.
- `vaco-parse-audio-misc` let any ALAC `bitDepth` byte through; ALAC codes only
  16/20/24/32.
- `vaco-parse-vpx`'s `vpcC` `bitDepth` nibble could report `0`; VP9 codes only
  8/10/12.
- `vaco-demux-mxf`'s `ComponentDepth` used `u8::try_from(..).ok()`, admitting
  `0` and `65..=255`.

So: when a fuzz assertion fires, **fix the instance, then grep for the class.**
The harness encodes an invariant about a *field*, and every parser that
populates that field is a candidate. Corpus coverage decides which one fires
first, and that is an accident of the corpus, not a statement about which
parsers are correct.

The general defect is a parser copying a syntax element into an output field
without checking it against the values its own specification defines. Anywhere
a field is read and stored unvalidated, the next `ffprobe`-shaped output is a
lie the user has no way to detect.
