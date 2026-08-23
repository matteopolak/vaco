# `vaco-registry`

## What it is

The one place that knows which demuxers, muxers, decoders, filters and
protocols a build contains. Everything above layer 6 — `vaco-probe`, `vaco-cli`,
`vaco-sched` — asks here instead of naming a component crate, which is what
keeps the crate graph a fan-in rather than a mesh.

It is **generated**. `crates/registry/vaco-registry/src/generated.rs` and the
delimited region at the end of `Cargo.toml` are both written by
`cargo xtask gen-registry` from the `vaco-component.toml` fragment each
component crate ships. No agent writes either file, so ~120 crates can register
themselves with zero contention on a shared working tree (plan 19 §3.4).

## How it works

### The fragment

Each component crate ships one file, in its own directory:

```toml
# crates/format/vaco-demux-mp4/vaco-component.toml
[[component]]
kind       = "demuxer"
name       = "mov,mp4,m4a,3gp,3g2,mj2"
long_name  = "QuickTime / MOV"
feature    = "demux-mp4"
ctor       = "vaco_demux_mp4::DEMUXER"
extensions = "mov,mp4,m4a,3gp,3g2,mj2,psp,m4b,ism,ismv,isma,f4v,avif,heic,heif"
mime_types = "video/mp4,audio/mp4,application/mp4,video/quicktime,…"
```

`kind`, `name` and `ctor` are always required; everything else is per-kind. The
`kind` vocabulary is `demuxer`, `muxer`, `decoder`, `encoder`, `parser`,
`filter`, `protocol`, `bitstream_filter`. Lists are one comma-separated string,
as the reference spells `AVInputFormat::extensions`.

`ctor` names a `const`/`static` **descriptor**, never a function. That is the
load-bearing part: `-demuxers`, `-codecs` and `-h demuxer=mp4` must print
capabilities without opening a file or allocating a decoder.

### Two additions to the frozen schema

Both are extensions, not changes — nothing an existing fragment uses moved.

| Key | Meaning |
|---|---|
| `default` | `true` by default. `default = false` keeps the feature out of the registry's `default` feature list, which is the D4 opt-out for anything patent-encumbered. It requires a `feature`, since an always-on component is in every build by definition. |

And one rule the schema implied but did not state: **`feature` may be omitted**,
which makes the component always-on and its crate a non-optional dependency. A
crate with one featured and one always-on component gets a non-optional edge,
because the edge has to satisfy the strictest component in it.

### What the generator emits

`src/generated.rs`:

* `COMPONENTS` — one [`Component`] metadata row per fragment table, ordered by
  `(kind, name, crate)`. This is the *listing* surface: `-formats`, `-codecs`,
  `-demuxers` render exactly these rows, in exactly this order, so two builds
  with the same feature set list components identically (D6 compares the listing
  output byte for byte).
* One typed table per kind that has a descriptor type: `DEMUXERS`, `MUXERS`,
  `DECODERS`, `PARSERS`, `FILTERS`, `PROTOCOLS`.
* For `encoder` and `bitstream_filter` — kinds with no descriptor type yet — a
  `const _: () = { let _ = &<ctor>; };` block. Taking a reference needs no trait
  bound, so the path is checked at compile time even though the type is unknown.
  A typo in a fragment is a compile error rather than a component that silently
  is not there.

`parser` was in that second group until `vaco-codec-core` grew
[`ParserDesc`](../signal/vaco-codec-core.md). Promoting it was one row in
`KINDS` in `xtask/src/registry.rs` plus a `desc_ty`/`table` arm — which is what
the "to add a kind" note below describes, executed.

Every row carries the `#[cfg(feature = …)]` its fragment named.

The `Cargo.toml` region:

```toml
# BEGIN GENERATED — `cargo xtask gen-registry`. Do not edit by hand.
[features]
default = ["demux-mp4"]
"demux-mp4" = ["dep:vaco-demux-mp4"]

[dependencies.vaco-demux-mp4]
path = "../../format/vaco-demux-mp4"
optional = true
# END GENERATED
```

Generating the manifest half matters more than it looks. Without it,
registering a component would still need a hand edit to a file shared by every
component author — the contention plan 19 §3.4 exists to remove, moved one file
along. A *delimited region* rather than a whole generated manifest keeps the
hand-written half (package metadata, the always-on `-core` dependencies, lints)
reviewable, and means the generator can only ever damage its own text.

A generated path dependency naming a directory that does not exist would fail
manifest parsing for the entire workspace. That cannot happen here by
construction: fragments are found by walking crate directories, so every crate
named in the output was seen on disk a moment earlier.

`default` lists every feature that did not opt out, so a component can never be
silently absent from a default build — the same rule `gen-fuzz` applies to its
targets, and for the same reason: a wave lost three crates' fuzz targets to a
`default` line that silently dropped entries.

### The TOML reader

`xtask` is dependency-free by design, so `xtask/src/registry.rs` carries about
150 lines of TOML reader. It handles exactly the frozen schema — `[[component]]`
headers, bare keys, double-quoted strings with `\n \t \r \" \\`, bare
`true`/`false`, `#` comments outside strings — and **rejects** everything else
rather than guessing. An array value, a nested table, a number, a duplicate key
and an unknown key are each a named error pointing at a line, because a fragment
the reader half-understands would register the wrong thing rather than nothing.

Cross-crate name collisions are rejected too, per alias: two crates registering
a demuxer that both answer to `mp4` would make lookup order decide which one
`-f mp4` selects.

## How to change it

**To register a component:** write `vaco-component.toml` in your own crate and
run `cargo xtask gen-registry`. Do not edit `generated.rs` or the generated
manifest region; CI runs the generator with `--check` and fails on a difference.
Running a generator is not editing its output (plan 19 §3.6).

**To add a kind with a typed table:** the descriptor type has to exist in the
trait layer first. Then add a row to `KINDS` in `xtask/src/registry.rs` with its
`Kind` variant, teach `Kind::desc_ty` and `Kind::table` about it, and flip
`vaco_registry::Kind::has_table`.

**To add a fragment key:** add it to `KEYS` and to `build()`. Adding a key is
safe; changing what an existing key means is the same freeze the trait layers
get, and needs the orchestrator.

## Configuration

Cargo features only, and all of them are generated.

* `default` — every component whose fragment did not say `default = false`.
* `--no-default-features` — empty tables, no component crate compiled at all.
  Every accessor is total on an empty registry; `tests` in `lib.rs` assert that,
  because an accessor that only works when something is registered is a latent
  panic in exactly this build.

## The parser provider

`Parsers` is the crate's one piece of *behaviour* rather than lookup, and it is
the seam D14.1 exists for.

```rust
let parser: Option<Box<dyn Parser>> = vaco_registry::Parsers.parser_for(CodecId::H264);
```

`vaco-demux-mp4` needs an H.264 sequence parameter set to report `profile`,
`pix_fmt` and `has_b_frames`, and `cargo xtask layer-check` forbids it from
depending on `vaco-parse-h264`. So it asks by `CodecId` and this answers from
`PARSERS`. The demuxer names no codec crate; a `--no-default-features` build
gets `None` and reports what the container itself states.

Three properties are load-bearing:

* **The budget is chosen here.** `ParserProvider::parser_for` takes no `Limits`
  — the trait is frozen — and a parser on the probe path is handed
  attacker-controlled bytes before anything has validated them. `Parsers` builds
  with `Limits::strict()`, the same default `vaco_format_core::Discovery`
  applies to the driver wrapped around it, so the two agree without either
  knowing about the other.
* **`Parsers` stays a unit struct.** Making it carry a `Limits` field would be a
  source-breaking change for every existing `&vaco_registry::Parsers`, and both
  binaries have one. When something needs a different budget, add a second
  provider that carries one rather than re-shaping this.
* **`ParserDesc::make` is a `fn` field**, so a descriptor is inspectable without
  constructing anything — the same rule every other `ctor` follows.

`fuzz/fuzz_targets/registry_discovery.rs` is the target for the composition:
arbitrary bytes through a real demuxer into a real parser, which is the one path
no per-crate target covers (`dem_mp4` and friends all run with `NoParsers`, by
design).

## Dependencies

The five `-core` crates that define the descriptor types (`vaco-core`,
`vaco-codec-core`, `vaco-format-core`, `vaco-filter-core`,
`vaco-protocol-core`), `vaco-limits` for the budget `Parsers` builds with, plus
one optional path dependency per component crate, generated.

## A note for anyone writing another generator

`gen-registry` pipes its Rust through `rustfmt`, and that is not cosmetic. The
committed `generated.rs` was rustfmt-unstable at first, and a single
`cargo fmt -p vaco-registry` was enough to make `cargo xtask gen-registry
--check` report the file stale — so every contributor would have had a formatter
that silently rewrites a generated file and a CI gate that fails immediately
afterwards, with neither one obviously at fault.

`gen-pixfmt` already took this route. Any generator in this tree that emits Rust
needs it, and `--check` should be run **after** a `cargo fmt` at least once to
prove the output is a fixed point.

## Known gaps

Each is a missing piece elsewhere, reported rather than worked around.

* **No `EncoderDesc` or `BitstreamFilterDesc`.** `vaco-codec-core` defines
  `DecoderDesc` and `ParserDesc`, so two of the eight kinds get a metadata row
  and a compile-time path check but no typed table. `Kind::has_table` reports
  which.
* ~~**`ParserProvider` returns `None` for every codec.**~~ Closed. The gap was
  exactly one descriptor type, as the comment on `Parsers` predicted — but the
  prediction was half the work. A `ParserDesc` alone gets a parser built and
  still describes nothing in MP4 or Matroska, because in those containers the
  H.264 sequence parameter set is in `avcC` and in **no packet at all**. The
  missing half was `Parser::set_extradata`, and it is the half that carries the
  fields: measured on `av.mp4`, 8 of 8 bitstream-derived stream values arrive
  through the record and 0 through the packet path.
* **No priority on `DemuxerDesc`, `DecoderDesc` or `ParserDesc`.** `decoder_for` returns the
  first enabled implementation in registry order, and `parser_desc_for` does the
  same; both are deterministic but arbitrary once two implementations of one
  codec exist. `vaco-format-core`
  reports the same gap for probe tie-breaking.
* **`MuxerDesc` has no `mime_types`.** The fragment carries them for every kind,
  so the metadata row is complete; the descriptor is not.
* **`vaco-protocol-file` ships no fragment**, so `protocol_registry()` is empty
  and `vaco-probe` has to register `file:`/`pipe:` itself. That crate owes a
  fragment.
