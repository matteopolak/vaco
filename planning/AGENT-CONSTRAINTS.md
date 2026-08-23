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
- Run **no git commands**. Not `add`, not `commit`, not `stash`, not `checkout`.
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
