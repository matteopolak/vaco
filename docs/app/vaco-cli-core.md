# `vaco-cli-core`

Layer 7 (application). Depends on `vaco-core`, `vaco-expr`, `vaco-opts`,
`vaco-registry`, `vaco-textformat`, `thiserror`. Dev: `proptest`.

## What it is

The shared command-line machinery behind all three binaries — `vaco` (ffmpeg),
`vaco-probe` (ffprobe) and `vaco-play` (ffplay). It turns an argument vector
into a validated, scoped option structure and nothing else: it opens no files,
runs no pipeline and instantiates no component.

Four pieces:

| Module | Contents |
|---|---|
| `table` | The option descriptor tables and **the scope model**. Start here. |
| `help` | CL-04: `-h`'s topic grammar, the command-line-option renderer and the `AVOptions`-block renderer. See its own section below. |
| `spec` | The stream-specifier grammar and matcher. |
| `metaspec` | The *metadata* specifier grammar, which shares a syntactic slot with the stream one and is a different language. |
| `map` | The `-map` value grammar. |
| `value` | Which grammar each option's *value* is written in, and the two numeric parsers. |
| `lex` / `split` | Tokenising one argv entry, and the grouping pass. |
| `num` | The `strtol` scanner every numeric field in the grammar is built on. |
| `stream` | The facts a specifier is matched against, plus the 19 disposition flags. |

### Why not `clap`

The reference's command line is not a flag set. It is a positional, stateful
stream of option groups over an option universe that is not known until
components are chosen. `-b:v 1M` before `-i` and after `-i` are different
options on different objects; `-c:v:0` embeds a sub-language in the *name*
token; `-crf 20` is valid only because some encoder happens to declare `crf`;
`-nostats` negates a boolean while `-noqwerty` is an error; `-/filter:v f.txt`
reads the value out of a file. Each is survivable in a general parser. Together
they are a different machine, so it is written out here.

## How it works

### The scope model

This is the design decision everything else rests on.

| Scope | Flag | Binds to |
|---|---|---|
| Global | `GLOBAL` | The whole run. Position is irrelevant. |
| Per-file | `PER_FILE` | The **next** file mentioned. |
| Per-stream | `PER_FILE｜PER_STREAM` | A per-file option carrying a stream specifier, so one file holds several values. |

Per-file options additionally carry `INPUT` and/or `OUTPUT`, deciding which kind
of file they may bind to. Three consequences:

* **Scope is checked after grouping, never during.** A misplaced per-file option
  is a *file-opening* failure, not a *splitting* failure. The two phases print
  different follow-up lines and exit with different statuses (8 vs 234) — see
  `error::Phase`.
* **Per-stream is not a fourth scope.** It is a per-file option whose value is a
  list of (specifier, value) pairs, resolved once the file's streams are known.
  **The last matching occurrence wins**, regardless of how specific it is:
  `-c:a:1 flac -c:a copy` gives stream `a:1` `copy`.
* **Trailing per-file options are dropped, silently.** `ffmpeg -i a -f null -
  -c:v libx264` exits 0 and ignores the `-c:v`. We keep them in
  `CommandLine::orphaned` so a caller *may* warn, but nothing here treats them
  as an error, so the acceptance set is unchanged.

The grouping pass itself:

```text
pending = []
for each argv entry:
    option?  -> global           : hoist, position irrelevant
             -> opens an input   : close a group with `pending`, kind = Input
             -> otherwise        : push onto `pending`
    bare?    -> close a group with `pending`, kind = the tool's positional kind
leftover `pending` is DISCARDED
```

`Positional::OutputFile` for `vaco`, `Positional::InputFile` for `vaco-probe`.

### The stream-specifier grammar

The manual describes a colon-separated list of components. That is close but not
right, and the difference decides which command lines work. What the reference
runs is a single-pass token loop over a fixed set of fields, with three
properties no published EBNF captures:

1. **Four tokens are terminal.** After an index (`0`), a stream id (`#1`, `i:1`),
   a metadata match (`m:k:v`) or `u`, nothing may follow — not even a colon.
   `v:0:u` is rejected; `v:u` is fine.
2. **The colon is a separator, not a requirement.** After a non-terminal token
   the loop eats one colon if present and continues either way, so `p:1v` ≡
   `p:1:v` and `g:0u` ≡ `g:0:u`.
3. **Two tokens carry lookahead constraints, and they are different ones.** A
   media-type letter is only one when the next byte is *not alphanumeric*
   (`v-` parses the `v` then trips on `-`; `vu` fails whole). `u` is only `u`
   when the next byte is a colon or the end (`u_` fails whole).

Because the index token is terminal it is always last, so **matching is simple
even though parsing is not**: every other token is a conjunctive predicate whose
order cannot matter. Filter the streams in container order, then take the n-th
survivor. There is no ordered-narrowing subtlety to get wrong.

Numeric fields use C `strtol` with base 0 — octal `010` is 8, hex `0x10` is 16,
`0b1` stops at the `b`, and overflow saturates silently. The *index* form
additionally requires a leading digit, which the `p:`/`g:`/`i:`/`#` forms do
not: `-c:v:+0` is rejected while `-c:p:+1` is accepted. That asymmetry is real
and is reproduced.

### Option values: two numeric grammars, not one

Plan 14 §2.5 states that "every numeric option value goes through the expression
evaluator", citing `-b:v 2*1000`. **Probing shows the opposite**, and the
difference decides which command lines work.

Of the 128 argument-taking options in the `vaco` table, **41 take a plain
number and reject an expression outright**:

```text
$ ffmpeg -i in.mkv -ac 1*2 -f null -
Expected number for ac but found: 1*2
```

Exactly **11 reach the evaluator**, and none of them for the reason plan 14
gives. The three routes:

| Route | Options |
|---|---|
| implemented as an `AVOption` rather than a table option | `cpucount`, `cpuflags`, `abort_on`, `profile`, `discard`, `disposition`, `apply_cropping` |
| the ratio grammar, which is expression-backed | `aspect`, `time_base` (and `r`, via the rate grammar) |
| a codec option reached by name | `b`, `ab`, and every component option — `-crf`, `-qp`, … |

Plan 14's own example sits in the third row: `-b:v` is an `AVOption` on the
codec. That is exactly why it evaluates while `-ac` does not, and it is why the
rule is "AVOptions evaluate" rather than "numbers evaluate".

The remaining 76 are 41 strings, 5 durations, 1 rate, 1 `-map`, and 28 bespoke
grammars the consuming binary owns (log levels, filter graphs, `key=value`
metadata, hardware device specifications, `-target` presets).

**Two dialects, one language.** On the `AVOption` path — which is the CLI's
expression path — `default`, `max` and `min` are **constants naming the
option's own metadata**, and they shadow the builtin functions of the same
name:

```text
$ ffmpeg … -crf max(1,2) …
[Eval] Invalid chars '(1,2)' at the end of expression 'max(1,2)'
$ ffmpeg … -crf min-1 …
Value -2.000000 for parameter 'crf' out of range [-1 - 3.40282e+38]
```

The second line is the proof that the constants are real: crf's minimum is -1,
so `min-1` is -2. Every other builtin still resolves (`abs`, `gcd`, `hypot`,
`if`, `st`/`ld`, `while`, `root`, `taylor` all verified), and the same shadowing
appears on `-cpucount`, so it is the option system's binding rather than one
codec's. The filtergraph path is *not* like this — there `max` and `min` are the
ordinary builtins.

`value::OptionConstants` models the binding. Where the option's schema is not in
hand, `OptionConstants::UNKNOWN` still binds the three names — which is what
decides acceptance — and evaluates them to NaN. Closing that needs the
component schema from `vaco-opts`.

**The plain-number grammar is `av_strtod`**, which is not C's `strtod`: it adds
the SI prefixes (`2k`, `2h`, `2E`), the `i` binary modifier (`2ki` = 2048), the
`B` times-eight suffix (`2kB` = 16000) and the `dB` suffix, and its hexadecimal
is integer-only. That grammar already exists as the expression language's number
lexer, so `value::strtod` calls `vaco_expr::scan_number` rather than growing a
second copy that would drift.

Three checks run, in this order, and the order is observable:

1. whole-string parse → `Expected number for {name} but found: {value}`
2. range → `The value for {name} was {value} which is not within {min} - {max}`
3. integrality, for integer fields only → `Expected int64 for {name} but found {value}`

`-fs 1e30` stops at (2); `-fs 20dB` passes (2) and stops at (3), because 20 dB is
9.999999999999998. Note the colon after "found:" in the first message and its
absence in the third — the asymmetry is the reference's.

## The help system (`help`)

CL-04. Two renderers, because `ffmpeg -h full` uses two: the command-line
options section has no flag column, every `AVOptions` block has an
eleven-column one. Conflating them is the easiest way to get this wrong.

* [`help::parse_topic`] turns `-h`'s raw argument into a [`help::Topic`]:
  `None`/empty is bare `-h`, `"long"`/`"full"` are the two depths, anything
  with `=` is `kind=name`, a bare word from the seven recognised kinds
  (`decoder`/`encoder`/`demuxer`/`muxer`/`filter`/`bsf`/`protocol`) is that
  kind with no name, and everything else is unrecognised. `vaco-cli` owns
  turning a `Topic::Kind` into an actual lookup — this crate has no registry.
* [`help::render_options_help`] is the command-line half: [`table::OptTable`]
  grouped into sections (D9: option *names* are reproduced, section headings
  and help prose are ours), formatted with a `max(18, len) + 2` name field.
* [`help::render_schema_block`] is the `AVOptions` half: one call renders a
  whole `vaco_opts::Schema` — the class-name header, every option at
  `max(18, len) + 1` name / `max(12, len) + 1` type, the eleven-column flag
  field verbatim, then its named constants at `max(15, len) + 1` /
  `max(12, len) + 1`, inheriting the *owning option's* flag column (every
  measured example does; `ConstDesc::flags` itself defaults empty).
* [`help::ends_in_options_block`] answers the one thing the blank-line rule
  needs: did the body's last line belong to an option or const row. `vaco-cli`
  uses it to choose one blank line before the `Exiting with exit code 0`
  trailer, or two.

### Measured, not recalled (D17, plan 13 §1b)

Every number below is `ffmpeg 8.1`/`ffprobe 8.1` under `LC_ALL=C`, no pipe,
`homebrew`/arm64. Re-run these if the pinned reference version moves:

```sh
LC_ALL=C ffmpeg -h            >h.out;      LC_ALL=C ffmpeg -h long >hl.out
LC_ALL=C ffmpeg -h full       >hf.out
LC_ALL=C ffmpeg -h protocol=file      >hpf.out
LC_ALL=C ffmpeg -h demuxer=matroska   >hdm.out
LC_ALL=C ffmpeg -h demuxer=mp4        >hdmp4.out   # has private options; ours does not (see vaco-cli.md)
LC_ALL=C ffmpeg -h muxer=matroska     >hmm.out      # has private options and named constants
LC_ALL=C ffmpeg -h decoder=nonesuchxyz -h encoder=nonesuchxyz -h filter=x \
                 -h bsf=x -h protocol=x -h demuxer -h muxer -h decoder \
                 -h encoder -h filter -h bsf -h protocol   # the "no name"/"unknown name" matrix
```

* **`-h` always consumes the next argv entry if one exists, whatever it looks
  like**, and does not error when none does. `ffmpeg -h -i x` reports `Unknown
  help option '-i'.` — `-i` was consumed as the topic, `x` is never looked at.
  [`table::ArgFlags::OPTIONAL_ARG`] is that shape; `split::split_with` treats
  a missing value as `None` rather than [`error::CliError::MissingArgument`]
  only for options carrying it.
* **`-sources`/`-sinks` carry the same `OPTIONAL_ARG` shape as `-h`, measured
  the same way** (CL-04, second wave): `ffmpeg -hide_banner -sources` with no
  device name prints "Device name is not provided." rather than a
  missing-argument error, and `ffmpeg -hide_banner -sources -i x` silently
  consumes `-i` as the device name (no device named `-i` exists, so the whole
  invocation prints nothing and exits 0 — `x` is never reached). Both tables
  declared a `device` argument placeholder for `-h`'s benefit from the start
  but neither `HAS_ARG` nor `OPTIONAL_ARG`, so the value was never actually
  consumed until this fix — a real bug, not a documentation gap: without it,
  `-sources lavfi` treated `lavfi` as an unrelated positional token rather
  than this option's own value.
* **Name field: `max(18, len) + 1`, not `+ 8`.** Confirmed against `-h
  protocol=file`'s five options (name lengths 6–10, one field width
  throughout — not distinguishing enough on its own) and cross-checked against
  all ~14,000 lines of `-h full`, where the field grows past the minimum at
  exactly `len + 1` for every name from 2 to 32 characters, no exception.
  Type field: same rule, minimum 12.
* **Only `-h`/`-version`/`-buildconf` print `Exiting with exit code 0` on
  *stdout*, unconditionally — even at `-loglevel quiet`.** None of the other
  thirteen listing commands do (confirmed across all of them). It is not the
  ordinary log stream: `-muxers -loglevel debug` prints the same line, but to
  *stderr*, as the shared debug-level exit trace every invocation gets: the
  `-h` family's copy is unconditional and lands on stdout regardless.
* **Blank lines before the trailer: one normally, two when the body's last
  block was an `AVOptions`/consts block.** True for the success paths
  (`-h full`, `-h protocol=file`, `-h muxer=matroska`) and for the "not
  found" one-liners (`Codec 'x' is not recognized…`, `Unknown format 'x'.`) —
  which get exactly one, the same as any other non-`AVOptions` body, not zero.
* **`-h decoder`/`-h demuxer`/… (bare, no `=`) is a different "no name" case
  from `-h decoder=`/`-h demuxer=` (an explicit empty name).** The first
  short-circuits before any lookup (`No codec name specified.`, `Unknown
  format '(null)'.` — the reference's own C `NULL` literal, printed by `%s`);
  the second runs the lookup with an empty string and gets the ordinary
  "not found" message (`Codec '' is not recognized…`, `Unknown format ''.`).
  `demuxer`/`muxer` share one message shape because both are
  `AVFormatContext` lookups in the reference.
* **An unrecognised `kind` reports only the kind text.** `-h zzzz=x` says
  `Unknown help option 'zzzz'.`, not `'zzzz=x'.` — [`help::parse_topic`]
  always splits on `=` first and leaves kind-validity to the caller.
* **A found `protocol` prints no header at all** — straight into `<name>
  AVOptions:` (or nothing, if the protocol declares no schema). Every other
  found kind (`Demuxer`/`Muxer`/`Decoder`/`Encoder`/`Filter`/`Bit stream
  filter`) prints `"{Kind} {name} [{long_name}]:\n"` first.

### What this build cannot show, and why — not this crate's gap

This build has zero muxers, zero decoders, zero encoders and zero filters
(D5), so most of `-h <kind>=<name>`'s "found" branches are exercised only by
`demuxer=<one of our three>` and (once `--features protocol-http` is on)
`protocol=http`/`protocol=https`. Two real gaps sit in other crates, not here,
and are recorded in `docs/app/vaco-cli.md`'s "Reported upstream" rather than
worked around:

* `vaco_format_core::DemuxerDesc`/`MuxerDesc` carry no options-schema hook —
  unlike `vaco_protocol_core::ProtocolDesc::options` — so even a demuxer with
  private options (none of ours have any yet) could not show them.
* `vaco_codec_core::DecoderDesc` and `vaco_filter_core::FilterDesc` carry
  none either, and `vaco-cli`'s `codec_kind`/`filter` "found" branches are
  therefore unreachable *and* unmeasured — implemented as real lookups so a
  landing decoder lights the path up rather than staying silently wrong, but
  their header-only shape is a best guess, not something checked against a
  real row.

## Method: how the grammar was established

Per D7/D15 no FFmpeg source was read. The command line is an *interface*, so it
was derived by black-box probing of `ffmpeg 8.1` / `ffprobe 8.1` (Homebrew,
arm64 macOS; `libavformat 62.12.100`) and from `ffmpeg -h full`.

**Sample file.** Three streams — video, audio, audio:

```sh
ffmpeg -f lavfi -i testsrc=size=64x48:rate=10:duration=1 \
       -f lavfi -i sine=frequency=440:duration=1 \
       -f lavfi -i sine=frequency=880:duration=1 \
       -map 0:v -map 1:a -map 2:a -c:v mpeg4 -c:a aac t.mkv
```

**Acceptance.** `ffmpeg -nostdin -loglevel error -i t.mkv -c:"$SPEC" copy -f
null -` for each candidate, recording exit status and the first stable stderr
line. 1669 specifier candidates were generated as the cross product of the
token alphabet plus hand-written edge cases; 114 `-map` candidates likewise. The
whole transcript is committed as `tests/reference.rs` and replayed offline by
`tests/conformance.rs`, so the contract is checked on every `cargo test` without
the reference binary present.

**Selection.** What a specifier *selects* was read back with
`-metadata:s:"$SPEC" probe=HIT` followed by `ffprobe -show_entries
stream_tags=probe`, which reveals exactly which streams an option reached.

**Option table.** Names, argument placeholders and specifier kinds were
extracted mechanically from the section headers of `ffmpeg -h full` /
`ffprobe -h full`. Two properties the help output does *not* state reliably were
then probed per option:

* **Takes an argument?** Run the option alone and look for
  `Missing argument for option 'X'.` The help printer omits the `<argname>`
  placeholder for several options that do take a value (`-thread_queue_size`,
  `-shortest_buf_duration`), so the help text cannot be trusted for this.
* **Input, output, or both?** Run it before and after `-i` and look for
  `cannot be applied to {input,output} url`. All 166 non-exiting options were
  classified this way; the result agrees with the help section headers wherever
  those state a side, which is the cross-check that the method works.

**Value grammars.** Every argument-taking option was fed a junk value and
classified by *which parser complained*, since each grammar has its own message:
`Expected number for …` (plain number), `Invalid duration for option …`
(duration), `Undefined constant or missing '('` (expression), `Invalid
framerate value` (rate). Integer and floating-point options were then separated
by feeding `0.5` and seeing whether the integrality check fired. This is
measurement, not inference from the argument placeholder — which would have got
`-b` and `-ac` the same and both wrong.

**The expression path was confirmed filtergraph-free** (plan 13 §1b). `-crf`
goes from `argv` through `av_opt_set` straight to the evaluator, and its range
check echoes the value, so it settles the whitespace and associativity
questions that a `-vf` probe could not:

```text
-crf -2^2          rejected: the value is -4, so the sign follows the whole chain
-crf max(1,0/0)    rejected: NaN, so `max` is a comparison select
-crf ---1          rejected: "Undefined constant … in '--1'"
-crf 1 2           accepted: 12, so whitespace is deleted rather than skipped
-crf 0-20dB        accepted: 0.1, so the sign belongs to the decibel literal
```

Each is a `vaco-expr` D17 behaviour, re-confirmed here on the CLI's own path.

**Numeric bases.** Probed by reading the index back out of the reference's own
diagnostics — e.g. `-metadata:c:010` reports `Invalid chapter index 8`, which
settles that the chapter index is base 0 rather than base 10.

## Reference deviations reproduced deliberately (D17)

Each is annotated at its site with a `// D17:` comment.

| Behaviour | What a sane grammar would do | What the reference does |
|---|---|---|
| A specifier on an option that has none | reject `-y:vv`, `-t:zzz 1` | accept and never look at the suffix |
| `-metadata:gg`, `-metadata:g:0` | reject the tail | `g` matches on its first byte; the tail is unread |
| `-metadata:c:x` | reject | `strtol` never fails, so it is chapter 0 |
| `-map [v` | require the closing bracket | never checks for one |
| `Cannot combine multiple program/group designators…` | end with a newline | emits none, so the next log line is glued on |
| `Parsed 'usable only'` | not print at all | prints it at `AV_LOG_ERROR` **on the success path**, for every specifier containing `u` |
| `Stream map '' matches no streams.` | name the map | interpolates an empty string (ffmpeg 8.1) |
| `-crf max(1,2)` | call the builtin | parse error: `max` is a constant on this path and shadows the function |
| the `int64` out-of-range message | print `INT64_MAX` | prints `9223372036854775808.000000`, one too high, because the bound goes through a `double` before `%f` |
| `-ac ""` | reject | accepted as zero, because C sets `endptr = nptr` on failure and the tail is then empty — while `-ac " "` *is* rejected |

The last two are the binary's to reproduce, not this crate's — this crate does
not log. `StreamSpecifier::usable` is the trigger for the first;
`MapSpec` for the second.

One place the *documentation* is wrong rather than the binary: `-h full` still
advertises `-map <[-]input_file_id[:stream_specifier][,sync_file_id[…]]>`, but
ffmpeg 8.1 rejects every comma. We follow the binary.

## Non-UTF-8 arguments

Real command lines carry filenames that are not valid UTF-8, and the reference —
being C — never notices. Our position:

* **File paths and option values stay `OsString` and are never transcoded.** They
  round-trip byte for byte to the platform API, which is the only property that
  matters for them.
* **Option names and specifiers must be UTF-8.** They are matched against a
  static table of ASCII names and an ASCII grammar, so a non-UTF-8 name is
  unrecognised by construction. We say so at the boundary
  (`CliError::NonUtf8OptionName`) rather than carrying bytes into a lookup that
  must fail.

The only observable difference is the *rendering* of such a name inside the
diagnostic: the reference writes the raw bytes, we write `U+FFFD`.
`CliError::raw_operand` hands the original `OsString` back, so a caller that
wants byte-identical stderr writes those bytes itself.

Byte inspection uses `OsStr::as_encoded_bytes`, which is safe and stable.
Nothing reconstructs an `OsStr` from bytes — that needs `unsafe`, which D2
forbids.

**D16** applies to whatever a caller does with a group's URL: `fd:` is not
implemented and `pipe:` supports only 0, 1 and 2, because `FromRawFd` is
`unsafe`. Nothing in this crate interprets a URL, so the restriction does not
bite here, but a consumer must honour it.

## How to change it

* **A new option** is a row in `src/tables/ffmpeg.rs` or `src/tables/ffprobe.rs`.
  Give it exactly one of `GLOBAL` / `PER_FILE`; a `PER_FILE` row needs `INPUT`,
  `OUTPUT` or both; and give it a `ValueKind` **established by probing**, not by
  reading the argument placeholder. `tests` in `src/table.rs` enforce the first
  two rules and that the kind agrees with `HAS_ARG`.
* **Do not add a second number parser.** `value::strtod` is `av_strtod` and is
  built on `vaco_expr::scan_number`, so the CLI grammar and the expression
  language's literals cannot drift apart. They are the same function in the
  reference too.
* **The grammar** lives in one function, `StreamSpecifier::token`. Adding a token
  means adding a branch and deciding whether it is terminal. Add the new
  spellings to `tests/reference.rs` *by probing the reference*, never by
  reasoning — the whole value of that file is that it was observed.
* **Help strings are ours, not the reference's** (D9: option names are interface
  facts, help text is not). Never paste one in. The consequence — that `-h`
  cannot be byte-identical — is accepted project-wide, not a bug to fix.
* **Do not "correct" a `// D17:` site.** Each one marks a place where the
  reference is wrong and we match it anyway, because a user migrating a script
  cares that behaviour matches, not that we are more faithful to a manual.
* `indexing_slicing`, `unwrap_used`, `expect_used` and `panic` are denied
  workspace-wide, and argv is untrusted (D6). Every scan here advances with
  `strip_prefix` / `get(..)`; keep it that way.

### Regenerating the transcript

`tests/reference.rs` is generated by observation. To extend it, run the probe
commands in the Method section above against a reference binary and append rows;
record the binary's version in the file's header if it changes. A row is a fact
about a shipped binary, so a row that disagrees with the implementation means
the implementation is wrong.

## Configuration

None. No environment variables, no feature flags, no files read at build time.
`OptTable` is chosen by the calling binary — `table::ffmpeg()` or
`table::ffprobe()` — and `split_with` takes an `AvOptionOracle` so the caller
decides when an unknown option name becomes an error.

## Dependencies

| Crate | Used for |
|---|---|
| `vaco-core` | `MediaType`, `Dict` (case-insensitive tag lookup) |
| `vaco-expr` | the expression language, and its number lexer — which is also the plain-number grammar |
| `thiserror` | the error taxonomy |
| `vaco-opts` | declared for the component-option seam; **not yet used** |
| `vaco-registry` | declared for the listing commands; **not yet used** |
| `vaco-textformat` | declared for help/listing output; **not yet used** |

The last three are the dependency edges plan 14 specifies for the help and
listing systems, which are not implemented yet (roadmap CL-04). They are kept so
the crate graph matches the architecture, at the cost of making this crate's
build depend on the whole workspace compiling.

## What is deliberately not here

* **This section used to say the help system and the listing commands were
  not here at all.** That was true when the table alone existed; it is now
  stale. `help::render_options_help`/`render_schema_block` (this crate) and
  `vaco-cli`'s `help.rs`/`listing.rs` (thirteen listings byte-identical or
  structurally correct, plus `-h`/`-h long`/`-h full`/`-h <kind>=<name>`) are
  CL-04's actual shape now — see [The help system](#the-help-system-help)
  above and `docs/app/vaco-cli.md`'s own listing section for what shipped and
  what remains: the private-options-block hook on four descriptor types, and
  `vaco-probe`'s own dispatch to these renderers.
* **The ratio and rate parsers.** `ValueKind::Rate` and the ratio-valued
  options (`-aspect`, `-time_base`) are *classified* here but parsed in
  `vaco-core::parse`, which is where the size, colour and duration grammars
  already live. **That parser is currently not expression-backed and needs to
  be**: the reference's `av_parse_ratio` evaluates the whole string as one
  expression and then approximates, so `-aspect 2*3/4` is 1.5 (rendered 3:2)
  and `-r 5*5` is 25/1 — both verified. `vaco_core::parse::rational` splits on
  `/` and calls `str::parse`, so it rejects all three. Raised for the
  `vaco-core` owner rather than worked around here; adding a second ratio
  parser in this crate is exactly the drift the shared number grammar avoids.

* **The bespoke value grammars** — log levels, `-target` presets, hardware
  device specifications, `-program`/`-stream_group` specifications. Each is
  `ValueKind::Custom`: one parser and one message per option, owned by the
  binary that uses it. Durations, sizes, rates and colours already live in
  `vaco-core::parse`.
* **`-map_metadata`'s two-sided form** (`outfile[,metadata]:infile[,metadata]`).
  The metadata specifier grammar it is built from is implemented; the pairing is
  the consuming binary's.
* **`-loglevel` / `-report` plumbing.** Plan 14 §2.9. Needs a `tracing`
  subscriber, which is the binary's to install.
