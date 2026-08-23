# `vaco-format-ebml`

Layer 4. RFC 8794 (EBML) alone: variable-length integers, the element
header, a reader over an in-memory slice and over a seekable stream, and a
writer for both. It knows nothing about Matroska or any other format built
on EBML.

Issue #575 (FM-25, Matroska mux), epic #20.

## What it is

The generic half of the EBML layer that used to live entirely inside
`vaco-demux-matroska::ebml`. That crate's own module docs said, before this
one existed, that the layer was "kept behind a module boundary ... so that
it can be promoted to `vaco-format-ebml` unchanged if a Matroska muxer ...
wants it" — `vaco-mux-matroska` is that muxer, and this crate is the
promotion, done exactly as predicted: the VINT codecs, `Header`/`Size`/
`Caps`, the `Slice` reader, the RFC 8794 §7 value accessors, `read_header`,
and the mechanical half of the open-element stack moved here verbatim.
`vaco-demux-matroska::ebml` now re-exports them under the same names, so
none of that crate's 76 pre-existing tests (or its `matroska_ebml` fuzz
target) needed to change.

## How it works

### The split: EBML grammar here, schema elsewhere

RFC 8794 defines the VINT grammar, the element header, and the unknown-size
termination rule (§6.2). It does **not** define which element IDs exist or
which element may legally sit inside which — that is a property of whatever
format is built on top (Matroska's own element tree, RFC 9559 §5). This
crate's boundary follows that split exactly: every function here operates on
a bare `u32` element ID and never asks what it means.

`stack::Stack::terminations_for` is the clearest example. The mechanism —
walk outward from the innermost open frame, popping unknown-size frames
until one admits the new ID as a legal child, or a root element closes
everything — is generic. The answer to "is `id` a legal child of `parent`"
is supplied by the caller as a closure:

```rust
stack.terminations_for(id, ROOT, |child, parent| schema::is_child_of(child, parent), schema::is_root)
```

`vaco-demux-matroska::ebml::Stack` is a thin wrapper that closes over its own
`schema::is_child_of`/`schema::is_root`, so every call site there still
writes the old one-argument `stack.terminations_for(id)`.

### Two readers, matching two shapes of element

| Reader | Input | Used for |
|---|---|---|
| `reader::Slice` | `&[u8]` already in memory | any bounded master read whole: `Info`, `Tracks`, `Cues`, `Tags` |
| `reader::read_header` | `vaco_io::IoContext` | one element header at the stream's current position |

Bounded masters are read whole and walked in memory because that is both
simpler and faster. The streaming path exists for an element that may be of
unknown size and arbitrarily large — a Matroska `Cluster` is the motivating
case on *both* sides of this crate: `vaco-demux-matroska` reads one that
way, and `vaco-mux-matroska` — see its own docs — measured that the
reference does **not** write one that way, buffering a whole `Cluster`
before emitting it, so the streaming writer path (`writer::write_header`/
`write_header_unknown`) is used only for `Segment`, whose size genuinely
cannot be known until every packet has been written.

### The writer mirrors the reader, one level up

`writer::element`/`write_uint`/`write_int`/`write_float`/`write_string`/
`binary` each build one complete, self-contained element into an owned
`Vec<u8>` — ID, shortest-VINT size, body — for a master small enough to
assemble before its size is known. `writer::write_header`/
`write_header_unknown`/`patch_known_size` instead write just the ID and
size octets directly to an `IoWriter`, for a master too large to buffer,
whose caller streams the body and either patches the size afterward (a
seekable sink) or leaves the RFC 8794 §6.2 unknown-size marker in place (a
non-seekable one).

### The two RFC 9559 lacing functions that live here anyway

`vint::read_signed_vint`/`vint::signed_vint` implement RFC 9559 §10.3.3's
"the unsigned VINT read normally, then biased by `2^(7n-1) - 1`" — which is
a Matroska *use* of the generic VINT grammar, not a new one. They live
beside `read_size`/`vint_min` rather than in a Matroska-specific crate
because the bias arithmetic has nothing to do with element IDs and
everything to do with the VINT width RFC 8794 already defines; only the
*meaning* of the resulting number (a lace's frame-size delta) is Matroska's.

## How to change it

- **Adding a new accessor or VINT variant** goes in `vint.rs` (encode/decode)
  or `reader.rs` (in-memory accessors); keep the split between "decodes a
  byte shape" and "decodes a byte shape *and* knows what the value type
  means" — the former belongs here, the latter does not.
- **`stack::Stack`'s frame ceiling** (`Stack::MAX_FRAMES`, currently 16, tied
  to `element::MAX_DEPTH`) is a workspace-wide default for "how deep may an
  EBML-based format legally nest", chosen because Matroska's own recursive
  elements (`SimpleTag`, `ChapterAtom`) are the deepest known case. A future
  EBML-based format needing more nesting should track its own counter
  alongside this type rather than raising the shared constant.
- **Gotcha: `id_bytes` is not a general "shortest VINT for an arbitrary
  `u32`" function.** It classifies a value's octet width from the numeric
  ranges a *real* marker-bearing element ID falls into (RFC 8794 §5's Class
  A/B/C/D), which only agrees with the strict per-octet marker rule for
  values that already carry a correctly placed marker bit — exactly the
  Matroska element ID constants it is meant to re-encode. Feeding it an
  arbitrary integer that was never built with `vint`/a real ID's bit pattern
  can pick the wrong width. `tests/vint_proptest.rs`'s `element_id_round_trips`
  documents this by construction: it builds IDs with `vint(value, len)`
  rather than drawing an arbitrary `u32`.
- **Gotcha: the all-ones VINT is reserved.** `vint_min` steps up a width
  before it would ever emit the all-ones pattern at that width, precisely
  because that pattern means "unknown size" per §6.2 — a value that
  legitimately needs all-ones-at-this-width silently gets encoded one octet
  wider instead. `vint_min_never_emits_the_unknown_marker` is the property
  test that would fail if this regressed.

## Configuration

None. This crate has no options and reads no `FormatOptions`. A caller
wanting a narrower frame or depth cap than the defaults tracks its own
counter (see *How to change it*).

## Dependencies

`vaco-core` for `Error`/`Result`, and `vaco-io` for `IoContext` (the
streaming header reader) and `IoWriter` (the streaming header writer). No
codec or format-specific dependency of any kind — this is the layer
everything else in the Matroska stack is built on, not the other way
around.

## Fuzzing

`fuzz/fuzz_targets/ebml_grammar.rs` (`fuzz-crate: vaco-format-ebml`) feeds
arbitrary bytes into the child walker, the VINT decoders, and a `Stack`
built from the input's own bytes against a small synthetic schema — the
same shape `vaco-demux-matroska`'s own `matroska_ebml` target already
proved out against the *Matroska* schema, run here against a schema this
crate does not itself define, to keep the target honestly scoped to what
this crate owns. See that file's header for the exact properties asserted.

`vaco-demux-matroska`'s pre-existing `matroska_ebml` fuzz target is
unchanged and continues to fuzz the same functions, now re-exported from
here rather than defined in that crate.
