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

## Muxers reach the registry now; there are still no encoders

Until the container wave, D5 scoped v0.1 to demuxing: `crates/format/` held
three `vaco-demux-*` crates and no `vaco-mux-*`, `vaco_registry::muxers()` was
empty, and this section described `-f null` as the *only* output a build like
that could produce. That built correctly and then went stale the day the
container wave landed: `exec::muxer_for` kept returning a format name, but
`exec::run_pipeline` still always built the local `NullMuxer` regardless of
what that name was — so `vaco -i in.mp4 -c copy -f matroska out.mkv` exited 0,
printed a plausible stream mapping and summary line, and never created
`out.mkv` at all. Silent success, found by trying the obvious command while a
conformance run was fresh (`planning/CONFORMANCE-FINDINGS.md` #6). There are
still no decoders and no encoders.

The registry now has 63 muxers, and both halves of the CLI reach all of them:

* **`exec::muxer_for` resolves every `-f`/extension through the registry**,
  `vaco_registry::muxer_by_name` and `muxers_for_extension`, uniformly — `null`
  is one registered muxer among the 63 (`vaco_mux_utility::MUXER_NULL`) rather
  than a name this crate special-cases. `-f null -` still works exactly as
  before and is still this crate's own test workhorse; it needs no container
  knowledge and makes every stage observable through packet counts alone.
* **`exec::run_pipeline` opens what `muxer_for` named**, for real:
  `crate::output::create` opens the destination through the same protocol
  registry `input.rs` uses to read (`file:`/`pipe:`, with `-` mapped to
  `pipe:1` — see `output::normalize`'s docs for why that mapping cannot live in
  the generic URL parser), and `(MuxerDesc::open)` turns that into a
  `Box<dyn Muxer>` this crate hands to `PipelineSpec::add_output_with`.
  `vaco-sched` builds a `vaco_format_core::mux::MuxBuilder` over it there and
  consumes that into a `MuxWriter` at `PipelineSpec::build` (gap 8,
  `planning/INTERFACE-GAPS.md`, closed) rather than driving the trait object
  directly — see #8 below and `docs/app/vaco-sched.md`. Streamcopy end to end
  — the one thing a build with no encoders can do — is now enough to
  actually remux.
* **Every output this build cannot write is still refused with a message
  naming the real reason**, and the refusal still distinguishes three cases —
  it is just that the first row now applies to far fewer formats:

  | `-f` / extension | first line | exit |
  |---|---|---|
  | a format this build can **read** but has no registered muxer for | `No muxer for 'x': this build reads that format but cannot write it. D5 scopes v0.1 to demuxing, so \`-f null\` is the only output.` | 8 |
  | a name nothing claims (`-f nosuchformat`) | `Requested output format 'nosuchformat' is not known.` | 234 |
  | no `-f`, unhelpful extension | `Unable to choose an output format for 'out.zzz'; …` | 234 |

  The last two are the reference's own wording and status, byte-identical modulo
  the pointer it prints in its log prefix. The first has no reference behaviour
  to match, because the reference is never built without muxers;
  `AVERROR_MUXER_NOT_FOUND` is the code that names the situation. (The message
  text still names D5 by number; it was true when written and is now only true
  for the formats that remain demux-only — fixing the wording to say so
  precisely, without hardcoding a list that will itself go stale, is a small
  follow-up.)

**There are no encoders**, so an output stream must still carry `-c copy`.
Without one the run takes the reference's *own* path for a build missing an
encoder — `Automatic encoder selection failed Default encoder for format null
(codec none) is probably disabled. Please choose an encoder manually.`, exit 8 —
which is a message the reference already emits for exactly this situation, and
therefore the right one to reproduce rather than invent. (The run-together
"failed Default" is the reference's own missing separator; reproduced under
D17.)

### The two constructions every real output pays for

`MuxerDesc` has no `flags` field the way `DemuxerDesc` does, so whether a
format is `FormatFlags::NOFILE` (`null`, `mkvtimestamp_v2`) is only knowable by
constructing an instance and asking. The reference never opens a real file for
such a format (`-f null out.bin` leaves `out.bin` untouched), so
`exec::open_output` constructs once against a throwaway
`vaco_format_core::vacoraw::MemorySink` to learn the answer, and only reaches
the protocol layer — which for `file:` truncates on open, a visible side
effect — once it knows the destination is real. A `flags` field on `MuxerDesc`
would remove the throwaway construction; see "Reported upstream" below.

### What the acceptance criterion is now

For a real container: the bytes on disk, read back — see
`tests.rs`'s `an_actual_muxer_writes_bytes_a_prober_can_read_back`, which
remuxes a fixture to a real `.mkv` and reopens it through a second,
independent invocation. For `-f null -`, and for anything else not worth a
byte comparison yet, it remains what it always was: the same argv produces

1. the same **stream selection** (`Stream #0:0 -> #0:0 (copy)`),
2. the same **stderr text and exit code**,
3. the same **packet counts through the pipeline** (`nullmux::OutputTally`,
   now populated by `nullmux::TallyingMuxer` wrapping whatever the registry
   returned, real or `null`).

### The muxing-overhead line, measured both ways

`[out#N/fmt] video:…KiB … muxing overhead: X` is `unknown` exactly when
nothing was actually written to compare against (a `NOFILE` container).
Everywhere else it is `100 * (total − payload) / payload`, six decimal digits,
where `total` is bytes actually written — not `stat()` of the finished file.
Measured against `ffmpeg 8.1` remuxing one 10 908-payload-byte file three ways:

| destination | total bytes written | printed |
|---|---|---|
| seekable `.mkv` | 12 168 | `11.551155%` |
| seekable `.mp4` | 12 650 | `15.969930%` |
| `.mkv` over a real, unseekable pipe | 12 038 | `10.359369%` |

The pipe row has no file to `stat`, and the reference still prints a number —
which is why `total` here comes from `output::HighWaterSink`'s high-water mark
on the sink itself (the furthest position any `write` call ever reached, not
`position()` read once at the end: a container that seeks back to patch a
header and does not seek forward again would otherwise under-report). See
`exec::summary_line`'s doc comment for the full derivation.

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
| `output` | opening one output for writing: protocol env, `-` → `pipe:1`, `HighWaterSink` |
| `nullmux` | packet/byte tallying (`Sink`, `OutputTally`, `TallyingMuxer`); the original standalone `null` sink, now redundant — see the module's own doc comment |
| `exec` | muxer resolution, opening the real muxer, the codec check, `PipelineSpec`, the driver |
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

**The eight formerly-`ENOSYS` listings — all now render:**

* `-pix_fmts` renders from `vaco_pixfmt::PixFmt::all()`'s generated table —
  name, component count, average bits-per-pixel and per-component depth, plus
  `H`/`P`/`B` from `PixFmtFlags`. `I`/`O` ("supported for conversion") is
  libswscale's own hand-maintained capability list, which no crate in this
  workspace exposes; it is a small measured exception table in
  `listing::{INPUT_ONLY,OUTPUT_ONLY,NEITHER}`, captured from the reference and
  nothing else (49 of 267 formats are not simply "software implies both").
  **Every one of 267 rendered rows is byte-identical to the reference in
  content** (`diff <(sort ours) <(sort theirs)` is empty) after three named,
  measured corrections for `vaco-pixfmt` data that disagrees with the
  reference — see `write_pix_fmts`'s doc comment in `listing.rs` for exactly
  which three, and `vaco-cli-core`'s doc file's "Reported upstream"-style note
  below for the crate they belong to. **Row order does not match**: this
  build's table is in family/subsampling order, not the reference's
  `AVPixelFormat` enum-assignment history, which would mean hardcoding an
  arbitrary authorial sequence rather than format-dictated data (D7) — the
  same tradeoff `-codecs` already made, for the same reason.
* `-sample_fmts` and `-layouts` are **fully byte-identical**, end to end,
  confirmed against the running binary, not just the render function:
  `vaco_sampfmt::SampleFmt::ALL` and `vaco_chlayout::{Channel::named,
  ChannelLayout::standard}` were already built with the reference's own print
  order in mind (see those crates' doc comments), so this crate only needed
  to add the column layout.
* `-colors` renders from `vaco_core::parse::{color_names,color_by_name}`,
  plus two small tables this listing alone needs: the reference's exact
  CamelCase display spelling (that table is lower-case, because `color()`
  matches case-insensitively) and which 7 of the table's 147 names to
  exclude — the alternate `grey`-family spellings D17 already documents this
  crate accepting as *input* that the reference does not. **Fully
  byte-identical**, confirmed end to end (140 rows).
* `-hwaccels`/`-devices`/`-sources`/`-sinks` have no hardware backend or
  device layer to draw on at all (D13's `vaco-hw-*` crates are a separate,
  later work package), so each renders the real, measured header (and, for
  `-devices`, the real legend) with zero rows under it — an empty list under
  a real header, which is what the reference itself would print given none
  of the corresponding thing registered, not a guess at what a populated one
  would look like. `-sources -sinks` additionally needed a `vaco-cli-core`
  table fix: they carried a `device` argument placeholder for `-h`'s benefit
  but neither `ArgFlags::HAS_ARG` nor `ArgFlags::OPTIONAL_ARG`, so the
  argument was declared but never actually consumed. Fixed alongside this
  work — see `vaco-cli-core`'s doc file.
* **The brief that scoped this work named `-colorspaces` as one of the
  fourteen listing commands to cover. It does not exist in ffmpeg 8.1** —
  measured directly: `ffmpeg -colorspaces` exits 8, "Unrecognized option
  'colorspaces'." (A first probe of it through a pipe reported exit 0 —
  `head`'s status, not ffmpeg's — plan 13 §1b's exact trap, caught by
  re-probing without a pipe.) The real option is `-colors` (named colours),
  shipped above; `-colorspaces` is not implemented under either spelling
  because it is not a real target.
* `vaco-probe`'s own `-h` dispatch is still out of scope for this crate's
  Scope-declared area (`crates/app/vaco-cli/` and `crates/app/vaco-cli-core/`
  only). The shared `ffprobe()` option table's `-h`/`sources`/`sinks` entries
  carry the same fixes as `ffmpeg()`'s, but wiring `vaco-probe`'s binary to
  call `vaco_cli_core::help`'s renderers, or this crate's listing renderers,
  was not done, since that binary's `src/` is a different crate.

**Reported upstream, not fixed here (`vaco-pixfmt`):** comparing every one of
that crate's 268 formats against the reference's 267 found three gaps, all
compensated for display in `listing.rs` rather than fixed at the source: (1)
one extra format, `cuarray`, that ffmpeg 8.1 does not have; (2) `bgr8`'s
component-depth array is in the wrong order (`2-3-3` where the reference and
this crate's own documented logical-channel convention both say `3-3-2`); (3)
the twelve Bayer formats model one raw-sample component where the reference
models three uneven-depth ones, and `xv30be`/`v30xbe` are missing
`PixFmtFlags::BITSTREAM` (their little-endian siblings correctly have neither
implementation mark it). See `listing::write_pix_fmts`'s doc comment for the
exact measurements.

### Known divergences

| what | why | where |
|---|---|---|
| **`-crf 20` is rejected** | The `AVOption` oracle answers from what this build contains, which is `FormatOptions` and nothing else — there are no encoders to declare `crf`. The reference applies the same rule to itself and gets a different answer because it has encoders. Closes on its own as codecs land. The alternative, accepting every unknown name, makes `-qwrty 3` a silent no-op. | `cli::Oracle` |
| **Non-UTF-8 filenames are refused** | Every layer below takes a `&str`. The reference opens them. Reported rather than papered over with a lossy conversion that would open a *different* file. | `cli::url_of` |
| **`-pix_fmts` row order does not match** | This build's table is in family/subsampling order, not the reference's historical `AVPixelFormat` enum order — see above. Every row's *content* is byte-identical; only the sequence differs. | `listing::write_pix_fmts` |
| **`-pix_fmts`: three named `vaco-pixfmt` data gaps, compensated for display** | `cuarray` (extra format), `bgr8` (component order), the twelve Bayer formats (component count/depths) and `xv30be`/`v30xbe` (missing `BITSTREAM`) — see "Reported upstream" above. | `listing::write_pix_fmts` |
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
* **When a muxer lands, nothing here needs to change.** `exec::muxer_for`
  already asks `vaco_registry::muxer_by_name` and `muxers_for_extension` first
  and only falls through to the refusals, and `exec::run_pipeline` opens
  whatever descriptor comes back through `exec::open_output`. A new
  `vaco-mux-*` crate registering itself is the whole of what is needed for
  `vaco -f <its name>` to start writing real files.
* **`-c copy` is still the only encoder path.** `check_codecs` rejects anything
  else; adding a real encoder means adding a case there and to
  `vaco-sched`'s `KindSpec::Encode`, not touching `exec::muxer_for`.
* **Unreachable CLI options, and why**: `-movflags`, any other `-f`-specific
  flag, `-metadata`, chapters and attachments all need somewhere to land that
  does not exist yet. See "Reported upstream" below — both gaps are in
  `vaco-format-core`, not here, and `exec::open_output`'s doc comment is the
  code-level pointer to the first of them.
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
reads), `vaco-limits`, `vaco-core`. `vaco-pixfmt`, `vaco-sampfmt` and
`vaco-chlayout` (CL-04, second wave) back `-pix_fmts`, `-sample_fmts` and
`-layouts` respectively — all three are layer-1 model crates, so this is a
downward dependency like every other one above, not a new kind of edge.

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
7. **RESOLVED (2026-08-23), still worth recording why it took two steps.**
   `MuxerDesc::open` carries no options, so `-movflags`/`-fflags`-shaped
   per-muxer construction options could not reach a muxer through the
   registry. `planning/INTERFACE-GAPS.md` gap 5 substituted
   `Muxer::set_option(&mut self, name, value)` (mirroring
   `vaco_opts::OptionsExt::set_str`'s contract) rather than widening
   `MuxerDesc::open`'s signature, which turned out not to be additive on this
   workspace's ~90 already-declared descriptors. This crate does not yet call
   `set_option` from `exec::open_output` for a private per-muxer option (no
   `-movflags` CLI wiring landed in this wave); it does call
   `Muxer::set_metadata` (see #8) and pass the generic
   `vaco_format_core::FormatOptions` table through
   `PipelineSpec::add_output_with` — the *shared* option surface FW-11 covers.
   A caller-facing `-movflags`/`-id3v2_version`-class per-muxer option is
   still unimplemented in this crate, tracked separately from CL-16/FW-11.
8. **RESOLVED (CL-16, gap 1; ordering RESOLVED 2026-08-23, gap 8).** `Muxer`
   had no metadata channel, so `-metadata`/chapters/attachments had nowhere to
   go even once parsed. `planning/INTERFACE-GAPS.md` gap 1 added
   `vaco_format_core::metadata::MuxMetadata` and `Muxer::set_metadata`.
   `exec::metadata_of` resolves `-metadata`/`-metadata:s:…` against an
   output's own stream list; `exec::resolve_mapped_metadata` finishes it with
   `-map_chapters`/`-map_metadata`'s source input.

   `run_pipeline` used to call `set_metadata` on the freshly opened muxer
   *before* handing it to `PipelineSpec::add_output_with`, because
   `vaco-sched`'s `MuxWork` drove a raw `dyn Muxer` with no way to reach back
   into it afterward — gap 8's ordering face (8a). That is fixed now:
   `run_pipeline` calls `add_output_with` first, gets an `OutputRef` back, and
   calls `vaco_sched::spec::PipelineSpec::set_output_metadata(oref, meta)`,
   which attaches the metadata to the `MuxBuilder` `add_output_with` built.
   `PipelineSpec::build` delivers it to `Muxer::set_metadata` at
   `MuxBuilder::open` — after `PipelineSpec::map` has declared every stream
   and settled their time bases, but before the header, which is the
   ordering `Muxer::set_metadata`'s own doc comment always described.
   `vaco-mux-mp4`/`vaco-mux-matroska`'s lazy per-stream resolution (deferred
   to `write_header` time specifically so it survives *either* ordering) is
   no longer load-bearing for this call path, but neither crate needed to
   change: their `set_metadata` overrides just store the `MuxMetadata` and
   resolve later regardless of when they were called, so correcting the
   caller's ordering does not affect them either way, and their own
   `set_metadata_before_add_stream_still_resolves_*` regression tests stay
   green unmodified.

   **Not resolved**, and reported fresh rather than worked around:
   `-disposition` and `-program` parse (`vaco-cli-core`'s option tables
   already declare them) but have no channel to write through —
   `Muxer::add_stream` takes only `CodecParameters`, which carries neither a
   disposition bit nor a program membership, and `MuxMetadata` (gap 1's fix)
   does not cover either. Closing this needs the same shape of addition gap 1
   was, scoped to disposition/program specifically.
9. **`MuxerDesc` has no `flags` field**, unlike `DemuxerDesc`, which explicitly
   carries one "so a caller composing `Discovery` can reach them through the
   registry" (that crate's own doc comment). The same reasoning applies on the
   mux side and does not have a field to land in: `exec::open_output` has to
   construct a muxer against a throwaway sink just to read `.flags()` and learn
   whether the format is `NOFILE`, before it can decide whether touching the
   filesystem is even correct. A `flags: FormatFlags` field, populated the same
   way `DemuxerDesc::flags` is, would remove that throwaway construction
   entirely.
10. **RESOLVED (2026-08-23, gap 8's remaining two faces).** `PipelineSpec::map`
    used to call `Muxer::add_stream` directly on the raw trait object, which
    meant `MuxBuilder::add_stream`'s `query_codec` check (M15) — implemented,
    tested — was never asked from this crate's own call path, and the
    bitstream-filter stage (M6, `BsfChain`/`BsfProvider`) never ran either.
    `run_pipeline` needed no change for either: it already called
    `spec.map(tap, oref, &p)`, and `map` now routes through `MuxBuilder`
    internally. Two things worth knowing before assuming this closes
    `planning/CONFORMANCE-FINDINGS.md` finding 19's six known-incompatible
    remux pairs:
    - **`query_codec` is a coarser question than those six pairs need.**
      `CodecSupport` answers "can this container ever hold this codec", and
      H.264-in-AVI/MPEG-TS/FLV and AAC-in-AVI are all `Supported` in general —
      the six pairs fail on narrower *stream-content* constraints (a packet
      with no PTS at all; ADTS-framed AAC where the container needs raw
      `AudioSpecificConfig` framing) that `vaco-mux-avi`'s and
      `vaco-mux-mpegts`'s own `add_stream` already checked, reachable through
      the old direct call just as much as the new `MuxBuilder` one. Measured
      directly (`vaco -i av-src.ts -c copy -f avi`, an MPEG-TS→AVI ADTS-AAC
      pair): refused before and after this change, same message, because the
      refusal was never gated on `query_codec` in the first place.
    - **M6 running is necessary but not sufficient.** Neither `vaco-mux-avi`
      nor `vaco-mux-mpegts` calls `Muxer::check_bitstream` to ask for a
      filter — both apply their own inline length-prefix-to-Annex-B
      conversion instead — so M6 now executing on every packet does not yet
      change what either crate produces. See `docs/app/vaco-sched.md`'s note
      on `PipelineSpec::set_output_bsfs` for what is still missing (a
      `vaco-bsf-*` crate and a `BsfProvider`, neither of which exists here
      yet) before that stage does anything for a real file.

## Deferred

Named, with the issue that owns each, because a deferred feature that is written
down is a decision and one that is not is a surprise.

| Deferred | Issue |
|---|---|
| `-h <kind>=<name>` private-options blocks for `mp4`/`mpegts` (blocked on an options-schema hook in other crates); `vaco-probe`'s own `-h`/listing dispatch (a different crate) | CL-04 |
| `-metadata`/`-map_metadata`/`-map_chapters` implemented; `-disposition`/`-program` parse but have no `Muxer` channel to write through (see "Reported upstream" #8) | CL-16 |
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
| Differential remux tests (container bytes) | CL-18 — no longer blocked on a muxer (one landed and this crate reaches it), but the full byte-identity matrix (XF-03, #211) this issue depends on is still open. This crate's own `an_actual_muxer_writes_bytes_a_prober_can_read_back` is a single round-trip check, not that matrix. |

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

CL-04's second wave (the eight formerly-`ENOSYS` listings) extended `TOKENS`
with `-pix_fmts`/`-sample_fmts`/`-layouts`/`-colors`/`-hwaccels`/`-devices`/
`-sources`/`-sinks` and two real reference device names (`lavfi`,
`avfoundation`) this build's empty device registry never matches — exercising
`-sources`/`-sinks`' new `OPTIONAL_ARG` shape (consumes the next argv entry
unconditionally, including one that looks like another option) alongside
`-h`'s existing coverage of the same mechanism.

```
cargo +nightly fuzz run cli_run --features cli -- -max_total_time=150
exit=0  execs=2333991   find fuzz/artifacts -type f: empty
```
