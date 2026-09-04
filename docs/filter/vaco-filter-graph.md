# `vaco-filter-graph`

Layer 5b. The filtergraph *description language*: the string behind `-vf`,
`-af` and `-filter_complex`, the three levels of escaping it carries, how its
labels wire filters together, and the policy that decides which conversion
filter repairs which kind of format mismatch.

It knows **no filters**. They arrive through `registry::FilterRegistry`, which
is what lets the whole language be tested against `vaco-filter-core`'s mock
filters long before a filter library exists — and what keeps layer 5b from
having to be written before layer 5c.

---

## What it is

| Module | Contents |
|---|---|
| `span` | `Span`, and the caret renderer every diagnostic uses |
| `lex` | the one escaping-aware scan, parameterised by what terminates a token |
| `ast` | the grammar, the parser, the printer, and argument splitting |
| `error` | `GraphError`, its `ErrorKind`, and "did you mean" |
| `registry` | `FilterRegistry`, `Instantiate`, `Instance`, and `pads` |
| `build` | instantiation, link resolution, validation, `BuiltGraph` |
| `convert` | `DefaultConverters` — the auto-conversion **policy** |
| `mock` | seven worked filters, and the proof that the traits are usable |

```text
text ──ast::parse──> Ast ──build::build──> BuiltGraph ──configure──> Graph
        grammar,          labels, pad          negotiation and
        escaping          resolution           auto-conversion
```

---

## How the grammar was recovered, and where the plan is wrong

Everything here is user-facing syntax, so none of it may be guessed. Each rule
was measured against ffmpeg 8.1, mostly through the `lavfi` input device's
`-dumpgraph` option, which prints the parsed graph directly:

```sh
ffmpeg -v error -f lavfi -dumpgraph 1 -i "color=c=red:s=32x32:d=0.04,hflip" -f null -
#  +----------------+
#  | Parsed_color_0 |default--[32x32 1:1 yuv420p]--Parsed_hflip_1:default
```

That is the shortest path to the graph parser there is: one option, straight to
`avfilter_graph_parse`, with the result printed rather than inferred from a
downstream effect. Plan 13 §1b's rule is about probing *other* parsers through
a filtergraph; here the filtergraph **is** the thing under test.

### The grammar, as measured

```text
GRAPH       := WS? SWS_PREFIX? CHAIN (';' CHAIN)* WS?
SWS_PREFIX  := "sws_flags" '=' <token to ';'> ';'      -- literally first
CHAIN       := FILTER (',' FILTER)*
FILTER      := LABEL* NAME ('=' ARGS)? LABEL*
NAME        := <token stopping at '=' ',' ';' '['>     -- split at the first '@'
ARGS        := <token stopping at '[' ']' ',' ';'>
LABEL       := '[' <token stopping at ']'> ']'
```

### Five places plan 16 §2.1 is wrong

| Plan says | Measured | Command |
|---|---|---|
| "an empty filterchain is an error" — implying a trailing separator is one | A **trailing** `,` or `;` is accepted; a leading one and `;;` are not | `-i "color=…,hflip;"` → runs; `-i "color=…,hflip;;"` → `No such filter: ''` |
| auto instance names are `name@N` with `N` per **name** | They are `Parsed_<name>_<N>` with `N` counting **every** filter in the graph | `-i "color=…,scale=16:16,hflip,scale=8:8"` → `Parsed_scale_1`, `Parsed_hflip_2`, `Parsed_scale_3` |
| whitespace is skipped "around the `=` that introduces arguments" | Around it, yes — but not *inside a name*, and the name is not trimmed internally | `-i "movie =ab"` → opens `ab`; `-i "hflip @x"` → `No such filter: 'hflip '` |
| — | `]` does not terminate a filter name | `-i "hflip]x"` → `No such filter: 'hflip]x'` |
| errors are named for what they are | An unknown filter with a *positional* argument reports the argument, not the filter | `-i "zzz=ab"` → `No option name near 'ab'`; `-i "zzz@t=a=b"` → `No such filter: 'zzz'` |

The last row is a diagnostic wart rather than an interface fact, and we do not
reproduce it: we report `no such filter: 'zzz'` in both cases. Plan 16 §2.5
already says graph diagnostics are prose the differential harness allowlists.

### Two more measured facts the plan does not mention

* **`sws_flags=` tolerates leading whitespace but not a space before its `=`.**
  ` sws_flags=bicubic;…` is the prefix; `sws_flags =bicubic;…` is a filter
  called `sws_flags`. The `;` is mandatory — `sws_flags=bicubic` alone is not a
  graph.
* **A label may contain almost anything, including `[`.** `[a[b]` is a label
  named `a[b`; only `]` ends one, and `[]` is `Bad (empty?) label`.

---

## Escaping: three levels, kept apart on purpose

Plan 13 §1b records two occasions on which an agent measured a *filtergraph's*
unescaping and attributed it to the parser underneath. This crate **is** that
unescaping, so the levels are named rather than blurred.

| Level | Applied by | Stops at | Here |
|---|---|---|---|
| 3 | the shell | shell metacharacters | not ours |
| 2 | the graph scanner | `[` `]` `,` `;` | `lex::next_token` with `StopSet::GRAPH` |
| 1 | the option scanner | `:` then `=` | `FilterSpec::arguments` |
| 0 | a list-valued option | `\|` | `Arg::list_values` |

### The load-bearing measurement

**The graph scanner unescapes.** It does not hand the argument text through
still escaped for the option layer to deal with:

```sh
ffmpeg -f lavfi -i "movie=a\:b" -f null -
#  -> Failed to avformat_open_input 'a'
```

If `\:` had survived the graph scan, the option layer would have honoured the
escape and the filename would have been `a:b`. It split there instead, which
means the backslash was already gone. That single fact is why "each level
doubles the backslashes" is the right rule of thumb, and why
`Arg::raw_value` is still escaped while `FilterSpec::args` is not.

`movie` is the probe throughout: its `filename` option is echoed back verbatim
in the error message, so a vector's decoded value is readable without a codec,
a font, or a frame.

### The scanner's exact behaviour

* `'…'` is a literal run. **A backslash inside a quoted run is data**, verified:
  `movie='a\\b'` yields `a\b`, which is only consistent with `\\` reaching the
  option layer intact. Quotes do not nest; adjacent runs concatenate
  (`'a''b'` → `ab`).
* Leading whitespace is skipped and trailing whitespace is trimmed — but only
  whitespace written *bare*. `movie=\' ab \'` yields `' ab '` at level 2 and
  ` ab ` at level 1; `movie=a\\ ` yields `a\`, because the escaped backslash is
  data and the bare space after it is not.
* **Two leniencies, deliberately kept.** An unterminated `'` runs to the end of
  the token and a lone trailing `\` is a literal backslash — neither is an
  error:

  ```sh
  ffmpeg -f lavfi -i "movie='ab" -f null -   # -> 'ab'
  ffmpeg -f lavfi -i 'movie=ab\' -f null -   # -> 'ab\'
  ```

  `vaco_core::escape::unescape` rejects both. Rejecting them here would fail
  command lines that work against the reference today, so the scanner accepts
  them and records a `lex::Quirk` on the `Ast` instead, which a verbose log can
  surface. **This is a divergence from `vaco-core` worth knowing about at the
  option layer too** — see *Wanted from other crates*.

### The canonical worked example, verified

Plan 16 §2.3's worked example is **correct**, checked byte for byte:

```sh
printf '%s' "movie=this is a \\\\\\'string\\\\\\'\\\\: may contain one\\, or more\\, special characters" > vector
ffmpeg -f lavfi -i "$(cat vector)" -f null -
#  -> 'this is a 'string': may contain one, or more, special characters'
```

Both the backslash form and the quoted-run form
(`'this is a '\\\''string'\\\''\: …`) decode to the same text. Both are in
`tests/escaping.rs` as vectors.

Careful with your shell when re-taking this: `zsh`'s builtin `echo` interprets
backslashes, so a vector that looks like two backslashes on screen may be three
in the file. `xxd` the file before trusting it — the same rule D17.1's note
draws about reading bytes with `grep`.

---

## Link resolution

Order matters and is not obvious. Pad counts depend on options
(`amix=inputs=3`, `split=4`), so links cannot be resolved from the syntax tree
alone: instantiate first, then resolve, then validate.

Two mechanisms, applied in this order per filter:

**Explicit labels.** A leading `[L]` connects to an open output named `L`, or
records an unmatched input a later chain may satisfy — forward references work,
and `[a]hflip[out];[0:v]null[a]` is a legal graph. A trailing `[L]` connects to
an unmatched input named `L`, or opens an output.

**Unlabelled auto-connection, within a chain.** Output pads that got no label
are carried to the next filter. Its *labelled* inputs take pads `0..n` and the
carried list fills what remains. That ordering is measured, not assumed:

```sh
ffmpeg -f lavfi -dumpgraph 1 \
  -i "color=c=red:s=64x64:d=0.1[x];color=c=blue:s=8x8:d=0.1,[x]overlay" -f null -
#  -> the output link is 64x64, so `[x]` took overlay's *main* pad (0) and the
#     carried 8x8 stream took the overlay pad (1). Labels first, carried after.
```

Whatever is unmatched at the end is exported as an `OpenPad`.

### Where we are stricter than the reference, and where we are not

* **A label consumed twice is not an error here**, and is not there either.
  `[0:v]null[a];[a]hflip[out];[a]null[out2]` leaves the second `[a]` as an
  unresolved *input*, which `ffmpeg` then fails to bind
  (`Stream specifier 'a' … matches no streams`). We export it as an open input
  and let the caller decide, which is the same shape.
* **A cycle is rejected here and is not there.** `[a]null[a]` parses in the
  reference and fails later for a different reason
  (`A filtergraph has zero outputs`). We run Kahn's algorithm at build time and
  name the participants. Both reject; ours says why.

---

## Auto-conversion: mechanism there, policy here

`vaco-filter-core` finds the link with no common format, coalesces every
property that conflicts on it into one request, and splices in whatever comes
back. It must not know that a filter called `scale` exists. So `convert.rs`
supplies:

| Media | Properties | Converter |
|---|---|---|
| video | pixel format | `scale`, carrying the `sws_flags=` prefix |
| audio | sample format, sample rate, channel layout | `aresample` |
| subtitle/data, hardware contexts | — | **nothing**, which produces the conflict diagnostic |

The chosen output format is **not** re-derived here.
`vaco_filter_core::negotiate::loss` carries a 35-row corpus measured against
the reference with the tier order pinned by compile-time assertions —

> chroma-total > alpha > depth > colour model > chroma coarsening > packing

— and this module asks `loss::best_video`, `loss::best_audio_format` and
`loss::best_rate` which of the downstream's accepted values costs least. A
property the downstream leaves open is copied from upstream, so a converter
never silently changes something nobody asked about.

Converter naming matches the reference (`auto_scale_0`, `auto_aresample_0`)
because scripts grep for it — with one caveat recorded under *Signature gaps*.

**Where the options come from**, in increasing precedence and matching upstream:
the application's `-sws_flags`; the `sws_flags=…;` graph-string prefix, which is
parsed here and applied to every auto-inserted `scale` in that graph; and
nothing per link, because auto-inserted converters are not individually
addressable. Verified:

```sh
ffmpeg -v verbose -f lavfi \
  -i "sws_flags=bicubic+accurate_rnd;testsrc2=s=32x32:d=0.04,format=pix_fmts=rgb24,format=pix_fmts=gray" \
  -f null - 2>&1 | grep auto_scale_0
#  -> [auto_scale_0] w:iw h:ih flags:'bicubic+accurate_rnd' interl:0
```

---

## How to change it

* **Adding a grammar rule** means a stop set in `lex::StopSet` and a branch in
  `ast::parse`. Measure it against the reference first and put the command in
  the test, next to the expectation — every case in `tests/grammar.rs` carries
  one.
* **The parser has no recursion at all**, and must not grow any. Depth is the
  classic way a hand-written filtergraph parser turns 200 KB of `[` into a
  stack overflow; here it costs bytes.
  `parsing_is_iterative_so_depth_costs_bytes_not_stack` is the guard.
* **Gotcha — the `:` split happens on *decoded* argument text.** `\:` becomes a
  real separator by then. Splitting `FilterSpec::args` is correct; splitting the
  original source text is not.
* **Gotcha — do not use `vaco_core::escape` for the graph level.** It rejects
  two inputs the reference accepts (above). `lex` exists for that reason and
  nothing else.
* **Writing a registry.** Implement `FilterRegistry`; `mock::MockRegistry` is
  the worked example. A filter whose pad count depends on its options returns a
  `FilterDesc` whose pad slices come from `registry::pads` and a `NodeFormats`
  of the same length — the builder checks that they agree, because the
  scheduler takes counts from one and media types from the other.
* **Changing which converter fixes what** is `convert.rs` and only
  `convert.rs`. Do not re-derive the loss weights; they are measured and
  compile-time-asserted in `vaco-filter-core`.

---

## Configuration

No environment variables and no feature flags.

| Knob | Where | Default | Effect |
|---|---|---|---|
| `sws_flags=…;` | the graph string | none | arguments for every auto-inserted `scale` |
| `BuiltGraph::sws_opts` | per graph | from the prefix | the same, settable by the application |
| `BuiltGraph::swr_opts` | per graph | empty | `-aresample_swr_opts`; there is no graph-string prefix for it, matching upstream |
| `AutoConvert` | per configure | `All` | `None` is `-noauto_conversion_filters` |
| `registry::pads::MAX` | compile time | 64 | the largest dynamic pad count the shared static slices express |

`pads::MAX` is ours. The reference allows far more — `amix` accepts thousands
of inputs — but a static `[Pad; 32768]` would cost most of a megabyte for a
case nobody writes, and a registry that needs more supplies its own slice.

---

## Performance

`cargo bench -p vaco-filter-graph`, divan, release profile, Apple silicon.

| Bench | Median |
|---|---|
| `parse` / `scale=640:480,format=pix_fmts=yuv420p` | 463 ns |
| `parse` / three chains with labels and options | 1.25 µs |
| `parse` / the canonical escaping example | 770 ns |
| `parse_deep_chain` / 256 filters | 30.9 µs (≈ 120 ns per filter) |
| `build_chain` / four filters, instantiated and wired | 10.1 µs |

A filtergraph is parsed once per run, so these are a guard against pathological
input rather than a hot path. The number to watch is the per-filter cost of the
deep chain: it is linear, which is what says the parser has no accidental
quadratic in it.

---

## Testing

* **84 tests**: 29 unit, 55 across four integration files.
* `tests/grammar.rs` — every rule above, each with the reference command that
  established it.
* `tests/escaping.rs` — the level separation, both canonical worked examples,
  and a check that a `vaco-expr` expression survives the graph layer intact,
  asserted by parsing the recovered text with the real expression engine rather
  than by eye.
* `tests/build.rs` — link resolution, validation, dynamic pad counts, and
  graphs driven to completion through `vaco-filter-core`'s scheduler with
  backpressure engaged.
* `tests/properties.rs` — `proptest`: parsing never panics, escaping is exactly
  invertible at every level, split-then-join is the identity, and
  `parse(print(parse(s)))` is structurally `parse(s)`.

### Fuzzing

Three targets (D6). Per plan 19 §13, the exec count and what was found, not a
verdict. Measured 2026-09-03, one campaign each, `-timeout=10
-rss_limit_mb=2048`, corpora carried over from earlier runs:

| Target | Input | Execs | Time | cov / ft | Findings |
|---|---|---|---|---|---|
| `graph_parse` | bytes → `parse`, print, reparse; `render` on error | 971,406 | 300 s | 939 / 4816 | none |
| `graph_build` | bytes → `parse_and_build`, mock registry; `render` on error | 1,610,888 | 300 s | 734 / 2621 | none |
| `graph_hostile` | grammar-driven descriptions → parse, build, attach, `configure` | 427,289 over three runs | 8,371 execs, then 600 s, then 600 s | 4423 / 22910 | one parser bug, one harness bug |

`graph_parse` also replays the durable source-level regression
`fuzz/seeds/graph_parse/regression-sws-prefix-roundtrip-288b48fa`, which keeps a
leading backslash from being mistaken for the `sws_flags=` graph prefix when a
literal filter has that name. The matching `graph_hostile` seed stores the
structure-aware generator input, not the rendered graph string.

`graph_hostile` is the structure-aware one. It draws filter names, labels,
option keys and values from small pools so that duplicate labels, forward
references, cycles, `outputs=` counts past `usize`, unicode and empty names and
a filter called `sws_flags` are the common case, then splices raw
metacharacters into the rendered string. It reaches five times the coverage of
the byte-level targets because it also attaches sources and sinks to the open
pads and negotiates, with and without auto-conversion. It runs at ~400 exec/s
against ~3,000–5,000 for the byte targets, for the same reason. The artifact it
writes is the `arbitrary` byte stream, not the graph string; run the binary
with `GRAPH_HOSTILE_DUMP=1` to see what a crashing input said.

`find fuzz/artifacts -type f` was empty after the last campaign of each.

**Recursion.** The parser is `parse` → `parse_chain` → `parse_filter` →
`parse_labels`, each a loop over `&str` and none of them re-entrant, so nesting
in the input costs bytes, never stack, and there is no depth limit to tune.

**`graph_hostile` found a round-trip bug at exec 8371**, on `\sws_flags=x|y;`:
the backslash keeps the parser from reading it as the `sws_flags=` prefix, so
it parses as one filter named `sws_flags` — but `Display` printed it back as
`sws_flags=x|y`, which re-parses as the prefix followed by an empty graph. The
printer now escapes the one position where a name can collide with the prefix.
`fuzz/seeds/graph_hostile/regression-sws-prefix-roundtrip-288b48fa` replays it
in CI and `a_filter_literally_named_sws_flags_survives_printing` pins it.

The second run's only artifact was a timeout in the generator itself — nested
`Escaped` text doubled per level, and a 129-byte input took more than ten
seconds before the parser ever saw it. That is a harness bug, now bounded, and
it is recorded here rather than as a seed because the parser was not involved.

**`graph_parse` found a real bug at exec 667**, on the input `"\t\t\t@"`: an
instance tag with nothing before it produced a filter whose *name* was the
empty string, which every later stage would have had to defend against. The
reference rejects it — `ffmpeg -f lavfi -i "@x"` → `No such filter: ''` — and
so do we now. `an_instance_tag_with_no_filter_name_is_the_same_as_no_name`
pins it.

---

## Signature gaps

Interfaces are frozen (plan 19 §6), so these are **reported, not changed**.

1. ~~**`Graph::configure_converting` discards the arguments the factory
   produced.**~~ **Fixed.** `Insertion` now carries `args`, so the factory's
   `sws_flags=` reaches the `scale` it was parsed for and the builder closure
   reads `spec.args` directly. The workaround here — asking
   `DefaultConverters::args_for(spec.filter)` for them again — is gone; it only
   ever worked because the same crate owns both halves, and a filter library
   supplying its own builder would have lost them silently. Worth noting the
   gap was invisible from inside the workspace for exactly that reason, which
   is what a "signature gaps" section is for.
2. **`Graph` exposes no accessor for a node's `FilterDesc`.** The builder needs
   pad counts and media types before anything is connected, and `Graph::connect`
   validates pad indices against the descriptor it holds. `build.rs` keeps its
   own parallel `Vec<FilterDesc>`; they are `Copy`, so it costs nothing, but two
   copies of the same fact can disagree.
3. **`Insertion` names converters with one counter for all kinds.**
   `negotiate` builds the name as `auto_{filter}_{inserted.len()}`, so a graph
   needing one of each gets `auto_scale_0` and `auto_aresample_1`. The reference
   counts per kind: `auto_scale_0`, `auto_aresample_0`. Scripts grep for these
   names, so it is an interface fact rather than a cosmetic one.
4. **`FilterDesc` has no `PadSpec::Dynamic`** (`vaco-filter-core`'s own gap 8).
   Closed here rather than reported, because it is closable: `registry::pads`
   hands out `&'static [Pad]` subslices of a shared static array, so
   `split=outputs=4` realises four pads without touching a frozen signature. The
   cap is `pads::MAX`.

---

## Wanted from other crates

* **`vaco-core`: `escape::unescape` and `escape::split_raw` are stricter than
  the reference.** They return `UnterminatedQuote` and `TrailingBackslash` where
  ffmpeg 8.1 accepts the input and produces a value. This crate works around it
  at the graph level by owning its scanner, but the *option* level uses the same
  grammar, so `vaco-opts` will hit it: `-vf "drawtext=text='hello"` works
  against the reference and would fail against us. Either relax those two cases
  or expose a lenient variant beside them.
* **`vaco-filter-core`: demand does not propagate through an idle filter.**
  `Graph::score` gives a filter no priority while its inputs are empty, and
  `request_inputs` runs only when a filter *activates*, so a request never
  travels back through a filter that has nothing to do. Every graph in that
  crate's tests is headed by a buffer source, where frames push demand forward,
  so nothing exercised it. Reproduced with its own mocks:

  ```text
  Counter -> Invert -> sink   ->   GraphStatus::Deadlock, zero frames out
  ```

  `Counter -> sink` works, which is why the crate's `a_generator_source_produces_only_on_demand`
  passes. It matters for real graphs: `-lavfi "testsrc2,hflip"` is exactly this
  shape, and so is any pipeline whose source only sends when `source_wants`
  says to. `a_generator_behind_a_filter_does_not_yet_start` in `tests/build.rs`
  pins the current behaviour so that fixing it fails a test rather than passing
  silently.

---

## Deliberately deferred

* **A filter library.** `mock::MockRegistry` has seven filters and exists to
  prove the language, not to be one.
* **`-dumpgraph` byte compatibility.** `BuiltGraph::dump` and `to_dot` are
  diagnostic prose; plan 16 §2.5 already allowlists them in the differential
  harness.
* **The `/`-prefixed option name that loads a value from a file**
  (`drawtext=/text=/path/to/file.txt`). Plan 16 §2.3 puts it in `vaco-opts` at
  argument-parse time, not in the graph parser, and that is the right place.
* **Command-script integration (`sendcmd`, `zmq`).** Runtime target matching and
  timed delivery now live on `vaco_filter_core::Graph`, which owns the run loop
  and the instance labels this builder supplies. This crate still does not
  parse command scripts; `sendcmd`/`asendcmd` are leaf filters, and `zmq` is
  excluded by the dependency policy.

---

## Dependencies

`vaco-core` (errors, `Rational`, `MediaType`), `vaco-filter-core` (the trait
layer, `Graph`, `NodeFormats`, `ConverterFactory`, `negotiate::loss`),
`vaco-frame`, `vaco-pixfmt`, `vaco-sampfmt`, `vaco-chlayout` (the format
vocabulary the converter policy ranges over). Dev: `proptest`, `divan`,
`vaco-expr`.

**No `vaco-scale` and no `vaco-resample`,** though the frozen manifest declared
both. The auto-conversion policy names `scale` and `aresample` as *strings*, and
must: layer 5b cannot depend on the filters it asks for, which is the whole
point of the `ConverterFactory` seam. The edges were never used, and an unused
edge misrepresents the layering — the same reasoning that removed
`vaco-filter-core`'s unused `vaco-opts` edge.
