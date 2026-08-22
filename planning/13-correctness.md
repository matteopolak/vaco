# 13 — Correctness: Differential Conformance, Fuzzing, Testing, CI, Provenance, Release

Governing constraints: `planning/00-decisions.md` **D6** (differential testing + fuzzing are first-class,
both gate merges) and **D7** (clean room). Architecture: `planning/10-architecture.md`.
Research backing: `planning/research/06-devices-build-test.md` §3–§4 (FATE, checkasm, OSS-Fuzz target
decomposition), `planning/research/07-legal-patents-licensing.md` §1 (clean-room policy),
`planning/research/05-fftools-cli.md` §3 (ffprobe writer surface).

---

## 0. Premise, and what we explicitly do not reuse

The user requirement, verbatim:

> "We will also need fuzzing (both to catch panics, etc.) and to ensure our output is identical when given
> the same flags and input files compared to ffmpeg."

Two obligations, and they are the same obligation viewed from two sides:

1. **Sameness** — for a given input and argument vector, our observable output equals the reference's.
2. **Robustness** — for *any* input, including hostile ones, we terminate, bounded, without panicking.

The differential conformance harness proves (1). The fuzzing programme proves (2), and the *differential*
fuzzer proves (1) holds on inputs nobody thought to write a test for. Everything else in this document
exists to make those two engines cheap to run, hard to bypass, and legally defensible.

### 0.1 What we do not reuse — a hard list

| Upstream artifact | Status | Consequence |
|---|---|---|
| `tests/checkasm/*` | **GPL** | We build `vaco-checkasm` clean-room from the *concept* (per-kernel variant verification + cycle bench). No file, no test list, no structure copied. |
| `tests/tiny_ssim.c`, `tests/tiny_psnr.c` | **GPL** | We implement SSIM/PSNR from the published definitions (Wang et al. 2004 for SSIM). Cite the paper, not the file. |
| `tests/ref/**` (4,936 reference digests) | Compilation-copyright risk + terrible provenance optics (research 07 §1.5d) | **Never copied.** Our expected values are either spec-defined, self-consistent (round-trip invariants), or generated fresh from the reference binary at test time and discarded. |
| `tests/fate/*.mak`, `tests/fate-run.sh` | LGPL/BSD-ish, but irrelevant | We design our own harness. The *shape* of the idea (declare case → run CMD → compare to ref with a mode and a tolerance) is a method of operation and is free; the files are not used. |
| `tools/target_*_fuzzer.c` | LGPL | We mirror only the **decomposition** — one fuzz entry per component category, driven by a corpus list — which research 06 §4 explicitly flags as "a proven shape worth mirroring". Zero code. |
| `doc/ffprobe.xsd` | Expression (research 07 §1.5e) | We generate our schema from our own Rust types and use our own namespace. |

The **reference binaries** (`ffmpeg`, `ffprobe`, `ffplay`) are used freely as black boxes. §1.7 sets out
exactly why that is safe and where the line is.

### 0.2 New crates this plan introduces

Additions to `planning/10-architecture.md` §3 (Tools) and Layer 0:

| Crate | Layer | Purpose |
|---|---|---|
| `vaco-limits` | 0 | The resource-budget type threaded through every component that touches untrusted input. §2.2 is the whole reason it exists at layer 0 rather than in a test harness. |
| `vaco-conformance` | tools | The differential harness: manifest loader, runner, normalisers, comparators, divergence-allowlist engine, reporter. |
| `vaco-corpus` | tools | Corpus fetch/verify/mutate/minimise; content-addressed object-store client. |
| `vaco-checkasm` | tools | Kernel differential verification + cycle benchmarking (already in architecture §3). |
| `vaco-fuzz-support` | tools | Shared fuzz-target scaffolding: `Guard`, `ProgressGuard`, structured input types, corpus-replay harness. |
| `vaco-fuzz-alloc` | tools | Counting `GlobalAlloc` used **only** in fuzz binaries. Requires `unsafe impl GlobalAlloc`; see §2.2.3 for its D2 allowlist entry and the CI assertion that it never reaches a shipped artifact. |
| `xtask` | tools | `layer-check`, `unsafe-audit`, `provenance-check`, `docs-check`, `assert-release-features`. Pure Rust; no shell scripts we cannot test. |

---

## 1. The differential conformance harness

This is the centrepiece. Everything about it is designed around one sentence: *the reference binary is an
oracle we query, never a source we read.*

### 1.1 The model

A **case** is a triple plus expectations:

```
Case = (Media, Argv, Comparison) → Verdict
```

- **Media** — a content-addressed input (or several, or none for source filters).
- **Argv** — the complete argument vector, run identically against both binaries modulo the program name
  and a small, declared set of *invocation normalisers* (§1.3.1).
- **Comparison** — one of the eight modes in §1.2, plus its parameters (tolerances, which streams to
  capture, which normalisers to apply).
- **Verdict** — `Agree`, `Divergence(allowed: DivergenceId)`, `Divergence(unexplained)`, `OursCrashed`,
  `RefCrashed`, `Skipped(reason)`.

Both sides run in a sandbox with a fixed environment: `TZ=UTC`, `LC_ALL=C`, `SOURCE_DATE_EPOCH` fixed,
no network, a scratch CWD, stdin closed (`-nostdin`), a wall-clock timeout, and an RSS cap. Any case that
depends on ambient state is by definition a broken case.

Execution is **hermetic and parallel**: cases are independent, each gets its own temp dir, and the runner
is a work-stealing pool sized to the core count. There is no shared mutable state, so a case's failure
cannot perturb another's — a property FATE does not have and which matters when you are running 120k cases.

### 1.2 Comparison modes

Ten modes. Choosing the right one is the single most important authoring decision, so each has an
explicit "applies when" and an explicit "does not apply when". C0–C8 are described here; **C9
(`three-way`) is in §1.10 and C10 (`quality-band`) in §1.11**, because both arrived with D10/D11 and
both depend on the fidelity-grading machinery.

#### C0 — `exact-bytes`

Full byte equality of the captured stream(s) after the declared normalisation chain.

**Applies to:**
- Every `vaco-probe` writer output: `default`, `compact`, `csv`, `flat`, `ini`, `json`, `xml`
  (research 05 §3.3). This is the D5 v0.1 acceptance criterion and it is non-negotiable — byte-identical,
  including field order, spacing, escaping, `N/A` handling, and `-show_optional_fields` semantics.
- Deterministic remuxes: stream copy (`-c copy`) into a container where every byte we emit is determined
  by the input plus the declared muxer options (WAV, raw, MPEG-TS with pinned PCR/PAT/PMT settings,
  Matroska with pinned cluster/timecode settings and `-fflags +bitexact`).
- Listing commands: `-formats`, `-codecs`, `-decoders`, `-encoders`, `-filters`, `-pix_fmts`,
  `-sample_fmts`, `-layouts`, `-protocols`, `-bsfs`, `-colors`, `-sections`, `-h <topic>` — for the
  subset of components we implement, after filtering to the intersection (§1.3.2).
- Exit codes, always, as a co-assertion on every mode.

**Does not apply to:** anything an encoder produced; anything containing a timestamp of creation, a
version string, or a library identification string *before* normalisation.

#### C1 — `exact-bytes-normalised`

C0 with a non-empty normalisation chain (§1.3). Kept as a distinct mode name so that a case's manifest
makes the presence of normalisation visible at a glance during review — a normaliser is a small piece of
permitted blindness and it should never be invisible.

#### C2 — `container-structure`

Both outputs are parsed by **our own** container walker into a canonical tree (box/atom hierarchy for
ISOBMFF, EBML element tree for Matroska, PES/section tree for MPEG-TS, chunk tree for RIFF), and the trees
are compared node-by-node with per-node rules (`compare`, `compare-length-only`, `ignore`,
`compare-as-set`).

**Applies to:** remuxes where byte equality is unattainable but structural equality is meaningful —
e.g. differing free-space padding, differing `mdat`/`moov` interleave chunk sizes, differing but
semantically equal EBML integer widths.

**Does not apply to:** proving decode correctness. A structurally identical file can still carry different
payload bytes; C2 must always be paired with C4 or C3 on the payload.

Note the useful asymmetry: because *our* parser reads both files, a bug in our parser shows up as a
spurious agreement, not a spurious failure. C2 therefore only ever runs alongside a mode that does not
share that parser.

#### C3 — `frame-hash` (our framecrc/framemd5 equivalent)

Per-frame digests of decoded output. **Two distinct implementations, and the distinction matters:**

- **C3a `pipe-hash` (primary, and the one to prefer).** Both binaries are told to decode to a raw stream
  (`-f rawvideo -pix_fmt <fmt> -`, or `-f f32le`/`-f s32le` for audio) and the raw bytes are piped into
  *our* hasher, which splits on the frame/sample-block size we computed from the declared format and emits
  `(index, byte_offset, size, blake3)` per frame. **This mode has no dependency whatsoever on matching
  FFmpeg's framecrc text format** — the only thing crossing the process boundary is raw samples. It is the
  highest-integrity comparison we have and it should be the default for decode conformance.
- **C3b `framecrc` / `framemd5`.** We implement `-f framecrc` and `-f framemd5` muxers for CLI
  compatibility, because scripts use them. Their *output text* is then itself a C0 conformance target.
  C3b is how we test those muxers; it is not how we test decoders.

**Applies to:** decoder conformance at scale (a 300-frame clip yields 300 independent assertions), audio
decode, filter output, scaling and resampling output.

#### C4 — `raw-exact`

Full byte equality of the complete decoded raw stream (pixels or PCM). Same pipe mechanism as C3a but
compared in full rather than by digest, so a failure reports the exact byte offset, the frame index, the
plane, the row and the column.

**Applies to:** any decoder we claim is bit-exact — which is **all of them** except where the codec's own
specification defines a tolerance (see C5). Decoders are held to bit-exactness. This is the strongest and
most important claim in the whole project.

**Failure reporting:** C4 failures produce a visual artifact (a PNG diff heat-map for video, a WAV of the
difference signal plus a per-sample max-error plot for audio) uploaded with the CI run. Debugging a
one-pixel divergence from a hex offset is miserable; debugging it from a picture of where the error lives
in the frame is usually immediate.

#### C5 — `raw-tolerant`

C4 with an explicit, per-case, justified numeric tolerance. **The tolerance is never a default; a case
that does not name one gets zero.** Parameters: `max_abs`, `max_ulp`, `max_rms`, `metric`.

**Applies only to** codecs whose specification defines conformance in terms of a tolerance rather than
bit-exactness:
- **Opus** — RFC 6716 §6 / RFC 8251: the float decoder is conformant if `opus_compare` scores above the
  threshold against the reference output; only the fixed-point decoder is bit-exact. We implement the
  RFC's comparison metric ourselves from the RFC text and use `metric = "opus-compare"`.
- **Vorbis, AAC (float paths), MP3 (float paths), AC-3** — ISO/IEC 11172-4 and 13818-4 define compliance
  as an RMS error bound against the reference decoder output, not equality.
- Float filter chains and float resampling, where the tolerance is stated in ULPs and justified by an
  error analysis in the crate's `docs/` page.

Every C5 case carries a `justification` string naming the clause that defines the tolerance. A C5 case
with a hand-waved tolerance is rejected in review.

#### C6 — `structured-diff`

Both outputs are parsed into a section tree (trivially for `json`; via our own parsers for `xml`, `ini`,
`flat`, `default`, `compact`, `csv`) and diffed field by field. Every difference is matched against the
divergence allowlist (§1.4); unmatched differences fail.

**Applies to:** the metadata surface where we *expect* a small number of allowed divergences —
`program_version`, `library_versions`, `format_long_name`, `encoder` tags — and to the exploratory sweeps
in the nightly tier where we want a field-level report rather than a binary verdict.

**Does not apply to:** the D5 acceptance criterion. C6 is weaker than C0 by construction and must never be
used to launder a C0 case that is failing. CI enforces this: a case may not be *downgraded* from C0 to C6
without a divergence-allowlist entry, and the allowlist file has a CODEOWNERS gate.

#### C7 — `behavioural`

Compares only the *class* of the outcome: exit code, whether the input was accepted or rejected, the error
category (not the message text — we write our own error prose), and whether any output was produced.

**Applies to:** malformed and truncated inputs, unsupported-feature paths, option-parsing errors, and —
crucially — the differential fuzzer (§2.4), where full-output agreement on a corrupted file is neither
achievable nor desirable.

#### C8 — `cross-decode` (the interoperability mode)

The one mode that does not compare our output to theirs directly. Four sub-checks, run as a matrix:

| # | Produce with | Consume with | Assert |
|---|---|---|---|
| X1 | reference mux | our demux+decode | frame hashes equal reference's own decode of its own file |
| X2 | our mux | reference demux+decode | reference's frame hashes equal our decode of our file |
| X3 | reference encode | our decode | our frames bit-exact against reference decode of the same bitstream |
| X4 | our encode | reference decode | reference decodes it without error and the result is within the quality band |

**This is how encoders and muxers are held to account.** State the policy plainly, because it is a
frequent source of confusion:

> **Decoders and demuxers must be bit-exact against the reference. Encoders and muxers must be
> interoperable and quality-competitive, not bit-identical.**

Our encoders are independent implementations; requiring byte-identical output would require reproducing
FFmpeg's rate-control and mode-decision heuristics, which research 07 §1.5b classifies as Tier-2
author's-original-choice material we must *not* reproduce. So encoders are judged on X4 plus a
quality-metric band — which is mode **C10 `quality-band`**, specified in §1.11.2 — and muxers on X2
plus C2. §1.11.1 gives the full table of which operation gets byte comparison and which gets quality
comparison; read it before authoring any encoder case.

### 1.3 Normalisation

Normalisers are **named, versioned, individually enabled per case, and listed in the manifest.** There is
no implicit normalisation. Each lives in `vaco-conformance/src/normalise/` with a unit test.

#### 1.3.1 Invocation normalisers (applied to argv)

| Name | Effect | Why |
|---|---|---|
| `bitexact` | Adds `-bitexact` (probe) / `-fflags +bitexact -flags +bitexact` (transcode) to both sides | Suppresses version-dependent output at source. Prefer this to output normalisation whenever it works — deleting a difference is worse than never creating it. |
| `hide-banner` | Adds `-hide_banner -nostdin` | Removes build-configuration text that is meaningless to compare. |
| `loglevel` | Pins `-loglevel <level>` on both sides | stderr volume is otherwise environment-dependent. |
| `path-token` | Copies the media into the case temp dir under a fixed name | So `format.filename` compares equal without post-hoc string surgery. |

#### 1.3.2 Output normalisers

| Name | Effect | Justification |
|---|---|---|
| `strip-sections` | Removes `program_version` and `library_versions` sections entirely | These identify the producing software; matching them is impossible and undesirable (and copying FFmpeg's version prose would be a §1.5a problem). Cases that *test* these sections use C6 with dedicated allowlist entries. |
| `line-endings` | CRLF → LF | Platform artifact of the harness, not of the program. |
| `float-canonical` | Canonicalises `-0` vs `0`, `inf` spelling, and `%f` trailing-zero forms **only when the case declares a float tolerance** | Never enabled by default; a formatting difference in probe output is a real bug. |
| `component-intersection` | For listing commands, restricts both sides to the set of components we implement | We are not obliged to have every FFmpeg codec to be conformant on the ones we do. The intersection set is computed from our registry, and the case additionally asserts that our set is a **subset** — emitting a component the reference does not have is a failure unless allowlisted. |
| `stderr-class` | Reduces stderr to a sorted multiset of severity levels | Message text is ours; severity is behaviour. |

A normaliser that would hide a class of real bug is rejected. The review question for every proposed
normaliser is: *"name a bug this would conceal."* If the answer is non-empty, it is not a normaliser, it
is a divergence-allowlist entry with a scope (§1.4), which carries much heavier governance.

### 1.4 The divergence allowlist

The dangerous part of any differential harness: the place where "it differs and that's fine" goes to
accumulate until the harness proves nothing. Design it as a governed register, not a suppression file.

#### 1.4.1 Format

Single file, `tests/conformance/divergences.toml`, CODEOWNERS-gated to the correctness owner.

```toml
schema = 1

# The ratchet. CI fails if the live count for a category exceeds its cap.
[caps]
identification         = 40
wallclock              = 15
encoder-nondeterminism = 25
float-lastbit          = 30
upstream-bug           = 20
unimplemented          = 50   # monotonically ratcheted DOWN; never raised without an RFC
unexplained            = 10   # hard ceiling, never raised

[[divergence]]
id        = "DIV-0007"
title     = "format.format_long_name differs for every format"
category  = "identification"
# Scope MUST name a concrete field or byte-region and a concrete suite selector.
# `field = "*"` and `suite = "*"` are rejected by the loader.
scope     = { suite = "probe-*", section = "format", field = "format_long_name" }
rule      = { kind = "value-differs", ours_pattern = "^.+$", theirs_pattern = "^.+$" }
justification = """
`format_long_name` is descriptive prose authored by FFmpeg. research/07 §1.4 lists copying FFmpeg
prose as RED (pure expression, zero functional necessity). We therefore author our own long names.
Downstream tooling keys on `format_name`, which we DO match byte-exactly (see suite probe-formatname).
"""
opened      = 2026-09-04
review_by   = 2027-09-04
owner       = "@correctness-owner"
approved_by = ["@correctness-owner", "@legal-liaison"]
issue       = "vaco#412"

[[divergence]]
id        = "DIV-0031"
title     = "MP4 mvhd creation_time when muxing without -bitexact"
category  = "wallclock"
scope     = { suite = "remux-isobmff-nonbitexact", box_path = "moov.mvhd", field = "creation_time" }
rule      = { kind = "both-are-wallclock", max_skew_seconds = 120 }
justification = """
Both implementations stamp the current time. `-bitexact` zeroes it on both sides and the bitexact
suite asserts C0 there; this entry only covers the deliberately-non-bitexact suite that exists to
prove we stamp *something plausible*.
"""
opened      = 2026-10-11
review_by   = 2027-04-11
owner       = "@muxer-owner"
approved_by = ["@correctness-owner", "@muxer-owner"]
issue       = "vaco#588"
```

#### 1.4.2 The six categories, and what justifies each

| Category | What legitimately lands here | The justification a reviewer must accept |
|---|---|---|
| `identification` | Strings naming the producing software: `format_long_name`, `codec_long_name` prose, `encoder` tags, `writing_application`/`muxing_app`, `program_version`, `library_versions`, our XML namespace. | *We must not copy this prose (research 07 §1.4 RED list), and downstream tooling keys on the machine name, not the prose.* The entry must name the machine-readable field that we **do** match exactly. |
| `wallclock` | `creation_time`, `modification_time`, `TDRC`-style tags, anything derived from `now()`. | *Both sides stamp the current time; there is a `-bitexact` suite that pins it and asserts C0 there.* An entry without a corresponding bitexact suite is rejected — otherwise the field is never tested at all. |
| `encoder-nondeterminism` | Reference-encoder output that is not reproducible run-to-run (threading-order-dependent rate control, `libx264`-style deterministic-only-with-`-threads 1`). | *We verified non-reproducibility by running the reference twice and diffing.* The entry must cite the verification run. If the reference **is** reproducible, this category is not available — the divergence is ours. |
| `float-lastbit` | Documented last-bit differences in float paths, with a numeric bound. | *The spec defines a tolerance* (cite the clause) *or an error analysis in `docs/<crate>.md#numerics` bounds it.* Bound must be a number, and the harness enforces it as a maximum, so it cannot silently widen. |
| `upstream-bug` | Reference behaviour that contradicts the specification and that we deliberately do not reproduce. | *Cite the spec clause we follow and the upstream ticket.* Requires a filed upstream bug (link mandatory) — if we believe it is a bug, we say so publicly. This category is the one place we are intentionally *not* identical, and it should be small and loud: every entry is listed in the release notes. |
| `unimplemented` | Temporary. A feature we have not built yet, where the reference emits something and we emit nothing. | *Issue link + a hard expiry date.* CI fails the day `review_by` passes. This is the only category that may reference a not-yet-existing capability, and its cap ratchets down. |
| `unexplained` | Escape hatch of last resort. We observe a divergence, cannot explain it from the spec, and cannot reach it by black-box probing. | Requires the full §1.7.3 triage ladder to be exhausted and recorded, **two** approvals, a 90-day expiry, and it **blocks the owning module from being marked "done"**. Hard cap of 10 across the whole project. |

#### 1.4.3 How it is prevented from becoming a dumping ground

Seven mechanisms, all machine-enforced:

1. **No wildcards in scope.** `field = "*"` or `suite = "*"` is a loader error. Every entry names a
   concrete field, box path, or byte region. A divergence you cannot localise is a bug you have not
   understood.
2. **Category caps with a ratchet.** `divergences.lock` records the live count per category. CI fails if a
   count rises above its cap **or** above its recorded value in the lock file without the lock file being
   updated in the same PR — so growth is always visible in the diff, never incidental.
3. **Expiry.** Every entry has `review_by`. CI fails on expiry. Renewal is a PR with a fresh
   justification, which forces a human to re-argue the case annually.
4. **Dead-entry detection.** The runner increments a hit counter per entry per run and emits it in the run
   report. An entry with zero hits across 30 consecutive nightly runs is reported as dead and its deletion
   is auto-proposed by a bot. Stale suppressions are how these files rot.
5. **CODEOWNERS + two approvals.** The file requires the correctness owner; `unexplained` and
   `upstream-bug` additionally require the module owner.
6. **Blast-radius report.** `just divergence-report` prints, for every entry, how many cases it currently
   suppresses. An entry suppressing >2% of a suite is flagged for review regardless of its expiry — it
   means the scope is wrong or the divergence is systemic.
7. **Publication.** The full table ships in the release notes and in `docs/conformance-divergences.md`.
   Making the list a public artifact is the cheapest possible discipline: nobody wants to explain a
   sprawling suppression list to users.

### 1.5 Case authoring, storage, discovery, execution

#### 1.5.1 Manifest format

Declarative TOML under `tests/conformance/`, one file per suite, with a **matrix expansion** so that a
20-line file yields thousands of cases. This matters: hand-writing 120k cases is not going to happen, and
a generator in code is not reviewable.

```toml
# tests/conformance/probe/isobmff.toml
schema = 1
suite  = "probe-isobmff"
tool   = "probe"                      # probe | transcode | play-headless
tier   = "core"                       # smoke | core | full | manual
owner  = "@isobmff-owner"

# --- inputs ---------------------------------------------------------------
[[media]]
id     = "h264-aac-30f"
source = "corpus://vaco/mp4/h264-aac-30f.mp4"     # resolved via corpus/*.lock
tags   = ["video", "audio", "progressive"]

[[media]]
id     = "prores-10f"
source = "corpus://vaco/mov/prores422-10f.mov"
tags   = ["video", "intra"]

[[media]]
id     = "frag-init"
source = "corpus://vaco/mp4/fragmented-init.mp4"
tags   = ["fragmented"]
tier   = "full"                        # per-media tier override

# --- argument matrix ------------------------------------------------------
# Cases = cartesian product of all axes, minus `exclude`, plus `extra`.
[[axis]]
name = "writer"
values = [
  { id = "default",       argv = ["-of", "default"] },
  { id = "default-nk",    argv = ["-of", "default=nk=1:nw=1"] },
  { id = "compact",       argv = ["-of", "compact"] },
  { id = "csv",           argv = ["-of", "csv"] },
  { id = "flat",          argv = ["-of", "flat"] },
  { id = "flat-nonhier",  argv = ["-of", "flat=h=0"] },
  { id = "ini",           argv = ["-of", "ini"] },
  { id = "json",          argv = ["-of", "json"] },
  { id = "json-compact",  argv = ["-of", "json=c=1"] },
  { id = "xml",           argv = ["-of", "xml"] },
  { id = "xml-strict",    argv = ["-of", "xml=x=1"] },
]

[[axis]]
name = "sections"
values = [
  { id = "fmt",      argv = ["-show_format"] },
  { id = "streams",  argv = ["-show_streams"] },
  { id = "all",      argv = ["-show_format", "-show_streams", "-show_chapters", "-show_programs"] },
  { id = "packets",  argv = ["-show_packets", "-read_intervals", "%+#20"] },
  { id = "frames",   argv = ["-show_frames", "-read_intervals", "%+#10"], tier = "full" },
  { id = "entries",  argv = ["-show_entries", "stream=index,codec_type:format=duration"] },
]

[[axis]]
name = "pretty"
values = [
  { id = "plain",  argv = [] },
  { id = "pretty", argv = ["-pretty"] },
  { id = "sexa",   argv = ["-sexagesimal"], tier = "full" },
]

# Combinations that are meaningless or that the reference itself rejects.
[[exclude]]
when   = { writer = "xml-strict", pretty = ["pretty", "sexa"] }
reason = "xsd_strict is documented as incompatible with -unit/-prefix/-sexagesimal (research 05 §3.3)"

# --- comparison -----------------------------------------------------------
[compare]
mode      = "exact-bytes"
capture   = ["stdout", "exit-code"]
stderr    = "class-only"
timeout   = "20s"

[normalise]
invocation = ["bitexact", "hide-banner", "loglevel", "path-token"]
output     = ["strip-sections", "line-endings"]
```

A transcode suite looks the same but names `tool = "transcode"`, captures an output file, and typically
uses C3a/C4/C8:

```toml
# tests/conformance/decode/h264-bitexact.toml
schema = 1
suite  = "decode-h264"
tool   = "transcode"
tier   = "core"
owner  = "@h264-owner"

[[media]]
id     = "conf-BA1_Sony_D"
source = "suite://itu-h264/AVCv1/BA1_Sony_D.jsv"   # fetched, never vendored (§4)
tags   = ["baseline"]

[[axis]]
name   = "threads"
values = [
  { id = "t1", argv = ["-threads", "1"] },
  { id = "t4", argv = ["-threads", "4"] },
  { id = "auto", argv = ["-threads", "0"], tier = "full" },
]

[compare]
mode      = "raw-exact"                  # C4
capture   = ["output-file", "exit-code"]
raw       = { kind = "video", pix_fmt = "yuv420p" }
output    = ["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]
tolerance = { max_abs = 0, max_ulp = 0 }   # explicit zero; never implicit
timeout   = "120s"
```

#### 1.5.2 Case identity

The case id is stable and derived deterministically:

```
probe-isobmff/h264-aac-30f/writer=json,sections=all,pretty=plain
```

It appears in the failure report, and the report always ends with the exact reproduction command:

```
just conformance-run 'probe-isobmff/h264-aac-30f/writer=json,sections=all,pretty=plain'
```

which re-runs that single case, prints both argv lines, both outputs, the unified diff, and the pinned
reference version. Reproduction friction is the main reason differential harnesses get ignored; making
this one command is worth the plumbing.

#### 1.5.3 Authoring flow

```
just conformance-new probe-isobmff        # scaffolds a manifest from a template
just conformance-run probe-isobmff        # runs the suite locally
just conformance-explore <media> <argv…>  # ad-hoc: runs both, shows the diff, suggests a manifest stanza
just conformance-report                   # HTML report: pass/fail/skip by suite, divergence hit counts
```

`conformance-explore` is the workhorse for the implementer's inner loop: it is the sanctioned way to
interrogate the oracle. It never writes anything to the repo; the author copies the suggested stanza in
by hand, which keeps a human in the loop on every case that lands.

There is deliberately **no** `--update-expected` / golden-file blessing mode. There are no golden files —
the reference is regenerated every run. This removes the single most common failure mode of snapshot
testing (blessing a regression) and it removes the §1.5d compilation-copyright question entirely, because
no FFmpeg-derived output ever lands in the repository.

#### 1.5.4 Discovery and skipping

The runner walks `tests/conformance/**/*.toml`, expands matrices, filters by tier and by
`--suite`/`--media`/`--tag` selectors, then resolves media. A case is `Skipped` (not failed) when:

- the media is not present locally and `--offline` is set;
- the case requires a feature not compiled into the binary under test (declared as `requires = ["codec-h264"]`);
- the reference binary lacks the component (detected by probing its listing output, never by version-sniffing).

Skips are counted and reported; a tier fails if its skip rate exceeds a declared budget, so silent
erosion of coverage is impossible.

### 1.6 The reference binary

#### 1.6.1 What is pinned, and how

`tools/refbin/refspec.toml`:

```toml
schema = 1

[stable]                                     # gates CI
version      = "8.0"
tarball      = "https://ffmpeg.org/releases/ffmpeg-8.0.tar.xz"
sha256       = "…"
configure    = [
  "--disable-gpl", "--disable-nonfree", "--disable-doc",
  "--disable-programs", "--enable-ffmpeg", "--enable-ffprobe",
  "--disable-autodetect",
  # explicit component set, mirroring our `default` feature tier
]
image_digest = "ghcr.io/vaco/refbin@sha256:…"

[next]                                       # nightly only, non-blocking
version      = "8.1"
tarball      = "https://ffmpeg.org/releases/ffmpeg-8.1.tar.xz"
sha256       = "…"
configure    = [ … ]
image_digest = "ghcr.io/vaco/refbin@sha256:…"

[full]                                       # nightly only; broad component set for exploratory sweeps
version      = "8.0"
configure    = ["--enable-gpl", …]
image_digest = "ghcr.io/vaco/refbin-full@sha256:…"
```

**Which version.** A released tag, never a git snapshot — snapshots are unreproducible oracles. Two pins
are live at all times: `stable` (gates) and `next` (nightly, advisory). `full` exists because some
divergences are only visible with components our default reference build omits; it is never a gate.

**How obtained.** A dedicated workflow downloads the pinned tarball, verifies the SHA-256, builds it
inside a pinned base-image digest with the pinned configure line, and pushes the result to our registry.
CI then pulls by **image digest**, not tag. Reference builds are never done on a developer machine as part
of the normal loop; `just refbin-pull` fetches the image.

**Why the configure line is pinned and narrow.** The reference must be configured to approximate our
`default` feature tier. Otherwise a case fails because the reference has a component we deliberately do
not ship, and the harness spends its credibility on noise. `--disable-autodetect` is the important flag:
it makes the build a function of the configure line rather than of whatever happened to be installed on
the builder.

**Distribution.** The reference image is a *test dependency*. It is never linked, never vendored, never
placed in a release artifact, and the release CI asserts its absence. The image we publish is a build of
GPL/LGPL software, so the publishing workflow also publishes the exact source tarball URL, its hash, and
our build script alongside it, and the registry description carries the corresponding written offer.
*(Flag for counsel — research 07 §5.4: confirm that publishing a built reference image to our own
registry with source-and-scripts alongside satisfies GPL §3/§6. The zero-risk alternative is to build in
CI on every run with an aggressive layer cache and publish nothing; adopt that if counsel is unsure.)*

#### 1.6.2 When upstream changes behaviour

Version bumps go through `just refbin-bump <version>`, which:

1. Builds the candidate reference.
2. Runs `conformance-full` against **both** the old and the new pin.
3. Emits a **behaviour-drift report**: every case whose *reference output* changed between the two
   versions, grouped by suite, with diffs.
4. Refuses to complete until every drift line is triaged into one of four buckets, recorded in the PR:

| Bucket | Meaning | Action |
|---|---|---|
| `follow` | Upstream fixed a bug; the new behaviour matches the spec better. | We change to match. Normal implementation PR, with a spec citation. |
| `regression` | Upstream introduced a divergence from the spec. | Allowlist entry, `category = "upstream-bug"`, upstream ticket filed. |
| `intentional-change` | Documented behaviour change (e.g. a new default). | We follow, and note it in our own release notes as a compatibility note. |
| `harness-artifact` | Our normalisation or configure line was version-sensitive. | Fix the harness; not a behaviour question. |

Because `next` runs nightly and non-blocking, drift is discovered weeks before the bump, and the triage
work is spread out rather than blocking a release. This is the whole reason for the two-pin design.

**Multi-version assertion.** The nightly also runs `conformance-core` against the previous major. Cases
that pass on one version and fail on the other, in either direction, reveal that we have encoded
version-specific behaviour — usually a sign we implemented what the binary does rather than what the spec
says. Those are reported as `version-sensitive` findings and reviewed.

### 1.7 The clean-room argument

This section is normative. It is what a contributor reads before touching the harness.

#### 1.7.1 Why running the reference binary is safe

Clean room is an evidentiary technique that defeats proof of **access to** and **substantial similarity of
protected expression** (research 07 §1.1). Black-box differential testing touches neither element:

1. **We never access the expression.** The harness consumes an executable's observable output. It does not
   read, decompile, or disassemble the program. The thing we acquire is a set of *facts about behaviour* —
   "given this file and these flags, this program printed these bytes." Facts are not copyrightable
   (*Feist Publications v. Rural Telephone*, 499 U.S. 340 (1991)).
2. **Behaviour is functionality, and functionality is filtered out.** Under *Computer Associates v. Altai*,
   982 F.2d 693 (2d Cir. 1992), elements dictated by external factors — interoperability requirements,
   industry practice, published standards — are filtered out before any similarity comparison. Our output
   matches theirs because both conform to the same specification and the same command vocabulary, which is
   exactly the explanation *Altai* filtration is designed to credit. *SAS Institute v. World Programming*,
   C-406/10 (CJEU 2012), holds directly that the functionality of a program and its data file formats are
   not protected by copyright, and that observing, studying and testing a program to determine its
   underlying ideas is lawful.
3. **EU law expressly permits it.** Directive 2009/24/EC Art. 5(3) grants a lawful user the right to
   observe, study or test the functioning of a program to determine the ideas and principles underlying
   it, while performing acts they are entitled to perform. Art. 8 makes that right **non-waivable by
   contract**. Running `ffprobe` on a file is the paradigm case.
4. **Behavioural convergence is the goal, and it is not evidence of copying.** research 07 §1.6.2 step 5
   says this explicitly: "Behaviour convergence is fine and expected — that is conformance, not copying."
5. **Nothing FFmpeg-derived enters our repository.** By design (§1.5.3) there are no golden files. The
   reference's output exists only in a temp directory for the duration of a case. This sidesteps the
   FATE-compilation question (research 07 §1.5d) completely rather than arguing about it.

#### 1.7.2 The bright-line rule

> **You may run the reference binary as often as you like. You may not read its source.
> When our output differs and you cannot explain why, you escalate — you do not go looking in the source
> for the answer.**

That is the whole rule. It fits on a sticker and it goes in `CONTRIBUTING.md`, in the PR template, and at
the top of `tests/conformance/README.md`.

#### 1.7.3 The divergence triage ladder

When a case fails and the cause is not obvious, work down this ladder. Steps 1–4 keep you clean. Step 5
requires someone else. Step 6 is an admission of defeat and is capped.

1. **Re-read our spec document and the public standard clause** the code was written from. Roughly half of
   all divergences are a misread normative clause, and the answer is in the document you already have.
2. **Minimise the input.** `just conformance-minimise <case-id>` bisects the media (structure-aware, using
   our own parsers) down to the smallest file that still diverges. A 12-byte reproducer usually explains
   itself.
3. **Interrogate the oracle.** Vary flags, vary the input, run the reference on synthetic files you
   construct, use `-loglevel trace` on the reference, run its `-h` output. *This is what the oracle is
   for*, and it is unlimited. Most remaining divergences fall here.
4. **Read Tier-A material** (research 07 §1.6.1): FFmpeg's user documentation, man pages, Trac tickets,
   mailing-list threads, and commit **messages**. Commit messages are prose *about* the change and are
   Tier A; the diffs they link to are Tier B. Do not click through to the diff.
5. **Escalate to a gatekeeper.** File a `question: behaviour` issue describing the observed divergence.
   A designated dirty-team member for that module reads whatever they need to and replies with a
   **behavioural statement** — a description of what the program does, written as facts, added to
   `spec/<module>.md` and gatekeeper-reviewed for expression leakage. You, the requester, remain clean; the
   gatekeeper becomes (or already is) dirty for that module and may not author implementation code there.
   The resulting commit carries `Vaco-Provenance: cleanroom-doc:spec/<module>.md`.
6. **Record it as `unexplained`.** Hard cap of 10 project-wide, 90-day expiry, blocks the module from being
   marked done. If you are reaching for this often, the spec document for that module is inadequate and
   that is the real problem.

#### 1.7.4 What would cross the line

Contributors should be able to recognise the failure modes by name:

| Action | Verdict | Why |
|---|---|---|
| Running the reference 10,000 times a night in CI | **Fine** | Observation at scale is still observation. |
| Storing a minimised *input* that triggers a divergence | **Fine** | It is our file (or a licensed corpus file), not their expression. |
| Storing the reference's *output* as a committed expected value | **Avoid** | research 07 §1.5d: the individual line is a fact, the curated set is a compilation-copyright risk and a bad look. Our design has no golden files, so this cannot happen by accident. |
| Opening `libavformat/mov.c` because a box parses differently | **Line crossed** | You are now a dirty-team member for that module and may not commit implementation code to it. This is the specific act §1.7.2 forbids. |
| Asking an LLM "how does FFmpeg parse the `elst` box?" | **Line crossed** | research 07 §1.6.4. The model has FFmpeg in its training data; this is source access with extra steps and it destroys the provenance record. |
| Asking an LLM "what does ISO/IEC 14496-12 §8.6.6 say the `elst` box contains?" | **Fine** | Question about a public standard. Verify against the standard itself, and cite the clause in the trailer. |
| Disassembling the reference binary to recover an algorithm | **Line crossed** | This is reverse-engineering the expression, not black-box observation. Permitted only via the two-team T2 route (research 07 §1.7) for formats with no published spec, and never for anything with a spec. |
| Copying a constant table out of FFmpeg because "it's from the spec anyway" | **Line crossed** | research 07 §1.5b: FFmpeg's tables are often reordered, pre-scaled or packed for their implementation; those transformations are theirs. Transcribe from the spec and record the clause. |

#### 1.7.5 The harness itself is ours

`vaco-conformance` shares no code, no file layout, no test-declaration syntax and no comparison-tool
lineage with FATE. The concepts it uses — run a command, compare to a reference, allow a tolerance — are
methods of operation, universally used, and unprotectable. The specific expression is written from
scratch. Same for `vaco-checkasm` (GPL upstream) and our SSIM/PSNR implementations (GPL upstream;
we implement from Wang, Bovik, Sheikh & Simoncelli, *IEEE TIP* 13(4), 2004, and from the standard PSNR
definition).

### 1.8 Tiers, gating, and time budget

| Tier | Trigger | Cases (at full project scope) | Budget | Blocking |
|---|---|---|---|---|
| `smoke` | Every PR | ~400 | **≤ 4 min** on an 8-core runner | **Yes** |
| `core` | Merge to `main`; also on PR when the PR touches a crate a suite maps to | ~6,000 | **≤ 20 min** sharded 4× | **Yes** |
| `full` | Nightly | ~120,000 (matrix expansion + conformance suites) | **≤ 3 h** sharded 16× | Opens an issue; blocks release |
| `exhaustive` | Weekly + pre-release | ~1.2M (full corpus × full option matrix, plus `next` and `full` reference pins) | ≤ 12 h | Blocks release |

**Composition of `smoke`** (the only tier most contributors ever see): one media file per container family,
one case per ffprobe writer, one C4 decode case per codec we ship, one remux case per muxer, the option-
parsing cases, and every case that has ever caught a regression (promoted automatically — see below).

**Regression promotion.** When a `core`/`full`/`exhaustive` case fails and is then fixed, the fixing PR
promotes that case to `smoke` by adding `promote = "smoke"` to its media or axis entry. The smoke tier
therefore concentrates over time on the cases that empirically catch bugs, which is the only sound basis
for choosing a fast subset. A budget guard fails CI if `smoke` exceeds 4 minutes, forcing a periodic
demotion review rather than unbounded growth.

**Suite→crate mapping.** `tests/conformance/suites.toml` maps each suite to the crates it exercises, so a
PR touching `crates/format/vaco-demux-matroska` automatically runs the Matroska `core` suites on the PR
rather than waiting for merge. Mapping is verified by a nightly job that runs each suite with the mapped
crates instrumented and fails if a suite covers a crate it does not declare.

### 1.9 Implementation shape

```rust
// crates/tools/vaco-conformance/src/lib.rs
pub struct Case {
    pub id:        CaseId,
    pub tool:      Tool,                 // Probe | Transcode | PlayHeadless
    pub media:     Vec<MediaRef>,
    pub argv:      Vec<String>,          // tool-neutral; the runner prepends the binary
    pub compare:   Compare,
    pub normalise: Normalisers,
    pub requires:  Vec<FeatureName>,
    pub timeout:   Duration,
}

pub enum Compare {
    ExactBytes      { capture: Captures },
    ContainerStruct { walker: WalkerKind, rules: Vec<NodeRule> },
    FrameHash       { raw: RawSpec, algo: HashAlgo },
    RawExact        { raw: RawSpec },
    RawTolerant     { raw: RawSpec, tol: Tolerance, metric: Metric },
    StructuredDiff  { parser: SectionParser },
    Behavioural     { class: BehaviourClass },
    CrossDecode     { legs: Vec<CrossLeg> },
}

pub enum Verdict {
    Agree,
    AllowedDivergence(DivergenceId),
    Divergence(DiffReport),
    OursFailed(FailureKind),      // panic, non-zero exit we did not expect, timeout, limit exceeded
    ReferenceFailed(FailureKind),
    Skipped(SkipReason),
}

pub trait Runner {
    fn run(&self, case: &Case, bin: &Binary, dir: &Path) -> io::Result<Observation>;
}

pub struct Observation {
    pub stdout:    Vec<u8>,
    pub stderr:    Vec<u8>,
    pub exit:      ExitStatus,
    pub artifacts: BTreeMap<String, PathBuf>,
    pub wall:      Duration,
    pub peak_rss:  u64,
}
```

The runner is a plain work-stealing pool over cases; each case gets a fresh temp dir, both processes are
spawned with `Command`, output is captured with a size cap (a runaway reference must not fill the disk),
and the temp dir is retained only on failure and uploaded as a CI artifact.

### 1.10 Three-way comparison (D11)

D10 admits external pure-Rust crates; D11 requires each to sit behind exactly one Vaco crate with
mutually exclusive `backend-external` / `backend-native` features. That gives us a third oracle, and a
two-way pass/fail throws away most of its value.

#### 1.10.1 The mode

**C9 — `three-way`.** Run the same case against three implementations:

| Slot | Binary / build | Notes |
|---|---|---|
| `N` | `vaco` built with `--features <crate>/backend-native` | our own implementation |
| `X` | `vaco` built with `--features <crate>/backend-external` | the wrapped crate, behind our API |
| `R` | the pinned reference binary | §1.6 |

Because the backend is a feature of *our* binary and D11 forbids any external type from crossing the
crate boundary, `N` and `X` are the same program with one crate's internals swapped. The argv is
identical for all three; only `R`'s program name differs. This is the property that makes the
comparison meaningful — every difference is attributable to the codec, not to the harness or the CLI.

#### 1.10.2 The verdict lattice

Three observations give five outcomes, and the classification is the whole point:

| Pattern | Verdict | Reading | Action |
|---|---|---|---|
| `N = X = R` | **Agree** | Everyone converged. | Pass. |
| `N = R ≠ X` | **ExternalDiverges** | *The wrapped crate is the outlier.* | Strong evidence to promote `backend-native` to default and retire the wrap. Files against the external crate's grade, not ours. |
| `X = R ≠ N` | **NativeDiverges** | *Our implementation is the outlier.* | **Our bug, with high confidence.** Highest-priority finding — two independent implementations agree against us. |
| `N = X ≠ R` | **BothDivergeFromReference** | Two independent implementations agree with each other and disagree with the reference. | Either the reference is doing something extra-specification, or both of us misread the same clause. Triage ladder §1.7.3, and a strong candidate for `category = "upstream-bug"` — but only after re-reading the spec, because "two implementations agreed" is weaker evidence than it feels. |
| all three differ | **Scattered** | The specification is ambiguous, or the input is malformed and no behaviour is defined. | Escalate. Almost always means the case is under-specified, not that three implementations are all broken. |

`NativeDiverges` and `ExternalDiverges` between them localise the defect in one run. That is worth
roughly an afternoon of bisection per finding, which is why C9 is worth the extra build.

#### 1.10.3 Manifest and cost

```toml
[compare]
mode   = "three-way"
inner  = "raw-exact"                 # the pairwise comparison applied within the lattice
slots  = { native = "vaco-codec-flac/backend-native",
           external = "vaco-codec-flac/backend-external" }
tier   = "core"
```

`inner` may be any of C0–C6, so three-way works for probe output, frame hashes, raw pixels and
structured diffs alike. The runner builds the two backend variants once per session and caches them by
feature-set hash; the per-case cost is one extra process, not one extra build.

**Where it runs.** Every codec crate that currently has *both* backends is in the `core` tier for
three-way — this is exactly the migration window where the mode pays for itself. Once a crate drops one
backend, its cases fall back to C0–C8. The `backend-matrix` CI job (§5) additionally builds `default`
with every backend flipped, so a native backend cannot rot while `backend-external` is the default.

**The differential fuzzer inherits it.** `diff_` campaigns (§2.4) run three-way wherever both backends
exist, using the same lattice. `NativeDiverges` from a fuzzer is the single most valuable signal the
whole programme produces: an input on which two independent implementations agree and we do not.

### 1.11 Byte comparison versus quality comparison — where the boundary sits

Getting this boundary wrong is the failure mode the coordinator correctly flagged. Put it too far one
way and we mask real bugs; too far the other and CI produces permanent, ignorable false failures. State
it as a rule with no ambiguity:

> **Byte comparison applies to every operation whose output is fully determined by its input and its
> declared options. Quality comparison applies to every operation that involves a lossy encoder's
> rate–distortion decisions.**

The dividing question is not "is the codec lossy?" — it is "does the specification determine the output
bits?". Lossy *decoding* is fully determined (that is what a decoder conformance spec is *for*). Lossy
*encoding* is not: the bitstream is a search result, and no two searchers agree.

#### 1.11.1 The classification table

| Operation | Comparison | Rationale |
|---|---|---|
| **Demux** (all containers) | C0 exact / C2 structural | Packet boundaries and timestamps are determined by the file. |
| **Probe / metadata output** (all writers) | **C0 exact bytes** | The D5 acceptance criterion. |
| **Decode — lossless codecs** (FLAC, ALAC, PCM, ADPCM, FFV1, PNG, utility/lossless video) | **C4 raw-exact, zero tolerance** | Lossless means lossless. Any difference is a bug, full stop. |
| **Decode — lossy integer-defined codecs** (H.264, HEVC, VVC, AV1, VP8, VP9, MPEG-1/2/4, H.263, ProRes, DNxHD, JPEG at the IDCT the spec pins) | **C4 raw-exact, zero tolerance** | These specifications define decoding as exact integer arithmetic. Conformance suites (§4) exist precisely to assert bit-exactness. A tolerance here would conceal the most common real decoder bug — an off-by-one in a transform or a predictor. |
| **Decode — spec-tolerant lossy codecs** (Opus float path, Vorbis, AAC float path, MP3 float path, AC-3, DTS) | **C5 raw-tolerant**, bound cited from the spec | The specification itself defines conformance as a bounded error, not equality (RFC 6716 §6 / RFC 8251 for Opus; ISO/IEC 11172-4 and 13818-4 RMS bounds for MPEG audio). The fixed-point Opus decoder path, where we implement one, is C4. |
| **Bitstream filters** | **C0 exact bytes** | A BSF is a defined transformation of bytes. |
| **Remux / stream copy** | **C0** where deterministic, **C2 + C8/X2** otherwise | Payload bytes must be identical; container framing may legitimately differ (§1.2 C2). |
| **Scale, pixel-format conversion** | **C4 raw-exact** where the algorithm is pinned by the flags; **C5** for float intermediate paths with a stated ULP bound | Our ops-graph will not match FFmpeg's kernel decomposition bit-for-bit in every mode. Where it cannot, that is an `Equivalent` grade with a bound, not a licence to be sloppy — and `inv_scale_graph` (§2.4.4) still holds our own graph to bit-exactness against our own scalar reference. |
| **Resample, rematrix, sample-format conversion** | **C4** for integer paths and identity cases; **C5** with a stated SNR bound for filtered rate conversion | Same reasoning. |
| **Encode — lossless codecs** (FLAC, ALAC, PNG, FFV1, utility) | **C0 exact bytes achievable, and required once we claim it** | A lossless encoder with the same declared parameters *can* be made bit-identical, because the format pins the residual coding. Where we deliberately choose a different (better) search, that is a documented `Equivalent` with X4 as the acceptance test. |
| **Encode — lossy codecs** (AV1, VP8, VP9, Opus, Vorbis, MP3, AAC, and every future one) | **C10 quality-band. Byte comparison is not merely hard here — it is meaningless.** | Our AV1 encoder will never match libaom's bitstream, and neither will `rav1e`'s, and libaom's does not match its own across versions or thread counts. Asserting bytes would produce a permanent red that everyone learns to ignore, which is worse than no test. |

#### 1.11.2 C10 — `quality-band`

The encoder comparison mode. It does not compare bitstreams at all.

```
source ──┬─▶ reference encode (pinned opts) ──▶ ref bitstream ──▶ decode ──▶ ref recon
         └─▶ our encode      (same opts)     ──▶ our bitstream ──▶ decode ──▶ our recon

assert:  quality(our recon, source)  ≥  quality(ref recon, source) − Δq
         size(our bitstream)         ≤  size(ref bitstream)        × (1 + Δs)
         time(our encode)            ≤  time(ref encode)           × Δt
         reference decoder accepts our bitstream without error or warning   (C8/X4)
         our decoder accepts the reference bitstream bit-exactly            (C8/X3)
```

- **Metrics:** PSNR (Y and per-plane), SSIM (from Wang et al. 2004 — implemented by us, *not* from
  `tiny_ssim.c`, which is GPL), and a VMAF-equivalent perceptual metric where a permissively-licensed
  pure-Rust implementation clears the D10 gates; otherwise PSNR+SSIM only, and we say so. For audio:
  ODG-style or, for Opus specifically, the RFC's own `opus_compare` criterion reimplemented from the RFC.
- **Δq, Δs, Δt are per-codec, per-preset, declared in the manifest, and reviewed like a tolerance
  (§1.12.3).** They are *bands*, not point targets, and the manifest records both the floor (fail) and
  the watch threshold (report).
- **Ratchet, not a fixed bar.** `tests/conformance/quality.lock` stores the measured BD-rate and speed of
  every encoder case at its last accepted value. CI fails on regression beyond the band and **records an
  improvement automatically**, so the bar only ever moves in our favour. This is what turns a noisy
  quality comparison into a usable gate.
- **Determinism requirement on our side.** Our encoders must be bit-reproducible for a fixed
  (input, options, thread count, seed). That is a property we control and we assert it directly:
  `inv_encode_determinism` runs the same encode twice and requires identical bytes. Non-reproducibility
  in *our* encoder is a bug even though non-reproducibility in the reference's is expected.
- **Determinism is not required of the reference.** C10 re-runs the reference encode only when the
  quality lock is being refreshed, and pins `-threads 1` plus any documented deterministic flags to
  reduce noise; residual noise is absorbed by the band.
- C10 runs in the `full` tier (nightly) and pre-release. It is too slow and too noisy for a PR gate;
  the PR gate for encoders is X4 (does the reference decode our output?) plus `inv_encode_determinism`,
  both of which are fast and binary.

#### 1.11.3 The trap to avoid

Do not let C10 leak leftward. A decoder failing C4 must never be "fixed" by reclassifying it as C5 or
C10 — that is exactly the C0→C6 downgrade the allowlist governance in §1.4.3 blocks, and the same rule
applies here. Reclassification of any case to a weaker mode requires a `divergences.toml` entry, two
approvals and an expiry. The comparison mode a case uses is a claim about the specification, not a knob
for making CI green.

### 1.12 Fidelity grading (D11)

D11 requires every codec to carry a grade in `docs/codec-status.md`, established by the harness and
re-checked in CI. Grading is not a separate mechanism — it is the harness's aggregate verdict over a
codec's cases, and it shares its governance with the divergence allowlist.

#### 1.12.1 How the grade is computed

`vaco-conformance grade` runs, for one codec, **its full case set at the `full` tier** across the whole
corpus assigned to it, then reduces:

```
for each case → Verdict
grade(codec) =
    Unmeasured  if  the codec has no cases, or coverage < the declared corpus floor,
                    or the last successful grading run is older than 30 days
    Divergent   if  any case yields Divergence(unexplained), OursFailed, or a
                    NativeDiverges/Scattered three-way verdict
                    OR the codec's only passing cases rely on an expired or
                       `unexplained`-category allowlist entry
    Equivalent  if  every case yields Agree or AllowedDivergence, and at least one
                    AllowedDivergence is in scope, and every applied entry is a live,
                    non-expired, category ∈ {identification, wallclock, float-lastbit,
                    encoder-nondeterminism, upstream-bug} entry
    Exact       if  every case yields Agree with zero allowlist entries applied
                    and zero tolerance consumed (measured, not declared: the harness
                    records the *observed* maximum deviation, and Exact requires it to be 0)
```

Two details that make the grade honest rather than decorative:

- **`Exact` is measured, not declared.** A case may declare a tolerance and still observe zero deviation.
  The harness records observed maxima; a codec whose declared tolerances are never consumed is graded
  `Exact` and the unused tolerances are reported as removable. This is the same dead-entry pressure as
  §1.4.3(4), applied to tolerances.
- **`Unmeasured` has a staleness clock.** A codec graded once and never re-run is not measured, it is
  remembered. 30 days without a successful grading run reverts it to `Unmeasured`, which (per D11) means
  it cannot ship in a default build. That is deliberately aggressive: it makes the nightly grading run
  load-bearing, so it gets fixed when it breaks.

Grades are computed **per backend** where both exist, and the codec's effective grade is the grade of the
backend selected by the `default` feature set. A crate whose `backend-external` is `Divergent` but whose
`backend-native` is `Exact` ships with `backend-native` in `default` — which is precisely the promotion
decision D11 wants the data to drive.

#### 1.12.2 How it is recorded

The harness writes a machine-readable lock file; the human-readable page is generated from it, so the two
cannot disagree.

```toml
# tests/conformance/fidelity.lock   — generated by `just grade`, committed, diffable
schema = 1
generated = 2026-11-14T02:11:07Z
reference = { pin = "stable", version = "8.0", image = "sha256:…" }

[[codec]]
name          = "flac"
crate         = "vaco-codec-flac"
backend       = "backend-native"
operation     = "decode"
grade         = "Exact"
cases         = 1842
corpus_bytes  = 412_338_112
observed_max_deviation = 0
divergences   = []
graded_at     = 2026-11-14T02:03:11Z

[[codec]]
name          = "flac"
crate         = "vaco-codec-flac"
backend       = "backend-external"     # claxon
operation     = "decode"
grade         = "Equivalent"
cases         = 1842
divergences   = ["DIV-0044"]           # 24-bit residual edge case, bounded
observed_max_deviation = 0
graded_at     = 2026-11-14T02:07:52Z

[[codec]]
name          = "av1"
crate         = "vaco-codec-av1"
backend       = "backend-external"     # rav1e
operation     = "encode"
grade         = "Equivalent"
mode          = "quality-band"
bd_rate_vs_ref = 3.8                   # % worse; band allows 8.0
speed_vs_ref   = 0.71                  # ×; band allows 0.50
divergences   = ["DIV-0061"]
graded_at     = 2026-11-14T02:41:19Z

[[codec]]
name      = "vp9"
crate     = "vaco-codec-vp9"
backend   = "backend-native"
operation = "decode"
grade     = "Unmeasured"
reason    = "corpus coverage 41% < floor 80%"
```

`docs/codec-status.md` is **generated** from this file by `just codec-status` and CI fails if the
committed page differs from the regenerated one. The page carries, per codec: grade, operation, backend,
case count, corpus bytes, the divergence ids in play, the quality numbers for encoders, and the grading
timestamp. No hand-maintained status table, ever — those are always wrong within a month.

#### 1.12.3 How a tolerance gets proposed and reviewed — unified with the allowlist

The coordinator's instinct is right: this is the same discipline, so it is the same file and the same
governance. **A tolerance is a divergence-allowlist entry with numeric parameters.** Two new categories
join the six in §1.4.2:

| Category | Covers | Extra required fields |
|---|---|---|
| `spec-tolerance` | A bound the *specification itself* defines (Opus, MPEG audio RMS bounds). | `spec_clause` (mandatory), `bound`, `metric`. Reviewer's test: *does the cited clause actually state this bound?* |
| `quality-band` | Encoder C10 bands: Δq, Δs, Δt. | `metric`, `floor`, `watch`, `measured_at`, `lock_ref`. Reviewer's test: *is the band justified by measured variance, and is the ratchet wired up?* |

The proposal flow, deliberately identical to §1.4:

1. Author runs `just conformance-explore` and establishes the *observed* deviation across the corpus, not
   a guess. The proposal must carry the observed distribution (max, p99, mean), because a tolerance
   proposed without measurement is a wish.
2. Author adds a `divergences.toml` entry with the numeric bound set to **the observed maximum rounded up
   to the nearest meaningful unit, not to a round number that leaves headroom.** Headroom is where future
   regressions hide.
3. Two approvals, CODEOWNERS-gated: the correctness owner plus the module owner. `spec-tolerance` entries
   additionally require the reviewer to confirm the cited clause.
4. The entry gets a `review_by`, counts against its category cap, and is subject to the same dead-entry
   detection — a tolerance never consumed for 30 days is proposed for deletion, which automatically
   upgrades the codec from `Equivalent` toward `Exact`.
5. The harness enforces the bound as a **maximum**, and records the observed value every run. Silent
   widening is impossible: widening the number is a diff in a CODEOWNERS-gated file.

The effect is that `Equivalent` is never a vague status. It always decomposes into a specific list of
numbered, expiring, owned, measured entries — which is exactly what makes the promotion decision in D11
evidence-based rather than a matter of opinion.

#### 1.12.4 How CI enforces "Unmeasured and Divergent cannot ship in a default build"

Three independent checks, because this is a shipping gate and one check is not enough.

**(a) `codec-status` (every PR, blocking).**
```
just grade --check
```
Regenerates `docs/codec-status.md` from `fidelity.lock` and fails on any difference; validates that every
codec reachable from the `default` feature set has `grade ∈ {Exact, Equivalent}`; validates that every
`Equivalent` grade's divergence ids exist, are live, are non-expired, and are in an approved category;
fails on any `Unmeasured` or `Divergent` codec in `default`. Because the lock file is committed, this
check needs no reference binary and runs in seconds on a PR.

**(b) `feature-gate-consistency` (every PR, blocking).**
An `xtask` that reads `fidelity.lock` and the workspace feature graph and asserts the implication in both
directions:
- every codec with `grade ∈ {Divergent, Unmeasured}` is **not** reachable from `vaco`/`vaco-probe`/
  `vaco-play` with `--features default` (checked via `cargo tree -e normal --features default` plus the
  generated registry manifest, so a codec cannot sneak in through a transitive feature);
- every codec reachable from `default` **has** a row in `fidelity.lock`. A codec with no row is
  `Unmeasured` by definition and fails. This is the direction that catches a newly added codec whose
  author forgot to grade it — the common case.

**(c) `release-fidelity` (release workflow, blocking).**
Runs the **actual grading** against the actual reference (not the lock file) for every codec in the
release feature set, and requires the freshly computed grades to match the committed lock exactly. This
is the check that catches a stale or hand-edited lock file. It is slow (it is a `full`-tier run) and so it
only guards releases — but nothing ships without it.

Plus the D11 structural check, which belongs here because it is what makes the boundary enforceable:

**(d) `single-wrapper` (every PR, blocking).** Asserts every third-party media crate appears in exactly
one `Cargo.toml` under `crates/`. A second occurrence fails the build, per D11. Implemented in `xtask`
against a list of "media crates" derived from `docs/dependencies.md` — so adding an external codec crate
without an adoption record also fails.

**Interaction with D9's "never publish a full convenience binary".** The `default` feature set is the only
thing we publish, and (a)+(b) mean the published binary provably contains only `Exact` and `Equivalent`
codecs. `full-rf` is a build-it-yourself tier and may contain `Unmeasured` codecs; the release workflow
asserts we never produce an artifact from it. §7.5 covers the assertion.

### 1.13 Grading, three-way and the fuzzer: how the pieces compose

A single codec's lifecycle, end to end, showing every mechanism firing in order:

1. Crate lands wrapping an external crate (`backend-external`). Grade: `Unmeasured`. **Cannot ship in
   `default`** — check (b) fails if anyone tries.
2. Cases authored (§1.5), corpus assigned, fuzz targets generated (§2.1). `just grade` runs; the codec
   reaches `Equivalent` with two `float-lastbit` entries, or `Divergent` if something is unjustifiable.
3. `Divergent` schedules a native implementation (D11). `Equivalent` ships, with the entries visible in
   `docs/codec-status.md` and in the release notes.
4. A native backend is written. Both backends now exist, so C9 three-way turns on automatically for that
   crate's `core` suites and for its `diff_` fuzz campaign.
5. Three-way findings localise every divergence to `N`, `X`, or the reference, without bisection.
6. When `backend-native` grades `Exact` and `backend-external` does not, the `default` feature flips to
   native. Nothing outside the crate changes (D11), and the existing cases are the acceptance criteria
   for the swap — no new tests needed, which is the entire point of the D11 boundary.
7. The external dependency is dropped, `single-wrapper` stops tracking it, `THIRD_PARTY.md` shrinks, and
   `docs/dependencies.md` records the retirement.

---

## 2. Fuzzing

D6: *"Every demuxer, every bitstream parser, every decoder gets a fuzz target from the day it lands — a
component without a fuzz target is not 'done'."* This section makes that operational.

### 2.1 Target taxonomy

Mirroring the decomposition research 06 §4 identifies in FFmpeg's OSS-Fuzz entry points
(`target_dem_fuzzer`, `target_dec_fuzzer`, `target_enc_fuzzer`, `target_bsf_fuzzer`, `target_sws_fuzzer`,
`target_swr_fuzzer`) — the shape only, none of the code.

| Prefix | One target per | Count at full scope | Input |
|---|---|---|---|
| `dem_` | demuxer | ~90 | raw bytes = container file |
| `parse_` | bitstream parser (research 02 §1.8 lists 66 upstream) | ~66 | raw bytes = elementary stream, fed in `arbitrary`-chosen chunk sizes |
| `dec_` | decoder | ~200 (our shipped set) | structured: options + extradata + packet sequence |
| `enc_` | encoder | ~40 | structured: options + generated raw frames |
| `bsf_` | bitstream filter (research 02 §1.9 lists 50 upstream) | ~40 | structured: options + packet sequence |
| `mux_` | muxer | ~50 | structured: stream declarations + packet sequence |
| `proto_` | protocol parser / URL handling | ~20 | raw bytes = server response or URL string |
| `sws_` | scale ops-graph | 1 + per-op | structured: src/dst formats, sizes, flags, pixel data |
| `swr_` | resample | 1 + per-stage | structured: formats, layouts, rates, sample data |
| `tx_` | transform (FFT/MDCT/DCT) | 1 | structured: kind, size, flags, data |
| `opt_` | option parsing / stream specifiers | 4 | structured or raw string |
| `expr_` | the `eval` expression language | 2 | raw bytes = expression text; structured AST |
| `graph_` | filtergraph parser + negotiation | 2 | raw bytes = graph description string |
| `probe_` | format probing / scoring | 1 | raw bytes |
| `cli_` | full argv parsing of `vaco`/`vaco-probe` | 2 | structured argv |
| `diff_` | differential (§2.4) | per format family | mutated real media |
| `inv_` | internal-invariant differential (§2.4.4) | per kernel family | structured |

**Layout.** `fuzz/fuzz_targets/<prefix><name>.rs`, one crate `fuzz/` per workspace with a
`Cargo.toml` that feature-gates each target so `cargo fuzz build` of one target does not compile all 500.

**Generation.** Targets for the mechanical categories (`dem_`, `dec_`, `bsf_`, `mux_`) are **generated**
from the same component manifest that generates the registry (architecture §6, layer 6). Adding a
demuxer to the manifest emits its registry line *and* its fuzz target *and* its corpus directory. This is
what makes "a component without a fuzz target is not done" enforceable rather than aspirational: CI has a
job that regenerates and diffs, so the target cannot be missing.

### 2.2 The four bug classes in safe Rust

There is no memory corruption to find. The bug classes are different, and three of the four need
*design*, not a fuzzer flag.

#### 2.2.1 Panics

Index-out-of-range, `unwrap`/`expect`, explicit `panic!`/`assert!`, integer overflow in debug builds,
division by zero, slice-range inversion, `Vec` capacity overflow.

**Policy:** a panic reachable from untrusted input is a **bug of the same severity as a memory-safety bug
in C**. It is a denial of service in any process that embeds us, and `panic = "abort"` in our release
binaries (architecture §8) makes it fatal.

Enforcement beyond fuzzing:
- `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`, `clippy::indexing_slicing`,
  `clippy::integer_arithmetic` (or `arithmetic_side_effects`) are **`deny` at workspace level** for every
  crate in `crates/format/`, `crates/codec/`, `crates/io/`, and `crates/dsp/`. Escaping the lint requires a
  local `#[allow]` **with a comment giving the proof of impossibility**. A `xtask` check greps for
  `#[allow(clippy::indexing_slicing)]` without an adjacent `// SAFETY-ARG:` comment and fails.
- `overflow-checks = true` in the `release` profile of the **fuzz and test** builds specifically, so
  overflow is caught in the configuration where the fuzzer runs fast. The shipped release profile keeps
  overflow checks off for speed; the nightly `release-overflow` test job (§5) runs the whole suite with
  them on, which closes the gap.
- Prefer `get()`/`get_mut()`/`chunks_exact` over indexing throughout parsing code. The bitstream reader's
  checked-tail/unchecked-body split (architecture §7.4) is the sanctioned pattern; it makes the unchecked
  body's proof local and reviewable.

#### 2.2.2 Unbounded or attacker-controlled allocation

The dominant real-world bug class for a media parser, and the one people wave at with `-rss_limit_mb` and
declare handled. It is not handled by a fuzzer flag; the fuzzer flag only tells you it happened.

**Design: `vaco-limits` at layer 0, threaded through every constructor.**

```rust
// crates/core/vaco-limits/src/lib.rs
#![forbid(unsafe_code)]

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_alloc_total:   u64,   // cumulative, per component instance
    pub max_alloc_single:  u64,
    pub max_dimension:     u32,   // per-axis, video
    pub max_frame_bytes:   u64,
    pub max_channels:      u16,
    pub max_sample_rate:   u32,
    pub max_streams:       u32,
    pub max_side_data:     u32,
    pub max_probe_bytes:   u64,
    pub max_metadata_bytes:u64,
    pub deadline:          Option<Instant>,
    pub fuel:              Fuel,  // §2.2.4
}

impl Limits {
    /// Generous: matches real-world media. The CLI default.
    pub fn permissive() -> Self { … }
    /// Conservative: for untrusted input, fuzzing, and library embedders. 64 MiB total.
    pub fn strict() -> Self { … }

    /// The ONLY sanctioned way to allocate a buffer whose length derives from input.
    pub fn alloc_vec<T: Copy + Default>(&self, budget: &AllocBudget, n: usize)
        -> Result<Vec<T>, LimitError>
    {
        let bytes = (n as u64).checked_mul(size_of::<T>() as u64).ok_or(LimitError::Overflow)?;
        budget.charge(bytes, self)?;                 // checks single + cumulative
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| LimitError::AllocFailed)?;
        v.resize(n, T::default());
        Ok(v)
    }

    /// Growable variant for "declared size, unknown truth" — never allocates the declared
    /// size up front. Grows geometrically as bytes actually arrive.
    pub fn alloc_incremental<T>(&self, budget: &AllocBudget, declared: usize) -> IncrementalVec<T> { … }
}
```

Rules, all mechanically enforced:

1. **Every component constructor takes `&Limits` as a required argument.** Not an `Option`, not a builder
   field with a default — a positional parameter. There is no code path that skips it.
2. **`clippy.toml` `disallowed-methods`** bans `Vec::with_capacity`, `Vec::reserve`, `Vec::resize`,
   `vec![_; n]`, `String::with_capacity`, `HashMap::with_capacity` and `BytesMut::with_capacity` inside
   `crates/format/`, `crates/codec/`, `crates/io/`. The `#[allow]` escape requires a `// SAFETY-ARG:`
   comment proving the length is a compile-time constant or an already-charged value.
3. **Two-phase reservation.** Never allocate a declared size before the bytes exist. A box header claiming
   a 4 GiB payload gets `alloc_incremental`, which allocates in chunks bounded by what has actually been
   read. This is the specific defence against the classic "declared length" amplification, and it means a
   16-byte file can never cause a gigabyte allocation.
4. **Derived-dimension checks are up front.** `width × height × bytes_per_pixel × planes` is validated
   against `max_frame_bytes` before any frame buffer is touched, in checked arithmetic.
5. **Pools are bounded.** `vaco-pool` (architecture, layer 1) has a hard ceiling on total pooled bytes; a
   pool that would exceed it returns an error rather than growing.
6. The CLI exposes `-limits permissive|strict|custom:…` and defaults to `permissive`; embedders get
   `strict` by default because a library used on untrusted input should be conservative unless told
   otherwise.

**Testing the limits themselves.** `limit_*` fuzz targets run every component under
`Limits::strict()` with a deliberately tiny budget and assert that *every* failure is a clean
`Error::LimitExceeded` — never a panic, never an abort, never success with a 900 MB buffer.

#### 2.2.3 Belt-and-braces: the counting allocator (fuzz builds only)

`vaco-fuzz-alloc` wraps the system allocator, counts live bytes, and aborts with a distinctive message
above a ceiling. It is the safety net that catches allocations we forgot to route through `Limits`.

```rust
// crates/tools/vaco-fuzz-alloc/src/lib.rs
//! TEST-ONLY. Never in a shipped artifact. See planning/13-correctness.md §2.2.3.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static CEILING: AtomicUsize = AtomicUsize::new(256 << 20);

pub struct Counting;

// SAFETY: delegates every operation unchanged to `System`; the only added behaviour is
// an atomic counter and a process abort. No pointer arithmetic, no aliasing claims.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        if live > CEILING.load(Ordering::Relaxed) {
            eprintln!("VACO-FUZZ-ALLOC-CEILING live={live} req={}", l.size());
            std::process::abort();
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    // realloc / alloc_zeroed likewise
}
```

**D2 allowlist entry** (`tools/unsafe-allowlist.toml`):

```toml
[[crate]]
name              = "vaco-fuzz-alloc"
reason            = "GlobalAlloc cannot be implemented in safe Rust; needed as the fuzzing allocation backstop"
justification_doc = "planning/13-correctness.md#223"
approved_by       = ["@security-owner", "@correctness-owner"]
in_default_build  = false
test_only         = true      # CI asserts it appears in NO binary's dependency graph
```

The `unsafe-audit` CI job (§5) asserts `cargo tree -e normal -p vaco -p vaco-probe -p vaco-play` never
mentions it, for any feature combination.

Additionally, libFuzzer is run with `-rss_limit_mb=2048 -malloc_limit_mb=512`, which catches the same
class one layer lower. Three layers (design, counting allocator, fuzzer flag) because this is the class
most likely to reach production.

#### 2.2.4 Non-termination and hangs

Three independent mechanisms, because a wall-clock timeout alone tells you nothing about *where*.

**(a) Structural progress guarantee.** Every stepping API carries a contract:

- `Demuxer::read_packet()` — each call either advances the input position by ≥1 byte, or returns
  `Ok(None)`/`Err`. It may not return `Ok(Some(packet))` without consuming input.
- `Decoder::receive_frame()` — each call either produces a frame, or consumes a queued packet, or returns
  `Ok(None)`.
- `Filter::activate()` — must return `Progressed` only if it consumed an input frame or produced an output
  frame.

`vaco-fuzz-support::ProgressGuard` wraps any component and, in fuzz and test builds, asserts the contract
on every call, counting consecutive no-progress ticks and panicking at 64. This converts an infinite loop
in the *scheduler* into an immediate, localised, reproducible failure instead of a 10-second timeout with
no stack.

**(b) Fuel for input-derived loops.** Any loop whose trip count is a function of input data charges
`limits.fuel.consume(n)?`. Fuel is a deterministic counter, not a clock, which means a fuel exhaustion is
*reproducible* — the same input always exhausts at the same point, so it minimises and regresses cleanly.
Wall-clock deadlines are not reproducible and are therefore the fallback, never the primary.

**(c) Wall-clock deadline.** `Limits::deadline` is checked at every packet and frame boundary. Exceeding
it is a clean `Error::DeadlineExceeded`.

**The important distinction:** a libFuzzer `-timeout` firing is a **bug** (we failed to bound something).
A `LimitExceeded`/`DeadlineExceeded`/`FuelExhausted` error is **correct behaviour** and the target returns
normally. The fuzz targets encode this: they treat our error types as success and only crash/hang/panic as
failure.

**(d) Slow-unit detection.** A nightly job runs the whole corpus through each target with timing and
computes ns/byte. Any input more than 100× the target's median is filed as a `slow-unit` finding even
though it completed — this is how you catch quadratic behaviour before it becomes a hang on a slightly
larger file. libFuzzer's `-report_slow_units` feeds the same queue.

#### 2.2.5 Incorrect-but-not-crashing output

The class fuzzers usually miss entirely, and the reason §2.4 exists. Three attacks:

1. **Differential against the reference** (§2.4) — the highest-value.
2. **Internal differentials** (§2.4.4) — SIMD vs scalar, threaded vs single-threaded, seek-then-decode vs
   decode-from-start, ops-graph vs naive scale. These need no subprocess and run at full fuzzer speed.
3. **Round-trip invariants** — mux→demux, encode→decode, write→parse, forward→inverse transform. These
   are cheap, run at full speed, and catch the whole class of "we wrote it wrong but consistently".

### 2.3 Structured fuzzing with `arbitrary`

Raw bytes are the right input for anything that genuinely consumes a byte stream: demuxers, protocol
responses, elementary-stream parsers, probing, expression text, filtergraph text. For everything else,
raw bytes waste almost the entire budget on inputs rejected in the first microsecond.

Where `arbitrary` earns its place:

```rust
// crates/tools/vaco-fuzz-support/src/inputs.rs
use arbitrary::{Arbitrary, Unstructured};

/// Decoder sessions: the option surface and packet framing are structure, the payload is bytes.
#[derive(Arbitrary, Debug)]
pub struct DecodeSession<'a> {
    pub opts:      DecoderOpts,          // constrained enum/range fields, not free integers
    pub extradata: &'a [u8],
    pub packets:   Vec<FuzzPacket<'a>>,
    pub flush_after: Option<u8>,         // exercise the drain path
    pub seek_after:  Option<u8>,
}

#[derive(Arbitrary, Debug)]
pub struct FuzzPacket<'a> {
    pub data:     &'a [u8],
    pub keyframe: bool,
    pub pts_delta: i16,                  // small deltas find timestamp bugs; huge ones find overflow
    pub side_data: Vec<(SideDataKind, &'a [u8])>,
}

/// swscale: the interesting space is the (src_fmt, dst_fmt, size, flags) product, not the pixels.
#[derive(Arbitrary, Debug)]
pub struct ScaleJob {
    pub src_fmt: PixFmtId,               // Arbitrary impl draws from the real enum, uniformly
    pub dst_fmt: PixFmtId,
    pub src_w: Dim, pub src_h: Dim,      // Dim = newtype with Arbitrary in 1..=8192, biased to edges
    pub dst_w: Dim, pub dst_h: Dim,
    pub flags: ScaleFlags,
    pub src_range: Range, pub dst_range: Range,
    pub colorspace: ColorSpaceId,
}

/// Muxers: the packet stream is structure; only payload is bytes.
#[derive(Arbitrary, Debug)]
pub struct MuxJob<'a> {
    pub streams: Vec<FuzzStreamDecl>,
    pub packets: Vec<(u8 /*stream*/, FuzzPacket<'a>)>,
    pub opts:    MuxerOpts,
}

/// Option parsing / stream specifiers: generate from the grammar, not from bytes.
#[derive(Arbitrary, Debug)]
pub enum SpecifierAst { Index(u16), Type(MediaTypeCh), Program(u16, Option<u16>),
                        Meta(TinyString, Option<TinyString>), Usable, Disposition(DispFlag), … }
```

Design notes that matter more than the derive:

- **Bias to edges.** `Dim`'s `Arbitrary` impl draws `0, 1, 2, 3, 15, 16, 17, 4095, 4096, 4097, 8192,
  u32::MAX` with elevated probability and a uniform value otherwise. Off-by-one bugs cluster at powers of
  two and at format-specific block boundaries (16 for macroblocks, 64 for CTUs, 128 for AV1 superblocks).
  A uniform `u32` finds none of them.
- **Constrain the option surface, then fuzz the constraint.** `DecoderOpts` draws from valid values so the
  decoder actually runs. A *separate* `opt_` target fuzzes the string→option parser with raw bytes, which
  is where the parser bugs live. Mixing the two wastes both.
- **`arbitrary` derives must be deterministic and stable.** Changing a struct invalidates the corpus, so
  structured-input types are versioned: the corpus directory is `corpus/dec_h264/v3/`, and bumping the
  version is a deliberate act that triggers a re-seed rather than silently degrading a campaign.
- **Keep a raw-bytes target alongside every structured one** for the parsing components. Structured
  fuzzing explores the state space; raw bytes explore the parser. `dec_h264` (structured) and
  `parse_h264` (raw) are both required.

### 2.4 The differential fuzzer

The highest-value fuzzer we can build, and the one that needs the most careful engineering, because the
naive design (spawn the reference per input inside `fuzz_target!`) collapses to ~200 exec/s and wastes the
coverage engine.

#### 2.4.1 The core trick: split the loop

Coverage-guided generation on **our** side (fast, in-process, millions of execs) is decoupled from
differential verification against the reference (slow, subprocess, thousands of execs). They communicate
through a queue.

```
              ┌──────────────────────────────────────────────────┐
  seeds ───▶  │ Phase A — coverage-guided generation (libFuzzer)  │
  (real media)│  in-process, OUR demuxer/decoder only, ~1e5/s     │
              │  every NEW-COVERAGE input is written to the       │
              │  promotion queue                                  │
              └───────────────────────┬──────────────────────────┘
                                      │  promotion queue (content-addressed)
              ┌───────────────────────▼──────────────────────────┐
  real media ▶│ Phase B — structure-aware mutation (vaco-corpus)  │
              │  box/EBML/TS-aware field mutation, ~1e4/s          │
              └───────────────────────┬──────────────────────────┘
                                      │
              ┌───────────────────────▼──────────────────────────┐
              │ Phase C — differential campaign runner            │
              │  N worker processes; each: run reference, run     │
              │  ours, classify. ~5e2/s/core. Batched, resumable. │
              └───────────────────────┬──────────────────────────┘
                                      ▼
                              triage buckets (§2.4.3)
```

Phase A is where coverage is discovered; Phase C is where truth is checked. Running C at A's rate is
neither possible nor necessary — coverage saturates long before the differential budget does.

#### 2.4.2 Why not link the reference library

We could get 1e5/s by calling `libavformat` in-process. We do not, for three reasons, and it is worth
saying explicitly so nobody proposes it later:

1. **Licence.** Linking LGPL/GPL libraries into our test binaries creates a distribution question for
   every CI artifact and an unnecessary argument. Subprocess invocation of a shipped binary creates none.
2. **Clean room.** Linking requires headers, which are source. §1.7.4 puts that on the wrong side of the
   line for anyone who touches the harness.
3. **Fidelity.** The user's requirement is about the *binaries* — "identical when given the same flags".
   The CLI layer (option parsing, stream selection, the writer framework, timestamp handling in the
   ffmpeg tool itself) is a large part of what we must match, and the library API bypasses all of it.

#### 2.4.3 The agreement rule and triage

For each mutant, run both. Classify:

| Ours | Reference | Class | Handling |
|---|---|---|---|
| accept | accept | **must agree** | C6 structured diff (probe) or C3a frame hash (decode) modulo the allowlist. Disagreement = **hard finding**, highest priority. |
| reject | reject | agree | Pass. Error *text* is not compared; error *category* is compared loosely under C7. |
| reject | accept | `stricter` | **Soft finding.** Often correct (we validate something they do not), often a bug (we reject a legal file). Queued, deduplicated, triaged weekly. Tracked as a count with a ratchet — the number should trend down. |
| accept | reject | `laxer` | **Soft finding.** We accept something they reject. Usually harmless, occasionally a missing validation that a `dec_` fuzzer will later turn into a panic. Same queue. |
| panic / hang / limit-abort | anything | **hard crash** | Immediate. Minimised and filed as §2.5.4 regression. |
| anything | crash / hang | `ref-crash` | Recorded, not acted on. We do not file upstream bugs from fuzzing without a human reviewing the finding first. |

Deduplication is by BLAKE3 of the **minimised** input plus the divergence signature (which field, which
frame index, which byte offset class) — not by raw input, which would file thousands of tickets for one
bug.

#### 2.4.4 Internal differentials (no subprocess, full speed)

These belong in the fast libFuzzer loop and are, per unit of CPU, the best value in the whole programme:

| Target | Asserts |
|---|---|
| `inv_kernel_<name>` | SIMD kernel ≡ scalar reference, for every lane width and every ISA tier the host supports. This is `vaco-checkasm` run as a fuzzer rather than over fixed vectors. Architecture §7.3 already makes the scalar reference mandatory; this is what consumes it. |
| `inv_threads_<codec>` | decode with `-threads 1` ≡ decode with `-threads N`, for N ∈ {2,4,8}. Catches the frame-threading race class, which is otherwise nearly untestable. |
| `inv_seek_<fmt>` | seek to T then decode ≡ decode from start and discard to T (modulo declared keyframe semantics). The single most bug-dense area of any demuxer. |
| `inv_scale_graph` | ops-graph scale ≡ naive per-pixel reference scale. |
| `inv_resample_identity` | 1:1 same-format resample is a bit-exact copy; s16→f32→s16 round-trips exactly; identity rematrix is identity. |
| `inv_tx_roundtrip` | forward ∘ inverse = identity × N within a declared ULP bound; FFT ≡ naive DFT; MDCT/IMDCT satisfies TDAC. |
| `inv_mux_demux_<fmt>` | mux(packets) → demux → packets, modulo a per-format `RoundTripFidelity` descriptor (§3.2). |
| `inv_bsf_idempotent` | for BSFs that are idempotent by definition (`extract_extradata`, `*_metadata` with no changes), applying twice ≡ applying once. |
| `inv_probe_stability` | probing the same buffer twice yields the same score; probing a prefix never scores higher than probing the whole. |

#### 2.4.5 Structure-aware mutation

Generic bitflips die at the first length field. `vaco-corpus mutate` implements format-aware operators,
each of which preserves enough structure to reach deep code:

- **Container-level:** truncate at a box/element/packet boundary; duplicate a box; reorder sibling boxes;
  swap two boxes' contents; set a length field to `0`, `1`, `len-1`, `len+1`, `u32::MAX`, or a value
  pointing back into an earlier box; splice a box from a different file of the same format; change a
  fourcc/EBML ID to another *valid* one; nest a box inside itself.
- **Elementary-stream-level:** truncate at a NAL/OBU/frame boundary; drop a NAL; duplicate an SPS/PPS with
  a changed parameter; flip a single syntax element in a header while leaving the payload intact;
  re-emulation-prevention-byte a stream.
- **Timestamp-level:** set PTS/DTS to extremes, invert their order, introduce discontinuities and
  wraparound at the format's modulus (33-bit for MPEG-TS PCR — a classic bug farm).
- **Generic:** bitflip, byteflip, chunk splice, arithmetic on suspected integers, interesting-value
  insertion. Kept as the tail of the operator distribution, not the head.

Operator weights are per-format and tuned by measured coverage gain, recorded in
`corpus/mutators.toml`.

#### 2.4.6 Running it

```
just fuzz-diff <family> [--duration 1h] [--workers N]
```

Cadence: `diff_` campaigns run **nightly** for 30 minutes per format family, and **weekly** for 6 hours on
a larger runner. They are not PR-gating (too slow, too noisy) — but every hard finding they produce
becomes a conformance case *and* a fuzz regression, both of which are PR-gating from then on. That is the
ratchet: the slow engine feeds the fast gate.

### 2.5 Corpus

#### 2.5.1 Sources

| Source | Licence handling | Vendored? |
|---|---|---|
| **Our own generated media** — synthesised from CC0/CC-BY sources (Blender open movies, `media.xiph.org` derf clips) and from `lavfi`-equivalent synthetic sources, transcoded into every container/codec combination we support | We record source + licence per file; outputs are mechanical transformations of CC-licensed inputs | Small ones (<64 KiB) yes; rest in object store |
| **Public conformance suites** (§4) | Fetched at test time from the canonical URL; **never vendored** (research 07 §1.5d) | No |
| **Crash-derived minimised regressions** | Ours; derived from our own fuzzing | Yes, if <32 KiB |
| **Findings from the differential campaigns** | Ours | Promoted to the object store |
| **OSS-Fuzz corpus** (if enrolled) | Google's infrastructure; corpus is derived from our seeds | Synced, not vendored |

We deliberately do **not** seed from FFmpeg's `fate-suite` rsync corpus, despite D6 mentioning FATE
samples as a seed source: that corpus is a curated compilation hosted by the project, and re-hosting it
is an unnecessary provenance argument when generating our own equivalents is cheap. Where a fate-suite
file is itself an independently-published sample (a vendor's demo clip, a public conformance stream), we
fetch it from its own origin and record that origin.

#### 2.5.2 Storage and addressing

Content-addressed, BLAKE3, in an S3-compatible bucket (R2). Manifests are lock files in the repo:

```toml
# corpus/vaco-media.lock
schema = 1
[[entry]]
path    = "vaco/mp4/h264-aac-30f.mp4"
blake3  = "b3:9f2c…"
size    = 184320
licence = "CC-BY-3.0"
source  = "derived from Big Buck Bunny (Blender Foundation) via tools/corpus/gen-mp4.sh"
targets = ["dem_mp4", "dec_h264", "dec_aac", "diff_isobmff"]
```

- `testdata/` in git holds **only** fixtures under 64 KiB that a unit test needs directly. Hard cap
  enforced by CI (total `testdata/` size ≤ 20 MiB).
- No git-lfs: it makes clones slow and forks painful, and we do not need version history on immutable
  content-addressed blobs.
- `just corpus-fetch` downloads, verifies every hash, and populates `~/.cache/vaco/corpus/`. Verification
  failure is fatal — a corpus is a security boundary.

#### 2.5.3 CI caching, minimisation, growth

- **Cache key** = hash of the lock file. Content is immutable, so hit rate is ~100% and a corpus update is
  a visible lock-file diff.
- **Minimisation:** weekly `cargo fuzz cmin` per target, plus our semantic minimiser for structured
  formats (which can do things `cmin` cannot, like dropping an entire box while fixing up parent lengths).
- **Caps per target:** 2,000 files / 50 MiB after minimisation. The weekly job enforces it and reports
  what it dropped.
- **Coverage guard:** the weekly job measures corpus coverage per target before and after minimisation and
  fails if coverage drops by more than 0.2 pp. A minimiser that loses coverage is a bug.
- **Stats publication:** corpus size, coverage %, exec/s and new-coverage-rate per target are published to
  a dashboard. A target whose new-coverage rate has been zero for 30 days is either saturated (retire it
  to weekly) or wedged (its input format is wrong) — either way it needs a human.

#### 2.5.4 Crash → regression test

```
just fuzz-triage <target> <crash-file>
```

1. `cargo fuzz tmin` + our semantic minimiser.
2. Compute `id = blake3(minimised)[..16]`.
3. Write `fuzz/regressions/<target>/<id>.bin` (if <32 KiB; larger goes to the object store with a stub) and
   `fuzz/regressions/<target>/<id>.toml`:

```toml
id       = "9f2c1a4b6d8e0f13"
target   = "dem_matroska"
kind     = "panic"
message  = "attempt to subtract with overflow at crates/format/vaco-demux-matroska/src/cluster.rs:214"
found    = 2026-11-03
found_by = "fuzz-nightly"
issue    = "vaco#901"
fixed_in = "commit 4d9a…"
```

4. The fixing PR must include both the fix and the regression files, and the PR body links them.
5. `fuzz/regressions` is replayed as an **ordinary `cargo test`** (`fuzz_regressions` in
   `vaco-fuzz-support`, which runs every stored input through its target's body function) — so PR CI
   catches reintroduction in seconds without a fuzzer. This is the mechanism that makes fuzzing findings
   permanently gating.
6. Every regression input is also added to that target's corpus seed set.

### 2.6 Continuous fuzzing infrastructure

**Phase 1 — self-hosted (from day one).**
- `fuzz-smoke` on PRs: 60 s per target, but only for targets whose crate the PR touches (mapping from the
  same manifest that generates the targets). Plus the full `fuzz_regressions` replay, which is always run.
- `fuzz-nightly`: a sharded matrix, 30 min per target, corpus synced to R2 before and after. Findings open
  issues automatically with the minimised input attached.
- `fuzz-weekly`: 6 h per target on a large runner, plus `cmin`, plus the `diff_` campaigns, plus
  slow-unit and coverage reporting.

**Phase 2 — ClusterFuzzLite (as soon as the repo is public).** It is the right intermediate step: it gives
PR-scoped coverage-guided fuzzing with corpus persistence and crash deduplication, on our own CI, without
OSS-Fuzz's onboarding requirements. Budget 10 min per PR across changed targets.

**Phase 3 — OSS-Fuzz enrolment. Yes, pursue it, but only when four preconditions hold:**
1. The project is public, with a maintained `SECURITY.md` and a security contact who answers.
2. ≥ 20 stable targets that have run for a month without infrastructure flakes.
3. The build works in OSS-Fuzz's base image with our **pinned nightly** (architecture D8). This is the real
   friction: OSS-Fuzz's Rust support tracks its own toolchain. Mitigation — the fuzz targets must build on
   the OSS-Fuzz toolchain, which means `portable_simd` usage has to be behind a feature that the fuzz build
   can disable, falling back to scalar kernels. **Design consequence, worth stating now:** every SIMD
   kernel already requires a scalar reference (architecture §7.3), so a `--no-default-features` scalar-only
   build must remain a first-class, tested configuration. It is in the CI feature matrix (§5) partly for
   this reason.
4. We are prepared for the 90-day disclosure clock on every finding.

The value of OSS-Fuzz is real (free large-scale compute, a corpus that compounds, and the reputational
signal), but it is an accelerator, not a foundation. The foundation is ours.

---

## 3. Unit and property testing standards

### 3.1 The bar every crate must clear

A crate is not "done" — and CI's `crate-standards` job fails — unless all of these hold:

1. `#![forbid(unsafe_code)]` at the crate root, or an entry in `tools/unsafe-allowlist.toml`.
2. A `docs/<name>.md` page linked from `docs/README.md`, covering the five headings the repository
   standard requires (what / how it works / how to change it / configuration / dependencies).
3. Unit tests colocated with the code (`#[cfg(test)] mod tests`), covering: the empty input, the
   single-element input, the maximum-size input, every error variant the module can produce, and every
   `match` arm on a public enum.
4. **A property test for every algebraic or invertible operation the crate exposes** (§3.2). "There is
   nothing invertible here" is an acceptable answer, recorded as a one-line comment in the crate docs.
5. **A fuzz target if the crate parses untrusted input** (D6). Generated automatically for the
   mechanical categories (§2.1); hand-written otherwise.
6. Doc examples on every public item whose usage is not obvious, run by `cargo test --doc`.
7. A conformance suite entry if the crate is reachable from the CLI surface.
8. A `fidelity.lock` row if the crate implements a codec (§1.12).
9. A benchmark if the crate contains a DSP kernel or sits on the hot path (D8).

Items 2, 5, 7 and 8 are the ones that are easy to skip and expensive to retrofit, so each has its own
CI check rather than relying on review.

### 3.2 Where proptest earns its place

Property testing is not free — a bad property is a flaky test — so it is targeted at operations with a
real algebraic law. The concrete list, by crate:

| Crate | Properties |
|---|---|
| `vaco-core` (`Rational`) | Reduction is canonical (`gcd(n,d)=1`, `d>0`); `a+b`, `a*b`, comparison agree with exact `i128` arithmetic; **`rescale` never panics for any `(i64, Rational, Rational)`** (the single most valuable property in the crate — timestamp rescaling overflow is a classic); each rounding mode satisfies its stated contract (`Down` ≤ `Nearest` ≤ `Up`, `Inf`/`Zero`/`NearInf` directionality); `rescale(rescale(t, a, b), b, a)` differs from `t` by at most the declared rounding error. |
| `vaco-bitstream` | Writer→reader round-trip for arbitrary `(value, width)` sequences; Exp-Golomb (`ue`, `se`) round-trip including the boundary values; **the checked-tail/unchecked-body reader is byte-for-byte equivalent to a naively-fully-checked reference reader on every input** — this is the property that lets us trust architecture §7.4's optimisation; the reader never reports success past the buffer end; `bits_left` is always consistent with the position. |
| `vaco-opts` | `parse(display(x)) == x` for every option type; `key=value:key=value` round-trip through every escaping level; range enforcement rejects exactly the out-of-range values; named constants and flag sets round-trip; unknown keys always error (never silently ignored). |
| `vaco-expr` | Evaluation is deterministic and side-effect-free apart from `st`/`ld`; arbitrary well-formed ASTs never panic; results match a deliberately-slow tree-walking reference interpreter; parse→print→parse is idempotent. |
| `vaco-pixfmt` / `-sampfmt` / `-chlayout` | Name↔value round-trip **exhaustively** (these enums are finite; proptest is the wrong tool — iterate all variants); descriptor self-consistency (plane count matches component descriptors; `bits_per_component ≤ storage_bits`; computed buffer size ≥ Σ plane sizes; subsampling factors divide the declared alignment); channel-layout mask↔name round-trip; custom-order layouts preserve order. |
| `vaco-color` | RGB→YUV→RGB round-trips within a stated ε for every (primaries, matrix, range) triple; transfer function ∘ inverse ≈ identity across the full domain including the linear segment near zero; limited↔full range mapping is monotone and maps endpoints exactly; primaries conversion matrices are invertible and their inverse is the reverse conversion. |
| `vaco-tx` | FFT ≈ naive DFT within a size-dependent ULP bound; forward ∘ inverse = identity × N; **Parseval's identity** (a strong whole-transform check that catches scaling errors a spot check misses); MDCT/IMDCT satisfies TDAC under the declared window; the i32 fixed-point paths are **bit-exact against a reference integer implementation** (D10 names this as the reason `vaco-tx` is ours rather than `rustfft`'s, so it must be tested as such). |
| `vaco-scale` | Same-format, same-size scale is a bit-exact copy; a solid-colour input yields a solid-colour output of the same colour for every filter; the ops graph equals the naive scalar reference; scaling by 1:1 with any filter is identity; output is deterministic across slice-threading configurations. |
| `vaco-resample` | 1:1 same-format is identity; s16→f32→s16 round-trips exactly; identity rematrix is identity; a DC input yields DC output after the edge transient; rate ratio `r` then `1/r` recovers the signal within a stated SNR; sample counts obey the declared input/output relation exactly (off-by-one in sample accounting is the classic resampler bug). |
| `vaco-demux-*` / `vaco-mux-*` | **Mux→demux round-trip**: an arbitrary packet stream (random sizes, timestamps, flags, side data, stream counts) muxed then demuxed yields the same packets, **modulo a per-format `RoundTripFidelity` descriptor** that declares what the container cannot carry (e.g. WAV has no per-packet timestamps; AVI has no per-packet side data). The descriptor is itself tested: a format claiming to preserve something that it drops fails. Also: demux is deterministic; demuxing a prefix never yields a packet the full file does not. |
| `vaco-textformat` | For every writer with a parseable output (`json`, `xml`, `ini`, `flat`), `parse(write(tree)) == tree`; for `default`/`compact`/`csv` we write test-only parsers and assert the same; escaping round-trips for every string in the Unicode sample set plus the `string_validation` modes. |
| `vaco-cli-core` | Arbitrary argv drawn from the option grammar never panics; stream-specifier matching against a random stream set agrees with a slow reference matcher; `-opt:spec` resolution order is deterministic; help output is generated for every registered option. |
| `vaco-filter-graph` | Arbitrary graph descriptions never panic; a graph that parses can be printed and re-parsed to the same graph; format negotiation either succeeds or reports an incompatibility (never loops); auto-inserted conversion filters preserve the declared frame properties. |
| `vaco-limits` | Every `alloc_vec` call either returns a buffer of exactly the requested length or a `LimitError`; charges are additive and never overflow; a budget can never be exceeded by a sequence of successful calls. |

`proptest` (not `quickcheck`) for the shrinking quality; failures are persisted to
`proptest-regressions/` and committed, which makes them ordinary regression tests.

### 3.3 Coverage

- **Tool:** `cargo-llvm-cov`, line and region coverage, per crate.
- **Ratchet, not a fixed bar.** `coverage.lock` stores each crate's floor. CI fails if a crate drops more
  than 0.5 pp below its floor; a PR that raises coverage updates the floor automatically. Fixed global
  thresholds either get set so low they are meaningless or so high they get bypassed; a ratchet is the
  only version that survives contact with a real project.
- **Entry floors for new crates:** 85% line for pure-logic crates (layers 0–1: core, opts, expr,
  bitstream, pixfmt, color, limits), 70% line for parsing crates (formats, codecs), 60% for CLI glue.
  Steady-state targets: 90% for layers 0–1, 85% workspace-wide.
- **Fuzz corpora count toward coverage.** The nightly coverage job replays each target's corpus under
  instrumentation and merges the profile. This is how parser crates realistically reach their floor, and
  it creates a healthy incentive: growing the corpus improves the coverage number.
- **Region coverage is reported but not gated** for the first year — it is noisy on `match`-heavy parsing
  code — with the intent to gate it once the numbers stabilise.
- **Mutation testing** (`cargo-mutants`) runs weekly on layers 0–1. Report-only, not gating, for the
  first year. It is the only tool that catches the "test calls the function and asserts nothing useful"
  failure mode, which coverage cannot see. Surviving mutants are triaged into a backlog.

---

## 4. Codec conformance suites

The independent proof. Differential testing proves we match FFmpeg; conformance suites prove we match the
*specification*, which is the stronger claim and the one that survives a reference-binary bump.

**Universal policy (research 07 §1.5d):** conformance bitstreams are copyrighted works with specific
distribution terms. **We never vendor them.** They are fetched at test time from their canonical origin,
verified by hash, cached, and gated behind a `conformance-suites` feature and the `full` tier. Every
suite has an entry in `corpus/suites.toml` recording its licence and terms.

```toml
# corpus/suites.toml
[[suite]]
id       = "itu-h264"
name     = "ITU-T H.264.1 / JVT draft conformance bitstreams"
origin   = "https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance/"
licence  = "ITU-T terms of use; freely downloadable; redistribution NOT granted"
vendored = false
index    = "corpus/suites/itu-h264.index.toml"   # per-file name + sha256 + expected md5 (from the suite's own .yuv)
gate     = "conformance-suites"
tier     = "full"
notes    = "Each bitstream ships with its own reference decoded YUV; we compare against THAT, not against ffmpeg."
```

That last line is the point of the whole section: for these suites the expected output ships **with the
suite**, from the standards body. No reference binary is involved, and no provenance question arises.

### 4.1 The suites, by codec

| Codec | Suite | Origin | Licence / terms | Expected output | Integration |
|---|---|---|---|---|---|
| **H.264 / AVC** | ITU-T H.264.1 conformance bitstreams (JVT) — ~200 streams covering Baseline/Main/High/High10/High422/High444, MBAFF/PAFF, CABAC/CAVLC, SP/SI, FMO/ASO, long-term refs, MVC and SVC subsets | ITU wftp3 JVT site (`draft_conformance/`); mirrored by ISO/IEC 14496-4 | Free download, ITU terms; **redistribution not granted** | Ships with per-stream reference YUV | C4 raw-exact against the suite's own YUV. The gold standard for H.264 decoder correctness. |
| **H.265 / HEVC** | ITU-T H.265.1 conformance (JCT-VC bitstream exchange) — Main/Main10/RExt/SCC/MV-HEVC/SHVC sets | ITU wftp3 JCT-VC site | Free download, ITU terms | Reference YUV + MD5 per stream | C4. **Decode only** — D9 makes HEVC RED for encode, and decode ships only where hardware-delegated or explicitly opted in. |
| **H.266 / VVC** | ITU-T H.266.1 conformance (JVET bitstream exchange) | ITU wftp3 JVET site | Free download, ITU terms | Reference YUV + MD5 | C4. Opt-in only per D4/D9. |
| **AV1** | (a) **Argon coverage streams** (Allegro DVT, released by AOMedia) — the exhaustive syntax-coverage set, tens of thousands of streams; (b) **libaom test vectors** (`av1-test-vectors`) | `storage.googleapis.com/aom-test-data`; Argon via AOMedia's published location | Argon: AOM's published terms — **read and record them at adoption; do not assume BSD**. libaom vectors: BSD-2 with the libaom licence | Both ship reference MD5s | C4 against the suite MD5s. Argon is large; the `full` tier runs a sampled subset, the weekly `exhaustive` tier runs all of it. |
| **VP9** | libvpx test vectors (`vp90-2-*`) | `storage.googleapis.com/downloads.webmproject.org/test_data/libvpx/` | BSD-3 (libvpx) | `.md5` alongside each `.webm` | C4. |
| **VP8** | libvpx test vectors (`vp80-00-comprehensive-001..017` and the rest) | same | BSD-3 | `.md5` per stream | C4. RFC 6386 also publishes the reference decoder, but reading it is a licence question, not a clean-room one — it is BSD, so reading it is *permitted*; we still prefer the spec text. |
| **Opus** | RFC 6716 / **RFC 8251** test vectors (`opus_testvectors`) | `opus-codec.org/testvectors/` and the RFC | BSD-style (Xiph/IETF) | Reference decoded output per vector | **C5** with `metric = "opus-compare"`: RFC 6716 §6 defines conformance by the `opus_compare` score, not equality, for the float decoder. We implement the comparison metric **from the RFC text**. Where we implement the fixed-point path, it is C4 bit-exact. |
| **Vorbis** | Xiph test vectors + the `libvorbis` test suite streams | `xiph.org` / `svn.xiph.org` archives | BSD-3 (Xiph) | Reference PCM | C5 with an RMS bound. |
| **FLAC** | **IETF CELLAR `flac-test-files`** (the RFC 9639 test corpus) — subset, uncommon block sizes, 24-bit, high channel counts, malformed files | `github.com/ietf-wg-cellar/flac-test-files` | **CC0** — the cleanest licensing of any suite here | Decoded WAV alongside, plus the streams' own MD5 in the STREAMINFO | **C4 bit-exact**, and additionally self-verifying: every FLAC file carries the MD5 of its own decoded output in STREAMINFO, so correctness is checkable with no external reference at all. Use both. |
| **ALAC** | Apple's ALAC reference implementation test files | `github.com/macosforge/alac` | Apache-2.0 | Reference output | C4. |
| **MP3** | ISO/IEC 11172-4 and 13818-4 compliance bitstreams | ISO (**paid**); some layer-3 streams circulate freely | ISO terms, purchase required for the official set | RMS-bound reference PCM | C5 with the ISO-defined RMS bound. **Gap:** the official set costs money. Budget for it or rely on differential + the freely available subset, and say which in `corpus/suites.toml`. |
| **AAC** | ISO/IEC 14496-26 conformance | ISO (**paid**) | ISO terms | Reference PCM with tolerance | C5. **D9 makes AAC RED for encode and decode** — only remuxing ships by default. The suite matters only for the opt-in decoder; deprioritise accordingly. |
| **AC-3 / E-AC-3** | ATSC A/52 has no free conformance suite; Dolby's is licensed | — | Licensed | — | **Gap, acknowledged.** Fall back to differential (C5) plus our own synthesised streams. D9 additionally blocks E-AC-3 pending counsel. |
| **MPEG-2 video** | ISO/IEC 13818-4 conformance streams | ISO (paid); a subset is freely mirrored | ISO terms | Reference YUV | C4 (MPEG-2 decode is exactly defined). |
| **JPEG** | ITU-T T.83 / ISO/IEC 10918-2 compliance data | ITU/ISO | Free (T.83 is downloadable) | Reference output with the spec's tolerance | C4 for the pinned-IDCT paths, C5 otherwise per T.83's own criterion. |
| **JPEG XL** | `libjxl/conformance` | `github.com/libjxl/conformance` | Apache-2.0 / BSD-3 | Reference output + tolerance definition | C5 per the conformance repo's own metric. |
| **PNG** | **PngSuite** (Willem van Schaik) — the canonical corner-case set including every bit depth, interlacing, and deliberately corrupt files | `schaik.com/pngsuite/` | "Use as you wish" | Expected renderings documented | C4 for the valid files; **C7 behavioural** for the corrupt ones (we must reject exactly what should be rejected). |
| **WebP** | libwebp test data | `chromium.googlesource.com/webm/libwebp-test-data` | BSD-3 | Reference output | C4. |
| **Matroska** | `Matroska-Org/matroska-test-files` — 8 files exercising the format's awkward corners (unknown-size clusters, damaged files, EBML lacing) | `github.com/Matroska-Org/matroska-test-files` | BSD | No reference decode; the value is demux behaviour | C6 structured + C7 behavioural against the reference; the files' *documented* intent is the spec. |
| **MP4 / ISOBMFF / DASH** | DASH-IF conformance streams; the ISO/IEC 14496-32 conformance set | `conformance.dashif.org`; ISO | DASH-IF: free; ISO: paid | Manifest + segment expectations | C6/C2 for structure, C4 for payload. |
| **MPEG-TS** | DVB/ATSC test streams where freely published; otherwise our own generated set | Various | Mixed — record per file | — | C6 + C4. |
| **G.711 / G.722 / G.726 / G.729** | ITU-T test vectors published with each Recommendation | ITU wftp3 | Free with the Rec | Reference output, bit-exact | **C4 bit-exact** — these specs define exact integer output. |
| **Loudness (EBU R128)** | EBU Tech 3341 compliance material | `tech.ebu.ch` | EBU terms, free | Documented expected LUFS values | C5 against the documented values with the tolerance Tech 3341 states. |
| **Subtitles (WebVTT, TTML)** | W3C test suites | `w3c/webvtt` / `w3c/ttml-tests` | W3C Test Suite Licence (BSD-ish) | Expected parse trees | C6. |

### 4.2 Integration mechanics

```
just suites-fetch [<id>…]     # download, verify hashes, populate the cache
just suites-run <id>          # run one suite
just suites-status            # per-suite pass/fail/skip, and per-codec coverage of the suite
```

- Each suite gets a generated conformance manifest (`tests/conformance/suites/<id>.toml`) with
  `source = "suite://<id>/<path>"` media references and the suite's own expected output as the
  comparison target. The manifest generator reads the suite's index file; adding a suite is a
  `corpus/suites.toml` entry plus a generator run, not hundreds of hand-written cases.
- **Known-failure tracking.** A suite has streams we do not yet support. Each is listed in
  `tests/conformance/suites/<id>.expected-fail.toml` with a reason and an issue link, and the runner
  **fails if a stream on that list unexpectedly passes** — so progress is detected, not just regression.
  The list's length is a headline metric in `docs/codec-status.md`.
- **Suite coverage is part of the fidelity grade.** A codec whose corpus floor is not met by its suite
  coverage is `Unmeasured` (§1.12.1), which means suite integration is not optional busywork — it is
  what lets a codec ship.
- **Availability failures are skips, not errors.** ITU's server goes down. A fetch failure marks the
  suite `Skipped(fetch)` and the job reports it; it does not fail CI, or the pipeline becomes hostage to
  a third party's uptime. But a suite skipped for 7 consecutive nightlies opens an issue, and a release
  cannot proceed with a skipped suite for any codec in the release feature set.

---

## 5. CI design

### 5.1 Pipeline overview

| Stage | Trigger | Wall-clock budget | Blocking |
|---|---|---|---|
| **PR** | every push to a PR | **≤ 15 min** (parallel) | yes |
| **Merge** | push to `main` | ≤ 45 min | yes (reverts on failure) |
| **Nightly** | 02:00 UTC | ≤ 4 h | opens issues; blocks release |
| **Weekly** | Sunday 02:00 UTC | ≤ 14 h | blocks release |
| **Release** | tag `v*` | ≤ 3 h | yes |

The 15-minute PR budget is a design constraint, not an aspiration. Everything in the PR stage is either
seconds-fast (lint, lock-file checks, regression replay) or explicitly budgeted (`conformance-smoke` at
4 min). Anything that cannot fit moves right.

### 5.2 PR jobs

| Job | What it does | Budget |
|---|---|---|
| `fmt` | `cargo fmt --all --check` | 30 s |
| `clippy` | `cargo clippy --workspace --all-targets --all-features -- -D warnings`, with the panic/allocation lint set from §2.2 | 4 min |
| `test-fast` | `cargo nextest run --workspace` (default features, linux-x86_64, dev profile with Cranelift) | 6 min |
| `doctest` | `cargo test --doc --workspace` | 2 min |
| `unsafe-audit` | §5.4 | 40 s |
| `layer-check` | §5.5 | 20 s |
| `licence` | `cargo deny check licenses bans advisories sources` + the `*-sys` manual-review gate (D9) | 90 s |
| `single-wrapper` | D11: every third-party media crate appears in exactly one `crates/**/Cargo.toml` | 20 s |
| `codec-status` | §1.12.4(a): regenerate `docs/codec-status.md` from `fidelity.lock`, diff, validate grades | 30 s |
| `feature-gate-consistency` | §1.12.4(b): no `Divergent`/`Unmeasured` codec reachable from `default`; every `default` codec has a lock row | 40 s |
| `conformance-smoke` | ~400 cases, tier `smoke`, against the `stable` reference image | **4 min** |
| `conformance-mapped` | `core` suites for the crates this PR touches (via `suites.toml`) | ≤ 6 min |
| `fuzz-regressions` | replay every file in `fuzz/regressions/` through its target as a normal test | 2 min |
| `fuzz-smoke` | 60 s per target, only for targets whose crate the PR touches | ≤ 5 min |
| `docs-check` | every `crates/*` has a `docs/` page linked from `docs/README.md`; `cargo doc -D warnings`; every doc has the five required headings | 3 min |
| `provenance` | DCO + trailer lint over the PR's commit range + PR-checklist completion (§6) | 20 s |
| `similarity-scan` | §6.4, only when `crates/` is touched, on an isolated runner | 5 min |
| `feature-quick` | `--no-default-features` build, `default` build, `scalar-only` build, plus 3 rotating single-feature builds seeded by PR number | 8 min |
| `bench-guard` | runs the 12 hottest benchmarks, fails only on a >25% regression (coarse guard; fine tracking is nightly) | 5 min |

### 5.3 Concrete job definitions

```yaml
# .github/workflows/pr.yml  (excerpt)
name: pr
on: { pull_request: { branches: [main] } }
concurrency: { group: pr-${{ github.head_ref }}, cancel-in-progress: true }

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"
  RUSTFLAGS: "-D warnings"

jobs:
  conformance-smoke:
    runs-on: ubuntu-latest-8core
    timeout-minutes: 10
    container:
      # The pinned reference build. Digest, never a tag. See planning/13-correctness.md §1.6.
      image: ghcr.io/vaco/refbin@sha256:0f3c...      # ffmpeg 8.0, --disable-gpl --disable-nonfree
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with: { toolchain: nightly-2026-08-01 }      # matches rust-toolchain.toml
      - name: Restore corpus
        uses: actions/cache@v4
        with:
          path: ~/.cache/vaco/corpus
          key: corpus-${{ hashFiles('corpus/*.lock') }}   # immutable content ⇒ ~100% hit
      - run: just corpus-fetch --tier smoke
      - run: cargo build --release -p vaco -p vaco-probe
      - name: Run
        run: |
          just conformance smoke \
            --reference "$(command -v ffprobe)" \
            --reference-ffmpeg "$(command -v ffmpeg)" \
            --jobs "$(nproc)" \
            --budget 4m \
            --report target/conformance/smoke.json
      - if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: conformance-smoke-failures
          path: |
            target/conformance/smoke.json
            target/conformance/failures/**     # both outputs, the diff, argv, ref version

  unsafe-audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with: { toolchain: nightly-2026-08-01 }
      - run: cargo run -p xtask -- unsafe-audit
      # asserts: (1) every crate root has #![forbid(unsafe_code)] unless allowlisted;
      #          (2) the allowlist file is unchanged unless @security-owner approved this PR;
      #          (3) no allowlisted crate (incl. vaco-fuzz-alloc) appears in
      #              `cargo tree -e normal -p vaco -p vaco-probe -p vaco-play`
      #              for any of: default, full-rf, scalar-only, --no-default-features;
      #          (4) every allowlist entry has a justification_doc that resolves.

  feature-gate-consistency:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with: { toolchain: nightly-2026-08-01 }
      - run: cargo run -p xtask -- fidelity-gate --feature-set default
      # fails if any codec reachable from `default` has grade Divergent | Unmeasured,
      # or is missing from tests/conformance/fidelity.lock entirely.  See §1.12.4(b).

  licence:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
        with: { command: check licenses bans advisories sources }
      - name: sys-crate manual review gate (D9)
        run: cargo run -p xtask -- sys-audit
      # D9: crates.io metadata lies (x264/x265 declare MIT over GPL statics).
      # sys-audit fails on ANY crate in the graph with a build script that compiles
      # native code or links a foreign library — which under D10 Gate 1 must be zero —
      # and cross-checks every dependency against its docs/dependencies.md adoption record.
```

```yaml
# .github/workflows/nightly.yml  (excerpt)
name: nightly
on:
  schedule: [{ cron: "0 2 * * *" }]
  workflow_dispatch:

jobs:
  conformance-full:
    strategy:
      fail-fast: false
      matrix:
        shard: [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]
        reference: [stable]
    runs-on: ubuntu-latest-16core
    timeout-minutes: 200
    steps:
      - uses: actions/checkout@v4
      - run: just refbin-pull ${{ matrix.reference }}
      - run: just corpus-fetch --tier full
      - run: just suites-fetch --all || echo "SUITE_FETCH_PARTIAL=1" >> "$GITHUB_ENV"
      - run: just conformance full --shard ${{ matrix.shard }}/16 --reference-pin ${{ matrix.reference }}

  refbin-drift:
    # Non-blocking early warning: run `core` against the `next` pin and report drift. §1.6.2
    runs-on: ubuntu-latest-16core
    continue-on-error: true
    steps:
      - uses: actions/checkout@v4
      - run: just refbin-pull next
      - run: just conformance core --reference-pin next --report target/drift-next.json
      - run: cargo run -p xtask -- drift-report --against stable --input target/drift-next.json
      # opens/updates a single tracking issue rather than one per case

  fuzz-nightly:
    strategy:
      fail-fast: false
      matrix: { target: ${{ fromJSON(needs.enumerate.outputs.targets) }} }   # generated, §2.1
    runs-on: ubuntu-latest-4core
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@v4
      - run: just corpus-sync-down ${{ matrix.target }}
      - run: |
          cargo fuzz run ${{ matrix.target }} -- \
            -max_total_time=1800 -rss_limit_mb=2048 -malloc_limit_mb=512 \
            -timeout=10 -report_slow_units=5 -print_final_stats=1
      - if: always()
        run: just corpus-sync-up ${{ matrix.target }}
      - if: failure()
        run: just fuzz-triage ${{ matrix.target }} --file-issue

  grade:
    # Recomputes every codec's fidelity grade and commits the lock if it changed. §1.12
    runs-on: ubuntu-latest-16core
    timeout-minutes: 180
    steps:
      - uses: actions/checkout@v4
      - run: just refbin-pull stable
      - run: just grade --all --write
      - run: cargo run -p xtask -- codec-status --write
      - uses: peter-evans/create-pull-request@v6
        with:
          title: "chore: refresh fidelity grades"
          branch: bot/fidelity
          # A grade change is a reviewed event, not an auto-merge.

  release-overflow:
    # Catches arithmetic overflow that debug catches and release hides.
    runs-on: ubuntu-latest-8core
    env: { RUSTFLAGS: "-C overflow-checks=on -D warnings" }
    steps:
      - uses: actions/checkout@v4
      - run: cargo nextest run --workspace --release
```

### 5.4 The unsafe-code audit, concretely

`tools/unsafe-allowlist.toml` is the single source of truth:

```toml
schema = 1

[[crate]]
name              = "vaco-hw-videotoolbox"
reason            = "FFI to Apple VideoToolbox; unavoidable per D2"
justification_doc = "docs/hw/videotoolbox.md#unsafe"
approved_by       = ["@security-owner"]
in_default_build  = false
test_only         = false

[[crate]]
name              = "vaco-fuzz-alloc"
reason            = "GlobalAlloc cannot be implemented in safe Rust; fuzzing allocation backstop"
justification_doc = "planning/13-correctness.md#223"
approved_by       = ["@security-owner", "@correctness-owner"]
in_default_build  = false
test_only         = true
```

`xtask unsafe-audit` performs four assertions (listed in the YAML above). The important one is (3): the
`forbid(unsafe_code)` guarantee is only meaningful if the crates that *do* use unsafe are provably absent
from what we ship, and that is a dependency-graph fact, not a policy statement. It is checked for every
published feature set, not just `default`.

D10's honest caveat applies and is worth repeating in CI output: `forbid(unsafe_code)` is a guarantee
about **our** crates, not our dependencies. `cargo geiger` runs in the nightly and its per-dependency
unsafe counts are published in `docs/dependencies.md`, so the adoption record carries the number that was
true at adoption and CI flags when it grows.

### 5.5 The layer-acyclicity check

```toml
# layers.toml
schema = 1

[[layer]]
n = 0
crates = ["vaco-core", "vaco-simd", "vaco-opts", "vaco-expr", "vaco-bitstream", "vaco-limits"]
[[layer]]
n = 1
crates = ["vaco-pixfmt", "vaco-sampfmt", "vaco-chlayout", "vaco-color", "vaco-frame", "vaco-packet", "vaco-pool"]
# … through layer 7

[rules]
same_layer_deps      = "deny"          # except within a declared sibling group
skip_layer_deps      = "allow"         # layer 5 may depend on layer 0 directly
dev_dependencies     = "exempt"        # test-only edges are not architecture

[[sibling_group]]
name   = "media-model"
layer  = 1
crates = ["vaco-frame", "vaco-packet", "vaco-pixfmt", "vaco-sampfmt", "vaco-chlayout", "vaco-color", "vaco-pool"]
reason = "Frame must name PixFmt and ChannelLayout; these are one conceptual module split for compile time."
```

`xtask layer-check` builds the workspace dependency graph from `cargo metadata`, maps each crate to its
layer, and fails on: any upward edge, any same-layer edge outside a declared sibling group, any cycle
(Tarjan SCC — belt and braces, since Cargo already rejects cycles, but this catches cycles introduced via
dev-dependencies that Cargo permits and that make the architecture untrue), and any workspace crate
missing from `layers.toml`. That last check is what stops the file from silently going stale.

### 5.6 Build matrix

| Axis | Values |
|---|---|
| Platform | `linux-x86_64-gnu`, `linux-x86_64-musl`, `linux-aarch64-gnu`, `macos-aarch64`, `macos-x86_64`, `windows-x86_64-msvc`, `windows-aarch64-msvc` |
| Feature set | `--no-default-features`, `default`, `full-rf`, `scalar-only` (portable-SIMD off — the OSS-Fuzz precondition, §2.6), `hw-<backend>` per platform |
| Profile | `dev` (Cranelift), `release` (LLVM), `release-overflow` (nightly only) |
| Toolchain | the pinned nightly only, plus a **non-blocking** job on `nightly` upstream to give early warning of a breaking `portable_simd` change |

PR runs linux-x86_64 × default × dev. Merge runs the full platform × {default, full-rf, no-default}
matrix. Nightly adds `scalar-only`, `release-overflow`, and `cargo hack --each-feature --workspace check`
(which is the only thing that reliably catches "this crate only compiles when some unrelated feature is
on" — architecture §4 names this explicitly).

### 5.7 Benchmark regression detection

- `criterion` (or `divan`) benchmarks per DSP kernel and per end-to-end pipeline (D8).
- Results are stored per commit in a benchmark history (a JSON series in a `benchmarks` branch or an
  external store), keyed by (bench id, platform, cpu model).
- **Nightly** compares against the trailing 7-day median for the same runner class, not against the
  previous commit — single-commit comparison on shared CI hardware is pure noise. Alert threshold: 5%
  slower than the median with p<0.01 over 10 samples.
- **PR** runs `bench-guard` only: the 12 hottest benchmarks, failing only at >25%. That threshold is set
  above CI noise deliberately; it catches algorithmic disasters, not regressions. Fine-grained detection
  is a nightly job's business.
- `just bench-compare <ref>` gives a developer the local A/B, which is where real performance work
  happens.
- `vaco-checkasm` also reports per-kernel cycle counts, and its output feeds the same history — so a
  SIMD kernel losing to its own scalar reference is visible.

### 5.8 Patent- and licence-posture assertions

D4 requires CI to publish the default binary "with the encumbered feature set provably absent (assert on
the compiled feature list, not on intent)". Four assertions, in ascending order of strength:

1. **Feature-name assertion.** `build.rs` records the enabled feature list into `VACO_BUILD_FEATURES`.
   `xtask assert-release-features` runs the **built binary** (`vaco -build_features`) and diffs its output
   against `release/expected-features-<tier>.txt`, a reviewed, committed file. Fails on any feature
   matching `^patent-encumbered-`, `^gpl-`, `^nonfree-`, or `^ffi-`.
2. **Dependency-graph assertion.** No crate on the encumbered/GPL/nonfree register appears in
   `cargo tree -e normal` for the release feature set. This catches an encumbered codec pulled in
   transitively by a feature nobody audited.
3. **Runtime-surface assertion.** The built binary's own listing output is checked: `vaco -encoders` must
   not offer HEVC, VVC, AAC, AC-3, E-AC-3 or DTS encode; `vaco -decoders` must not offer AAC decode
   (D9 gates decode too, and permits only remux). This is the assertion that matters, because it tests
   what a user can actually invoke.
4. **AAC-remux carve-out check.** D9 keeps AAC *remuxing* in the default build. So the assertion is
   asymmetric and must be written as such: AAC must be present as a *stream-copy-capable codec id* and
   absent as an encoder and decoder. A blanket "no AAC" check would be wrong and would be worked around;
   spell out the carve-out in `expected-features-default.txt` with a comment citing D9.

Plus D9's standing rule, enforced by the release workflow refusing to run on any tier other than
`default`: **never publish a "full" convenience binary.**

---

## 6. The provenance and clean-room evidence trail

research 07 §1.6.3 defines what the evidence must be. This section defines how it gets produced without
anyone resenting it — because a provenance system people route around is worse than none, since it
creates a false record.

**Design principle: the contributor does two things once, and one small thing per PR. Everything else is
machine-side.**

### 6.1 Once per contributor

```
just setup
```

installs git hooks and, on first run, prompts for the DCO + clean-room attestation, writing
`.git/vaco-attestation`:

```
name  = "Jane Doe"
email = "jane@example.com"
dco   = "1.1"
attested = 2026-09-02
dirty_modules = []          # modules for which this contributor is on the dirty team
```

`dirty_modules` is the contributor's own copy; the authoritative register is private (research 07
§1.6.3(e)) and maintained by the gatekeeper. The local copy exists so the pre-commit hook can *warn* when
you are about to commit implementation code to a module you are dirty for — catching the honest mistake
before it becomes a provenance problem.

### 6.2 Per commit: the trailers, auto-filled

A `prepare-commit-msg` hook pre-populates the trailer block from the attestation and from the paths in
the staged diff, leaving only the spec reference for the human:

```
<subject>

Signed-off-by: Jane Doe <jane@example.com>
Vaco-Provenance: spec
Vaco-Spec-Ref: ITU-T H.264 (08/2021) §9.3.1.1, Table 9-12
Vaco-Clean-Room: yes
```

- `Vaco-Provenance` ∈ `spec | rfc | paper | blackbox | cleanroom-doc:<path> | original`.
- `Vaco-Spec-Ref` is required when provenance is `spec`/`rfc`/`paper`; **its source id is validated
  against `provenance/*.yaml`**, so a typo or a citation to a document we have not recorded acquiring
  fails CI rather than sitting in the log looking authoritative.
- `Vaco-AI-Assisted: yes` is required for commits with substantial AI-generated codec logic (research 07
  §1.6.4) and triggers the extra review path in §6.3.
- The hook only fires for commits touching `crates/codec/**`, `crates/format/**`, `crates/dsp/**` and
  `crates/filter/**`. A README fix needs nothing beyond a DCO sign-off. Scoping the requirement to where
  it matters is the main reason it will actually be honoured.

`xtask provenance-check` validates the PR's whole commit range: trailer presence, enum membership,
spec-ref resolution, DCO sign-off, and — for `Vaco-Provenance: original` on a codec path — a flag for
human review, because "original" on a codec is unusual and worth a look.

### 6.3 Per PR: the checklist

The PR template carries research 07 §1.6.3(b) verbatim, with one addition for D11:

```markdown
## Clean-room checklist
- [ ] I have NOT read FFmpeg/libav/x264/x265/VLC/GStreamer source for the module(s) this PR touches.
- [ ] Every constant table added cites the specification clause it was transcribed from.
- [ ] No table was copied from another implementation's source (including permissively-licensed
      ones) without being recorded in `THIRD_PARTY.md` with its licence.
- [ ] No text (comments, help strings, docs) was copied from FFmpeg or from a standards document.
- [ ] Tests compare against spec-defined output or a freshly-run reference binary, not against
      checksums copied from another project's repository.
- [ ] `Vaco-Provenance:` trailer present on every commit.
- [ ] If any Tier-B material was consulted: I am the dirty-team member for this module and I have
      NOT authored implementation code here.
- [ ] If this PR adds or changes an external dependency: `docs/dependencies.md` records the D10
      gate assessment, and the crate is reachable from exactly one Vaco crate (D11).
```

A bot comments on each PR with: the spec clauses cited across the commit range, the list of modules
touched, and a warning if any author is on the dirty register for those modules. That last check turns
the honour system into a cross-check without anyone doing work.

### 6.4 The CI similarity scan

The artefact you want to be able to show a court, and cheap to run.

**Mechanism.** Winnowing fingerprints (Schleimer, Wilkerson & Aiken, *SIGMOD 2003* — the MOSS algorithm),
implemented by us:

1. Normalise: strip comments and whitespace, canonicalise identifiers to positional tokens, canonicalise
   integer literals except those in declared constant tables.
2. Tokenise, take k-grams with k = 40 tokens, hash, and keep the minimum hash in each window of w = 20 —
   giving a fingerprint set with a guaranteed detection threshold of k + w − 1 = 59 tokens.
3. Index a local FFmpeg checkout (plus x264, x265, libvpx, dav1d, GStreamer, VLC) **once**, into a
   fingerprint database stored as a build artifact.
4. On each PR touching `crates/`, fingerprint the changed files and query the index.
5. Report any match at or above the threshold.

**Three design points that matter more than the algorithm:**

- **The scan runs on an isolated runner.** The FFmpeg checkout exists only there. No developer machine
  needs it, and the "just clone it to run the check locally" temptation is removed by making the check
  a hosted service (`just similarity-check` submits the diff and returns the report).
- **The report must not leak the matched text.** It emits: our file, our line range, the token length of
  the match, the matched corpus name, and a hash of the matched region — **never the upstream text**.
  Otherwise the CI log becomes a side channel that contaminates every clean implementer who reads it.
  This is a genuine failure mode and the rule needs to be in the job's own README.
- **Expected false positives are allowlisted structurally, not case by case.** Spec-mandated constant
  tables will match, because both implementations transcribed the same table from the same standard.
  Those are allowlisted by reference to the `provenance/*.yaml` table entries — so the allowlist is a
  *consequence* of the provenance record rather than a second list to maintain. A table match with no
  provenance entry is a finding.

**What a hit means.** Not automatic guilt. A hit against a permissively-licensed corpus (dav1d, libvpx)
is a *licence-attribution* question, resolved by a `THIRD_PARTY.md` entry or a rewrite. A hit against
FFmpeg is a clean-room question and goes to the gatekeeper. Either way the PR is blocked until resolved,
and the resolution is recorded.

### 6.5 Provenance records

`provenance/<format>.yaml`, per research 07 §1.6.3(c), one per format/codec, recording primary sources,
spec author, gatekeeper, implementers, attestations, and every constant table with its source clause and
its derivation method (`transcribed-from-spec` / `generated-at-build-time` / `derived-independently`).

Two mechanisms keep them true rather than decorative:

- `xtask provenance-check` cross-references: every `static`/`const` array over a declared size threshold
  (say 32 elements) in a codec crate must appear in the crate's provenance YAML. A large table with no
  provenance entry fails CI. This is the check that makes the record complete rather than
  best-effort.
- Tables marked `generated-at-build-time` are verified: CI regenerates them from the `build.rs`/`const fn`
  and asserts equality with whatever is committed. A table that claims to be generated but is actually
  pasted fails — and "pasted" is exactly the risk research 07 §1.5b warns about.

### 6.6 Cost, honestly accounted

| Actor | Work |
|---|---|
| New contributor | `just setup` once (~1 min) |
| Per commit on a codec path | fill in one `Vaco-Spec-Ref:` line the hook pre-formatted |
| Per PR | tick an 8-box checklist |
| Everything else | machine |

That is the whole burden. The expensive parts — the similarity index, trailer validation, table
cross-referencing, the dirty-register cross-check — are all machine-side and run without anyone thinking
about them. This is deliberate: research 07 §1.7 concludes that the residual risk in the tiered model is
"an individual contributor lied on an attestation", and that risk is managed by automation, not by more
paperwork.

---

## 7. Release engineering

### 7.1 Reproducible builds

Target: two independent builders produce byte-identical artifacts.

- `rust-toolchain.toml` pins the exact nightly (D8); `Cargo.lock` is committed.
- Build inside a container pinned **by digest**, not tag.
- `SOURCE_DATE_EPOCH` set from the tag's commit date; `CARGO_INCREMENTAL=0`;
  `RUSTFLAGS="--remap-path-prefix=$PWD=/vaco --remap-path-prefix=$CARGO_HOME=/cargo"`.
- `codegen-units = 1`, `lto = "fat"` — required for performance anyway (D8), and they remove
  parallel-codegen nondeterminism as a side effect.
- No build-time timestamps, hostnames, or paths in the binary. `build.rs` emits only the commit hash,
  the feature list, and the toolchain id.
- **Verification is a job, not a hope:** `release-repro` builds every artifact twice on two different
  runners and fails if the hashes differ. Reproducibility claimed but unverified is worth nothing.

**PGO and reproducibility.** PGO makes the profile an *input*, so it must be pinned like any other input:
profiles are generated by a separate deterministic workflow from a fixed corpus with a fixed seed and a
fixed thread count, stored content-addressed in the object store, and pinned in `pgo/profiles.lock`. A
release build consumes a pinned profile; it never generates one. BOLT, if adopted, is treated identically.

Each artifact ships a `build-info.json`: source commit, toolchain hash, container digest, feature list,
PGO profile hash, and the reproducibility verification result.

### 7.2 Signing and attestation

- **Sigstore/cosign keyless signatures** on every artifact and container image, plus **SLSA v1.0 build
  provenance** via `actions/attest-build-provenance`. This is the default path and requires no key
  custody.
- **Minisign detached signatures** with a long-lived project key as a secondary, for downstreams that do
  not consume sigstore. The key lives in an HSM or in the org's secret store with a documented rotation
  procedure.
- **macOS:** codesign + notarize. Requires an Apple Developer ID — **a prerequisite to flag now**, since
  it needs a legal entity, which D9 already lists as a decision to make before incorporating.
- **Windows:** Authenticode via Azure Trusted Signing (or an EV certificate). Also needs the entity and a
  budget line. Until then, ship unsigned with clear documentation rather than pretending.
- `SHA256SUMS` and `SHA256SUMS.minisig` alongside every release.

### 7.3 THIRD_PARTY.md and SBOM

- `cargo about generate --config about.toml --template about.hbs` produces `THIRD_PARTY.md`,
  **once per shipped feature tier** (`default` and, for source builds, `full-rf`) — they have different
  dependency graphs and one file cannot describe both honestly.
- CI check on every PR: regenerate and diff. A PR that adds a dependency without regenerating fails. This
  is the mechanism that keeps attribution correct, and attribution is a licence *obligation* under every
  permissive licence we allow, not a courtesy.
- **SBOM:** `cargo cyclonedx` → `vaco.cdx.json` per artifact, attested alongside the binary.
- `docs/dependencies.md` (D10) carries the adoption record — the gate assessment, the date, the signer,
  the `cargo-geiger` unsafe count at adoption — and CI asserts every crate in the graph has a record.

### 7.4 Per-platform artifacts

| Target | Form |
|---|---|
| `x86_64-unknown-linux-gnu` | `.tar.zst` |
| `x86_64-unknown-linux-musl` | `.tar.zst`, fully static |
| `aarch64-unknown-linux-gnu` | `.tar.zst` |
| `aarch64-unknown-linux-musl` | `.tar.zst`, fully static |
| `aarch64-apple-darwin` | `.tar.zst`, signed + notarized |
| `x86_64-apple-darwin` | `.tar.zst`, signed + notarized |
| `x86_64-pc-windows-msvc` | `.zip`, Authenticode-signed |
| `aarch64-pc-windows-msvc` | `.zip`, Authenticode-signed |
| container | `ghcr.io/vaco/vaco`, multi-arch, cosign-signed, SBOM-attested |

Each archive contains `vaco`, `vaco-probe`, `vaco-play`, `THIRD_PARTY.md`, `LICENSE-MIT`,
`LICENSE-APACHE`, `build-info.json`, `vaco.cdx.json`, and the shell completions and man pages we generate
from our own option tables (never from FFmpeg's docs — research 07 §1.5a).

D10 Gate 1 (pure Rust, no FFI) makes this matrix dramatically easier than it would otherwise be: with no
native dependencies, cross-compilation is genuine cross-compilation and the musl static builds are
trivial. That is a correctness-relevant benefit as well as a packaging one — the artifact we test is the
artifact we ship, on every platform.

### 7.5 The release gate

`just release-check` — every item blocking, run by the release workflow:

```
[ ] conformance-full green on the `stable` reference pin
[ ] conformance-exhaustive green (weekly run within the last 7 days)
[ ] all codec conformance suites (§4) run; zero unexpected passes on expected-fail lists;
    zero skipped suites for any codec in the release feature set
[ ] release-fidelity: grades recomputed against the live reference and identical to fidelity.lock
[ ] every codec in the release feature set graded Exact or Equivalent   (§1.12.4)
[ ] zero expired divergence-allowlist entries; zero `unexplained` entries in a shipping module
[ ] divergence report generated and included in the release notes
[ ] fuzzing clean for 7 consecutive days across every target; zero open crash findings
[ ] fuzz-regressions replay green
[ ] no `Divergent`/`Unmeasured` codec reachable from `default`
[ ] assert-release-features: no patent-encumbered / gpl / nonfree / ffi feature enabled  (§5.8)
[ ] runtime-surface assertion passes, including the D9 AAC-remux carve-out
[ ] unsafe-audit: no allowlisted crate in any shipped binary's dependency graph
[ ] cargo-deny clean; sys-audit clean (D9); zero open RUSTSEC advisories
[ ] THIRD_PARTY.md and SBOM regenerated and matching
[ ] release-repro: double-build byte-identical on two runners
[ ] docs/codec-status.md regenerated and matching
[ ] the release tier is `default` — the workflow refuses any other tier  (D9)
```

---

## 8. Justfile targets

Extending architecture §8's list. `just` is the only interface a developer needs (D8).

```makefile
# ---- everyday -------------------------------------------------------------
build target="":            # cargo build (Cranelift dev profile)
test:                       # cargo nextest run --workspace
lint:                       # fmt --check + clippy -D warnings
fmt:
setup:                      # git hooks + DCO/clean-room attestation (§6.1)
ci:                         # everything the PR stage runs, locally

# ---- differential conformance --------------------------------------------
conformance tier="smoke" *ARGS:      # run a tier
conformance-run CASE_ID:             # re-run one case, print both outputs + diff
conformance-new SUITE:               # scaffold a manifest
conformance-explore MEDIA *ARGV:     # ad-hoc both-sides run; suggests a manifest stanza (§1.5.3)
conformance-minimise CASE_ID:        # structure-aware bisection of a diverging input (§1.7.3)
conformance-report:                  # HTML report: pass/fail/skip, divergence hit counts
divergence-report:                   # the allowlist table + blast radius per entry (§1.4.3)
three-way CRATE:                     # C9 run for one crate's backends (§1.10)

# ---- fidelity grading (D11) ----------------------------------------------
grade *ARGS:                         # recompute grades; --write updates fidelity.lock
grade-check:                         # validate lock vs docs/codec-status.md; the PR gate
codec-status:                        # regenerate docs/codec-status.md from the lock

# ---- reference binary ----------------------------------------------------
refbin-pull PIN="stable":
refbin-build PIN="stable":
refbin-bump VERSION:                 # build, dual-run, emit the drift report (§1.6.2)

# ---- corpora and suites --------------------------------------------------
corpus-fetch *ARGS:                  # download + verify by hash
corpus-gen:                          # regenerate our own synthesised media
corpus-mutate FORMAT COUNT:          # structure-aware mutation (§2.4.5)
corpus-sync-up TARGET:
corpus-sync-down TARGET:
corpus-minimise TARGET:
suites-fetch *IDS:
suites-run ID:
suites-status:

# ---- fuzzing -------------------------------------------------------------
fuzz TARGET *ARGS:
fuzz-all DURATION="60":
fuzz-list:                           # enumerate generated targets
fuzz-gen:                            # regenerate targets from the component manifest (§2.1)
fuzz-diff FAMILY *ARGS:              # the differential campaign (§2.4.6)
fuzz-triage TARGET FILE:             # minimise + write a regression fixture (§2.5.4)
fuzz-regressions:                    # replay every stored regression as a test
fuzz-coverage TARGET:

# ---- verification and audit ----------------------------------------------
layer-check:
unsafe-audit:
licence-check:                       # cargo deny
licence-report:                      # regenerate THIRD_PARTY.md
sys-audit:                           # D9: what *-sys crates actually link
single-wrapper:                      # D11 one-wrapper-crate rule
provenance-check:
similarity-check:                    # submits the diff to the isolated scanner (§6.4)
docs-check:
coverage:
mutants:

# ---- performance ---------------------------------------------------------
bench *ARGS:
bench-compare REF:
checkasm *ARGS:                      # kernel differential + cycle counts
pgo-build TARGET:

# ---- release -------------------------------------------------------------
release-check:                       # the full gate (§7.5)
assert-release-features TIER="default":
release-repro TARGET:
release TAG:
```

---

## 9. Rollout by milestone

Correctness infrastructure is built in the order that makes the *next* milestone provable, not all at
once. D5 is v0.1 = ffprobe on modern containers, byte-identical.

### v0.1 — must exist

- `vaco-conformance` with **C0, C6, C7** and the normalisation chain. C0 is the D5 acceptance criterion,
  so it is the first thing that works.
- The manifest loader, matrix expansion, case ids, `conformance-explore`, `conformance-run`.
- `divergences.toml` with governance from day one — the allowlist is cheapest to discipline when it has
  three entries, not three hundred.
- Reference pinning (`stable` only; `next` can wait), the refbin container, `refbin-bump` without the
  dual-run report.
- `vaco-limits`, complete, with the clippy allocation bans. **This must exist before the first demuxer**,
  because retrofitting a required constructor parameter across 90 crates is exactly the kind of change
  nobody ever does.
- Fuzz targets: `dem_*` for the four v0.1 containers, `parse_*` for the four v0.1 codecs, `probe_`,
  `opt_`, `cli_`. Plus `vaco-fuzz-support` with `Guard` and `ProgressGuard`.
- `fuzz-regressions` replay as a normal test.
- CI: the full PR stage except `conformance-mapped`, `similarity-scan` (needs the isolated runner) and
  the fidelity jobs (no codecs yet).
- Provenance: trailers, hook, `provenance-check`, PR template. Cheap, and retrofitting a provenance
  record onto existing commits is impossible.
- `corpus-fetch` and the content-addressed store.

### v0.2 — first decoders

- **C3a, C4, C5** and the codec conformance suite integration (§4) — this is what a decoder milestone
  means.
- `vaco-checkasm` and the `inv_kernel_*` fuzzers, in step with the first SIMD kernels.
- `inv_seek_*`, `inv_threads_*`.
- Fidelity grading (§1.12) and `docs/codec-status.md`, as soon as the first external-backend crate lands
  under D10/D11 — grading must exist *before* the first wrapped codec, or the first codec ships
  ungraded and sets the precedent.
- `similarity-scan` on an isolated runner.
- The `diff_` campaigns.

### v0.3 — muxers and encoders

- **C2, C8, C9, C10**, the quality-metric implementations, `quality.lock` and its ratchet.
- `inv_mux_demux_*`, `inv_encode_determinism`.
- Three-way comparison, wherever both backends exist.

### v1.0 — release engineering

- Reproducible-build verification, signing, notarization, SBOM, `release-check` complete.
- OSS-Fuzz enrolment if the §2.6 preconditions hold.

---

## 10. Open questions

1. **Reference-image distribution (counsel).** §1.6.1 — is publishing a built GPL/LGPL reference image to
   our own registry, with source URL, hash and build scripts alongside, sufficient for GPL §3/§6? If
   unsure, we build in CI every run and publish nothing. Low cost either way; decide before the first
   public CI run.
2. **Conformance-suite fetch terms.** Several suites (ITU, ISO) permit download but not redistribution.
   Does *caching them in our CI's object store* count as redistribution? Probably not (it is our own use),
   but the cache is technically a copy on infrastructure we control. Cheap mitigation: cache only in
   ephemeral CI storage for those specific suites, and accept the re-download cost. Needs a decision
   before §4 integration lands.
3. **Argon AV1 stream licensing.** §4.1 — record the actual published terms at adoption rather than
   assuming they are BSD. This one is load-bearing because Argon is by far the best AV1 coverage set.
4. **Paid conformance suites.** MPEG audio (ISO 11172-4 / 13818-4) and AAC (14496-26) cost money. Given
   D9 makes AAC RED anyway, AAC is easy to defer. MP3 is GREEN and shippable, so its suite is a real
   budget question. Decide at v0.2.
5. **VMAF-equivalent metric.** C10 wants a perceptual metric. Netflix's VMAF is BSD-2 but is a C library
   (D10 Gate 1 excludes it). Is there a pure-Rust implementation that clears the D10 gates, or do we
   implement the model ourselves from the published papers, or do we ship PSNR+SSIM only and say so? The
   honest default is the third.
6. **OSS-Fuzz and the pinned nightly.** §2.6 precondition 3 is the real risk. The `scalar-only` feature
   set is the mitigation and it is in the CI matrix for that reason, but confirm early that
   `cargo fuzz build` works on OSS-Fuzz's toolchain before promising enrolment.
7. **Fuzzing 500+ targets is a compute bill.** The nightly matrix at 30 min × 500 targets is 250 CPU-hours
   a night. Either we buy it, or we rotate (each target runs every N nights, prioritised by recent code
   change and by new-coverage rate). Rotation is almost certainly right; specify the scheduler at v0.3.
8. **Who is the correctness owner?** §1.4 and §1.12 hang two CODEOWNERS gates on this role. It needs a
   named person before the first divergence-allowlist entry, or the governance is decorative.
