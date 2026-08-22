# `vaco-codec-cbs` — the coded bitstream layer

## What it is

A codec-agnostic representation of a coded bitstream as an **ordered list of
units** that can be read, edited and written back — without decoding anything.
It is what bitstream filters are built on: `filter_units`, `hevc_metadata`,
`hevc_mp4toannexb`, `extract_extradata` and their H.264 and AV1 equivalents are
all operations on this list.

Layer 3 (`crates/signal/`). It contains **no codec syntax at all**; a codec crate
implements [`CbsCodec`] and gets the rest.

## How it works

```
  bytes ──split──► CbsFragment ──read_unit──► Content
                       │                         │
                       │                      (edit)
                       │                         │
  bytes ◄─assemble── CbsFragment ◄─write_unit────┘
```

| This crate | The codec crate |
|---|---|
| `CbsFragment`, `CbsUnit` — the unit list and its edits | `CbsCodec::split` / `::assemble` — the framing |
| `Cbs<C>` — the read → edit → write cycle | `CbsCodec::read_unit` / `::write_unit` — the syntax |
| budget accounting for the whole fragment | the per-element bounds inside one unit |

### `CbsUnit`

```rust
pub struct CbsUnit {
    pub unit_type: u32,           // nal_unit_type, obu_type, marker
    pub data: Vec<u8>,            // framing removed, ESCAPING INTACT
    pub origin: Option<UnitOrigin>, // where it came from, or None if synthesised
}
```

Three decisions in that struct carry most of the design:

**`unit_type` is a `u32`, not an enum.** `filter_units` needs to know "drop type
39", not what type 39 means. Every codec-specific meaning stays in the codec
crate, so this crate never grows a match over codecs.

**`data` holds escaped bytes.** De-escaping and re-escaping is not the identity:
a conforming encoder may leave a trailing `00 00` unescaped where a re-escape
would write `00 00 03`. A layer that stored the de-escaped form would rewrite
units a filter was asked to leave alone, and a filter that changes bytes it did
not touch is not a filter.

**`origin` is optional.** A unit read from a buffer knows its offset and its
framing width, so a filter can map a surviving unit back to the bytes it came
from — which is what keeps timestamps, `Packet::pos` and side data attached to
the right thing. A unit a filter *inserted* has no such origin and says so.

### `CbsCodec::Framing` is an associated type

H.26x has two framings (Annex B and length-prefixed), AV1 has two different ones
(Annex B and low-overhead), JPEG has one. A single `Framing` enum here would have
to enumerate every codec's — the "core knows about every component" shape plan 10
§1.5 forbids. It is also a *layer* problem: the H.26x framing types live in
`vaco-format-nalu`, which is **above** this crate, so naming them here is not
merely inelegant, it is impossible.

That is also why `split` and `assemble` are trait methods rather than something
this crate provides. Splitting a buffer into units *is* the framing.

## How to change it

- **Adding a codec**: implement `CbsCodec` in the codec crate. Five methods.
  `vaco-parse-hevc`'s `cbs::HevcCbs` is the worked example, at about 200 lines
  including the typed read path.
- **Adding a fragment operation**: `unit.rs`. Keep the "an index past the end
  appends or returns `None`" rule — `indexing_slicing` is denied workspace-wide
  precisely so an out-of-range index cannot become a crash.
- **Budget accounting** lives in `CbsFragment::push`/`insert`/`replace_data` and
  is released by `CbsFragment::release`. A caller that reuses one fragment across
  a whole stream must call `release` (or `Cbs::split`, which calls it) or the
  budget counts every packet the stream ever held.

### Gotchas

- `Cbs::split` clears the fragment and releases its budget first. If you want to
  accumulate units across calls, use `CbsCodec::split` directly.
- `Cbs::update_unit` clears the unit's `origin`, because the bytes are no longer
  the ones that were read. That is deliberate: an assembler that trusted a stale
  origin would emit a start code of the wrong length.
- The genericity is real but untested against a third codec. See the honest
  assessment below.

## Configuration

None. No features, no options, no environment. `Budget`/`Limits` come from the
caller.

## Dependencies

`vaco-core` (error taxonomy), `vaco-limits` (budget), `vaco-bitstream` (the
re-exported `to_rbsp` / `to_ebsp` / `violates_ebsp_constraint`). `vaco-codec-core`
is declared for a future `BitstreamFilter` adapter and is not yet used. No
external runtime dependencies.

## Does the representation actually serve H.264, or is it HEVC-shaped?

The brief asked this directly, so here is the evidence rather than the claim.

**What is tested.** `tests/two_codecs.rs` implements `CbsCodec` twice — once
H.264-shaped (one-byte header, five-bit type, `nal_ref_idc` in the header) and
once HEVC-shaped (two-byte header, six-bit type, layer and temporal ids) — and
runs split, drop-by-type, insert, typed read-modify-write, reframe and
byte-for-byte round trip through **both**. Both codecs share one `CbsFragment`
type and one `Cbs<C>` session type, and no operation needed a codec-specific
branch. The H.264 impl was written first and the HEVC one second, so the shape
was not fitted to HEVC after the fact.

**Where it would strain, honestly:**

1. **AV1 is the real test and it has not been run.** OBUs are not
   "delimiter, payload" — an OBU carries its own header with an optional size
   field, and an AV1 *temporal unit* nests OBUs in a way a flat unit list does
   not express. `CbsFragment` would hold the OBUs flat and the temporal-unit
   grouping would have to live in the codec's `Content`. That is workable but it
   is a genuine impedance mismatch, and until someone writes `vaco-cbs-av1` this
   layer has been proven against *two similar* codecs, not two different ones.
2. **`unit_type: u32` assumes a unit has exactly one type.** True for H.26x
   NAL units, OBUs and JPEG markers. An SEI NAL unit really carries *several*
   messages of different types, and a filter that wants "drop the SEI messages of
   type 5 but keep the rest of the unit" cannot express that as a `retain` — it
   has to decode, edit and re-encode the unit. That is the correct layering, but
   it is a place where the flat model does less than a caller might expect.
3. **No trace hook.** Plan 15 lists `trace_headers` as a `vaco-cbs-core`
   responsibility. It needs every codec's *reader* to report each syntax element
   as it is read, which is a change to the readers rather than to this crate, and
   adding an untested sink first would be speculative API. The shape it needs is
   a `&mut dyn SyntaxTrace` threaded through `read_unit`, with
   `element(bit_pos, name, bits, value)`; deferred deliberately.
4. **The write path is the codec's problem and both are incomplete.** This crate
   defines `write_unit`; `vaco-parse-hevc` implements it for raw units only and
   returns `Unsupported` for a typed parameter set, because a parameter-set writer
   that is not bit-exact corrupts a stream silently. Plan 15's D-19 budgets that
   separately. So "read/modify/write" is currently "read/move/write" — every
   filter that works by *moving whole units* works today; every filter that edits
   a parameter set's fields does not.

**Verdict**: the representation is codec-agnostic in the ways that have been
tested, and the two places it is most likely to need changing are AV1's nested
temporal units and the trace hook. Neither is a redesign; both are additions.

## Known divergences

`vaco-parse-hevc` records `ANNEXB_EXPRESSIVENESS_DIVERGENCE`: Annex B is a
strictly less expressive container than a length prefix, and two shapes cannot
survive a reframe through it — a unit whose bytes end in `0x00`, and a unit
containing `00 00 01`. Both are impossible in a conforming stream (§7.4.1.1) and
both are detected by `vaco_parse_hevc::cbs::annexb_safe`. This crate does not
work around it: refusing to change bytes it was not asked to change is the more
defensible default, and a filter that needs the guarantee should check first.
