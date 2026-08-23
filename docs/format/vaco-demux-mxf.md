# `vaco-demux-mxf`

Layer 4. The MXF (Material eXchange Format) demuxer: KLV/BER, the partition
pack and Random Index Pack, the primer, the structural-metadata graph, the
Generic Container essence element, and the Index Table Segment — one crate,
because the four layers are not separable (the index table refers to the
essence container, which refers to the structural metadata, which is keyed
by the primer). Registered as `mxf`.

Written from SMPTE ST 377-1 (file format), ST 379-1 (Generic Container),
ST 386 (D-10), ST 390 (OP-Atom), ST 336 (KLV), RP 210 (metadata dictionary),
clean-room (D7/D15), cross-checked against real files `ffmpeg 8.1` writes
(D6/D17). The exact command for every measured file is in `ul.rs`'s module
docs; every place this crate states a fact from a real file rather than
spec text says so in its own doc comment, and this document does the same.

---

## What it is

| Module | Layer | Contents |
|---|---|---|
| `ber` | KLV | BER length codec (definite form only; the indefinite marker and widths over 8 bytes are refused) |
| `klv` | KLV | One Key-Length-Value triplet over `IoContext`, bounded reads |
| `ul` | KLV | The 16-byte Universal Label type, and every well-known key this crate recognises |
| `partition` | KLV | The Partition Pack (header/body/footer) and the Random Index Pack |
| `primer` | KLV | The Primer Pack: local tag → UL |
| `localset` | metadata | The `Tag(u16) Length(u16) Value` item form shared by header metadata sets and the Index Table Segment |
| `properties` | metadata | The RP210 property dictionary this crate reads, resolved by UL through the primer |
| `metadata` | metadata | The instance-UID-keyed graph: `Preface` → `ContentStorage` → `Package` → `Track` → `Sequence` → `StructuralComponent`; `resolve_essence`'s cycle-guarded source-package chase |
| `descriptor` | metadata | Turning a picture essence descriptor into `CodecParameters` |
| `essence` | essence container | Generic Container essence element keys, frame-wrapped vs clip-wrapped, the track-number match |
| `index` | index tables | The Index Table Segment, CBE and VBE |
| `demux` | — | `MxfDemuxer`: drives all four layers, implements `Demuxer` |

---

## How it works

### The KLV layer

Every top-level object in an MXF file is a KLV triplet: a 16-byte Universal
Label, a BER length, and a value. `ber::decode` handles only the *definite*
BER forms KLV permits — a first byte with the high bit clear is the value
itself (short form); a first byte `0x80 | n` announces `n` following
big-endian bytes (long form). `n == 0` (the classic BER indefinite-length
marker) and `n > 8` (cannot fit a `u64`, and is the shape of a
denial-of-service attempt — `0xFF` alone claims 127 more length bytes) are
both refused on the marker byte, before any further byte is read.

`klv::read_header` reads the key and length and stops; `klv::read_value`
reads the value into memory, but only after checking the declared length
against a caller-supplied ceiling *and* a `vaco_limits::Budget` — the two
independent checks `vaco-limits` is built around (declared-size sanity
check, then budget cap). Essence payloads go through a **separate**
`packet_budget` in `MxfDemuxer`, charged and released per packet exactly
like `vaco-demux-mp4`'s, so a large file cannot be refused by a cumulative
cap meant for header metadata.

`partition::parse` reads the fixed-position Partition Pack layout (it
predates the local-set convention and never adopted it) and
`partition::find_rip` locates the Random Index Pack by its own convention:
the file's last 4 bytes are the RIP's total KLV length, so
`file_len - that length` is the RIP key's offset — a real file's RIP is
therefore found without scanning.

### The structural-metadata graph

Header metadata sets (`Preface`, `Identification`, `ContentStorage`,
`Package`, `Track`, `Sequence`, `StructuralComponent`, descriptors) and the
Index Table Segment share one local-set encoding: `Tag(u16 BE) Length(u16
BE) Value`, distinct from the KLV layer's BER length — confirmed by
decoding a real `IndexEntryArray` item (383 bytes, needing the full 16-bit
width) and a real `Preface` (all items under 128 bytes, which would have
looked identical under either encoding — the Index Table Segment case is
what rules a BER-length reading out).

A local tag means nothing on its own; `primer::parse` builds the file's
`Tag → UL` map, and `properties::Resolver` matches the **resolved UL**
against the properties this crate knows, never the raw tag number — a file
with an unusual or hostile primer still reads correctly, or its properties
are correctly left unrecognised, rather than being misattributed to
whatever property conventionally owns that tag number.

`metadata::scan_region` walks KLVs classifying each by its own key —
Filler skipped, a structural-metadata key parsed into the graph, the Index
Table Segment key parsed into `index_segments`, anything else skipped — and
stops at the first partition-pack-family key. This crate does **not** trust
`HeaderByteCount`/`IndexByteCount` arithmetic to bound the region: during
development those two fields' interaction against a footer partition's
actual byte layout did not reconcile cleanly against a measured file (see
`metadata::scan_region`'s doc comment), and reading forward by key is
simpler and just as correct for every partition, since every partition's
content is fully classified by key alone.

`Preface`, `Identification`, `ContentStorage`, `MaterialPackage` (class byte
`0x36`), `SourcePackage` (`0x37`), `Track` (`0x3b`), `Sequence` (`0x0f`),
`SourceClip` (`0x11`), `TimecodeComponent` (`0x14`) and `MPEGVideoDescriptor`
(`0x51`) were all decoded from a real file and their property tags are in
`properties::TABLE`, cited in `provenance/vaco-demux-mxf.toml`.

#### The cycle guard

`SourceClip.SourcePackageID` is a UMID pointing at another package — real
files use this to trace multi-generation history (a package derived from a
transcode of another), and nothing in the encoding stops a hostile file from
making that reference cyclic. `metadata::resolve_essence` walks Material
Package → Track → Sequence → SourceClip → (by UMID) Source Package,
repeating per generation, guarded by **two independent bounds**:

1. A visited-UMID `HashSet`: every package UMID is recorded before its
   Track/Sequence/SourceClip is followed, so a repeat is a proven cycle and
   resolution stops with `Error::InvalidData`.
2. `MAX_CHAIN_DEPTH` (64), independent of the set — a very large but finite
   chain is a different failure shape (slow, not infinite) and gets its own
   cap, the same "two answers" pattern `vaco-limits` uses for allocation.

`metadata::tests::a_source_clip_naming_its_own_package_terminates_instead_of_looping`
is the regression test.

### Essence containers

A Generic Container essence element's key shares a 12-byte prefix
(`essence::GC_ESSENCE_PREFIX`); byte 12 says frame-wrapped (`0x15..=0x18`,
measured: Picture is `0x15`) or clip-wrapped (`0x05..=0x08`, spec-derived,
**not exercised** — see below); the last 4 bytes are the "track number",
matched **verbatim** against a Track's `EssenceTrackNumber` property
(`0x4804`) to bind an essence element to the stream it belongs to. Verified:
`out.mxf`'s only Track states `15 01 05 00` and its essence elements'
key ends in exactly those four bytes, at exactly the file offsets
`ffprobe -show_packets` reports as `pos`.

`MxfDemuxer::read_packet` is a plain forward KLV walk: Filler, the Generic
Container System Item (`060e2b34.02050101.0d010301.04010100` — present once
per edit unit in a real file, spec-registered under the partition-pack
branch rather than the local-set branch, and not interpreted since nothing
in this crate needs its content) and any other unrecognised key are
skipped; a recognised essence element becomes a `Packet`.

**Timestamps are edit-unit-indexed, not bitstream-reordered.** `pts == dts
== the edit unit's position` for both. A long-GOP codec's true display order
differs from decode order (`ffprobe` reports `dts=-1` for the first frame of
`out.mxf`), and reproducing that needs parsing the elementary stream's own
picture headers — a bitstream fact, not a container fact, and out of this
layer's scope by the same D14.1 boundary that keeps a demuxer from depending
on a parser crate directly (`ParserProvider` is wired into
`MxfDemuxer::open` but not yet called, for exactly this reason: there is
nothing to hand it today).

### Index tables

`EditUnitByteCount` is the whole branch: nonzero means CBE (every edit unit
is exactly that many bytes, computed arithmetically — the D-10 shape);
zero means VBE, and `IndexEntryArray`'s per-entry `StreamOffset` is used.
This crate's corpus is VBE (long-GOP MPEG-2 has no fixed frame size), so VBE
is the measured path; CBE is implemented from the same verified tag table
but not exercised against a real CBE file (D-10 sample generation issue,
below).

`StreamOffset` is relative to a per-file "essence origin" this crate
determines empirically at `open` time (`demux::find_first_essence_offset`):
the byte offset of the *first* essence element found in the body partition.
Measured: entry 0's `StreamOffset` is always `0`, and entry *n*'s equals
`(offset of the nth essence element's KLV key) - (offset of the first)`
exactly — confirmed against a real file's `IndexEntryArray` and its
`ffprobe -show_packets` positions side by side (see `ul.rs` and `index.rs`
module docs for the numbers). This does not match a naive reading of
`BodyOffset` from the partition pack (which was `0` in the same file, yet
differed from the essence origin by several hundred bytes of leading Filler
and System Item) — the empirical anchor is what was verified, and it is
called out as a measurement rather than a spec citation for exactly that
reason.

`demux::build_indices` turns each `IndexTableSegment` into a
`vaco_format_core::seek::PacketIndex`; `MxfDemuxer::seek` calls
`PacketIndex::search`. **Scope limit, stated once, not per issue below:**
this crate supports one essence track per `BodySID` (every file in the
corpus is single-track); an index table that interleaves several tracks via
`DeltaEntryArray`'s slice numbers is not de-interleaved.

---

## How to change it

- **Add a structural-metadata property**: add a `PropertyId` variant and its
  measured `(PropertyId, Ul)` row to `properties::TABLE`, then read it with
  `MetadataSet::get`/the typed helpers in `metadata.rs` or `descriptor.rs`.
  Getting the UL from a real file (decode a primer pack, look up the tag
  that carries the property you want) is safer than transcribing from a
  dictionary by hand — see `properties.rs`'s module docs for why UL matching
  beats raw-tag matching.
- **Add a descriptor mapping** (a new `PictureEssenceCoding` UL, or a sound
  descriptor path): `descriptor.rs`'s `PICTURE_ESSENCE_CODING` table and
  `picture_parameters` are the place; a sound path does not exist yet (see
  Deferred work).
- **Multi-track `BodySID` support**: `demux::build_indices` and
  `MxfDemuxer::read_packet`'s track-number match are where a `DeltaEntryArray`-aware
  de-interleave would go; today every track binding just matches essence
  elements by GC track number regardless of which `BodySID` produced them,
  which is correct only because the corpus has one track per file.
- **Clip-wrapped / D-10**: `essence::clip_wrapped_spans` is written and
  tested against synthetic CBE and VBE segments, but `demux.rs` does not yet
  call it — `read_packet` only handles frame-wrapped elements. Wiring it in
  needs a real clip-wrapped or D-10 sample to verify against (see Deferred
  work for why one could not be produced with the installed reference).

---

## Configuration

No CLI options are exposed today (`MxfDemuxer::open` takes no `MxfOptions`).
Internal caps, all in source, all named for what they bound:

| Constant | Where | Bounds |
|---|---|---|
| `ber::MAX_ENCODED_LEN` | `ber.rs` | Longest BER length prefix accepted (9 bytes) |
| `MAX_PARTITION_PACK_BYTES` | `partition.rs` | One partition pack's value |
| `MAX_RIP_ENTRIES` | `partition.rs` | Random Index Pack entries |
| `MAX_PRIMER_ENTRIES` / `MAX_PRIMER_BYTES` | `primer.rs` | Primer pack entries / total size |
| `MAX_SET_BYTES` | `metadata.rs` | One structural-metadata set or Index Table Segment |
| `MAX_REGION_KLVS` | `metadata.rs` | KLVs read before `scan_region` gives up |
| `MAX_CHAIN_DEPTH` | `metadata.rs` | Source-package generations `resolve_essence` will chase |
| `MAX_INDEX_ENTRIES` | `index.rs` | `IndexEntryArray` entries |
| `MAX_PACKET_BYTES` | `demux.rs` | One essence element's value |

## Dependencies

`vaco-core` (errors, `Rational`/`Timestamp`), `vaco-io` (`IoContext`,
`MediaSource`), `vaco-limits` (`Budget`, the allocation/fuel model this
crate leans on throughout), `vaco-packet` (`Packet`), `vaco-format-core`
(`Demuxer`, `Stream`, `PacketIndex`, `DemuxerDesc`), `vaco-codec-core`
(`CodecId`, `CodecParameters` — **not** a `vaco-parse-*` crate: D14.1 keeps
this crate off the parser layer entirely, which is also why bitstream-level
facts like true display-order timestamps are out of scope, see above).

## Deferred work (see the closing report for the full, per-issue account)

- **D-10 / clip-wrapped verification.** `ffmpeg -f mxf_d10` on the installed
  8.1 build refused every quantiser this crate tried with "frame size does
  not match index unit size" — a CBR constraint at the *encoder* side that
  could not be satisfied with synthetic test content. `essence.rs`'s
  clip-wrapped span logic and `index.rs`'s CBE arithmetic are implemented
  from the same verified tag table as the VBE path but are unexercised
  against a real file.
- **Sound (audio) essence.** No `GenericSoundEssenceDescriptor`/
  `WaveAudioDescriptor` mapping exists; a source package whose only track is
  audio produces zero streams today.
- **`MultipleDescriptor`.** Recognised by class byte (`0x44`) and skipped
  rather than expanded into per-track sub-descriptors — a real, documented
  shape for multi-essence-track OP1a files, just not one in this crate's
  corpus.
- **True OP-Atom.** Supported as "one essence track per file"; discovering
  the sibling files a real multi-file OP-Atom edit is split across is not
  implemented.
- **Multi-`BodySID` / interleaved index tables.** See "How it works", Index
  tables.
