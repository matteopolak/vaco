# 14 — CLI Layer Plan (`vaco-cli-core`, `vaco-textformat`, `vaco-sched`, `vaco`, `vaco-probe`, `vaco-play`)

Conforms to `planning/00-decisions.md` (D1 CLI-compat-yes/ABI-compat-no, D2 no-unsafe, D6 differential
testing, D7 clean-room) and `planning/10-architecture.md` §3 Layer 7.

Primary source: `planning/research/05-fftools-cli.md` (the compatibility contract). Secondary source:
black-box observation of `ffprobe`/`ffmpeg` **8.1** (Homebrew build, arm64 macOS) — every "OBSERVED"
block below is a recorded output of a shipped reference binary, which D6/D7 explicitly permit. No FFmpeg
source was opened for this document.

Where the contract document and the observed binary disagree, this plan follows the **observed binary**
and flags the divergence in §10.

---

## 1. Scope and shape of the CLI layer

| Crate | Layer | Contents |
|---|---|---|
| `vaco-cli-core` | 7 | Option lexer/grouper/resolver, stream-specifier parser+matcher, `-h` help system, `-formats`/`-codecs`/… listing commands, `-loglevel`/`-report` plumbing, SI/duration/date/size/rate value parsers. |
| `vaco-textformat` | 7 | Section-schema model + the six writers. Depends on `vaco-core` only. |
| `vaco-sched` | 7 | The pipeline scheduler. |
| `vaco-probe` | 7 | ffprobe-equivalent binary. **v0.1 deliverable.** |
| `vaco` | 7 | ffmpeg-equivalent binary. |
| `vaco-play` | 7 | ffplay-equivalent binary. |

`vaco-cli-core` and `vaco-textformat` are separate crates because `vaco-probe` (v0.1) must build and ship
without `vaco-sched`, without any encoder, and without `vaco-filter-*`. The dependency edge
`vaco-cli-core → vaco-textformat` exists (help/listing commands emit through it); the reverse does not.

Non-goals for the whole layer: `libav*` C ABI, `-vsync`, `-async`, `-map_channel`, ffpreset/avpreset
search-path emulation before v0.4, and the `mermaid`/`mermaidhtml` graph writers before v0.4.

---

## 2. `vaco-cli-core` — the option-parsing engine

### 2.1 Why not clap (and not any other declarative parser)

FFmpeg's command line is not a flag set; it is a **positional stream of option groups over a dynamically
open option universe**. Seven properties, each individually survivable in clap, jointly fatal:

1. **Positional context sensitivity.** `-b:v 1M` before `-i` means one thing (an input-side decoder
   option, usually meaningless) and after `-i` another (an output encoder option). The same option name
   occurs many times with different bindings. clap's model is one value (or one `Vec`) per option ID for
   the whole invocation; recovering *which file* an occurrence attached to would require
   `ArgAction::Append` plus a parallel index reconstruction that clap does not expose reliably.
2. **The option universe is open and runtime-determined.** Any AVOption on any format, codec, filter,
   protocol or device is a CLI flag. `-id3v2_version 3` is valid only because the mp3 muxer was selected —
   which is not known until after parsing. A declarative parser must be told its options up front;
   ours must accept unknown option names, defer them, and only fail at the *end* if nothing consumed them.
   `allow_external_subcommands`/`trailing_var_arg` do not model "unknown option with a value that we
   resolve later against a component we have not chosen yet".
3. **`-opt:spec` suffixes.** The option *name token* contains a nested sub-language
   (`-metadata:s:a:1`, `-c:v:0`, `-disposition:s:m:language:eng`). clap matches whole names.
4. **`no`-prefix negation of a subset of options.** `-nostats` is `stats=false`; `-notimestamps` is not an
   option at all. Negation is legal only for the tool's own booleans, never for AVOptions (contract §1.4).
5. **`/`-prefix file indirection**, applied to the *name* token: `-/filter:v graph.txt`.
6. **Group separators that are themselves options.** `-i URL` closes the current input group; a bare
   argument closes the current output group; `-dec N` opens a loopback-decoder group.
7. **Numeric values are an expression language.** `-b:v 2*1000` is accepted (OBSERVED). Values also carry
   SI suffixes and can be durations, sizes, rates, ratios or channel layouts.

**Decision:** hand-write the parser. Zero clap, zero `structopt`, zero `argh`, zero `pico-args`. External
crates used: `bitflags` (option flags), `memchr` (escape scanning), `terminal_size` (help wrapping only).
Everything else is `std`. The parser is ~1200 lines and is the single most heavily unit-tested module in
the layer.

We keep exactly one clap-like idea: a **static descriptor table** per tool. It is data, not a builder DSL,
so it is trivially iterable for `-h` and for the differential tests.

### 2.2 Pipeline

```
argv ──▶ [1 lex]  ──▶ Vec<Token>
      ──▶ [2 resolve] ──▶ Vec<ParsedOption>       (name→descriptor, or Deferred AVOption)
      ──▶ [3 group]   ──▶ ParsedCommandLine       (global + ordered per-file groups)
      ──▶ [4 bind]    ──▶ tool-specific config    (per-file structs; deferred opts still strings)
      ──▶ [5 apply]   ──▶ AVOption application against instantiated components (vaco-opts)
      ──▶ [6 audit]   ──▶ error on any deferred option nothing consumed
```

Stages 1–3 live in `vaco-cli-core`; 4 is per-binary; 5–6 are `vaco-opts` + per-binary.

### 2.3 Stage 1 — lexing an option name token

```rust
/// Lexical decomposition of one argv entry that begins with '-'.
#[derive(Debug, Clone, PartialEq)]
pub struct NameToken {
    /// Number of leading dashes actually present (1 or 2). Both are accepted and equivalent,
    /// except that a bare "-" and "--" are NOT options (see below).
    pub dashes: u8,
    /// A '/' immediately after the dashes: the value is a path whose contents are the real value.
    pub file_indirect: bool,
    /// Option name with any 'no' prefix already stripped (resolution happens in stage 2).
    pub name: Range<usize>,
    /// Everything after the first unescaped ':' in the name token, verbatim. Not yet parsed.
    pub spec: Option<Range<usize>>,
}

pub fn lex_name(arg: &str) -> Option<NameToken>;
```

Rules, in order:

1. `arg` must start with `-`, be longer than 1 char, and not be exactly `--`. A bare `-` is the
   conventional stdin/stdout URL and is a **positional**, not an option. `--` is a positional too — FFmpeg
   has no end-of-options marker; we deliberately do not invent one.
2. Consume 1 or 2 dashes. Two dashes are accepted for every option (`--help` ≡ `-help`).
3. If the next char is `/`, set `file_indirect` and consume it.
4. Scan forward to the first `:` that is not preceded by a backslash. Everything before is `name`;
   everything after (exclusive of the `:`) is `spec`. `-codec:` yields `spec = Some("")`, which is the
   *empty specifier* and matches all streams — distinct from `-codec` which yields `spec = None`. The two
   behave identically for per-stream options but the distinction is preserved for diagnostics.
5. Names are case-sensitive.

Note step 4 is deliberately naive: it does not understand the specifier grammar. `-metadata:s:a:1` lexes
as name `metadata`, spec `s:a:1`; the interpretation of `s:a:1` as a *metadata* specifier rather than a
*stream* specifier is the descriptor's job (`OptFlags::PER_METADATA`).

### 2.4 Stage 2 — descriptors and name resolution

```rust
bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct OptFlags: u32 {
        const HAS_ARG      = 1 << 0;  // consumes the next argv entry
        const GLOBAL       = 1 << 1;  // never attaches to a file group
        const PER_FILE     = 1 << 2;
        const INPUT        = 1 << 3;  // legal in an input group
        const OUTPUT       = 1 << 4;  // legal in an output group
        const PER_STREAM   = 1 << 5;  // accepts :stream_specifier
        const PER_METADATA = 1 << 6;  // accepts :metadata_specifier
        const EXPERT       = 1 << 7;  // hidden from plain `-h`
        const EXIT         = 1 << 8;  // runs and terminates the process (-version, -formats, ...)
        const AUDIO        = 1 << 9;  // help-grouping only
        const VIDEO        = 1 << 10;
        const SUBTITLE     = 1 << 11;
        const GROUP_SEP    = 1 << 12; // -i, -dec: closes/opens a group
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Bool,                 // no argument; supports the `no` prefix
    Int, Int64, Float, Double,
    String,
    Duration,             // §2.7 time-duration grammar
    Size,                 // WxH or a named abbreviation
    Rate,                 // num/den | int | float | named
    Rational,
    Time,                 // date grammar (-timestamp)
    Directive,            // consumes its arg but has bespoke side effects (-i, -map, -dec)
}

pub struct OptDesc {
    pub name:    &'static str,
    pub kind:    ValueKind,
    pub flags:   OptFlags,
    pub argname: Option<&'static str>,   // for help: "-t <duration>"
    pub help:    &'static str,
    /// Non-None ⇒ this name is a pure alias. `-vf` → ("filter", Some("v")).
    pub alias:   Option<(&'static str, Option<&'static str>)>,
}

pub struct OptTable(pub &'static [OptDesc]);
impl OptTable {
    pub fn find(&self, name: &str) -> Option<&'static OptDesc>;
    /// Resolves `noXXX` to (desc(XXX), negated=true) when desc is ValueKind::Bool.
    pub fn find_negatable(&self, name: &str) -> Option<(&'static OptDesc, bool)>;
}
```

Resolution order for a lexed name, first match wins:

1. Exact hit in the tool's `OptTable`. If `alias` is set, rewrite to the target name and merge the alias's
   implicit specifier with the user's (`-vf x` ≡ `-filter:v x`; `-vf:0` is rejected — aliases with a baked
   specifier take no user specifier).
2. `no`-prefix: `name` starts with `no`, the remainder resolves to a `ValueKind::Bool` descriptor, and the
   descriptor's option was not itself found in step 1. Yields `negated = true`.
   *`no` is never applied to AVOptions* (contract §1.4) — for those, `-opt 0`/`-opt 1`.
3. Otherwise **Deferred**: an AVOption whose owning component is not yet known.

```rust
pub enum ParsedOption {
    Known {
        desc: &'static OptDesc,
        negated: bool,
        spec: Option<SpecText>,      // raw specifier text, parsed lazily by the consumer
        value: Option<OsString>,     // post-indirection
        argv_index: usize,
    },
    Deferred {
        name: String,
        spec: Option<SpecText>,
        value: OsString,
        argv_index: usize,
        consumed: Cell<bool>,        // set by stage 5; audited by stage 6
    },
}
```

**Deferred options always consume the next argv entry.** There is no way to know whether an unknown
option is a flag, so FFmpeg assumes an argument; so do we. `ffmpeg -i in.mp4 -zzz 1 -f null -` fails with
`Unrecognized option 'zzz'` / `Error splitting the argument list: Option not found` (OBSERVED); we match
the failure *point* (before any work is done) but not necessarily the message text.

### 2.5 Value acquisition, `/` indirection, and SI suffixes

Order of operations for a `HAS_ARG` option:

1. Take the next argv entry verbatim as an `OsString`. It is **never** re-lexed as an option, so
   `-metadata title=-y` works.
2. If `file_indirect`, treat that entry as a path, read the file, and use the bytes as the value.
   VERIFIED to work: `-/filter:v f.txt` with `f.txt` containing `scale=160:120` produced a 160x120 output
   (OBSERVED). **Unresolved:** whether a trailing newline is stripped — see §10-V3.
3. Type-parse per `ValueKind`.

```rust
/// Parse a numeric CLI value: an expression, then optional SI/binary/bit suffixes.
///   value  := expr [ prefix ] [ 'i' ] [ 'B' ]
///   prefix := 'K'|'k'|'M'|'G'      (also 'E','P','T','m','u','n','p','f' in the full ladder)
/// 'i'  ⇒ 1024-based instead of 1000-based
/// 'B'  ⇒ multiply by 8   (bytes → bits)
/// So `1KB` = 8000, `1KiB` = 8192, `1Mi` = 1048576.
pub fn parse_number(s: &str, kind: ValueKind) -> Result<f64, OptError>;
```

The mantissa is evaluated by `vaco-expr`, not by `str::parse` — `-b:v 2*1000` is accepted by the reference
(OBSERVED, no error). This means every numeric option value goes through the expression evaluator with an
empty variable set. That is cheap and it is the documented-by-behaviour contract.

Two traps to state loudly in `docs/cli/option-values.md`:

- `B` multiplies by **eight**, so `-fs 10MB` is 80 000 000 (bits-worth of a bytes-typed option). This is
  upstream behaviour and we reproduce it.
- `K` is decimal; `Ki` is binary. `-b:v 1M` is 1 000 000 bits/s.

### 2.6 Stage 3 — the grouping model

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind { Input, Output, Decoder }

pub struct OptionGroup {
    pub kind: GroupKind,
    /// Index among groups of the same kind, in command-line order. This is the `input_file_id`
    /// used by -map, and the `dec_idx` used by [dec:N].
    pub index: u32,
    /// The `-i` argument, the bare output argument, or the `-dec` argument.
    pub arg: OsString,
    pub opts: Vec<ParsedOption>,
    pub argv_span: Range<usize>,
}

pub struct ParsedCommandLine {
    pub global: Vec<ParsedOption>,
    pub groups: Vec<OptionGroup>,     // command-line order, kinds interleaved
}
```

Algorithm:

```
pending := []
for each argv entry:
    if it lexes as an option:
        resolve; acquire value
        if desc.flags contains GLOBAL:  global.push(opt)          # even if written between files
        else if desc.flags contains GROUP_SEP:                    # -i, -dec
            groups.push(OptionGroup{ kind, arg: value, opts: take(pending) })
        else:
            pending.push(opt)                                     # includes Deferred
    else:                                                          # bare argument
        groups.push(OptionGroup{ kind: Output, arg, opts: take(pending) })
if pending non-empty: error "Trailing options were found on the commandline."
```

Consequences we inherit deliberately:

- Options apply to the **next** file, and are consumed by it. `-t 10 -i a.mp4 -i b.mp4` limits only `a`.
- `GLOBAL` options are hoisted regardless of position, so `ffmpeg -i a -y out.mp4` and
  `ffmpeg -y -i a out.mp4` are identical.
- Deferred options are always `PER_FILE` — an unknown option before the first `-i` with no following file
  is a trailing-options error.
- `-dec N` is a `GROUP_SEP` directive whose preceding pending options are its decoder AVOptions
  (contract §2.12).
- Input groups must all precede output groups; violation is diagnosed after grouping, not during.

### 2.7 Shared value grammars (`vaco-cli-core::value`)

All from contract §5.6; all pure functions with exhaustive unit tests and a fuzz target each.

```rust
pub fn parse_duration(s: &str) -> Result<i64 /*µs*/, ValueError>;   // [-][HH:]MM:SS[.m…] | [-]S+[.m…][s|ms|us]
pub fn parse_date(s: &str) -> Result<i64 /*µs since epoch*/, ValueError>; // ISO-ish, or "now"; Z ⇒ UTC
pub fn parse_video_size(s: &str) -> Result<(u32, u32), ValueError>;  // WxH | 48 named abbreviations
pub fn parse_video_rate(s: &str) -> Result<Rational, ValueError>;    // num/den | int | float | 7 names
pub fn parse_ratio(s: &str) -> Result<Rational, ValueError>;         // expr | num:den; 1/0 and negatives parse OK
pub fn parse_color(s: &str) -> Result<Rgba, ValueError>;
```

`parse_ratio` returning "valid" for `1/0` and for negatives is a documented upstream quirk; callers must
range-check. We keep the quirk and put the range check in the callers, exactly as the contract states.

### 2.8 AVOption reflection — the `vaco-opts` seam

Stage 5 is where deferred options meet components. `vaco-opts` (Layer 0, per architecture §3) provides:

```rust
pub enum OptType {
    Flags, Int, Int64, UInt64, Double, Float, Str, Rational, Binary, Dict,
    Bool, ImageSize, PixFmt, SampleFmt, VideoRate, Duration, ColorSpace, ChLayout, Const,
}

bitflags! { pub struct OptEntryFlags: u32 {
    const ENCODING = 1<<0; const DECODING = 1<<1;
    const AUDIO = 1<<2; const VIDEO = 1<<3; const SUBTITLE = 1<<4;
    const EXPORT = 1<<5; const READONLY = 1<<6; const RUNTIME = 1<<7; // 'T' in -h output
    const DEPRECATED = 1<<8;
}}

pub struct OptEntry {
    pub name: &'static str,
    pub help: &'static str,
    pub ty: OptType,
    pub default: OptDefault,
    pub min: f64, pub max: f64,
    pub flags: OptEntryFlags,
    /// Named-constant namespace. Entries with ty == Const and the same unit are this option's
    /// symbolic values, and are what `-h encoder=x` prints indented under it.
    pub unit: Option<&'static str>,
}

pub struct OptSchema {
    pub name: &'static str,
    pub entries: &'static [OptEntry],
    /// Inherited schemas (the AVClass child-class chain): a muxer inherits the generic format schema.
    pub parents: &'static [&'static OptSchema],
}

pub trait Configurable {
    fn schema(&self) -> &'static OptSchema;
    fn set_str(&mut self, name: &str, value: &str) -> Result<(), OptError>;
    fn get_str(&self, name: &str) -> Result<String, OptError>;
}
```

The bag of not-yet-applied options, the `AVDictionary` role without copying its structure:

```rust
pub struct OptionsBag { entries: Vec<BagEntry> }
struct BagEntry { name: String, value: String, origin: usize /*argv index*/, consumed: bool }

impl OptionsBag {
    /// Apply every entry the target's schema (including inherited parents) recognises.
    /// Marks them consumed. Unrecognised entries are left alone for the next target.
    pub fn apply_recognised(&mut self, target: &mut dyn Configurable) -> Result<(), OptError>;
    /// After all targets have seen the bag.
    pub fn unconsumed(&self) -> impl Iterator<Item = &BagEntry>;
}
```

Per-file, the deferred options are split by media relevance and specifier before application:

- A deferred option with **no** specifier goes into the file-level bag (format/demuxer AVOptions) *and*
  into every stream's bag. First recogniser wins; being recognised nowhere is the stage-6 error.
- A deferred option with a specifier applies only to the matching streams' bags.

Stage 6 audit produces one error per unconsumed option, naming the argv index, and exits non-zero before
any output file is created.

`-h decoder=h264`, `-h muxer=mp4`, `-h filter=scale`, and `-h full` are all a walk of `OptSchema` + its
`Const` entries. They need no component instantiation — architecture §5's descriptor split gives us this
for free.

### 2.9 `-loglevel`, `-report`, banner

```
loglevel_arg := ( ('+'|'-') flag )* [ '+' ] level?
flag  := repeat | level | time | datetime
level := quiet|panic|fatal|error|warning|info|verbose|debug|trace | -8|0|8|16|24|32|40|48|56
```

Backed by a `tracing` subscriber in `vaco-core`. Colour auto-detect; `AV_LOG_FORCE_NOCOLOR` /
`AV_LOG_FORCE_COLOR` honoured verbatim. `-report` opens `PROGRAM-YYYYMMDD-HHMMSS.log`, writes the full
command line as the first line, forces level `debug` into the file sink only, and is also triggered by
`FFREPORT` (`:`-separated `key=value`, keys `file` and `level`). `-hide_banner` suppresses the
version/config/library block.

---

## 3. Stream specifiers — grammar, AST, matcher

### 3.1 Grammar

```ebnf
spec        ::= ε | segment ( ':' segment )*

segment     ::= disposition | group | program | stream_id_i | metadata
              | index | type | usable
                                        (* longest-match order; see note *)

index       ::= DIGIT+
type        ::= 'v' | 'V' | 'a' | 's' | 'd' | 't'
group       ::= 'g' ':' group_ref
group_ref   ::= DIGIT+ | '#' INT | 'i' ':' INT
program     ::= 'p' ':' INT
stream_id_i ::= '#' INT | 'i' ':' INT
metadata    ::= 'm' ':' esc_key [ ':' esc_val ]
disposition ::= 'disp' ':' dflag ( '+' dflag )*
usable      ::= 'u'

esc_key, esc_val ::= ( [^:\\] | '\\' ANY )*
```

Note on ambiguity: the alternatives are not prefix-free. `d` is the data-stream type and `disp` is the
disposition segment; `i:` is a stream-id segment while there is no `i` type. The scanner therefore tries
alternatives in **longest-literal-first** order: `disp:`, then `g:`, `p:`, `m:`, `i:`, `#`, then the
single-letter types, then a bare integer, then `u`. Anything else is a hard parse error.

`m:` is the only segment with in-band escaping: a `:` inside key or value must be written `\:`. The
scanner consumes `\` + next char as a literal pair. **Unresolved:** whether `\\` denotes a literal
backslash, and whether a trailing lone `\` is an error — §10-V4.

### 3.2 AST

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSpec {
    pub segments: Vec<SpecSegment>,
    /// Retained verbatim for error messages ("Stream specifier 'v:9' matches no streams").
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecSegment {
    Index(u32),
    Type(SpecMediaKind),
    Group(GroupRef),
    Program(i64),
    StreamId(i64),
    Metadata { key: String, value: Option<String> },
    Disposition(DispositionFlags),
    Usable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecMediaKind { Video, VideoNoPic, Audio, Subtitle, Data, Attachment } // v V a s d t

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupRef { Index(u32), Id(i64) }

impl FromStr for StreamSpec { type Err = SpecError; /* … */ }
impl StreamSpec {
    pub const ALL: StreamSpec;                       // empty segment list
    pub fn is_empty(&self) -> bool;
}
```

`DispositionFlags` is a `bitflags` set over the 19 names in contract §3.7:
`default, dub, original, comment, lyrics, karaoke, forced, hearing_impaired, visual_impaired,
clean_effects, attached_pic, timed_thumbnails, non_diegetic, captions, descriptions, metadata,
dependent, still_image, multilayer` — shared with `-dispositions`, `-disposition`, and the
`stream_disposition` output section, from one table.

### 3.3 Matching

The subtlety the contract calls out (§1.5, rule 1 vs the chaining rule) collapses into **one uniform
rule** if the matcher is written as an ordered narrowing:

> An `Index(n)` segment selects the **n-th element of the current candidate set**, in candidate-set
> order. As the first segment the candidate set is all streams in libavformat order, so this degenerates
> to absolute indexing; after `a` it means "the n-th audio stream"; after `g:0` it means "the n-th stream
> of group 0". No special case is needed.

Everything else is an order-preserving predicate filter.

```rust
pub struct MatchCtx<'a> {
    pub streams:  &'a [StreamInfo],     // libavformat order
    pub programs: &'a [ProgramInfo],
    pub groups:   &'a [GroupInfo],
}

impl StreamSpec {
    /// Ordered set of matching stream indices. Empty ⇒ no match (caller decides fatal vs `?`-optional).
    pub fn select(&self, ctx: &MatchCtx<'_>) -> Vec<u32> {
        let mut cand: Vec<u32> = (0..ctx.streams.len() as u32).collect();
        for seg in &self.segments {
            cand = match seg {
                SpecSegment::Index(n) => cand.get(*n as usize).copied().into_iter().collect(),
                SpecSegment::Type(k)  => cand.into_iter()
                    .filter(|&i| kind_matches(*k, &ctx.streams[i as usize])).collect(),
                SpecSegment::Program(id) => {
                    let set = ctx.programs.iter().find(|p| p.id == *id)
                        .map(|p| p.streams.as_slice()).unwrap_or(&[]);
                    cand.into_iter().filter(|i| set.contains(i)).collect()
                }
                SpecSegment::Group(r) => { /* by index or id, same shape */ }
                SpecSegment::StreamId(id) =>
                    cand.into_iter().filter(|&i| ctx.streams[i as usize].id == *id).collect(),
                SpecSegment::Metadata { key, value } => cand.into_iter().filter(|&i| {
                    match (ctx.streams[i as usize].tags.get_ci(key), value) {
                        (Some(_), None) => true,
                        (Some(v), Some(want)) => v == want,
                        (None, _) => false,
                    }
                }).collect(),
                SpecSegment::Disposition(want) => cand.into_iter()
                    .filter(|&i| ctx.streams[i as usize].disposition.contains(*want)).collect(),
                SpecSegment::Usable => cand.into_iter()
                    .filter(|&i| is_usable(&ctx.streams[i as usize])).collect(),
            };
            if cand.is_empty() { break; }
        }
        cand
    }
    pub fn matches(&self, ctx: &MatchCtx<'_>, idx: u32) -> bool { self.select(ctx).contains(&idx) }
}

fn kind_matches(k: SpecMediaKind, s: &StreamInfo) -> bool {
    use SpecMediaKind::*;
    match k {
        Video      => s.media == MediaType::Video,
        VideoNoPic => s.media == MediaType::Video
                      && !s.disposition.intersects(ATTACHED_PIC | TIMED_THUMBNAILS),
        Audio      => s.media == MediaType::Audio,
        Subtitle   => s.media == MediaType::Subtitle,
        Data       => s.media == MediaType::Data,
        Attachment => s.media == MediaType::Attachment,
    }
}

fn is_usable(s: &StreamInfo) -> bool {
    s.codec_id != CodecId::None && match s.media {
        MediaType::Video => s.width > 0 && s.height > 0,
        MediaType::Audio => s.sample_rate > 0,
        _ => true,
    }
}
```

Cost note: `select` is O(segments × streams); files have tens of streams and specifiers are matched a few
hundred times per run at most. No index structures. Where a matcher is hot (per-packet in `vaco`), the
selection is resolved **once** at setup into a `StreamMask` bitset and the spec is never re-evaluated.

Per-file caching matters for a second reason: metadata matching (`m:`) is only reliable for **input**
files (contract §1.5 note 6), because output stream metadata is not final until mux init. We resolve `m:`
specifiers against input streams at bind time and forbid them on output-only options, with a clear error.

### 3.4 Metadata specifiers (a different grammar on the same syntactic slot)

`-metadata`, `-map_metadata` and `-keep_metadata` take a *metadata* specifier, not a stream specifier:

```ebnf
meta_spec ::= ε | 'g' | 's' [ ':' spec ] | 'c' ':' INT | 'p' ':' INT
```

```rust
pub enum MetaSpec { Global, Stream(StreamSpec), Chapter(u32), Program(u32) }
```

`OptFlags::PER_METADATA` routes the raw specifier text here instead of to `StreamSpec::from_str`. This is
why stage 1 must not parse the specifier — it does not know which grammar applies.

### 3.5 Verification tests owed (see §10)

- `V` exclusion set: build an MP4 with a cover-art stream and an MKV with `timed_thumbnails`, and confirm
  both are excluded by `V` (contract says "attached pictures/thumbnails/cover art"; we assume
  `ATTACHED_PIC | TIMED_THUMBNAILS`).
- Case sensitivity of `m:` key and value comparison.
- `disp:` with an unknown flag name: error or no-match?
- `g:0:1` re-indexing vs `g:0:i:1` (group-*id* 1) disambiguation.

---

## 4. `vaco-textformat` — the writer framework

This crate is the **v0.1 acceptance surface**. Every design choice below is subordinated to one goal:
byte-identical output.

### 4.1 The section schema

`ffprobe -sections` prints the schema, including the local-name/unique-name split and four flags. OBSERVED
(ffprobe 8.1), abridged to show the structure — the full 70-row table is committed verbatim as
`crates/cli/vaco-textformat/src/sections.rs` and re-verified in CI against the reference binary:

```
W... = wrapper (contains other sections, no local entries)
.A.. = array of elements of the same type
..V. = variable number of fields with variable keys
...T = section has a unique type

W...   root
.A..      chapters
....          chapter
..V.              tags/chapter_tags
....      format
..V.          tags/format_tags
.A..      frames
....          frame
..V.              tags/frame_tags
.A..              side_data_list/frame_side_data_list
..VT                  side_data/frame_side_data
.A..                      timecodes
....                          timecode
.A..                      components/frame_side_data_components
..VT                          component/frame_side_data_component
.A..                              pieces/frame_side_data_pieces
..VT                                  piece/frame_side_data_piece
.A..              logs
....                  log
....          subtitle
.A..      programs → program → tags/program_tags, streams/program_streams
                              → stream/program_stream → disposition/…, tags/…
.A..      stream_groups → stream_group → tags, disposition,
             components/stream_group_components → component → subcomponents → subcomponent
                → pieces → piece → subpieces → subpiece → blocks → block
             streams/stream_group_streams → stream/stream_group_stream → disposition, tags
.A..      streams → stream → disposition/stream_disposition, tags/stream_tags,
                              side_data_list/stream_side_data_list → side_data/stream_side_data
.A..      packets → packet → tags/packet_tags, side_data_list/packet_side_data_list → side_data
....      error
....      program_version
.A..      library_versions → library_version
.A..      pixel_formats → pixel_format → flags/pixel_format_flags,
                                          components/pixel_format_components → component
```

**Correction to the contract:** `packets_and_frames` does **not** appear in ffprobe 8.1's `-sections`
output, though it is still an XSD root child. See §10-V1.

Model:

```rust
bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct SectionFlags: u8 {
        const WRAPPER     = 1 << 0;  // W
        const ARRAY       = 1 << 1;  // A
        const VAR_FIELDS  = 1 << 2;  // V
        const UNIQUE_TYPE = 1 << 3;  // T
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SectionId(pub u16);

pub struct SectionDesc {
    pub id: SectionId,
    /// Local name, used for the element/keys: "tags", "side_data", "stream".
    pub name: &'static str,
    /// Globally unique name; equals `name` when unambiguous. Used by -show_entries.
    pub unique_name: &'static str,
    pub flags: SectionFlags,
    pub children: &'static [SectionId],
    /// For VAR_FIELDS sections: the element name each key/value pair is rendered as.
    /// "tag" for tags, "side_datum" for side_data.  (OBSERVED)
    pub element_name: Option<&'static str>,
    /// Default-writer rendering. Populated by exhaustive observation, not derivable from flags.
    pub default_style: DefaultStyle,
}

pub enum DefaultStyle {
    /// `[STREAM] … [/STREAM]`
    Header,
    /// Flattened into the parent as `PREFIX:key=value`, e.g. `TAG:` and `DISPOSITION:`.
    InlinePrefix(&'static str),
}
```

`default_style` deserves its own note. OBSERVED: under `[STREAM]`, `tags` renders as `TAG:language=und`
and `disposition` as `DISPOSITION:default=1`, but under `[FRAME]`, `side_data` renders as a full
`[SIDE_DATA]` block. None of the four `-sections` flags separates these cases, and depth does not either.
We therefore treat it as an **observed per-section property** and populate it by running the reference
binary over a corpus that reaches every section (§5.6). A conformance test asserts the table matches.

The compact writer uses the same distinction with a lowercase prefix (`tag:`, `disposition:`), and adds a
type qualifier for `UNIQUE_TYPE` sections. OBSERVED:

```
frame|pts=0|side_datum/h_26_45__user_data_unregistered_sei_message:side_data_type=H.26[45] User Data Unregistered SEI message
```

i.e. the compound key is `<element_name>/<sanitised type value>:<field>`, where sanitising lowercases and
replaces every non-`[a-z0-9_]` run with `_`.

### 4.2 The writer trait

```rust
pub trait TextWriter {
    fn name(&self) -> &'static str;
    fn flags(&self) -> WriterFlags;

    fn init(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> { Ok(()) }
    fn fini(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()> { Ok(()) }

    fn section_header(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()>;
    fn section_footer(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>) -> Result<()>;

    fn print_int(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: i64) -> Result<()>;
    fn print_str(&mut self, o: &mut Out<'_>, ctx: &Ctx<'_>, key: &str, v: &str) -> Result<()>;
}

bitflags! { pub struct WriterFlags: u8 {
    /// Omit fields whose value is unavailable when -show_optional_fields is `auto`.
    const SUPPRESS_OPTIONAL = 1 << 0;   // json, xml
    /// Emit a document prologue/epilogue in init/fini.
    const DOCUMENT          = 1 << 1;   // json, xml, ini
}}

/// Everything the writer needs about where it is.
pub struct Ctx<'a> {
    /// Section descriptor stack, root first. `cur()` is the innermost.
    pub stack: &'a [&'static SectionDesc],
    /// Number of *elements* already emitted at each stack level (array indices).
    pub elem_index: &'a [u64],
    /// Number of *fields* already emitted in the current section (comma placement).
    pub field_index: u64,
    /// Set when the current section is UNIQUE_TYPE: the type string.
    pub unique_type: Option<&'a str>,
    pub opts: &'a FormatOpts,          // -unit -prefix -sexagesimal, string validation
}
```

The caller-facing façade — what `vaco-probe` actually writes against:

```rust
pub struct TextFormat<W: Write> { /* writer, Out, Ctx storage, entry filter */ }

impl<W: Write> TextFormat<W> {
    pub fn new(writer: Box<dyn TextWriter>, sink: W, schema: &'static Schema,
               opts: FormatOpts, filter: EntryFilterSet) -> Self;

    pub fn open(&mut self, id: SectionId) -> Result<()>;
    pub fn open_typed(&mut self, id: SectionId, ty: &str) -> Result<()>;
    pub fn close(&mut self) -> Result<()>;

    // Typed emitters. The Int/Str choice is a *property of the field*, not of the value —
    // it is what makes JSON emit `"channels": 1` but `"sample_rate": "44100"`.
    pub fn int(&mut self, key: &str, v: i64) -> Result<()>;
    pub fn int_opt(&mut self, key: &str, v: Option<i64>) -> Result<()>;
    pub fn str(&mut self, key: &str, v: &str) -> Result<()>;
    pub fn str_opt(&mut self, key: &str, v: Option<&str>) -> Result<()>;
    pub fn fmt(&mut self, key: &str, args: fmt::Arguments<'_>) -> Result<()>;

    // Domain helpers that encode the pretty/unit/prefix/sexagesimal rules once.
    pub fn ts(&mut self, key: &str, ts: Option<i64>) -> Result<()>;                 // raw integer ts
    pub fn time(&mut self, key: &str, ts: Option<i64>, tb: Rational) -> Result<()>; // "*_time" fields
    pub fn duration(&mut self, key: &str, secs: Option<f64>) -> Result<()>;
    pub fn value(&mut self, key: &str, v: Option<f64>, unit: Unit) -> Result<()>;   // size/bitrate
    pub fn rational(&mut self, key: &str, r: Rational) -> Result<()>;               // "num/den"
    pub fn tag(&mut self, key: &str, v: &str) -> Result<()>;                        // VAR_FIELDS
}
```

`open`/`close` are the only places the entry filter (`-show_entries`) and the optional-field policy are
consulted, so no writer implements policy.

### 4.3 The six writers — exact behaviour

All writers accept `string_validation`/`sv` ∈ `fail|ignore|replace` (default `replace`) and
`string_validation_replacement`/`svr` (default empty). Validation runs on every emitted string *before*
escaping: invalid UTF-8 sequences are replaced/dropped/fatal per the setting.

#### default

Options: `nokey`/`nk` (0), `noprint_wrappers`/`nw` (0).

- `Header`-style section: `[UPPER(name)]\n` … `[/UPPER(name)]\n`. Wrappers emit nothing.
- `InlinePrefix(p)` section: each field emits `p + ":" + key + "=" + value + "\n"` into the parent's
  stream; no header or footer.
- Fields: `key=value\n`, or bare `value\n` when `nk=1`.
- **No escaping at all.** OBSERVED: `TAG:NASTY=v=1,c:2|q"3\4;e[f]#g <&> ünï` round-trips verbatim.
- `nw=1` suppresses both `[SECTION]` and `[/SECTION]` but not the inline prefixes.

#### compact / csv

`csv` is the same writer with different defaults. Options: `item_sep`/`s` (`|`; `,` for csv),
`nokey`/`nk` (0; 1 for csv), `escape`/`e` (`c`; `csv` for csv), `print_section`/`p` (1).

- One line per `Header` section: `name` + sep + `key=value` + sep + …  + `\n`.
  `p=0` drops the leading section name (OBSERVED).
- `InlinePrefix` children flatten into the same line with a lowercase prefix: `tag:LANG=eng`,
  `disposition:default=1`.
- `UNIQUE_TYPE` children use `element_name/sanitised_type:field=value` (OBSERVED, §4.1).
- Escape modes, OBSERVED against value `v=1,c:2|q"3\4;e[f]#g <&> ünï` with sep `|`:

| mode | output | rule |
|---|---|---|
| `c` | `v=1,c:2\|q"3\\4;e[f]#g <&> ünï` | backslash-escape the item separator and `\`; also `\n`→`\n`, `\r`→`\r`. Nothing else. |
| `csv` | `"v=1,c:2\|q""3\4;e[f]#g <&> ünï"` | RFC 4180: quote iff the value contains sep, `"`, `\n` or `\r`; double internal `"`. |
| `none` | `v=1,c:2\|q"3\4;e[f]#g <&> ünï` | verbatim |

  (backslashes in the table are literal.) OBSERVED for csv on a normal field:
  `format,t.mp4,2,0,0,"mov,mp4,m4a,3gp,3g2,mj2",…` — the format_name is quoted because it contains `,`.

#### flat

Options: `sep_char`/`s` (`.`), `hierarchical`/`h` (1).

- Emits `path.key=value\n` with no section headers at all.
- Path is the `sep_char`-joined chain of section names from root (excluding `root`), with array elements
  suffixed by their index: `streams.stream.0.tags.NASTY`. With `h=0` the path is only the innermost
  section plus index: `stream.0.index=0` (OBSERVED).
- **Key sanitisation**: every char outside `[A-Za-z0-9_]` in a *key* becomes `_`.
  OBSERVED: tag `we-ird key.1` → `WE_IRD_KEY_1` (the case came from the container, the `_`s from us).
- **Value rendering** is type-driven: `int()` fields print bare (`index=0`, `start_pts=0`); `str()` fields
  print double-quoted and shell-escaped. OBSERVED escapes: `"` → `\"`, `\` → `\\`, `$` → `\$`,
  `` ` `` → ``\` ``. NOT escaped: `'`, literal tab, `<`, `>`, `&`, non-ASCII.

#### ini

Options: `hierarchical`/`h` (1).

- Document prologue `# ffprobe output\n`.
- **A `\n` is emitted before every section header, including wrapper sections.** OBSERVED via `od -c`:
  `# ffprobe output\n` `\n` `\n` `\n` `[streams.stream.0]\n`. That is one blank from `root`, one from
  `streams`, one preceding `[streams.stream.0]`. With `h=0` and two top-level shows there are three
  blanks; getting this wrong is a byte-diff, so it is a dedicated unit test.
- Section header is `[` + flat-style path + `]`, same path rules as `flat` (`h` behaves identically).
- Fields: `key=value\n`. **Keys are not sanitised** (OBSERVED: `WE-IRD_KEY.1=x`).
- **Value escaping**, OBSERVED: escape `=` → `\=`, `:` → `\:`, `\` → `\\`, `#` → `\#`, tab → `\t`,
  newline → `\n`. NOT escaped: `;`, `[`, `]`, `$`, `` ` ``, `'`, `"`, non-ASCII.
  (`:` and `#` being escaped but `;` not is surprising; it is what the binary does.)

#### json

Options: `compact`/`c` (0).

- 4-space indent per level. `WrapperArray` sections become JSON arrays, `Header` sections objects,
  `VAR_FIELDS` sections objects of key→string.
- `c=1` puts one section on one line as `{ "k": v, "k2": v2 }` — note the spaces immediately inside the
  braces (OBSERVED).
- **Number vs string is decided by the emitter call, not by the value.** OBSERVED on one audio stream:
  `"channels": 1` and `"duration_ts": 44100` are numbers, while `"sample_rate": "44100"`,
  `"bit_rate": "70303"`, `"nb_frames": "45"` and `"start_time": "0.000000"` are strings. Reproducing this
  requires a per-field int/str decision recorded in the field table (§4.5), *not* inferred from the type.
- String escaping: standard JSON (`"`, `\`, control chars). `/` is **not** escaped. Non-ASCII is emitted
  as raw UTF-8, not `\uXXXX` (OBSERVED: `ünï` verbatim).
- An empty object still emits its braces with a blank line between them (OBSERVED for a filtered-empty
  `tags`):
  ```json
  "tags": {

  }
  ```

#### xml

Options: `fully_qualified`/`q` (0), `xsd_strict`/`x` (0, implies `q`).

- Prologue `<?xml version="1.0" encoding="UTF-8"?>\n`; root `<ffprobe>` or, with `q=1` (OBSERVED):
  ```xml
  <ffprobe:ffprobe xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:ffprobe="http://www.ffmpeg.org/schema/ffprobe" xsi:schemaLocation="http://www.ffmpeg.org/schema/ffprobe ffprobe.xsd">
  ```
- 4-space indent. A blank line separates top-level root children (OBSERVED between `</streams>` and
  `<format …>`).
- **Scalars are attributes; nested sections are child elements.** This is the XSD convention and it holds
  throughout.
- `VAR_FIELDS` sections emit one child element per pair using `element_name`:
  `<tag key="language" value="und"/>`.
- `UNIQUE_TYPE` sections additionally carry `type="…"` on the element and emit their fields *both* as the
  `type` attribute and as `element_name` children. OBSERVED:
  ```xml
  <side_data type="H.26[45] User Data Unregistered SEI message">
      <side_datum key="side_data_type" value="H.26[45] User Data Unregistered SEI message"/>
  </side_data>
  ```
- Escaping in attribute values: `&`→`&amp;`, `<`→`&lt;`, `>`→`&gt;`, `"`→`&quot;`. `'` and `\` are left
  alone; non-ASCII is raw UTF-8. (OBSERVED.)
- **Quirk to reproduce:** an element with zero attributes is written `<stream >` — with a space before
  the `>`. OBSERVED with `-show_entries stream_tags=nasty`. We reproduce it; a conformance test pins it.
- `x=1` **validates the run configuration and refuses**: with `-unit` it prints
  `XSD-compliant output selected but option 'unit' was selected, XML output may be non-compliant.` /
  `You need to disable such option with '-nounit'` and exits **1** (OBSERVED). The same must hold for
  `-prefix`, `-byte_binary_prefix` and `-sexagesimal`.

### 4.4 Number formatting — the rules that decide byte identity

A single module, `vaco_textformat::num`, with no other code allowed to format a float.

| Kind | Rule |
|---|---|
| Integers | `{}` on `i64`. |
| `*_time`, `start_time`, `duration` | `{:.6}` of the `f64` seconds value. |
| Rationals (`r_frame_rate`, `time_base`, `sample_aspect_ratio`) | `{num}/{den}`; SAR/DAR use `{num}:{den}`. |
| `codec_tag` | `0x{:08x}` |
| `id` | `0x{:x}` |
| Sexagesimal (`-sexagesimal`) | `{h}:{m:02}:{s:09.6}` — hours **not** zero-padded. OBSERVED `0:00:01.000000`. Negative values prefix `-`. |
| Unit/prefix values (`-unit`, `-prefix`) | scale by the SI ladder, then: **if the scaled value is an exact integer print it as an integer, otherwise `{:.6}`**. |

That last rule was derived by sweep (OBSERVED, `-unit -prefix`, `format=size`):

| bytes | output |
|---:|---|
| 1 | `1 byte` |
| 999 | `999 byte` |
| 1000 | `1 Kbyte` |
| 1500 | `1.500000 Kbyte` |
| 999999 | `999.999000 Kbyte` |
| 1000000 | `1 Mbyte` |
| 2097152 | `2.097152 Mbyte` |
| 1000000000 | `1 Gbyte` |

Unit suffixes: `byte`, `bit/s`, `s`. With `-prefix` but no `-unit` the suffix is dropped and only the
prefix letter remains (`17.540000 K`, OBSERVED).

Float edge cases where Rust and C printf differ and we must special-case:

- `f64::NAN` → Rust `NaN`, C `nan`/`-nan`. We emit whatever the reference emits per field; in practice
  these fields are `N/A` instead.
- `f64::INFINITY` → Rust `inf`, C `inf`. Same token, but `-inf` vs `-inf` must be checked.
- `-0.0` → Rust `-0.000000`, C `-0.000000`. Same.
- Locale: irrelevant for us (Rust never localises), and we must make sure the reference is compared under
  `LC_ALL=C` so *it* doesn't localise either. The conformance harness sets it.

### 4.5 The field table and the optional-field policy

Every emitted field is a row in a generated table:

```rust
pub struct FieldDesc {
    pub section: SectionId,
    pub name: &'static str,
    /// Which emitter this field uses. Decides JSON number-vs-string and flat quoted-vs-bare.
    pub repr: FieldRepr,          // Int | Str
    /// True if the field participates in -show_optional_fields.
    pub optional: bool,
}
```

`-show_optional_fields` semantics, OBSERVED:

| setting | behaviour |
|---|---|
| `auto` / `-1` (default) | writers with `SUPPRESS_OPTIONAL` (json, xml) omit unavailable optional fields; the others print `N/A`. |
| `always` / `1` | every writer prints the field; json/xml print the literal string `"N/A"`. Empty arrays that would otherwise be suppressed are also printed (OBSERVED: `"programs": []`, `"stream_groups": []` appear only under `always`). |
| `never` / `0` | every writer omits them. |

An important, non-obvious consequence: `color_range`, `color_space`, `color_transfer` and
`color_primaries` with value `unknown` are treated as **optional** — they appear in the `default` writer
as `color_range=unknown` but are absent from JSON/XML under `auto`, and reappear as
`"color_range": "unknown"` under `always` (OBSERVED, all three cases). So `optional` is a per-field flag
plus a per-field notion of "unavailable" that can be a sentinel string, not only `None`. `FieldDesc` needs
a fourth column for that sentinel; it is populated from observation.

### 4.6 How we guarantee byte identity

Six mechanisms, all mandatory:

1. **Emission order is source order.** No `HashMap`, no `BTreeMap`, no `serde::Serialize` derive anywhere
   in the output path. Sections are emitted by straight-line functions whose statement order *is* the
   field order. `#![deny(clippy::…)]` plus a review rule; a `HashMap` in `vaco-probe`'s emit module is a
   review blocker.
2. **The field table is generated and checked.** `just conformance-fields` runs the reference over the
   corpus with `-show_optional_fields always -of json` and `-of default`, diffs the observed field
   sequence per section against `FieldDesc` rows, and fails on any mismatch — including ordering.
3. **One formatting module.** Nothing outside `vaco_textformat::num` may call `format!` on a float. A
   clippy lint (`disallowed_methods`) enforces it.
4. **Golden corpus.** `testdata/probe-golden/` holds, per input file, the reference output for the full
   cross-product of {6 writers × writer-option variants} × {`auto`,`always`,`never`} ×
   {plain, `-pretty`, `-sexagesimal`, `-unit`, `-prefix`} × {`-show_streams`, `-show_format`,
   `-show_packets`, `-show_frames`, `-show_chapters`, `-show_programs`, `-show_stream_groups`,
   combinations}. Goldens are regenerated by `just conformance-update` and reviewed as a diff.
5. **Byte comparison, not structural.** The harness compares `Vec<u8>`, including trailing newlines and
   blank lines. Structural JSON comparison is explicitly *not* good enough because the number-vs-string
   distinction and key order are the whole point.
6. **`-bitexact` in the harness.** Version strings and `format_long_name` vary by build; the harness runs
   both binaries with `-bitexact` and, for `program_version`/`library_versions`, compares only shape.

---

## 5. `vaco-probe`

### 5.1 Option surface

Grouped by implementation phase, all from contract §3.1.

**Tier A — v0.1, required for the acceptance criterion**

`-i`, `-o`, `-f`, `-of`/`-output_format`/`-print_format`, `-select_streams`, `-show_entries`,
`-show_format`, `-show_streams`, `-show_chapters`, `-show_programs`, `-show_stream_groups`,
`-show_packets`, `-show_frames`, `-show_data`, `-show_data_hash`, `-data_dump_format`,
`-show_error`, `-show_private_data`/`-private`, `-show_optional_fields`, `-show_versions`,
`-show_program_version`, `-show_library_versions`, `-show_pixel_formats`, `-sections`,
`-count_frames`, `-count_packets`, `-read_intervals`, `-unit`, `-prefix`, `-byte_binary_prefix`,
`-sexagesimal`, `-pretty`, `-bitexact`, plus every common option from §2.9.

**Tier B — v0.2** `-show_log` (needs the log-capture plumbing), `-analyze_frames`,
`-c:media_spec`/`-codec:media_spec` (needs decoders).

Exit code: `0` on success, `1` on any failure including "no input file specified" and an unopenable URL
(OBSERVED: `ffprobe nonexistent.mp4` → 1).

### 5.2 `-show_entries` as a parser

```ebnf
SECTION_ENTRIES ::= SECTION_ENTRY ( ':' SECTION_ENTRY )*
SECTION_ENTRY   ::= SECTION_NAME [ '=' [ LOCAL_ENTRIES ] ]
LOCAL_ENTRIES   ::= NAME ( ',' NAME )*
```

```rust
pub enum EntryFilter {
    /// Section named without '=' : print all its fields and enable all descendant sections.
    All,
    /// `section=a,b` : only these fields, in the section's natural order.
    Only(Vec<String>),
    /// `section=`   : print no fields.
    None,
}
pub struct EntryFilterSet { by_section: Vec<(SectionId, EntryFilter)> }
```

Semantics, with OBSERVED confirmations:

- A section name may be either the local or the unique name; unique wins on collision.
- Naming a section **implicitly enables printing it** — `-show_entries format=filename` needs no
  `-show_format`.
- Order in the spec is ignored; output uses the natural section/field order (contract §3.4).
- `section=` prints the section header but no fields: `-show_entries 'format='` yields the empty document
  for each writer — `{\n\n}` for json, `<ffprobe>\n</ffprobe>` for xml, and **nothing at all** for the
  default writer (OBSERVED). Note the default writer emits no `[FORMAT]` either, which contradicts
  contract §3.4's "the section header itself still emitted". Behaviour follows the binary; see §10-V2.
- Nested sections are addressed by their unique names: `-show_entries 'stream=index:stream_tags=language'`
  produces `index=` plus `TAG:language=` (OBSERVED).

### 5.3 `-read_intervals` as a parser

```ebnf
INTERVALS ::= INTERVAL ( ',' INTERVAL )*
INTERVAL  ::= [ START | '+' START_OFFSET ] [ '%' [ END | '+' END_OFFSET | '#' COUNT ] ]
```

```rust
pub struct ReadInterval { pub start: Option<Bound>, pub end: Option<EndBound> }
pub enum Bound   { Absolute(i64 /*µs*/), Relative(i64 /*µs*/) }
pub enum EndBound { Time(Bound), Packets(u64) }

pub fn parse_read_intervals(s: &str) -> Result<Vec<ReadInterval>, ValueError>;
```

Execution rules (contract §3.2):

- No `START` ⇒ no seek is performed for that interval.
- `+OFFSET` is relative to the **current position** after the previous interval, not to zero.
- `#N` reads N packets from the interval start, excluding flush packets.
- Seeking is inexact; when the interval has a duration, the absolute end is computed from the **actual
  found** seek position, not the requested one. This is the rule that makes naive implementations
  diverge; it is a dedicated differential test with a file whose keyframes are sparse.
- No `START` in the whole spec ⇒ read to EOF.

Examples that must round-trip: `10%+20,01:30%01:45`, `01:23%+#42`, `%+20`, `%02:30`.

### 5.4 `-select_streams` and ordering guarantees

`-select_streams` takes a full `StreamSpec` and restricts every stream-scoped section: `STREAM`,
`PACKET`, `FRAME`, and per-stream counting. It does **not** restrict `FORMAT`, `CHAPTER`,
`PROGRAM`/`STREAM_GROUP` membership listings, or the root-level version sections.

**Root child emission order** (OBSERVED, ffprobe 8.1, everything enabled at once):

```
program_version, library_versions, pixel_formats, packets, frames, packets_and_frames,
programs, stream_groups, streams, chapters, format, error
```

This matches the XSD child order in contract §3.8 and **contradicts** contract §3.5's stated order
(`chapters, format, frames, …`). We follow the observed/XSD order. See §10-V1.

Within `streams`, order is libavformat stream order. Within `packets`/`frames`, order is demux order.
`packets_and_frames` interleaves and numbers by type. Programs and stream groups list their member
streams in container order.

### 5.5 Pretty/unit/prefix/sexagesimal

`-pretty` ≡ `-unit -prefix -byte_binary_prefix -sexagesimal`. The four are independent otherwise, and each
is a plain boolean carried in `FormatOpts`. Application points:

| Field class | `-unit` | `-prefix` | `-sexagesimal` |
|---|---|---|---|
| `*_time`, `start_time`, `duration` | ` s` suffix | no effect (OBSERVED: `duration=1.000000` unchanged) | replaces the whole rendering with `H:MM:SS.ffffff` |
| `size`, `*_size` | ` byte` suffix | SI scale | — |
| `bit_rate`, `max_bit_rate` | ` bit/s` suffix | SI scale | — |
| everything else | — | — | — |

`-byte_binary_prefix` is documented to switch byte-valued fields to 1024-based prefixes. **It does not
work in ffprobe 8.1**: with `-unit -prefix -byte_binary_prefix`, a 2 097 152-byte file reports
`2.097152 Mbyte` (decimal), not `2 Mibyte` (OBSERVED, three independent invocations including via
`-pretty`). We implement the **observed** behaviour — the flag is accepted, affects `xsd_strict`
validation, and otherwise changes nothing — and record the divergence in §10-V5 so that if upstream fixes
it we know exactly what to change.

### 5.6 v0.1 acceptance test plan

Runner: `crates/tools/vaco-conformance`, invoked by `just conformance`. Environment is pinned:
`LC_ALL=C`, `TZ=UTC`, a recorded reference version string, and `-bitexact` on both sides.

**Corpus** (`testdata/` + `vaco-corpus` fetch), each ≤ 2 MB where possible:

| Family | Cases |
|---|---|
| MP4/MOV | AVC+AAC baseline; HEVC+Opus; AV1; fragmented; multi-track with cover art; `edts` edit lists; rotation matrix; timecode track; chapters; HDR10 mastering-display + CLL side data; per-stream metadata; attachment-less. |
| Matroska/WebM | VP9+Opus; AV1+FLAC; multiple subtitle tracks (text + image); attachments (font); chapters with nested tags; arbitrary stream tags (the escaping torture file); default/forced dispositions; `DURATION` tags. |
| MPEG-TS | Multi-program; PID-addressed streams; AAC-ADTS + H.264; discontinuities; a truncated tail. |
| Degenerate | Zero-length file; 1-byte file; file with no streams; unrecognised container; a directory path; an unreadable path; a named pipe. |

**Axes** (full cross-product, ~9 000 invocations, ~4 minutes wall clock in parallel):

1. Writer × writer options: `default`, `default=nk=1`, `default=nw=1`, `default=nk=1:nw=1`,
   `compact`, `compact=e=csv`, `compact=e=none`, `compact=p=0`, `compact=s=;`, `csv`,
   `flat`, `flat=h=0`, `flat=s=/`, `ini`, `ini=h=0`, `json`, `json=c=1`, `xml`, `xml=q=1`, `xml=x=1`.
2. Show flags: each of the 12 `-show_*` alone, plus `-show_format -show_streams`,
   `-show_packets -show_frames`, and "everything".
3. Formatting: none, `-pretty`, each of `-unit`/`-prefix`/`-byte_binary_prefix`/`-sexagesimal` alone.
4. `-show_optional_fields` ∈ {absent, `always`, `never`}.
5. Selection: absent, `-select_streams v`, `a`, `V`, `0`, `m:language:eng`, `disp:default`, `u`,
   `p:1:a:0` (TS only), `#0x101` (TS only).
6. Intervals: absent, `%+#5`, `%+1`, `1%+1`, `01:23%+#42`.

**Assertion:** exact byte equality of stdout, plus equality of the exit code. stderr is compared only for
the exit-code-1 cases and only for "did it fail at all", since message wording is not a compatibility
goal.

**Allowlisted divergences** (D6 requires these be explicit and reviewed):

- `program_version` / `library_versions` field *values* (our version numbers differ). Shape is compared.
- `format_long_name` for containers where our long name is chosen independently — an explicit per-format
  mapping table is maintained and *is* compared once populated.
- `codec_long_name` — same treatment.
- Any field we have not yet implemented is listed in `conformance/known-gaps.toml` with an owner and a
  target milestone. A gap without an entry fails the build.

**Acceptance gate for v0.1:** zero unexplained byte diffs across the whole matrix for the MP4/MOV,
Matroska/WebM and MPEG-TS families, with `known-gaps.toml` empty of anything outside the allowlist above.

---

## 6. `vaco` — the ffmpeg equivalent

### 6.1 Option catalogue by milestone

Contract §2 is the source. Reorganised by what has to exist for the option to mean anything.

**M-A — v0.2 (remux): demux + mux, streamcopy only, no decode**

| Group | Options |
|---|---|
| Files | `-i`, `-f`, `-y`, `-n`, bare output URL, `-o`-less positional model |
| Selection | `-map` (all forms), `-vn/-an/-sn/-dn`, `-ignore_unknown`, `-copy_unknown` |
| Codec | `-c`/`-codec` restricted to `copy`, `-tag`/`-vtag`/`-atag`, `-bsf` |
| Timing | `-ss`, `-to`, `-t`, `-sseof`, `-itsoffset`, `-itsscale`, `-copyts`, `-start_at_zero`, `-copytb`, `-copyinkf`, `-dts_delta_threshold`, `-dts_error_threshold`, `-muxdelay`, `-muxpreload`, `-avoid_negative_ts` (a muxer AVOption) |
| Metadata | `-metadata`, `-map_metadata`, `-map_chapters`, `-keep_metadata`, `-disposition`, `-timestamp`, `-timecode`, `-program`, `-streamid`, `-attach`, `-dump_attachment` |
| Rate/robustness | `-stream_loop`, `-readrate`, `-re`, `-readrate_initial_burst`, `-readrate_catchup`, `-fs`, `-frames`, `-abort_on`, `-max_error_rate`, `-xerror`, `-thread_queue_size`, `-max_muxing_queue_size`, `-muxing_queue_data_threshold`, `-discard`, `-bitexact` |
| Reporting | `-stats`, `-nostats`, `-stats_period`, `-progress`, `-report`, `-benchmark`, `-timelimit`, `-dump`, `-hex`, `-debug_ts`, `-stdin`/`-nostdin` |

**M-B — v0.3 (transcode): decoders, encoders, simple filtergraphs**

`-c` with real codecs, `-b`/`-q`/`-qscale`, `-r`, `-fpsmax`, `-s`, `-aspect`, `-pix_fmt`, `-sample_fmt`,
`-ar`, `-ac`, `-ch_layout`/`-channel_layout`, `-guess_layout_max`, `-filter`/`-vf`/`-af`,
`-filter_threads`, `-filter_buffered_frames`, `-sws_flags`, `-auto_conversion_filters`,
`-autorotate`, `-autoscale`, `-apply_cropping`, `-display_rotation`, `-display_hflip`, `-display_vflip`,
`-reinit_filter`, `-drop_changed`, `-enc_time_base`, `-fps_mode`, `-frame_drop_threshold`, `-shortest`,
`-shortest_buf_duration`, `-apad`, `-force_key_frames`, `-pass`, `-passlogfile`, `-bits_per_raw_sample`,
`-fix_sub_duration`, `-canvas_size`, `-recast_media`, `-isync`, `-vstats*`, `-stats_enc_*`,
`-stats_mux_pre`, `-rc_override`, `-max_alloc`.

**M-C — v0.4 (complex graphs and the long tail)**

`-filter_complex`, `-lavfi`, `-filter_complex_threads`, `-dec`, `-mastering_display`, `-content_light`,
`-stream_group`, `-reinit_opts`, `-target`, `-pre`/`-vpre`/`-apre`/`-spre`/`-fpre`,
`-print_graphs*`, `-sdp_file`, `-fix_sub_duration_heartbeat`.

**M-D — later / optional** `-init_hw_device`, `-filter_hw_device`, `-hwaccel*`, `-qsv_device`,
`-vaapi_device`, `-cpuflags`, `-cpucount`.

**Never** `-vsync`, `-async`, `-map_channel`. Contract §7 is explicit that these are gone from current
FFmpeg. We do not add legacy aliases: `-vsync` is a plain "Unrecognized option" error. The current
contract is `-fps_mode` for video sync, `-af aresample=async=…:min_hard_comp=…:first_pts=…` for audio
sync, and nothing at all for per-channel remapping (users write `-af pan=…` or `channelmap`).
Obsolete-but-live aliases we *do* keep, because the contract lists them as current:
`-vframes`/`-aframes`/`-dframes` → `-frames:v/a/d`, `-vcodec`/`-acodec`/`-scodec` → `-codec:v/a/s`,
`-aq` → `-q:a`, `-vtag`/`-atag` → `-tag:v/a`.

### 6.2 Stream selection — explicit rules and a decision procedure

This is contract §2.2 restated as an algorithm, because the prose form hides the interactions.

Definitions. For output file *O*:
- `M(O)` = the ordered list of `-map` arguments given for *O*.
- `U` = the set of unlabeled output pads of all complex filtergraphs (global, not per-output).
- `blocked(O)` = the set of media types disabled by `-vn`/`-an`/`-sn`/`-dn` on *O*.
- `supported(O, k)` = true if *O*'s muxer can carry a stream of type *k*.

**Rule 1 (auto-selection trigger).** Automatic selection runs for output *O* and type *k* iff
`M(O)` is empty **and** no unlabeled complex-graph output of type *k* was assigned to *O*.

**Rule 2 (unlabeled complex outputs).** All unlabeled complex-filtergraph output pads are appended to the
**first** output file, in graph-declaration order, before any other selection is resolved. If the first
output's muxer cannot carry one of their types, that is a fatal error — there is no fallback to a later
output. Unlabeled pads are *not* suppressed by `-vn`/`-an`/`-sn`/`-dn`.

**Rule 3 (`-map` disables auto).** If `M(O)` is non-empty, automatic selection is fully off for *O* for
**all** types, not just the mapped ones. Unlabeled-pad streams from Rule 2 remain and are additive.

**Rule 4 (labeled complex outputs).** Every labeled output pad must be consumed by exactly one `-map
[label]`. Zero uses ⇒ error. Two uses ⇒ error.

**Rule 5 (automatic picks), applied per type in the order video, audio, subtitle:**
- video: across all inputs, the stream with the greatest `width × height`; ties broken by
  (input index, stream index) ascending. Attached pictures/`V`-excluded streams do not participate.
- audio: greatest channel count; ties by (input index, stream index).
- subtitle: the **first** subtitle stream in (input index, stream index) order whose *kind* (text vs
  bitmap) matches the kind produced by *O*'s default subtitle encoder.
- data and attachment: never auto-selected.
- Types not in `supported(O, k)` are skipped. Types in `blocked(O)` are skipped.

**Rule 6 (codec independence, with the subtitle exception).** `-c`/`-codec` is resolved after selection
and does not influence it — **except** that if any subtitle encoder is explicitly set on *O*, Rule 5's
kind-matching is abandoned and the first subtitle stream of *any* kind is selected. ffmpeg does not
pre-validate compatibility; an incompatible pair aborts every output at encoder-init time.

**Rule 7 (default encoder).** A selected stream with no `-c` uses *O*'s muxer's registered default encoder
for that type.

**Rule 8 (negative maps).** A `-map -SPEC` removes from the currently-accumulated mapping list every entry
whose source matches `SPEC`. It never adds. It is applied in command-line order, so
`-map 0 -map -0:a:1 -map 0:a:1` re-adds the stream.

**Rule 9 (optional maps).** A trailing `?` makes a zero-match map a silent no-op. An out-of-range
*input file index* remains a hard error even with `?`.

**Rule 10 (disposition defaulting).** If no `-disposition` is given for *O* at all, then for each type
with ≥2 output streams and none carrying `default`, `default` is set on the first. Streams fed by a
complex filtergraph do not inherit input dispositions.

**Decision procedure** (pseudocode, run once per output file after parsing):

```
fn resolve_output(O, inputs, graphs) -> Vec<OutputStream> {
    let mut out = vec![];
    if O.index == 0 {
        for pad in graphs.unlabeled_outputs() {                 // Rule 2
            require!(supported(O, pad.kind), "filter output of type {k} not supported by {muxer}");
            out.push(OutputStream::from_pad(pad));
        }
    }
    if O.maps.is_empty() {                                       // Rule 1 + 3
        for k in [Video, Audio, Subtitle] {
            if blocked(O).contains(k) || !supported(O, k) { continue }
            if out.iter().any(|s| s.kind == k && s.from_unlabeled_pad) { continue }   // Rule 1
            if let Some(src) = auto_pick(k, inputs, O) { out.push(OutputStream::from(src)) } // Rule 5/6
        }
    } else {
        for m in &O.maps {                                        // Rules 8, 9, 4
            match m {
                Map::Label(l)   => out.push(OutputStream::from_label(graphs.take_label(l)?)),
                Map::Negative(s)=> out.retain(|o| !o.matches_source(s)),
                Map::Positive(s)=> {
                    let hits = s.select(inputs);
                    if hits.is_empty() && !s.optional { bail!("Stream specifier matches no streams") }
                    for h in hits {
                        if blocked(O).contains(h.kind) { continue }
                        out.push(OutputStream::from(h));
                    }
                }
            }
        }
    }
    graphs.assert_all_labels_consumed()?;                        // Rule 4
    apply_default_dispositions(&mut out, O);                     // Rule 10
    out
}
```

Edge cases that need black-box verification before we call this done (§10-V6):
1. Two output files, a complex graph with unlabeled outputs, and `-map` **only** on the second output —
   does the first output still auto-select the types the graph did not supply?
2. `-vn` on an output that also receives an unlabeled video pad: contract says the pad wins. Confirm.
3. Video auto-pick tie-break when two inputs have equal resolution but the *second* input's stream comes
   earlier in absolute stream order.
4. Whether `-map 0:v?` on a file with no video and `-vn` interact (both suppress; no error either way).
5. Whether Rule 6's subtitle exception is triggered by `-scodec copy` or only by a real encoder name.

### 6.3 `-map` as a parser

```ebnf
map_arg ::= label_form | source_form
label_form  ::= '[' NAME ']'
source_form ::= [ '-' ] INT [ ':' stream_spec ] [ ':' view_spec ] [ ':' '?' ]
view_spec   ::= 'view' ':' ( INT | 'all' )
              | 'vidx' ':' INT
              | 'vpos' ':' ( 'left' | 'right' )
```

```rust
pub enum MapArg {
    Label(String),
    Source {
        negative: bool,
        file: u32,
        spec: StreamSpec,          // ALL when omitted
        view: Option<ViewSpec>,
        optional: bool,
    },
}
pub enum ViewSpec { Id(u32), All, Index(u32), Position(ViewPosition) }
pub enum ViewPosition { Left, Right }

pub fn parse_map(s: &str) -> Result<MapArg, ValueError>;
```

Parsing note: the view specifier and the `?` are appended after the stream specifier and share its `:`
separator, so `parse_map` must scan the segment list and pull off a trailing `view`/`vidx`/`vpos` segment
and a trailing `?` **before** handing the remainder to `StreamSpec::from_str`. Since `StreamSpec` has no
`view`/`vidx`/`vpos` segments, the split is unambiguous; a lone `?` segment likewise.

Semantics:
- Default view for transcoding is `vidx:0` (base view only).
- View specifiers are rejected on streamcopy — streamcopy always copies all views.
- Repeatable; a given input stream may be mapped several times.

### 6.4 The timestamp and sync model

This is the correctness-bug reservoir. We specify it as a pipeline of transformations with a fixed order,
so that "how do these compose" has one answer.

**Stage I — demuxer-side, per input file.**

```
raw_pts (stream tb)
  ├─ ×  itsscale[stream]                                (float, per-stream)
  ├─ +  itsoffset[file]                                 (fixed offset)
  ├─ +  isync_delta[file]                               (start-time delta vs the -isync source input)
  ├─ discontinuity correction:
  │     if format has AVFMT_TS_DISCONT:  threshold = dts_delta_threshold (10 s)
  │     else:                            threshold = dts_error_threshold (30 h)
  │     a jump > threshold is corrected (DISCONT formats) or the timestamp dropped (others)
  │     disabled when -copyts unless wraparound is detected
  └─ ▶ input_ts
```

**Stage II — input trimming.**

```
-ss / -sseof   : seek to the nearest seek point ≤ target.
                 -accurate_seek (default on): with transcode, decode-and-discard up to the exact target;
                 with streamcopy, or -noaccurate_seek, keep everything from the seek point.
-t / -to       : -t wins if both are given. Both are evaluated on input_ts.
-stream_loop   : after each loop, all subsequent timestamps are offset by the loop duration.
```

**Stage III — start-offset normalisation.**

```
if !copyts:            output_base = -(first input_ts across mapped streams of this input)
if copyts:             output_base = 0
if copyts && start_at_zero:
                       output_base = -(input file's own start_time)
                       # so `-ss 50 -copyts -start_at_zero` yields output starting at 50 s
```

`-isync` requires `-start_at_zero` when `-copyts` is set, cannot chain (a sync source may not itself be
synced), and defaults to `-1` = off.

**Stage IV — encoder-side rate/sync (`-fps_mode`), video only.**

| mode | behaviour |
|---|---|
| `passthrough` | timestamps forwarded unchanged; no dup, no drop |
| `cfr` | duplicate/drop frames to land exactly on `1/r` grid; `-r` sets `r` |
| `vfr` | forward timestamps; drop a frame whose timestamp equals the previous one |
| `auto` (default) | `cfr` if the muxer wants constant rate, else `vfr` |

`-frame_drop_threshold` (default `-1.1`, in frame-duration units) is how far *behind* a frame may be
before `cfr`/`vfr` drops it rather than emitting it. Negative default means a frame more than 1.1 frame
durations early relative to the target slot is dropped.

**Stage V — encoder timebase.**

`-enc_time_base`: `0` (default) = media default (`1/framerate` video, `1/samplerate` audio); `demux` =
the demuxer's timebase; `filter` = the filtergraph output timebase; or an explicit rational/decimal.
A coarse timebase is what makes `-force_key_frames` fire *earlier* than requested (§6.5).

**Stage VI — muxer-side.** `avoid_negative_ts` (a muxer AVOption, not a CLI option) may shift everything;
`-muxdelay`/`-muxpreload` bound the interleaving window; `-copytb` chooses the streamcopy timebase source
(`1` demuxer, `0` decoder, `-1` automatic).

**Composition rules we commit to and test:**

1. `-copyts` disables Stage III and disables discontinuity correction, but does **not** disable Stage IV
   or Stage VI. A `-copyts` output can still have its timestamps moved by `avoid_negative_ts` or by
   `fps_mode=cfr`.
2. `-ss` on the *input* side happens in Stage II (before normalisation), `-ss` on the *output* side is a
   decode-and-discard applied after Stage III. They compose additively and both may be given.
3. `-itsoffset` is applied before `-ss` matching, so `-itsoffset 5 -ss 10` seeks to source position 5.
   VERIFY (§10-V7).
4. `-t` on input and `-t` on output are independent limits; the smaller wins in practice.
5. `-shortest` operates on *output* stream end times after Stage V, via the sync queue (§7.5), not on
   input durations.
6. `-readrate` throttles Stage I ingestion in wall-clock terms only; it never changes a timestamp.
   `-readrate_initial_burst` allows N media-seconds ungated at start; `-readrate_catchup` (≥ the primary
   readrate) applies after a stall.

**Test matrix owed** (differential, `vaco-conformance`, framecrc-level comparison):
every pair from {`-ss`, `-sseof`, `-t`, `-to`, `-itsoffset`, `-itsscale`} × {`-copyts`,
`-copyts -start_at_zero`, neither} × {streamcopy, transcode} × {`fps_mode` ∈ passthrough/cfr/vfr/auto},
over an MP4 with a non-zero `start_time`, an MPEG-TS with a PTS wrap, and a file with a mid-stream
discontinuity. That is ~600 cases and it is the single highest-value test set in the whole project.

### 6.5 `-force_key_frames`

Four syntaxes on one option; the parser dispatches on prefix.

```rust
pub enum ForceKeyFrames {
    /// `time[,time…]`, with the `chapters[delta]` token expanded at bind time.
    Times(Vec<TimeSpecItem>),
    /// `expr:EXPR`
    Expr(vaco_expr::Expr),
    /// `source`
    SourceKeyframes,
    /// `scd_metadata`
    SceneChangeMetadata,
}
pub enum TimeSpecItem { At(i64 /*µs*/), Chapters { delta: f64 } }

pub fn parse_force_key_frames(s: &str) -> Result<ForceKeyFrames, ValueError> {
    if let Some(e) = s.strip_prefix("expr:") { return Ok(Expr(vaco_expr::parse(e)?)) }
    match s { "source" => Ok(SourceKeyframes), "scd_metadata" => Ok(SceneChangeMetadata),
              _ => Ok(Times(parse_time_list(s)?)) }
}
```

- **Times.** Each time is rounded to the nearest output timestamp in the *encoder* timebase
  (`-enc_time_base`). A coarse timebase can therefore force the keyframe **earlier** than asked. The
  `chapters[delta]` token (e.g. `chapters-0.1`) expands to every chapter start plus `delta` seconds and
  may be mixed with literal times: `0:05:00,chapters-0.1`.
- **Expr.** Evaluated per frame in `vaco-expr` with constants `n` (processed frame count), `n_forced`,
  `prev_forced_n` (NaN before the first), `prev_forced_t` (NaN before the first), `t` (frame time,
  seconds). Non-zero ⇒ force. `expr:gte(t,n_forced*5)` is the canonical case and a required test.
  Note `prev_forced_*` are NaN, not 0 — `gte(t, prev_forced_t+5)` is false on the first frame, which is
  the classic user-facing surprise; we reproduce it.
- **source.** Force when the *source* frame carried a keyframe flag. If that exact source frame is
  dropped (by `fps_mode` or a filter), the **next available** frame is forced instead — so the count of
  forced keyframes is preserved even though the positions shift.
- **scd_metadata.** Force on frames carrying the `lavfi.scd.time` metadata key. Requires the `scdet`
  filter upstream; frame-duplicating filters downstream of `scdet` produce duplicate metadata and
  spurious keyframes. We warn when we can detect that shape in the graph.

The `ForceKeyFrames` evaluator is a small state machine owned by the encoder node; it sets
`Frame.flags |= FORCE_KEYFRAME` and never talks to the encoder directly.

### 6.6 Filtergraph options

**Simple graphs** (`-filter:v`/`-vf`, `-filter:a`/`-af`) are per-output-stream, 1-in/1-out, with implicit
`in`/`out` labels. One graph is constructed per output stream, wired
`decoder → [auto-conv] → user graph → [auto-conv] → encoder`. `-s` on an output appends a `scale` filter
at the **end** of the graph; `-aspect`, `-pix_fmt` and the autoscale/autorotate machinery insert filters
at fixed positions. Multiple `-af` on one output means multiple graphs, one per instance.

**Complex graphs** (`-filter_complex`, `-lavfi`) are global, standalone, N-in/M-out, repeatable (each
occurrence is a separate graph).

Input link sources, in precedence order when resolving `[X]`:
1. `[file:stream_spec]` — the `-map` source grammar, first match if ambiguous, may carry a view specifier.
2. `[dec:N]` — loopback decoder N (see below).
3. An output label of another complex graph.
4. Unlabeled input pad ⇒ connects to the first unused input stream of the matching media type.

Output labels are consumed by `-map [label]`; unlabeled outputs auto-attach to the first output file
(§6.2 Rule 2).

Two complex graphs may not be merged if merging would create a transcode cycle. A cycle *across* two
separate `-filter_complex` invocations (graph A → encode → loopback decode → graph B) is legal; the same
cycle inside one invocation is not.

Special case retained from the contract: a **bitmap subtitle stream may be used directly as a complex
video input**, converted to video sized to the largest video stream, or 720×576 if there is none. The
contract marks this experimental/temporary; we implement it behind the same behaviour and flag it in
`docs/` as unstable.

**Loopback decoders.** `-dec OUTPUT_STREAM_INDEX` is a group-separator *directive*: it creates a decoder
that consumes the encoded output of the given output stream and exposes it as `[dec:N]`, numbered
0,1,2,… in declaration order. Its decoding AVOptions are the pending options placed *before* it, exactly
like input-file options precede `-i`. In `vaco-sched` this is an edge from an encoder node back into a
filtergraph node; it is the only cycle the DAG permits and it is what makes deadlock avoidance non-trivial
(§7.6).

**Auto-conversion.** `-auto_conversion_filters` (default on) inserts `scale` for pixel format/size/colour
mismatches and `aresample` for sample-format/rate/layout mismatches wherever negotiation requires it,
across all four filter options. `-noauto_conversion_filters` makes negotiation failure fatal. `-sws_flags`
sets the default flags for auto-inserted scalers; the graph-level `sws_flags=…;` prefix does the same for
one graph.

`-pix_fmt` has two special forms: a `+` prefix on a format name makes an unselectable format an **error**
rather than a warning *and* disables automatic conversions for that stream; a bare `+` forces the same
pixel format as the input/graph output with conversions disabled.

### 6.7 Exit codes, `-progress`, `-report`, stats

**Exit codes** (contract §2.16):

| code | meaning |
|---|---|
| 0 | success (internal `EXIT` normalised to 0) |
| 1 | usage error: no input files, or no output file (OBSERVED: `ffmpeg -i t.mp4` → 1) |
| 69 | `-max_error_rate` (default 2/3) exceeded; processing still completed |
| 255 | ≥1 termination signal received; >3 repeated signals force an immediate abort |
| other | the raw, OS-truncated return value of the transcode/option-parse path |

**`-progress`** writes `key=value` lines to a URL every `-stats_period` (default 0.5 s) and once at the
end. OBSERVED block, streamcopy of a 10-frame file:

```
frame=10
fps=0.00
stream_0_0_q=-1.0
bitrate= 175.4kbits/s
total_size=17540
out_time_us=800000
out_time_ms=800000
out_time=00:00:00.800000
dup_frames=0
drop_frames=0
speed=1.8e+03x
progress=end
```

Byte-level notes: `bitrate` has a leading space (a `%6.1f` field), `fps` is `%.2f`, `speed` is `%.3g`
with an `x` suffix, `out_time` is `HH:MM:SS.ffffff`, and **`out_time_ms` carries microseconds, not
milliseconds** — an upstream misnomer we reproduce exactly. `stream_N_M_q` is emitted per output stream
(file N, stream M). The final key of every block is `progress=continue` or `progress=end`.

**`-stats`** writes a `\r`-terminated line to stderr. OBSERVED (ffmpeg 8.1):

```
frame=   10 fps=0.0 q=-1.0 Lsize=      17KiB time=00:00:00.80 bitrate= 174.4kbits/s speed=78.6x elapsed=0:00:00.01
```

Field widths are fixed (`frame=%5d`, `fps=%3.1f`, `q=%4.1f`, `Lsize=%8s`, `bitrate=%6.1f`). Note
`elapsed=` — present in 8.1 but **absent from the contract document** (§10-V8). Since stats go to stderr
and are explicitly documented as unstable, we match the field set and ordering but do not treat stats as
a byte-identity target.

**`-report`** as in §2.9. **`-vstats`/`-vstats_file`/`-vstats_version`** produce the per-frame coding log;
v2 (default) adds `out=`/`st=` and an `f` suffix on `q=`; fields are
`out, st, frame, q, PSNR, f_size, s_size(kB), time, br(kbits/s), avg_br(kbits/s)`.
**`-stats_enc_pre/-stats_enc_post/-stats_mux_pre`** plus their `_fmt` variants take a `{directive}`
template; directives are `fidx sidx n ni tb tbi pts ptsi t ti dts dt sn samp size br abr key`, default
format `{fidx} {sidx} {n} {t}`.

---

## 7. `vaco-sched` — the pipeline scheduler

### 7.1 The runtime decision: OS threads + bounded channels, not tokio, not rayon

**Not tokio.** The workload is CPU-bound with a handful of long-lived stages, not IO-bound with thousands
of tasks. Async buys multiplexing of *waiting*; we have almost none. Against it: every decoder/encoder API
is a blocking `send/receive` loop that would have to run on `spawn_blocking` anyway; futures would force
`Send + 'static` on our frame/packet types and push `Arc<Mutex<…>>` into the hot path; the executor adds a
scheduling layer between us and the codec loop with no corresponding benefit; and `async fn` in traits
still complicates the component seams (architecture §5) that we want to stay plain. Network protocols
(`vaco-protocol-http` etc.) live behind the `Reader` trait at Layer 2 and can use blocking IO on their
own thread — that is where the only genuine IO concurrency lives, and it does not justify colouring the
whole pipeline.

**Not rayon for the pipeline.** Rayon is a work-stealing pool for divisible, non-blocking work. A pipeline
stage that blocks on a channel while occupying a rayon worker is exactly the deadlock rayon's docs warn
about, and with N stages ≥ pool size it is not a hypothetical. Rayon **is** the right tool for
architecture §6 axis 3 (data parallelism inside filters, scaling, resampling) and we use it there and
only there — with the invariant that no rayon closure ever performs a channel operation.

**Decision:** one dedicated OS thread per pipeline component, created inside a `std::thread::scope` so
frames may borrow from a scope-lived arena; components communicate over bounded MPMC channels
(`crossbeam-channel`, MIT/Apache-2.0, on the D3 allowlist). Backpressure *is* the bounded channel.
Thread count is (number of components) + the rayon pool; for a typical transcode that is under 10 blocking
threads, which is fine — they are almost always either working or blocked on a full/empty queue.

`-thread_queue_size` (input), `-max_muxing_queue_size` + `-muxing_queue_data_threshold` (output),
`-filter_buffered_frames`, and `-shortest_buf_duration` are the user-visible capacities of these channels.

### 7.2 The component DAG

```
Demux ──packets──▶ Mux                             (streamcopy edge)
  │
  └────packets──▶ Dec ──frames──▶ FilterGraph ──frames──▶ Enc ──packets──▶ SyncQueue ──▶ Mux
                                       ▲                    │
                                       └────[dec:N]─────────┘   (loopback decoder edge)
```

```rust
pub type NodeId = u32;
pub type PortId = u16;

pub enum Node {
    Demux(DemuxNode),      // 1 input url, N output ports (one per stream)
    Dec(DecNode),          // 1 in, 1 out
    Filter(FilterNode),    // N in, M out
    Enc(EncNode),          // 1 in, 1 out
    Mux(MuxNode),          // N in, 0 out
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct Port { pub node: NodeId, pub port: PortId }

pub struct Edge { pub src: Port, pub dst: Port, pub cap: usize }

pub struct Schedule {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// Sender/receiver halves, indexed by edge id.
    wires: Vec<Wire>,
    cancel: CancelToken,
    first_error: OnceLock<Error>,
}
```

The message envelope carried on every edge:

```rust
pub enum Msg {
    Packet(Packet),
    Frame(Frame),
    /// Parameter change: the downstream node must reconfigure (drives -reinit_filter/-drop_changed).
    Params(Box<StreamParams>),
    /// End of this port's stream. `ts` is the end timestamp used by -shortest.
    Eof { ts: Option<Timestamp> },
}
```

Errors do **not** travel on the data channel; they travel via `first_error` + `CancelToken` so that a
failing node cannot be blocked by a full downstream queue while trying to report.

### 7.3 The node contract

```rust
pub trait Component: Send {
    fn name(&self) -> &str;
    /// Run to completion. Returns Ok(()) on clean EOF, Err on failure.
    /// Implementations must return promptly when `io.cancelled()`.
    fn run(&mut self, io: &mut NodeIo<'_>) -> Result<()>;
}

pub struct NodeIo<'a> {
    inputs:  &'a [Receiver<Msg>],
    outputs: &'a [Sender<Msg>],
    cancel:  &'a CancelToken,
}

impl NodeIo<'_> {
    /// Blocking receive that also wakes on cancellation.
    pub fn recv(&mut self, port: PortId) -> Result<Option<Msg>, Cancelled>;
    /// Blocking send that also wakes on cancellation. This is the backpressure point.
    pub fn send(&mut self, port: PortId, m: Msg) -> Result<(), Cancelled>;
    /// Receive from whichever input is ready first. Used by muxer and multi-input filters.
    pub fn recv_any(&mut self) -> Result<(PortId, Option<Msg>), Cancelled>;
    pub fn cancelled(&self) -> bool;
}
```

Every blocking operation is a `crossbeam::select!` over the real channel and the cancel channel. There is
no other blocking primitive in the crate — no `Mutex` held across a send, no `park`, no sleep loop.

### 7.4 Flush and EOF ordering

The rule, uniformly: **a node that has seen `Eof` on all its inputs drains itself, then emits `Eof` on all
its outputs, then returns.**

- `Dec`: on input `Eof`, call `send_packet(None)` then `receive_frame()` until it reports empty. That is
  the drain that flushes B-frame reordering delay. Only then propagate `Eof`.
- `Filter`: on input `Eof` for port *p*, mark that pad EOF and let `vaco-filter-core`'s activate loop
  continue — filters like `overlay` legitimately keep producing after one input ends
  (`eof_action=repeat`). Only when the graph itself reports EOF on an output does that output emit `Eof`.
- `Enc`: on input `Eof`, `send_frame(None)`, drain packets, propagate. This is where a delayed encoder's
  tail packets come from and where losing them silently truncates output.
- `Mux`: writes the trailer only after every input port has produced `Eof` **and** the sync queue has
  released everything it holds. Writing a trailer early is the classic remux corruption bug; the muxer
  node asserts on it.

`Params` messages force a mid-stream reconfiguration: `-reinit_filter` (default on) rebuilds the graph,
losing filter state (`n` counters) and buffered frames; `-drop_changed` (default off) drops the changed
frames instead. Both are per-input-stream.

### 7.5 `-shortest` and the sync queues

Two distinct queues, both in `vaco-sched`:

```rust
pub struct SyncQueue {
    kind: SyncQueueKind,             // Packets (pre-mux) | Frames (pre-encode)
    streams: Vec<SqStream>,
    /// Bound from -shortest_buf_duration (default 10 s) or -max_muxing_queue_size.
    limit: SqLimit,
    /// Streams that determine the end time. Set by -shortest.
    limiting: BitSet,
    /// Once known: the earliest end timestamp among limiting streams.
    finish_at: Option<Timestamp>,
}
struct SqStream { queue: VecDeque<Msg>, head_ts: Option<Timestamp>, eof: bool, limiting: bool }
```

Behaviour:

- The queue releases a message only when it is the globally earliest across all non-EOF streams — this is
  also how muxer interleaving is enforced (`-muxdelay`/`-muxpreload` set the window).
- With `-shortest`, when the first limiting stream reaches EOF at time *T*, `finish_at = T`. Every other
  stream is then truncated at *T*, an `Eof { ts: T }` is synthesised for each, and the cancel token's
  "graceful" variant is signalled upstream so demuxers stop reading.
- The buffering `-shortest` requires is exactly the latency `-shortest_buf_duration` bounds: we cannot
  know *T* until a stream ends, so everything already produced past a candidate *T* must be held. Exceeding
  the bound is a fatal error with a message naming the option, not a silent stall.
- `-apad` on an output is equivalent to `-af apad` and only matters together with `-shortest` — it is
  rewritten into the simple audio graph at bind time.

`-fs` (file size limit) is checked in the muxer node after each write and is soft: the output may exceed
the limit by the size of the last packet.

### 7.6 Deadlock avoidance with loopback decoders

The `[dec:N]` edge makes the graph cyclic: `Filter → Enc → Dec → Filter`. A cycle over bounded channels
deadlocks if every buffer fills. Two mechanisms:

1. **Cycle detection at build time.** We compute the SCCs of the component graph. Any SCC with more than
   one node must contain exactly one `Enc → Dec` back-edge; anything else is rejected with
   "filtergraph cycle". This is the machine form of the contract's "two complex graphs cannot be merged if
   doing so would create a transcoding cycle".
2. **A designated slack edge.** The back-edge of each legal cycle is given a strictly larger capacity than
   the sum of the forward capacities in that cycle, and the `Dec` node on a back-edge is permitted to
   *drop* rather than block if the token is set — the loopback decoder is a feedback tap, not a data path
   whose completeness the output depends on. This turns a potential deadlock into bounded memory plus a
   diagnosable warning.

Both are covered by a dedicated test: a `-filter_complex` feedback graph run to completion with the
smallest legal channel capacities.

### 7.7 Error propagation and cancellation

```rust
pub struct CancelToken { flag: Arc<AtomicU8>, tx: Sender<()>, rx: Receiver<()> }
pub enum CancelKind { None = 0, Graceful = 1, Abort = 2 }
```

- Any node returning `Err(e)` does `first_error.set(e)` then `cancel.abort()`.
- `cancel.abort()` closes the broadcast channel, which wakes every `select!` in every node, which makes
  every `recv`/`send` return `Cancelled`, which makes every `run` return promptly.
- `Graceful` (used by `-shortest`, `-t`, `-frames`, `-fs`) stops demuxers but lets the rest drain, so the
  trailer is still written and the output is valid.
- SIGINT/SIGTERM set `Abort` from the signal handler (a single atomic store — async-signal-safe). The
  count is tracked; >3 signals calls `std::process::abort()`. Exit code 255.
- `main` joins the scope, then reports `first_error` if set, then maps to the exit codes of §6.7.
- `-xerror` promotes any error to `Abort`; without it, a decode error on one stream is counted toward
  `-max_error_rate` and processing continues.

There is no `panic = "unwind"` recovery: per architecture §8 the binaries build with `panic = "abort"`, so
a panic in a component is a bug that terminates the process, and the fuzzers (D6) are what keep components
panic-free.

---

## 8. `vaco-play`

### 8.1 Stack

winit (windowing/input) + wgpu (presentation) + cpal (audio out). No SDL, no libplacebo, no Vulkan-specific
path. `-enable_vulkan` and `-vulkan_params` are accepted and ignored with a one-line notice, because wgpu
already selects Vulkan/Metal/D3D12 per platform; we document the mapping rather than pretend the option
does nothing.

### 8.2 Threads

Reusing `vaco-sched` would be over-engineering for a player, so `vaco-play` has its own small topology:
a demux thread, one decode thread per active stream, the winit event loop on the main thread (a hard
requirement on macOS), and cpal's own callback thread. Between them: three bounded frame queues (video,
audio, subtitle) with the same `crossbeam` primitives.

### 8.3 The clock and sync model

```rust
pub struct Clock {
    /// pts - now, in seconds. Reading the clock is `pts_drift + now`, adjusted for speed.
    pts_drift: f64,
    last_pts: f64,
    last_update: f64,
    speed: f64,
    /// Bumped on every seek; frames with a stale serial are discarded.
    serial: u32,
    paused: bool,
}
impl Clock {
    pub fn get(&self) -> f64;
    pub fn set(&mut self, pts: f64, serial: u32);
    pub fn sync_to(&mut self, other: &Clock, threshold: f64);
}

pub enum MasterClock { Audio, Video, External }
```

Master resolution (`-sync`): requested `Audio` with no audio stream → `Video`; requested `Video` with no
video stream → `Audio`; neither → `External` (wall clock). The *effective* master is recomputed on every
stream switch (the `a`/`v` keys).

- **Audio master** (default): the audio clock is derived from the cpal callback. cpal's
  `OutputCallbackInfo::timestamp()` gives `callback` and `playback` instants, which is a materially better
  latency estimate than a queued-bytes heuristic; the audio clock is
  `pts_of_first_sample_in_buffer - (playback_instant - now)`. Video frames are then shown early or late
  relative to it.
- **Video master**: the audio path resamples to stretch/compress toward the video clock (the `aresample`
  async mechanism), and frame dropping is disabled by default.
- **External**: both media clocks are driven toward wall clock.

**Frame timing** per displayed frame:

```
diag  = video_clock - master_clock
delay = nominal_frame_duration
if |diag| < NOSYNC_THRESHOLD:
    if diag <= -sync_threshold:      delay = max(0, delay + diag)      # we are late: shorten
    elif diag >= sync_threshold:     delay = delay + diag              # we are early: lengthen
                                     (or 2*delay for small durations)
sleep until last_shown + delay
```

`NOSYNC_THRESHOLD`, `sync_threshold` and the frame-duplication cutoff are **our** tuning parameters. We
deliberately do not copy ffplay's constants (D7: implementation, not interface); we pick values from
measurement and validate the *behaviour* — "AV drift stays under X ms over a 10-minute file" — rather than
the constants.

**Frame dropping** (`-framedrop`, default on unless the master clock is video): when the next decoded
frame's presentation time is already behind the master clock by more than one frame duration, drop it
without presenting and continue. Late-frame drops are counted and reported in `-stats`.

**Buffering** (`-infbuf`, default on for realtime protocols): the demux thread normally stops reading when
the packet queues exceed a byte budget and a duration budget; `-infbuf` removes both bounds.

### 8.4 Presentation path, and how it compares to SDL

ffplay uploads decoded frames into an SDL texture. SDL's YUV-capable texture formats are YV12, IYUV, NV12,
NV21, UYVY and YUY2 — 8-bit only — so ffplay must run swscale on the CPU for anything else (10-bit HDR,
P010, planar RGB, 4:4:4 at high depth, alpha). That CPU pass is often the single largest cost in playing a
10-bit HEVC file.

Our path: upload the decoded planes as-is into wgpu textures (`R8Unorm` for 8-bit planes, `R16Uint` for
10/12/16-bit planes, `Rg8Unorm`/`Rg16Uint` for interleaved chroma) and do subsampling reconstruction,
matrix conversion, transfer-function handling and range expansion in one fragment shader parameterised by
the frame's `vaco-color` descriptor. Consequences:

- No CPU colour conversion for any format we can express as planes — a real advantage over SDL.
- Correct H.273 handling (BT.2020 matrices, PQ/HLG transfer) becomes a shader uniform rather than a
  swscale invocation, so HDR tone-mapping is available without libplacebo.
- Costs: a WGSL shader per plane-layout family (roughly six), and a first-frame pipeline compile of a few
  milliseconds, which we hide behind the initial buffering.
- Present mode: prefer `Mailbox`, then `Immediate`, then `Fifo`. Under `Fifo` the compositor quantises
  presentation to vsync and our clock loses sub-frame control, so we detect it and widen the sync
  thresholds accordingly.
- `-nodisp` skips window creation entirely and runs the audio path only.

Formats we cannot express as plain planes (hardware surfaces from `vaco-hw-*`) go through the hardware
crate's own zero-copy import where the platform supports it, and fall back to a download + upload
otherwise.

### 8.5 Display modes

`-showmode` ∈ `0`/`video`, `1`/`waves`, `2`/`rdft`; default `video`, falling back to `rdft` when there is
no video stream or the video cannot be played. `w` cycles modes and also cycles through multiple `-vf`
graphs when several were supplied.

- **waves**: the last N audio samples rendered as a waveform, drawn into a small RGBA texture on the CPU
  and uploaded (it is tiny; a shader is not worth it).
- **rdft**: a real DFT over a sliding window (`vaco-tx`'s RDFT), magnitudes mapped to a colour ramp per
  frequency band, presented as a scrolling spectrogram. `vaco-tx` already exists for the audio codecs, so
  this costs a window function and a colour map.

### 8.6 Options

Main: `-x`, `-y`, `-fs`, `-an`, `-vn`, `-sn`, `-ss`, `-t`, `-bytes`, `-seek_interval` (default 10),
`-nodisp`, `-noborder`, `-alwaysontop`, `-volume` (0–100, clamped), `-f`, `-window_title`, `-left`,
`-top`, `-loop`, `-showmode`, `-vf`, `-af`, `-i`.

Advanced: `-stats`/`-nostats`, `-fast`, `-genpts`, `-sync`, `-ast`/`-vst`/`-sst` (full stream specifiers),
`-autoexit`, `-exitonkeydown`, `-exitonmousedown`, `-codec:a|v|s`, `-acodec`/`-vcodec`/`-scodec`,
`-autorotate`/`-noautorotate`, `-framedrop`/`-noframedrop`, `-infbuf`/`-noinfbuf`, `-filter_threads`,
`-video_bg` (colour name/code, or `tiles` (default checkerboard) or `none`), plus `-enable_vulkan` and
`-vulkan_params` as accepted no-ops.

Default stream selection is the "best stream" heuristic **scoped to the currently selected program**, which
is why `c` (cycle program) changes what `a`/`v`/`t` cycle through.

### 8.7 Bindings

| Key / action | Effect |
|---|---|
| `q`, `Esc` | Quit |
| `f` | Toggle fullscreen |
| `p`, `Space` | Pause |
| `m` | Toggle mute |
| `9` / `0` | Decrease / increase volume |
| `/` / `*` | Decrease / increase volume |
| `a` | Cycle audio stream (within the current program) |
| `v` | Cycle video stream |
| `t` | Cycle subtitle stream (within the current program) |
| `c` | Cycle program |
| `w` | Cycle video filters / show modes |
| `s` | Step to next frame (pauses first if not paused) |
| Left / Right | Seek −/+ 10 s (or `-seek_interval`) |
| Down / Up | Seek −/+ 1 minute |
| Page Down / Page Up | Previous / next chapter; −/+ 10 minutes if the file has no chapters |
| Right mouse click | Seek to (click x ÷ window width) of the duration |
| Left mouse double-click | Toggle fullscreen |

With `-bytes`, all seek keys operate on byte offsets. Seeks bump `Clock::serial`, and every queue is
flushed of stale-serial entries.

---

## 9. Milestones and effort

Estimates are engineer-weeks for one experienced Rust engineer, assuming the lower layers named as
dependencies already exist. "Test" weeks are included in the figure.

### v0.1 — `vaco-probe`, byte-identical (D5)

| Work item | Weeks | Depends on |
|---|---:|---|
| `vaco-cli-core`: lexer, grouping, descriptors, value grammars | 3 | `vaco-core`, `vaco-expr` |
| Stream-specifier parser + matcher + fuzz target | 1.5 | — |
| `vaco-opts` integration: deferred options, `apply_recognised`, audit | 1 | `vaco-opts` |
| Help system: `-h`, `-h long/full`, `-h <kind>=<name>`, listing commands | 1.5 | `vaco-registry` |
| `vaco-textformat`: schema, `TextFormat` façade, `default` + `compact`/`csv` | 2 | — |
| `vaco-textformat`: `flat`, `ini`, `json`, `xml` (incl. `q`/`x` modes) | 2 | — |
| `num` module + unit/prefix/sexagesimal + the field table generator | 1.5 | — |
| `vaco-probe`: option surface, section emitters for every section | 3 | demuxers |
| `-show_entries`, `-read_intervals`, `-select_streams` parsers | 1 | — |
| Conformance harness + corpus + the full acceptance matrix | 3 | D6 harness skeleton |
| Docs (`docs/cli/*`) | 1 | — |
| **Total** | **20.5** | |

Exit criteria: §5.6's matrix passes with an empty gap list for the three container families in D5.

### v0.2 — `vaco` remux

| Work item | Weeks |
|---|---:|
| `vaco-sched` core: DAG, wires, node contract, cancellation, EOF ordering | 4 |
| Sync queue + interleaving + `-shortest` (packet mode) | 2 |
| `-map` parser + stream selection rules §6.2 + decision-procedure tests | 2.5 |
| Timestamp model stages I–III, VI (streamcopy path) | 3 |
| Metadata/disposition/chapter/program mapping options | 2 |
| `-progress`, `-stats`, `-report`, exit codes | 1 |
| Differential remux tests (container bytes where deterministic) | 2.5 |
| **Total** | **17** |

### v0.3 — `vaco` transcode

| Work item | Weeks |
|---|---:|
| Decoder/encoder nodes, drain semantics, `-frames`, `-pass` | 3 |
| Simple filtergraph binding, auto-conversion, `-s`/`-aspect`/`-pix_fmt` placement | 3 |
| Timestamp stages IV–V: `-fps_mode`, `-enc_time_base`, `-frame_drop_threshold` | 3 |
| `-force_key_frames` (all four syntaxes) | 1 |
| `-shortest` frame mode, `-apad`, `-isync` | 1.5 |
| The ~600-case timestamp differential matrix (§6.4) | 3 |
| **Total** | **14.5** |

### v0.4 — complex graphs, loopback, graph dumps

| Work item | Weeks |
|---|---:|
| `-filter_complex`/`-lavfi`, link-label resolution, unlabeled-pad rules | 3 |
| Loopback decoders `[dec:N]`, cycle detection, slack-edge deadlock avoidance | 2 |
| `-print_graphs*` incl. mermaid/mermaidhtml writers in `vaco-textformat` | 1.5 |
| `-stream_group`/IAMF grammar, `-reinit_opts`, `-target` | 2.5 |
| **Total** | **9** |

### v0.5 — `vaco-play`

| Work item | Weeks |
|---|---:|
| winit + wgpu presentation path, shader family, present-mode handling | 3 |
| cpal audio out, ring buffer, audio clock from callback timestamps | 2 |
| Clock/sync model, frame dropping, buffering, seek + serials | 2.5 |
| waves/rdft display modes | 1 |
| Option surface + full binding table + stats overlay | 1.5 |
| **Total** | **10** |

### Beyond

Presets, hardware device options, `-analyze_frames`, `-show_log`, device sources/sinks, `-sdp_file`, and
the remaining expert options: ~6 weeks, scheduled opportunistically.

**Layer total: ~77 engineer-weeks.** The critical path for v0.1 is `vaco-cli-core` → `vaco-textformat` →
`vaco-probe`, and the two parser crates can be built in parallel with the demuxers.

---

## 10. Register of behaviours needing black-box verification

Each item names the divergence or uncertainty, the test that settles it, and the default we ship until
then. All tests live in `crates/tools/vaco-conformance/tests/` and run against the reference binary.

| ID | Question | Test | Interim behaviour |
|---|---|---|---|
| V1 | Contract §3.5 lists root children as `chapters, format, frames, …`; ffprobe 8.1 emits `program_version, library_versions, pixel_formats, packets, frames, packets_and_frames, programs, stream_groups, streams, chapters, format, error`. Also `packets_and_frames` is absent from `-sections`. | Run the reference with every `-show_*` at once and diff the child order; separately, find an invocation that produces `packets_and_frames` and check whether `-sections` lists it. | Follow the observed/XSD order. |
| V2 | Contract §3.4 says `section=` still emits the section header; observed, the default writer emits **nothing** for `-show_entries 'format='`. | Compare all six writers for `-show_entries 'format='` and `'stream='`. | Follow the observed binary (no header for `default`; empty container for json/xml/ini). |
| V3 | Does `/`-file-indirection strip a trailing newline from the file contents? | Two files with identical content differing only in a trailing `\n`, used as `-/metadata:g title`; compare the muxed metadata. | Strip a single trailing `\n`. |
| V4 | In `m:key:value`, does `\\` mean a literal backslash? Is a trailing lone `\` an error? Is comparison case-sensitive? | Files with tags containing `\`, `:` and mixed case; probe with each specifier form. | `\X` → literal `X` for any `X`; trailing `\` is an error; comparison case-sensitive. |
| V5 | `-byte_binary_prefix` produces decimal prefixes in ffprobe 8.1 (2 097 152 B → `2.097152 Mbyte`). | Sweep of sizes across the 1024 boundaries with and without the flag, on several ffprobe builds. | Accept the flag; it affects only `xsd_strict` validation. Revisit if upstream fixes it. |
| V6 | The five stream-selection edge cases in §6.2. | Five scripted `ffmpeg` runs whose `-map`/graph output stream list is captured with `ffprobe` and compared. | As written in §6.2. |
| V7 | Does `-itsoffset` apply before or after `-ss` matching? | `-itsoffset 5 -ss 10` on a file with per-second visual markers; identify the first output frame. | Before (`-ss` seeks the offset timeline). |
| V8 | `-stats` in 8.1 emits an `elapsed=` field absent from the contract. Also which fields appear on streamcopy vs transcode. | Capture `-stats` for both paths across two reference versions. | Match 8.1's field set; treat stats as non-byte-identical. |
| V9 | Which sections use `DefaultStyle::InlinePrefix` in the default writer. Observed: `tags`→`TAG`, `disposition`→`DISPOSITION`; `side_data`→`Header`. No `-sections` flag distinguishes them. | Corpus reaching every section; scrape the default-writer output; assert against the table. | Table populated by observation; conformance test guards it. |
| V10 | Does `parse_number` accept the full `vaco-expr` grammar (function calls, `st`/`ld`) or only arithmetic? Observed: `2*1000` is accepted. | Feed `-b:v 'if(1,1000,2000)'`, `-b:v 'st(0,5);ld(0)*100'`. | Full `vaco-expr` grammar. |
| V11 | JSON escaping of control characters (``) and of a lone surrogate in a tag. | Matroska file with a tag containing `\x01` and invalid UTF-8; all six writers × three `sv` modes. | `\u00XX` for C0 controls; `sv=replace` for invalid UTF-8. |
| V12 | The `<stream >` trailing-space quirk: does it appear for every attribute-less element, or only for some? | Emit every section with an empty entry filter under `-of xml`. | Reproduce for every attribute-less element. |

Anything in this table that is still unresolved when its milestone closes becomes an entry in
`conformance/known-gaps.toml` with an owner, per §5.6.

---

## 11. Documentation deliverables

Per the repository standard, each of these lands with the code, and `docs/README.md` indexes them:

`docs/cli/option-parsing.md`, `docs/cli/stream-specifiers.md`, `docs/cli/option-values.md`,
`docs/cli/help-system.md`, `docs/textformat/writers.md`, `docs/textformat/section-schema.md`,
`docs/textformat/byte-identity.md`, `docs/probe/options.md`, `docs/probe/conformance.md`,
`docs/vaco/stream-selection.md`, `docs/vaco/timestamps-and-sync.md`, `docs/vaco/filtergraphs.md`,
`docs/vaco/progress-and-exit-codes.md`, `docs/sched/architecture.md`, `docs/sched/sync-queues.md`,
`docs/play/architecture.md`, `docs/play/sync-model.md`, `docs/play/bindings.md`.

---

## Corrections from implementation (vaco-textformat, 2026-08-22)

Established by capturing 120 real `ffprobe` 8.1 outputs across six scenarios and
twenty writer/option specs, replaying them through the API, and comparing byte for
byte. All 120 match. Where this document and the binary disagreed, the binary won.

**§4.3's `ini` rule is wrong.** It says a blank line precedes every section
header. The real behaviour is two rules: a `[path]` header gets a blank line
before it *unless the previous line written was also a header*; and a section that
produced no output writes one blank line when it closes. The document generalised
from a single `od -c` capture that happened to be the second case.

**§4.4's sexagesimal negatives are wrong.** It says negatives take a leading `-`.
They do not: −0.02322 s prints `0:00:-0.023220`, the sign landing on the seconds
component, out of truncating division plus `%09.6f`. We reproduce it.

**§4.3's `xsd_strict` refusal set is wrong.** It refuses `unit` and `prefix` only;
`-byte_binary_prefix` and `-sexagesimal` are accepted.

**§4.1's compact type sanitisation is wrong.** Per character, not per run — the
document's own `h_26_45__user_data` example proves it, since `] ` becomes two
underscores.

**Research §3.3's `string_validation_replacement` default is wrong.** It says empty
string; 8.1 substitutes U+FFFD when the option is untouched. `svr=` explicitly
does delete.

**§4.5 on empty arrays is wrong, and the real cause is more interesting.**
`"programs": []` appears under `auto`, not only under `always`. The cause is the
entry filter: `-show_entries` matches **local** names as well as unique ones, and
`stream` is also the local name of `program_stream` and `stream_group_stream`, so
those root arrays get opened.

**§4.1 claims `default_style` is underivable. It is derivable** — not from the
section's own flags, but from its parent's: a section gets a header iff its parent
is the root or an array. `compact` differs on exactly one point, inlining every
variable-field section.

**The escaping tables were incomplete.** `compact e=c` also escapes `\b` and `\f`
and does *not* escape tab or VT; `flat` also escapes `\n`/`\r`; `ini` and `json`
escape remaining C0 as `\x00NN` / `\u00NN`. `xml` is the only writer where
`sv`/`svr` does anything.

**Confirmed as documented:** JSON number-versus-string is per-field (and so is
`flat`); `-byte_binary_prefix` is a no-op in 8.1; unit/prefix prints integers bare
otherwise six decimals — except `Unit::Second`, which never collapses (4000 s is
`4.000000 Ks` while 1000 bytes is `1 Kbyte`). `<stream >` with a trailing space is
real, and refined: attribute-capable sections open `<name ` and close `/>` or `>`;
arrays and variable-field sections open `<name>` with no space and never
self-close.

### Interface deviations, for review
- `section_footer` takes an extra `produced: bool` — the `ini` blank-line rule
  cannot be implemented without it, and only the façade knows.
- `TextFormat::new` drops the `schema` parameter (there is one static schema).
- `DefaultStyle` gained `Transparent` for the root and arrays.

### Outstanding
`element_name` is unverified for six sections — the `stream_group` component
family and the frame-side-data component family. Nothing reachable through `lavfi`
produces an IAMF stream group, and `-show_frames` is v0.2 (D14.4). They carry
placeholders, are marked in `sections.rs`, and affect only `xml` and `compact`.
