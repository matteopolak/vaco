# `vaco-cli-core`

Layer 7 (application). Depends on `vaco-core`, `vaco-opts`, `vaco-registry`,
`vaco-textformat`, `thiserror`. Dev: `proptest`.

## What it is

The shared command-line machinery behind all three binaries — `vaco` (ffmpeg),
`vaco-probe` (ffprobe) and `vaco-play` (ffplay). It turns an argument vector
into a validated, scoped option structure and nothing else: it opens no files,
runs no pipeline and instantiates no component.

Four pieces:

| Module | Contents |
|---|---|
| `table` | The option descriptor tables and **the scope model**. Start here. |
| `spec` | The stream-specifier grammar and matcher. |
| `metaspec` | The *metadata* specifier grammar, which shares a syntactic slot with the stream one and is a different language. |
| `map` | The `-map` value grammar. |
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
  `OUTPUT` or both. `tests` in `src/table.rs` enforce both rules.
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
| `thiserror` | the error taxonomy |
| `vaco-opts` | declared for the component-option seam; **not yet used** |
| `vaco-registry` | declared for the listing commands; **not yet used** |
| `vaco-textformat` | declared for help/listing output; **not yet used** |

The last three are the dependency edges plan 14 specifies for the help and
listing systems, which are not implemented yet (roadmap CL-04). They are kept so
the crate graph matches the architecture, at the cost of making this crate's
build depend on the whole workspace compiling.

## What is deliberately not here

* **The help system** (`-h`, `-h long/full`, `-h type=name`) and the listing
  commands (`-formats`, `-codecs`, …). Roadmap CL-04. The table carries
  everything they need — `EXPERT`, `VIDEO`/`AUDIO`/`SUBTITLE`/`DATA`, `argname`,
  `help` — so this is rendering work, not modelling work.
* **Numeric value grammars with expressions.** The reference accepts
  `-b:v 2*1000`, which means every numeric option value goes through the
  expression evaluator. `vaco-expr` is not a declared dependency of this crate,
  so this is deferred rather than half-done. `vaco-core::parse` already covers
  durations, sizes, rates and colours.
* **`-map_metadata`'s two-sided form** (`outfile[,metadata]:infile[,metadata]`).
  The metadata specifier grammar it is built from is implemented; the pairing is
  the consuming binary's.
* **`-loglevel` / `-report` plumbing.** Plan 14 §2.9. Needs a `tracing`
  subscriber, which is the binary's to install.
