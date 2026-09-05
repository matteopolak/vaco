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
| `descriptor` | metadata | Turning a picture or sound essence descriptor into `CodecParameters` |
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
measured: Picture is `0x15`) or, per ST 379-1 Table 1, clip-wrapped
(`0x05..=0x08`) — the last 4 bytes are the "track number", matched
**verbatim** against a Track's `EssenceTrackNumber` property (`0x4804`) to
bind an essence element to the stream it belongs to. Verified: `out.mxf`'s
only Track states `15 01 05 00` and its essence elements' key ends in
exactly those four bytes, at exactly the file offsets `ffprobe
-show_packets` reports as `pos`.

**The `0x05..=0x08` range is not reliable evidence of clip-wrapping —
corrected against a real D-10 file.** A previous pass could not produce a
real clip-wrapped or D-10 sample and shipped the ST 379-1 byte-range
reading unverified. A real `ffmpeg -f mxf_d10` file (obtained this session
— see "Deferred work" for the encoder recipe that finally worked)
contradicts it directly: its Picture essence key ends `05 01 01 00`,
squarely in the "clip-wrapped" range, yet the file carries **three (or
twenty-five, at the 1-second fixture size first measured) separate KLVs
sharing that exact key, one per frame** — confirmed both by hex-dumping the
file and by `ffprobe -show_packets` reporting one packet per KLV at those
same offsets. That is frame-wrapped by any operational definition. `essence::Wrapping`
now documents the byte-range classification as real but unreliable for
deciding framing; nothing in this crate uses it to decide how a real file's
packets are read.

`MxfDemuxer::read_packet` is a plain forward KLV walk: Filler, the Generic
Container System Item (`060e2b34.02050101.0d010301.04010100` — present once
per edit unit in a real file, spec-registered under the partition-pack
branch rather than the local-set branch, and not interpreted since nothing
in this crate needs its content) and any other unrecognised key are
skipped; a recognised essence element becomes a `Packet`. This is what
every file in the corpus — OP1a, OP-Atom and D-10 alike — has actually
turned out to need: "one KLV, one packet," regardless of item-type byte.

**Single-partition files need `metadata::scan_region` to know essence has
started.** The D-10 fixture exposed a real bug here, not a D-10 quirk: a
header partition can state a nonzero `body_sid` and carry essence directly
after its own metadata, with no separate body partition pack in between at
all. `scan_region` previously only stopped at a partition-pack-family key
or the Random Index Pack, so on a file shaped this way it walked straight
through the System Item and every essence element as "unrecognised,
skippable" KLVs, all the way to the footer partition near EOF —
`MxfDemuxer::open` then started reading packets from a position near the
end of the file and found none. Fixed by also stopping at an essence
element or the System Item key (`essence::GC_SYSTEM_ITEM_PREFIX`/
`is_generic_container_system_item`, measured off the same file). OP1a and
OP-Atom files with a genuinely separate body partition were and remain
unaffected — this is the shape they never took.

**Timestamps are edit-unit-indexed, not bitstream-reordered.** `pts == dts
== the edit unit's position` for both. A long-GOP codec's true display order
differs from decode order (`ffprobe` reports `dts=-1` for the first frame of
`out.mxf`), and reproducing that needs parsing the elementary stream's own
picture headers — a bitstream fact, not a container fact, and out of this
layer's scope by the same D14.1 boundary that keeps a demuxer from depending
on a parser crate directly (`ParserProvider` is wired into
`MxfDemuxer::open` but not yet called, for exactly this reason: there is
nothing to hand it today).

### Sound essence

`descriptor::sound_parameters` handles two distinct measured descriptor
classes, both folding into the same `StructuralClass::Descriptor` arm as
every picture descriptor kind (see `ul.rs`; `sound_parameters` tells them
apart by which properties are present, not by class byte):

- **`AES3PCMDescriptor` (`0x47`)** — OP1a's audio class in every real fixture
  this crate has seen (`ffmpeg -f mxf` with a `pcm_s16le` track). Its essence
  bytes are raw, tightly-interleaved PCM, verbatim — verified byte-exact
  against `ffprobe -show_packets`' `pos`/`size` on `op1a_mpeg2_pcm_sample.mxf`.
  This is the only class `sound_parameters` reports a `CodecId` for.
- **`GenericSoundEssenceDescriptor` (`0x42`)** — D-10's audio class in every
  real `ffmpeg -f mxf_d10` fixture with audio. `SampleRate`, `AudioChannelCount`
  and `AudioQuantizationBits` read correctly from it (see below), but **the
  essence bytes are not raw PCM**: measured directly (comparing this crate's
  raw KLV length against `ffprobe`'s reported packet size, then dumping the
  essence element's raw bytes word-by-word against `ffmpeg -c copy -f data`'s
  own extracted PCM) to be a fixed AES3-style bundle — a 4-byte element
  header of undetermined meaning, then, per sample instant (`1920` at 48
  kHz/25 fps), **8 fixed channel slots regardless of the descriptor's own
  `AudioChannelCount`** (both a 2-logical-channel and an 8-logical-channel
  fixture physically occupy all 8 slots), each slot a 4-byte word: 1 tag
  byte (the slot's own index, constant per slot) plus a little-endian 24-bit
  field holding the 16-bit PCM sample left-shifted 4 bits (`raw / 16 ==
  pcm16` confirmed on every sample checked). `4 + 1920 * 8 * 4 == 61444`,
  matching the raw KLV length exactly in both fixtures; `ffprobe`'s reported
  size is the unpacked logical-channel PCM only (`1920 * channels * 2`).
  Turning this into playable `pcm_s16le` needs the descriptor's channel
  count fed back into per-sample unpacking — bitstream/essence-format work,
  not container framing, the same D14.1 line this crate already draws for
  MPEG-2 timestamp reordering. So `read_packet` reports the real,
  unmodified KLV length (never a fabricated smaller size) and
  `sound_parameters` reports `codec_id: None` for this class specifically —
  `sample_rate`/channel layout/`format` stay accurate, the packet bytes are
  not claimed to be something they are not. `d10_mpeg2_aes3_sample.mxf`
  (muxed with `-d10_channelcount 8`, the SMPTE-386M-compliant value) is the
  regression fixture.

**`AudioSampleRate` is a distinct property from the generic `SampleRate`.**
On a sound descriptor, `SampleRate` states the *edit rate* (`25/1` on the
D-10 fixture — it only looked interchangeable with the true sample rate on
the first fixture tried, where both happened to read `48000/1`). The real
audio sample rate is tag `0x3d03`, registered as `PropertyId::AudioSampleRate`.

**A `MultipleDescriptor` (`0x44`) is expanded per track, not skipped.** A
package with more than one essence track (the common OP1a/D-10
video-plus-audio shape) points every track at the *same* `MultipleDescriptor`
id — which carries none of the real per-essence properties itself. Before
this was handled, every track in such a package resolved to that one
descriptor, and `build_streams` skipped it outright: a source package with
more than one essence track produced **zero** streams, video included, not
just audio. `metadata::resolve_track_descriptor` reads the
`MultipleDescriptor`'s `SubDescriptorUIDs` batch and matches each
sub-descriptor's own `LinkedTrackId` back to the track being resolved
(measured against a real two-track `ffmpeg -f mxf` file); it falls back to
the unchanged package-level descriptor id whenever it cannot resolve
further (not a `MultipleDescriptor`, an unknown id, no track id, or no
matching sub-descriptor) rather than dropping the track's descriptor
entirely.

### Index tables

`EditUnitByteCount` is the whole branch: nonzero means CBE (every edit unit
is exactly that many bytes, computed arithmetically — the D-10 shape);
zero means VBE, and `IndexEntryArray`'s per-entry `StreamOffset` is used.
Both are now measured: the original corpus (VBE, long-GOP MPEG-2 has no
fixed frame size) plus three real D-10 fixtures at 30/40/50 Mbit/s (CBE) —
see "Essence containers, corrected" below for how that sample was finally
produced.

**A real CBE file's `IndexDuration` cannot be trusted.** Measured directly
off a D-10 fixture's raw bytes: the Index Table Segment's `IndexDuration`
item is present and reads `0`, even though the file has a definite,
non-zero frame count recoverable from the essence container's own size.
Trusting it literally reported a zero-duration, zero-entry index for a file
that plainly had three real frames. `MxfDemuxer::open`'s
`effective_index_duration` closure now falls back to `essence_length /
EditUnitByteCount` (the same arithmetic `essence::clip_wrapped_spans`'s CBE
branch already used) whenever the stated value is not positive, used by
both `demux::build_indices` and the crate's own overall `duration()`.

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

The aggregate duration uses that effective edit-unit count at the matching
track's native edit rate. `duration()` and its compatibility alias
`duration_exact()` keep the same rational seconds without an intermediate
microsecond conversion.
A one-edit-unit `30000/1001` OP1a fixture generated by `ffmpeg 9.0.1` is the
black-box regression: `ffprobe` reports `time_base=1001/30000`,
`duration_ts=1`, `duration=0.033367`, and one packet, while Vaco retains the
exact `1001/30000`-second aggregate value.

---

## How to change it

- **Add a structural-metadata property**: add a `PropertyId` variant and its
  measured `(PropertyId, Ul)` row to `properties::TABLE`, then read it with
  `MetadataSet::get`/the typed helpers in `metadata.rs` or `descriptor.rs`.
  Getting the UL from a real file (decode a primer pack, look up the tag
  that carries the property you want) is safer than transcribing from a
  dictionary by hand — see `properties.rs`'s module docs for why UL matching
  beats raw-tag matching.
- **Add a descriptor mapping** (a new `PictureEssenceCoding` UL, a new sound
  quantization/channel shape): `descriptor.rs`'s `PICTURE_ESSENCE_CODING`
  table and `picture_parameters` are the picture-side place;
  `sound_parameters` is the sound-side place — see "Sound essence" above for
  why it only claims a `CodecId` for `AES3PCMDescriptor`, not
  `GenericSoundEssenceDescriptor`.
- **Unpack D-10's AES3 bundle into playable `pcm_s16le`**: the byte layout
  is fully measured (see "Sound essence" above) but not implemented — doing
  so needs the descriptor's channel count threaded into `read_packet` (or a
  post-processing step keyed off `GenericSoundEssenceDescriptor`), which
  today just returns each essence element's raw bytes uniformly regardless
  of media type. This is bitstream/essence-format work, not container
  framing (see the D14.1 note above); if it lands, `sound_parameters` should
  gain a `CodecId::PcmS16le` claim for this class once the packet bytes
  really are that.
- **Multi-track `BodySID` support**: `demux::build_indices` and
  `MxfDemuxer::read_packet`'s track-number match are where a `DeltaEntryArray`-aware
  de-interleave would go; today every track binding just matches essence
  elements by GC track number regardless of which `BodySID` produced them,
  which is correct only because the corpus has one track per file.
- **D-10**: landed — see "Essence containers" above. `demux::MxfDemuxer`
  handles it through the same "one KLV, one packet" path as everything
  else; no D-10-specific fast path exists or is needed.
- **Genuinely clip-wrapped essence (one KLV for a whole track)**:
  `essence::clip_wrapped_spans` is written, tested against synthetic CBE
  and VBE segments, and now has a count cap (`essence::MAX_CBE_SPANS`) —
  but `demux.rs` still does not call it, and, unlike D-10, this remains
  genuinely unreachable to verify: `ffmpeg 8.1`'s `mxf`/`mxf_d10` muxers
  have no clip-wrap option at all (checked this session). Wiring it in
  needs both a real sample from some other source and a non-byte-range way
  to decide when to call it, since the item-type byte no longer can be
  trusted for that (see "Essence containers" above) — comparing a KLV's
  declared length against the index table's own known edit-unit size is
  the leading candidate.

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
| `MAX_CBE_INDEX_ENTRIES` | `demux.rs` | `build_indices`'s CBE `for n in 0..count` loop, where `count` can now be driven by a real file's own size (see "Index tables") |
| `essence::MAX_CBE_SPANS` | `essence.rs` | `clip_wrapped_spans`'s own, still-unwired CBE loop — same shape, same cap, hardened alongside the one above |

## Dependencies

`vaco-core` (errors, `Rational`/`Timestamp`/`ExactDuration`), `vaco-io` (`IoContext`,
`MediaSource`), `vaco-limits` (`Budget`, the allocation/fuel model this
crate leans on throughout), `vaco-packet` (`Packet`), `vaco-format-core`
(`Demuxer`, `Stream`, `PacketIndex`, `DemuxerDesc`), `vaco-codec-core`
(`CodecId`, `CodecParameters` — **not** a `vaco-parse-*` crate: D14.1 keeps
this crate off the parser layer entirely, which is also why bitstream-level
facts like true display-order timestamps, and D-10's AES3 sample unpacking,
are out of scope, see above), `vaco-sampfmt` (`SampleFmt`, for
`sound_parameters`), `vaco-chlayout` (`ChannelLayout::default_for`, for
`sound_parameters`).

## Deferred work (see the closing report for the full, per-issue account)

- **D-10 verification: resolved.** The original "every quantiser refused
  with 'frame size does not match index unit size'" blocker was an
  incomplete encoder invocation, not a real limit — the working recipe
  needed `-intra_vlc 1 -qmax 12 -qmin 1 -non_linear_quant 1 -flags +ildct
  -g 1 -bf 0` alongside matched `-b:v`/`-minrate`/`-maxrate`/`-bufsize`/
  `-rc_init_occupancy` values at one of the three standard D-10 bitrates
  (30/40/50 Mbit/s). `tests/fixtures/d10_mpeg2_sample.mxf` is a real,
  `ffprobe`-verified 30 Mbit/s sample built this way.
- **Genuinely clip-wrapped essence (one KLV for a whole track): still not
  verifiable.** Distinct from D-10 — checked directly this session:
  `ffmpeg 8.1`'s `mxf` and `mxf_d10` muxers have no clip-wrap option at
  all, so unlike D-10 there is no encoder invocation left to try.
  `essence.rs`'s clip-wrapped span logic and `index.rs`'s CBE arithmetic
  remain implemented from the same verified tag table as the VBE path but
  unexercised against a real file, and — since D-10 disproved this crate's
  own byte-range wrapping heuristic — wiring them in now also needs a
  different way to decide *when* a KLV is clip-wrapped in the first place
  (see "How to change it").
- **Sound (audio) essence: landed for metadata, partial for D-10 packet
  bytes.** `AES3PCMDescriptor` (OP1a) is fully verified end to end —
  metadata and packet bytes both. `GenericSoundEssenceDescriptor` (D-10)'s
  metadata is fully verified; its packet bytes are a measured, fixed AES3
  bundle this crate reports honestly but does not unpack (`codec_id: None`)
  — see "Sound essence" above. `WaveAudioDescriptor` (`0x48`) remains
  spec-derived and unexercised, no real fixture has produced it.
- **`MultipleDescriptor`.** Landed: expanded per track via `SubDescriptorUIDs`/
  `LinkedTrackId` (see "Sound essence" above), not skipped. A sub-descriptor
  that is itself a further nested `MultipleDescriptor`, or a
  `SubDescriptorUIDs` array pointing at more than two essence tracks, has
  not been seen in a real fixture and is unexercised.
- **True OP-Atom.** Supported as "one essence track per file"; discovering
  the sibling files a real multi-file OP-Atom edit is split across is not
  implemented.
- **Multi-`BodySID` / interleaved index tables.** See "How it works", Index
  tables.
