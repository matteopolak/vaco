# `vaco-cli` — the `ffmpeg`-equivalent binary

## What it is

`vaco` is the transcoding binary (D1). This crate is its **spine**: argument
binding, opening inputs through the protocol and format registries, stream
selection, building a `vaco_sched::PipelineSpec`, driving it, and reporting —
argv in, a run executed, a correct exit code out.

It is deliberately *not* the whole of `ffmpeg`. The twenty work packages above
the spine (metadata mapping, `-progress`/`-stats`/`-report`, filtergraph
binding, `-force_key_frames`, the timestamp matrix, `[dec:N]`, `-stream_group`,
presets, hardware devices) are listed under [Deferred](#deferred), each with the
issue that owns it.

A **library plus a thin binary**, for the same reason as `vaco-probe`:
`cargo fuzz` links a library, and D6 makes a fuzz target mandatory for a crate
whose input is a user's command line. The binary keeps only argv, stdio and the
exit code. The lib target also brings the crate inside `cargo xtask wasm-check`
(D18), which it passes — the only OS coupling is `std::fs` behind `vaco-io`, and
there is no clock.

## The constraint that shapes everything: there are no muxers

D5 scopes v0.1 to demuxing. `crates/format/` contains three `vaco-demux-*`
crates and no `vaco-mux-*`; `vaco_registry::muxers()` is empty; there are no
decoders and no encoders either.

The design decision, and the reasoning:

* **`-f null` is implemented as a real sink** (`src/nullmux.rs`). `null` is a
  genuine format in the reference — `ffmpeg -i in.mkv -c copy -f null -` exits 0
  and prints `video:7KiB audio:16KiB …` — it needs no container knowledge, and
  it makes the whole spine runnable *and observable*: protocol → probe → demux →
  discovery → selection → `vaco-sched` → a counting sink. Without it the binary
  would compile, parse a command line and then have nowhere to send a packet,
  which is indistinguishable from being broken.
* **Every other output is refused with a message naming the real reason**, and
  the refusal distinguishes three cases rather than collapsing them:

  | `-f` / extension | first line | exit |
  |---|---|---|
  | a format this build can **read** (`matroska`, `mp4`, `mpegts`) | `No muxer for 'matroska': this build reads that format but cannot write it. D5 scopes v0.1 to demuxing, so \`-f null\` is the only output.` | 8 |
  | a name nothing claims (`-f nosuchformat`) | `Requested output format 'nosuchformat' is not known.` | 234 |
  | no `-f`, unhelpful extension | `Unable to choose an output format for 'out.zzz'; …` | 234 |

  The last two are the reference's own wording and status, byte-identical modulo
  the pointer it prints in its log prefix. The first has no reference behaviour
  to match, because the reference is never built without muxers;
  `AVERROR_MUXER_NOT_FOUND` is the code that names the situation.

Both halves, because either alone is wrong: a null sink with no explanation
leaves a user guessing why `out.mkv` produced nothing, and an error with no
working path leaves the whole stack untestable.

**There are no encoders either**, so an output stream must carry `-c copy`.
Without one the run takes the reference's *own* path for a build missing an
encoder — `Automatic encoder selection failed Default encoder for format null
(codec none) is probably disabled. Please choose an encoder manually.`, exit 8 —
which is a message the reference already emits for exactly this situation, and
therefore the right one to reproduce rather than invent. (The run-together
"failed Default" is the reference's own missing separator; reproduced under
D17.) This is why `nullmux::NULL_MUXER` declares **no** default codecs, unlike
the reference's `null`, which declares `wrapped_avframe` and `pcm_s16le`.

### What the acceptance criterion is instead

Not byte identity of an output file — there is none. It is that the same argv
produces:

1. the same **stream selection** (`Stream #0:0 -> #0:0 (copy)`),
2. the same **stderr text and exit code**,
3. the same **packet counts through the pipeline** (`nullmux::OutputTally`).

## How it works

```text
argv ─▶ [cli]      split, validate, bind          (vaco-cli-core, cli.rs)
     ─▶ [help]     -h and its four depths          (help.rs)
     ─▶ [listing]  -version/-formats/… and exit   (listing.rs)
     ─▶ [input]    protocol → probe → demux       (vaco-io, vaco-format-core)
     ─▶ [select]   -map, or the auto rules        (select.rs)
     ─▶ [exec]     a PipelineSpec, driven          (vaco-sched)
     ─▶ [exit]     stderr text and a status code   (exit.rs)
```

| Module | Owns |
|---|---|
| `cli` | binding a split command line; the `AVOption` oracle |
| `help` | CL-04: `-h`'s three depths and `-h <kind>=<name>`, wiring `vaco_cli_core::help`'s renderers to `vaco-registry` |
| `select` | `-map` and the automatic selection rules |
| `input` | opening one input: protocol env, probe, `Discovery` |
| `nullmux` | the `null` sink and its counters |
| `exec` | muxer resolution, the codec check, `PipelineSpec`, the driver |
| `exit` | `AvError`, `ExitCode`, `Diagnostic` |
| `listing` | `-version`, `-formats` and the other thirteen registry listings; CL-04 |

### Stream selection — the measured rule

Plan 14 §6.2 states the video rule as "the stream with the greatest
`width × height`". **That is wrong on its own.** Measured against ffmpeg 8.1:

```text
320x240 (default) vs 640x480     -> 320x240 wins
320x240 (default) vs 3840x2160   -> 3840x2160 wins
```

Both hold only if the `default` disposition is worth a finite number of pixels.
Bracketed by bisection on the second stream's area against a fixed `320x240`
(76 800) first stream carrying `default`:

| second stream | area | area − 76 800 | winner |
|---|---|---|---|
| 1920x1080 | 2 073 600 | 1 996 800 | first (default) |
| 2048x2048 | 4 194 304 | 4 117 504 | first (default) |
| 2538x2000 | 5 076 000 | **4 999 200** | first (default) |
| 2539x2000 | 5 078 000 | **5 001 200** | second |
| 2400x2400 | 5 760 000 | 5 683 200 | second |

The cliff is between 4 999 200 and 5 001 200, so the bonus is exactly
**5 000 000** and the comparison is strict — `select::DEFAULT_DISPOSITION_BONUS`.

The full rule:

* **video** — `score = area + 5 000 000 × default`, where `area` is
  `width × height` normally and **0** for a stream flagged `attached_pic`. A
  4000x4000 cover lost to a 64x64 track; an mp3 whose only video is its cover
  selects the cover. So an attached picture is deprioritised, not excluded.
* **audio** — `score = channels + 5 000 000 × default`. Bit rate and sample rate
  do **not** participate: 32 kbit/s beat 256 kbit/s and 8 kHz beat 48 kHz, both
  times because they came first. The audio bonus is only bounded *below* by
  measurement (no channel count can reach 5 000 000); it is written as the same
  constant for symmetry.
* **subtitle** — the first in order. Kind-matching against the output's default
  subtitle encoder (plan 14 §6.2 Rule 5) is unreachable without encoders and is
  not implemented.
* **data and attachment** — never auto-selected.
* **ties** — the earlier `(file index, stream index)` wins, across files as well
  as within one.

`-map` turns automatic selection off entirely, for every type. Maps apply in
command-line order; `-map -SPEC` removes from what earlier maps accumulated and
never adds; the same stream twice is a fan-out. `-vn`/`-an`/`-sn`/`-dn` filter
the result of *both* paths.

Two corners that took nine probes:

* **An output whose maps all matched nothing is dropped, not an error.**
  `-map 0:v:9?` exits 0 and creates no file, while `-vn -an -sn -dn` and
  `-map 0:v:0 -map -0:v:0` both exit 234 with "Output file does not contain any
  stream". The discriminator is not "did a stream reach the output" —
  `-map 0:v -vn` exits 234 with none — it is **did any positive map match any
  input stream at all**. `select::Selection::dropped`.
* **A negative map errors only when nothing has been accumulated yet.**
  `-map -0:v` fails with "Stream map '' matches no streams"; `-map 0:a -map -0:v`
  succeeds, and removes nothing either. Whether the reference's real predicate is
  emptiness or something that coincides with it on these inputs is not settled;
  the six probes are pinned in `select.rs`'s tests.

The empty `''` in "Stream map ''" is the reference's, not a formatting bug of
ours: it never fills the map text in. Reproduced under D17.

### Exit codes — the rule, not a table

The reference's process status is **the negative `AVERROR` truncated to eight
bits**. Measured with no pipe between the binary and the shell (plan 13 §1b: a
pipe swallows `$?`, and the usual `${PIPESTATUS[0]}` repair is *bash* — `zsh`
spells it `$pipestatus[1]`, so in `zsh` the bash form expands to nothing and the
comparison silently passes).

| invocation | `$?` | why |
|---|---|---|
| `ffmpeg` | 1 | usage |
| `ffmpeg -i in.mkv` | 1 | "At least one output file must be specified" |
| `ffmpeg -i nope.mkv -f null -` | 254 | `ENOENT` = −2 |
| `ffmpeg -i . -f null -` | 235 | `EISDIR` = −21 |
| `ffmpeg -i script.sh -f null -` | 183 | `INVALIDDATA` = −1 094 995 529, low byte `0xB7` |
| `ffmpeg -f null -` | 234 | `EINVAL` = −22 |
| `ffmpeg -i` | 234 | `EINVAL`, **not** option-not-found |
| `ffmpeg -qwerty 3 …` | 8 | `AVERROR_OPTION_NOT_FOUND` |
| `ffmpeg -i in.mkv -c:v nosuch -f null -` | 8 | `AVERROR_ENCODER_NOT_FOUND` |
| `ffmpeg -i nosuchproto://x -f null -` | 8 | `AVERROR_PROTOCOL_NOT_FOUND` |

The four-character tags all begin `0xF8`, which is why four unrelated failures
share exit 8. `AvError::exit` reproduces that by arithmetic, not by a table.

Two failures are not an `AVERROR` at all and exit `1`: an empty argument vector,
and no output file.

16 of 19 differential invocations match the reference's exit code exactly today;
the three that do not are all the reordered-video gap below.

`-h` (all four spellings and depths), `-h <kind>=<name>`, and every listing
command exit **0**, including the "not found"/"no name" `-h` outcomes — the
reference's own `-h zzzz=x` and `ffmpeg -h full` both exit 0, and there is no
error path here that does not. Only `-h`/`-version`/`-buildconf` additionally
print a literal `Exiting with exit code 0` on stdout, unconditionally
(measured even at `-loglevel quiet`); see `vaco_cli_core::help`'s doc comments
for the full measurement and why the other thirteen listings do not get it.

### `-h` and the listing commands

CL-04. `help.rs` wires `vaco_cli_core::help`'s two renderers (the
command-line-option grouping, and the `AVOptions`-block algorithm — both
measured against `ffmpeg 8.1`/`ffprobe 8.1`, see that crate's doc file) to
what this build's registry actually contains.

**Shipped, and measured structurally identical to the reference where this
build's contents allow it:**

* `-h` / `-h long` / `-h full` — our own prose (D9 bars copying help text),
  the reference's column algorithm and blank-line rules.
* `-h <kind>=<name>` for all seven kinds, including the full "no name" /
  "unknown name" matrix (`No codec name specified.`, `Unknown format
  '(null)'.` vs `Unknown format ''.`, `No protocol name specified.`, …) and
  the found path for `demuxer=<matroska|mp4|mpegts>` — `-h demuxer=matroska`
  is **byte-identical** to the reference (measured), because matroska has no
  private options in *either* implementation. `mp4`/`mpegts` diverge by
  missing the reference's private-options block — see the "not this crate's
  gap" note in `vaco-cli-core`'s doc file.
* `-formats`/`-demuxers`/`-muxers` (shared header+legend, `max(15, len)+1`
  name field), `-decoders`/`-encoders`/`-filters` (legend only — always zero
  rows, D5), `-bsfs`, `-protocols` (`Input:`/`Output:` sections — see the
  known gap below), `-codecs` (full six-column flags from
  `vaco_codec_core::CodecProperties` and `vaco_registry::can_decode`),
  `-dispositions` (already byte-identical: nineteen bare names).

**Known gaps in this build's data, not in the renderer:**

* `vaco_protocol_core::ProtocolFlags` carries no read/write capability bit, so
  `-protocols` cannot tell an input-only protocol from an output-only one and
  lists every enabled protocol under both `Input:` and `Output:`.
* `vaco_format_core::DemuxerDesc`/`MuxerDesc` and `vaco_codec_core::DecoderDesc`/
  `vaco_filter_core::FilterDesc` have no options-schema hook at all, so a
  private-options block can never be shown for any of them even where one
  exists in the reference (`mp4`, `mpegts`). `vaco_protocol_core::ProtocolDesc`
  already has one (`options: Option<fn() -> &'static Schema>`); the other four
  descriptor types not having the same field is the asymmetry worth fixing.
* By default this build's registry has **zero protocols at all** —
  `protocol-http` is a non-default feature and `vaco-protocol-file` ships no
  `vaco-component.toml` (already flagged in "Reported upstream" below) — so
  `-h protocol=file`/`-protocols` render structurally correctly but show
  nothing to test against without `--features protocol-http`.

**Deliberately not attempted**, named rather than left silent:

* `-pix_fmts`/`-sample_fmts`/`-layouts`/`-colors`/`-hwaccels`/`-devices`/
  `-sources`/`-sinks` still return `ENOSYS`. The first three need per-format
  component counts, bit depths and alpha/paletted/bitstream/hardware flags
  that `vaco-pixfmt`/`vaco-sampfmt`/`vaco-chlayout` do not currently expose
  through a public API this crate can reach; `-colors` was never wired to a
  renderer either. The last four need a hardware/device registry this
  project does not have yet (D13's `vaco-hw-*` crates are a separate work
  package). Headers for `-pix_fmts`/`-sample_fmts`/`-layouts` were measured
  and are recorded in `vaco-cli-core`'s doc file for whoever picks this up.
* **The brief that scoped this work named `-colorspaces` as one of the
  fourteen listing commands to cover. It does not exist in ffmpeg 8.1** —
  measured directly: `ffmpeg -colorspaces` exits 8, "Unrecognized option
  'colorspaces'." (A first probe of it through a pipe reported exit 0 —
  `head`'s status, not ffmpeg's — plan 13 §1b's exact trap, caught by
  re-probing without a pipe.) The real option is `-colors` (named colours),
  already tracked above as deferred; `-colorspaces` is not implemented under
  either spelling because it is not a real target.
* `vaco-probe`'s own `-h` dispatch is out of scope for this crate's
  Scope-declared area (`crates/app/vaco-cli/` and `crates/app/vaco-cli-core/`
  only). The shared `ffprobe()` option table's `-h` entry was fixed alongside
  `ffmpeg()`'s (same `OPTIONAL_ARG` bug, same fix), but wiring `vaco-probe`'s
  binary to call `vaco_cli_core::help`'s renderers was not done, since that
  binary's `src/` is a different crate.

### Known divergences

| what | why | where |
|---|---|---|
| **`-crf 20` is rejected** | The `AVOption` oracle answers from what this build contains, which is `FormatOptions` and nothing else — there are no encoders to declare `crf`. The reference applies the same rule to itself and gets a different answer because it has encoders. Closes on its own as codecs land. The alternative, accepting every unknown name, makes `-qwrty 3` a silent no-op. | `cli::Oracle` |
| **Non-UTF-8 filenames are refused** | Every layer below takes a `&str`. The reference opens them. Reported rather than papered over with a lossy conversion that would open a *different* file. | `cli::url_of` |
| **Eight listings are deferred** | `-pix_fmts`, `-sample_fmts`, `-layouts`, `-colors`, `-hwaccels`, `-devices`, `-sources`, `-sinks` return `ENOSYS` naming the gap. See [`-h` and the listing commands](#-h-and-the-listing-commands) below for what CL-04 shipped and what it deliberately did not. | `listing::render` |
| **No `av_dump_format` block** | The reference prints `Input #0, matroska,webm, from '…':` and a per-stream summary to stderr. Not reproduced; that is `vaco-probe`-shaped formatting work. | — |

## How to change it

* **Adding an option** starts in `vaco-cli-core`'s table, not here. This crate
  binds what that table already knows about; an option absent from the table is
  invisible to `split`.
* **Changing observable output needs a reference run in the commit.** Every
  wording and every exit code in `src/exit.rs`, `src/select.rs` and `src/exec.rs`
  carries an `OBSERVED` comment naming the invocation it came from. Probe with
  no pipe, or inside `bash -c`.
* **Selection changes go in `select.rs` and nowhere else.** `exec.rs` consumes a
  `Selection` and does not second-guess it.
* **When a muxer lands**, `exec::muxer_for` is the one function to change: it
  already asks `vaco_registry::muxer_by_name` and `muxers_for_extension` first
  and only falls through to the refusals. `nullmux` stays as the test sink.
* **Fixtures** are built with `vaco_demux_matroska::synth`, a dev-dependency. It
  is not a muxer: it writes exactly what it is told, including a per-track
  `FlagDefault`, which is the field the whole auto-selection rule turns on and
  which no real muxer will let a test control.
* **The driver is serial on purpose.** Plan 12's PF-0.x record has five confident
  performance predictions measuring backwards, most recently a threading design
  45–60× slower than serial. A demux-to-sink graph has nothing to overlap.
  Change it with a measurement, not a hunch.

## Configuration

No environment variables and no configuration files. Everything is an option,
and every option is in `vaco_cli_core::table::ffmpeg()`.

Cargo features: none of its own. What the binary can *do* is decided by
`vaco-registry`'s feature set (`demux-matroska`, `demux-mp4`, `demux-mpegts`,
`protocol-http`).

## Dependencies

`vaco-cli-core` (the option table and the specifier grammar), `vaco-registry`
(which components exist), `vaco-format-core` (probing, `Discovery`, the
`Demuxer`/`Muxer` traits), `vaco-sched` (the pipeline), `vaco-io` +
`vaco-protocol-core` + `vaco-protocol-file` (opening a URL), `vaco-codec-core`
(`CodecParameters`), `vaco-packet`, `vaco-opts` (the option schema the oracle
reads), `vaco-limits`, `vaco-core`.

Dev only: `proptest`, `tempfile`, `vaco-demux-matroska`.

## Reported upstream

Findings that belong to other crates and were **not** worked around here.

1. **`vaco-format-core`: `NOTIMESTAMPS` produces a packet `InterleaveQueue`
   rejects.** `interleave::MuxTimestamps::apply` under `FormatFlags::NOTIMESTAMPS`
   sets `pts` and `dts` to `Timestamp::NONE` and returns; `InterleaveQueue::push`
   then fails the same packet with "packet has no dts; interleaving cannot order
   it". Two functions in one module, each correct alone. The reference's `null`
   muxer carries `AVFMT_NOTIMESTAMPS` for exactly this reason, so a faithful
   `null` sink cannot be built until one of the two changes — either the queue
   passes untimestamped packets straight through, or a `NOTIMESTAMPS` muxer
   bypasses interleaving in `vaco-sched`. `nullmux::FLAGS` documents the
   workaround it forced.
2. **`vaco-format-core`: an empty `FormatFlags` means the *strictest* container.**
   `requires_strict_dts()` is `!TS_NONSTRICT`, so `PipelineSpec::add_output`'s
   default of `FormatFlags::empty()` silently opts a caller into strictly
   increasing DTS. Every caller that does not know to pass
   `add_output_with(..., TS_NONSTRICT, ...)` gets the harshest policy by
   accident. Worth inverting, or at least documenting on `add_output`.
3. **`vaco-format-core::time`: DTS is never reconstructed for a reordering
   codec.** R19 fills `dts = pts` only when `!st.reorders && st.delay == 0`,
   which is right; R20 generates PTS from DTS through `push_reorder`; **the
   mirror rule — DTS from PTS through the same buffer — does not exist.** So a
   reordered H.264 track in Matroska reaches the muxer with `dts = None` on
   every packet, even though `params.video.has_b_frames` is `Some(2)` and
   `set_stream_delay` has already been told. The reference's
   `ffprobe -show_entries packet=pts,dts` on the same file prints
   `dts = N/A, N/A, 0, 40, 80, 120, …` against
   `pts = 0, 160, 80, 40, 120, 320, …`, which is exactly `push_reorder` run in
   the other direction. Everything needed is present; only the rule is missing.

4. **`vaco-format-core::time` R22 can produce `dts > pts`.** On a fixture whose
   PTS run is `0, 160, 80, 40, 120` with a codec that does *not* reorder
   (`V_VP8`), R19 sets `dts = pts` and R22 then repairs the non-monotonic result
   by bumping:

   ```text
   pts=0    dts=0
   pts=160  dts=160
   pts=80   dts=161     <- repaired, now greater than its own PTS
   pts=40   dts=162
   pts=120  dts=242
   ```

   `dts > pts` is not a valid packet in any container, and the repair is silent.
   Pinned in this crate's
   `a_reordered_pts_sequence_on_a_non_reordering_codec_is_repaired_not_refused`
   so that fixing R22 surfaces here rather than as a mysterious new CLI failure.
5. **`vaco-io`: `IoContext` has no `into_source`.** A probed open therefore reads
   the URL twice, which is correct for a seekable transport and wrong for a pipe.
   Already reported by `vaco-probe`; repeated because it now blocks `vaco -i
   pipe:0` as well as `vaco-probe`.
6. **`vaco-protocol-file` and `vaco-protocol-core` ship no `vaco-component.toml`**,
   so `vaco_registry::protocol_registry()` is empty and every tool registers
   `file:` by hand. Also already reported by `vaco-probe`.

## Deferred

Named, with the issue that owns each, because a deferred feature that is written
down is a decision and one that is not is a surprise.

| Deferred | Issue |
|---|---|
| `-h`, `-h full`, `-h <kind>=<name>`, byte-identical listings | CL-04 |
| Metadata / disposition / chapter / program mapping | CL-16 |
| `-progress`, `-stats`, `-report` | CL-17 |
| Decoder and encoder nodes, `-frames`, `-pass` | CL-19 |
| Simple filtergraph binding, `-s`/`-aspect`/`-pix_fmt` | CL-20 |
| `-fps_mode`, `-enc_time_base`, `-frame_drop_threshold` | CL-21 |
| `-force_key_frames` | CL-22 |
| `-shortest`, `-apad`, `-isync` | CL-23 |
| The ~600-case timestamp differential matrix | CL-24 |
| `-filter_complex` / `-lavfi`, unlabeled-pad rules | CL-25 |
| `[dec:N]` loopback decoders | CL-26 |
| `-print_graphs*` | CL-27 |
| `-stream_group`, `-reinit_opts`, `-target` | CL-28 |
| Presets, hardware device options, `-sdp_file` | CL-34a |
| Timestamp stages I–III and VI (streamcopy) | CL-15 |
| Sync queue, interleaving, `-shortest` packet mode | CL-13 |
| Differential remux tests (container bytes) | CL-18 — blocked on a muxer |

## Fuzzing

`fuzz/fuzz_targets/cli_run.rs`, header `//! fuzz-crate: vaco-cli`, feature
`cli`.

`cli_argv` already fuzzes the splitter in `vaco-cli-core`; this target covers the
layer above it — binding, selection and spec construction — in two halves,
because a full run never sees a stream (every URL it is given fails to open) and
a direct `select::resolve` call never sees an argv. Beyond totality it asserts
that selection is a *function* (same inputs, same answer), that every pick names
a stream that exists, that `dropped` implies nothing was picked, and that a
failure never exits 0.

```
cargo +nightly fuzz run cli_run --features cli -- -max_total_time=150
exit=0  execs=3652690   find fuzz/artifacts -type f: empty
```
