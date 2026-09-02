# `vaco-textformat`

## What it is

The seven ffprobe output writers — `default`, `compact`, `csv`, `flat`, `ini`,
`json`, `xml` — and the section schema they render. It is the v0.1 acceptance
surface: D5 defines v0.1 as byte-identical `ffprobe` output, D6 makes that a
hard requirement, and every byte of it is decided here.

`csv` is not an eighth writer. It is `compact` with four different defaults.

Two more writers, `mermaid`/`mermaidhtml`, exist for `ffmpeg`'s own
`-print_graphs_format` (CL-27, `vaco-cli`'s `print_graphs.rs`) rather than for
`ffprobe`'s acceptance surface — see "The `GRAPH`/`FILTER` sections" and
`writers::GRAPH_ONLY_NAMES` below for why they are named separately from
`writers::NAMES`.

## How it works

### The section model

`sections.rs` holds the 65 sections of `ffprobe -sections`, transcribed from the
reference binary, plus 12 more (`GRAPHS`/`GRAPH`/`GRAPH_INPUTS`/`GRAPH_INPUT`/
`GRAPH_OUTPUTS`/`GRAPH_OUTPUT`/`FILTERS`/`FILTER`/`FILTER_INPUTS`/
`FILTER_INPUT`/`FILTER_OUTPUTS`/`FILTER_OUTPUT`) that are not: `ffprobe` has no
`-print_graphs` at all, and `ffmpeg` (which does) has no `-sections` to dump.
Those 12 are transcribed instead from `ffmpeg 9.0.1 -print_graphs
-print_graphs_format default|json` on a real `-filter_complex` run, observed
directly (D6) — see the doc comment directly above `GRAPHS` in `sections.rs`
for the exact invocation and the one field-set reduction it documents (no
per-link negotiated format fields, since `vaco-filter-graph::BuiltGraph`
exposes no resolved link format at this layer). Each row carries a local name
(`stream`, `tags`, `side_data`), a globally unique name (`program_stream`,
`stream_tags`, `packet_side_data`), the four `-sections` flags, and its
children.

Everything **renders** by local name; only `-show_entries` uses the unique name.
A program's stream is `[STREAM]`, `stream|…`, `streams.stream.0.…` and
`<stream>` — never `program_stream`.

Two columns are not in the `-sections` dump and were filled in by observation:

* `element_name` — what the `xml` writer calls each key/value child of a
  variable-field section, and what `compact` uses as its inline prefix. `tags` →
  `tag`, `side_data` → `side_datum`.
* `default_style` — whether the `default` writer gives a section its own
  `[HEADER]` block or flattens it into the parent as `PREFIX:key=value`. The
  observed rule, asserted by a unit test: **a section gets a header iff its
  parent is the root or an array.** `tags` and `disposition` hang off `stream`
  and inline; `side_data` hangs off the `side_data_list` array and gets a block.

`compact` differs from `default` on exactly one point: it inlines every
variable-field section, so packet side data reads
`side_datum/skip_samples:skip_samples=1024` on the packet's own line where
`default` opens a `[SIDE_DATA]` block.

### Driving it

`TextFormat` is the façade. `open` / `open_typed` / `close` move a cursor over
the schema; `int` / `str` / `tag` and the domain helpers (`time`, `duration`,
`value`, `rational`) emit fields. The façade owns the `-show_entries` filter and
the `-show_optional_fields` policy, so no writer implements policy.

**`int` versus `str` is a property of the field, not the value.** It is the only
thing that makes `json` print `"channels": 1` next to `"sample_rate": "44100"`,
and `flat` print `pts=-1024` next to `size="258"`. There is no rule; the
caller's field table decides, per field.

**Emission order is call order.** No map, no sort, no `Serialize` derive.

### Number formatting

`num` is the only module allowed to format a number.

| Kind | Rule |
|---|---|
| Integers | `{}` |
| `*_time`, `start_time`, `duration` | `{:.6}` |
| Rationals | `{num}/{den}`; aspect ratios `{num}:{den}` |
| `codec_tag` | `0x{:08x}` · `id` | `0x{:x}` |
| `-sexagesimal` | `{h}:{m:02}:{s:09.6}` — hours **not** padded |
| `-unit`/`-prefix` | SI ladder upward only; bare integer when exact, else `{:.6}` |

Two traps, both verified against 8.1:

* Seconds never collapse to a bare integer: 4000 s prints `4.000000 Ks`, while
  1000 bytes prints `1 Kbyte`.
* A negative sexagesimal value does **not** get a leading sign on the clock.
  −0.02322 s prints `0:00:-0.023220`. That falls out of truncating division and
  `%09.6f`, and the reference really does it.

## Per-writer escaping reference

Applied to values, after string validation. The reference for the whole table is
`tests/reference.rs`, which holds the exact stdout of ~120 `ffprobe` runs.

| | separator | `\` | `"` | `\t` | `\n` | `\r` | other C0 | `=` `:` `#` | `<` `>` `&` | non-ASCII |
|---|---|---|---|---|---|---|---|---|---|---|
| `default` | — | raw | raw | raw | raw | raw | raw | raw | raw | raw |
| `compact` `e=c` | `\|` → `\|` | `\\` | raw | raw | `\n` | `\r` | `\b` `\f` only | raw | raw | raw |
| `compact` `e=csv` | quotes field | raw | `""` | raw | quotes | quotes | raw | raw | raw | raw |
| `compact` `e=none` | raw | raw | raw | raw | raw | raw | raw | raw | raw | raw |
| `flat` | raw | `\\` | `\"` | raw | `\n` | `\r` | raw | raw | raw | raw |
| `ini` | n/a | `\\` | raw | `\t` | `\n` | `\r` | `\x00NN` | `\=` `\:` `\#` | raw | raw |
| `json` | n/a | `\\` | `\"` | `\t` | `\n` | `\r` | `\u00NN` | raw | raw | raw |
| `xml` | n/a | raw | `&quot;` | raw | raw | raw | U+FFFD¹ | raw | entities | raw |

¹ via `string_validation`, not escaping — see below.

Also:

* `flat` additionally escapes `$` → `\$` and `` ` `` → ``\` ``, and wraps the
  whole value in `"`. Integer-typed fields are printed bare and unquoted.
* `flat` **keys** are sanitised: every character outside `[A-Za-z0-9_]` becomes
  `_`, one per character. Case is preserved, so `WE-IRD_KEY.1` → `WE_IRD_KEY_1`.
  `ini` keys are **not** sanitised and print as `WE-IRD_KEY.1=x`.
* `flat` does not escape its own `sep_char` inside values, and `ini` escapes `:`
  and `#` but not `;`. Both are pinned by tests so a future "fix" is caught.
* `json` does not escape `/`, and emits non-ASCII as raw UTF-8, not `\uXXXX`.
* `compact`'s `escape=c` escapes `\b` and `\f` but not `\t` or `\v`.
* The `compact` type qualifier lowercases and replaces every non-`[a-z0-9_]`
  character with `_`, **one underscore per character**:
  `H.26[45] User Data Unregistered SEI message` →
  `h_26_45__user_data_unregistered_sei_message`.

### String validation

`string_validation`/`sv` ∈ `fail|ignore|replace` (default `replace`) and
`string_validation_replacement`/`svr`. Only `xml` rejects anything, because only
XML has characters it cannot represent (XML 1.0 §2.2: every C0 control but tab,
LF and CR). `fail` drops the whole field; `replace` substitutes.

**The documented default replacement is wrong.** ffprobe(1) says the default
`svr` is the empty string; 8.1 substitutes U+FFFD when the option is left alone.
Passing `svr=` explicitly does delete.

## The two blank-line state machines

These are where a plausible implementation silently diverges.

### `ini`

Plan 14 §4.3 says "a `\n` is emitted before every section header, including
wrappers". That is **not** 8.1's behaviour. Two rules reproduce every observed
case:

1. A `[path]` header gets a blank line before it **unless the previous line
   written was also a header**.
2. A section that produced *no output at all* writes one blank line when it
   closes. Only empty wrappers and arrays can.

So `-of ini -show_entries stream_tags=NASTY` opens with **one** blank line and
runs `[streams.stream.0]` straight into `[streams.stream.0.tags]`, while
`-of ini -show_entries stream=index` opens with **three** — the empty `programs`
array, the empty `stream_groups` array, and then rule 1.

(Why those arrays are open at all: `-show_entries` matches local names as well
as unique ones, and a local-name match selects *every* section carrying it.
`stream` is also the local name of `program_stream` and `stream_group_stream`.)

### `compact`

A section header writes its name **followed by** the item separator; every
later item writes the separator **before** itself. An empty section therefore
prints `stream|` with a trailing separator, and a nested header child reads
`…|component|index=1` with a separator on each side of its name. A section
footer writes a newline unconditionally, even onto an already-empty line, which
is what leaves a blank line after each `pixel_format` group.

## How to change it

Do not change a writer without a reference run to back it.

The workflow: run the `ffprobe` command, capture stdout with `od -c` or
`python3 -c 'import sys;print(repr(sys.stdin.buffer.read()))'`, add it to
`tests/reference.rs` with the command in a comment, then make the writer match.
`tests/torture.rs` replays each captured scenario through `TextFormat` and
compares byte for byte; `tests/snapshots.rs` pins the overall shape for review.

A change that does not move `tests/reference.rs` did not change behaviour.

Rows still unverified, all in `sections.rs`: `element_name` for the
`stream_group` component/piece/block family and the frame-side-data
component/piece family. No sample reachable from `lavfi` produces an IAMF
stream group or a side-data type with sub-components, and `-show_frames` is v0.2
(D14.4). They currently carry the section's own local name and affect only `xml`
and `compact`, only for those sections.

### Adding a writer

Implement `TextWriter`, add it to `writers::make` and `writers::NAMES`, and add
captures for it. The trait gets `Out` (a byte sink) and `Ctx` (the section
stack, array indices, unique type, and the run options); everything else is the
writer's own line state.

**Unless it is an `ffprobe -of` name.** `mermaid`/`mermaidhtml`
(`writers/mermaid.rs`) go in `writers::make` and a *separate* constant,
`writers::GRAPH_ONLY_NAMES`, instead of `NAMES`: `NAMES`' own conformance test
(`tests/torture.rs`'s `the_capture_set_covers_every_writer`) requires a real
captured `ffprobe` byte reference for every entry, and there is no meaningful
one for either — measured directly, `ffprobe -of mermaid -show_streams` exits
0 with no error and no stream output at all, since both formats only do
anything real behind `ffmpeg`'s `-print_graphs`. `make` still accepts both
names unconditionally, matching the reference's own "never rejects the format
name" behaviour; a caller that needs to validate a name against everything
this crate implements checks `NAMES.contains(..) || GRAPH_ONLY_NAMES.contains(..)`
(`vaco-cli`'s `print_graphs.rs` does not — see its own doc for why an unknown
`-print_graphs_format` name is a warning, not a validation error).

## Configuration

Run-wide, via `FormatOpts`:

| Option | Effect |
|---|---|
| `-unit` | append the unit suffix (`byte`, `bit/s`, `s`) |
| `-prefix` | scale by the SI ladder and insert the prefix letter |
| `-byte_binary_prefix` | **a no-op in 8.1** — verified by sweeping byte sizes |
| `-sexagesimal` | `H:MM:SS.microseconds` for time values |
| `-pretty` | all four of the above |
| `-show_optional_fields` | `auto` (default) / `always` / `never` |

Per writer, via `-of name=key=value[:key=value…]`:

| Writer | Options (alias, default) |
|---|---|
| `default` | `nokey`/`nk` 0 · `noprint_wrappers`/`nw` 0 |
| `compact` | `item_sep`/`s` `\|` · `nokey`/`nk` 0 · `escape`/`e` `c` · `print_section`/`p` 1 |
| `csv` | same, defaults `s=,` `nk=1` `e=csv` |
| `flat` | `sep_char`/`s` `.` · `hierarchical`/`h` 1 |
| `ini` | `hierarchical`/`h` 1 |
| `json` | `compact`/`c` 0 |
| `xml` | `fully_qualified`/`q` 0 · `xsd_strict`/`x` 0 (implies `q`) |

All seven also take `string_validation`/`sv` and
`string_validation_replacement`/`svr`.

`hierarchical=0` drops the **array** sections from `flat`/`ini` paths and keeps
everything else, so `streams.stream.0.tags.X` becomes `stream.0.tags.X` and
`packets.packet.0.side_data_list.side_data.0.x` becomes
`packet.0.side_data.0.x`.

`xsd_strict=1` refuses the run and exits 1 with

```
XSD-compliant output selected but option 'unit' was selected, XML output may be non-compliant.
You need to disable such option with '-nounit'
```

It checks **only** `unit` and `prefix`. Contrary to plan 14 §4.3,
`-byte_binary_prefix` and `-sexagesimal` are accepted under `xsd_strict=1` in
8.1. `TextFormat::validate` reports this as `Error::Option`; the CLI turns it
into the message and the exit code.

## Dependencies

* `vaco-core` — `Error`/`Result` and `Rational`.
* `bitflags` — the two flag sets.
* dev only: `insta` (shape snapshots), `proptest` (round-trip and containment
  properties).

Nothing else. `#![forbid(unsafe_code)]`.

## Reference binary

Every observation in this crate was made against:

```
ffprobe version 8.1 Copyright (c) 2007-2026 the FFmpeg developers
libavutil 60.26.100 / libavcodec 62.28.100 / libavformat 62.12.100
```

run under `LC_ALL=C`. D6 requires the differential harness to pin one reference
version explicitly; this is it. Re-record `tests/reference.rs` when it moves.

## Fuzzing

`fuzz/fuzz_targets/textformat_escape.rs`, feature `textformat`. Container
metadata reaches the escaping tables verbatim, so this is the crate's
untrusted-input boundary even though nothing here parses. The target asserts
round trips, separator containment, and that no writer panics or leaves a record
unterminated.

```sh
cargo fuzz run textformat_escape --fuzz-dir fuzz
```
