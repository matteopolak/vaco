# `vaco-mux-mxf`

Layer 4. The MXF (Material eXchange Format) muxer: OP1a, one video track
and at most one audio track, frame-wrapped. Registered as `mxf`.

Written clean-room (D7/D15): this crate does not read `ffmpeg` source. It
reuses its sibling `vaco-demux-mxf`'s own already-published, clean-room
measurement of the KLV/BER wrapper, the Partition Pack, the
structural-metadata graph, and Universal Labels and property tags
(`provenance/sources.toml`'s `ffmpeg-mxf-probe` family), plus a handful of
local-tag *numbers* this crate measured fresh against a real header this
session (`ffmpeg-mxf-mux-header-probe`) — the demux crate only ever needed
the resolved UL, never which conventional tag carries it. Every claim below
is cross-checked against both `vaco-demux-mxf` (a dev-dependency, used the
same way `vaco-mux-mp4` depends on `vaco-demux-mp4`) and a real `ffprobe`/
`ffmpeg` on the development machine.

---

## What it is

| Module | Contents |
|---|---|
| `ber` | BER length encode: `0x83`+3 bytes up to 16 MiB, `0x88`+8 bytes beyond |
| `klv` | Writing one Key-Length-Value triplet |
| `ul` | The Universal Labels and key-building functions this crate writes |
| `uid` | `InstanceUID`/Package UMID generation (a counter mixed with `vaco_time::unix_nanos()`, no new RNG dependency) |
| `localset` | The `Tag(u16) Length(u16) Value` item writer, batches, and the Primer Pack builder |
| `metadata` | Building the structural-metadata graph: `Preface` -> `ContentStorage` -> `Package` (Material and Source) -> `Track` -> `Sequence` -> `SourceClip`/`TimecodeComponent` -> `Descriptor` |
| `partition` | The Partition Pack's fixed-position layout |
| `essence` | Generic Container track-number assignment and essence element keys |
| `index` | VBE Index Table Segment construction |
| `mux` | `MxfMuxer`: implements `Muxer`, ties every layer together |

---

## How it works

### Partition layout — one decision that removes the need for a two-pass write

A "closed, complete" header partition (full structural metadata, generated
once at `init()`), directly followed by essence with **no separate body
partition pack** — the same single-partition-carries-essence shape
`vaco-demux-mxf` had to learn to read for real D-10 files, reused here
deliberately — then a "closed, complete" footer partition, then a Random
Index Pack.

The footer restates **nothing** from the header: only a fresh Index Table
Segment (the video track's real `IndexEntryArray`, and the real edit-unit
count now that every packet has been seen). This was not the first design
tried. An earlier version restated the *entire* graph in the footer (same
`InstanceUID`s, updated `Duration`) reasoning that a real `ffmpeg -f mxf`
file does exactly that — measured directly, `vaco-demux-mxf`'s own test
fixtures do carry a duplicate copy. But `vaco-demux-mxf::demux::MxfDemuxer::open`
reuses **one** `primer`/`resolver` pair across both its header and footer
`scan_region` calls, so a second primer pack was always redundant for this
crate's own reader — and cross-checking against a real `ffmpeg -i` on that
earlier design surfaced `Multiple primer packs` and `Multiple packages_refs`
warnings, then a genuine misreport: `Stream #0:0: Data: mpeg2video` instead
of `Video`. Dropping the footer's restatement (primer and structural sets
both) fixed every warning and the misreport, at the cost that
`Sequence`/`SourceClip.Duration` stays `-1` ("not known when written", a
value ST 377-1 explicitly permits) for the file's whole life. That cost is
paid safely: neither reader in this crate's scope uses that property for
the real duration — both derive it from the Index Table Segment's own
`IndexDuration`/entry count, which the footer does state correctly. Why the
duplicate-metadata design specifically triggered `ffmpeg`'s warnings was
not root-caused further (see "Deferred work"); dropping the duplication
outright was cheaper and is not a loss, since nothing needs it.

**The one field that does need a small backpatch**: the header partition
pack's own `FooterPartition`, which cannot be known until the footer is
about to be written. `vaco-demux-mxf::demux::MxfDemuxer::open` uses this
field, not the Random Index Pack, to find the footer. `MxfMuxer::write_trailer`
seeks back and overwrites just that 8-byte field on a seekable sink, then
returns to the real end of file; on a non-seekable sink the field stays
`0` and the footer is present but not reachable by a reader that trusts
`FooterPartition == 0` to mean "no footer" — an honest degradation, not a
silent one (`a_non_seekable_sink_still_produces_a_sequentially_readable_file`
is the regression test).

### The structural-metadata graph

One `MaterialPackage` and one `SourcePackage`, each carrying: a timecode
track (`TrackID = 1`, no descriptor, no essence — see below), then one
track per essence stream (`TrackID` starting at `2`). A single essence
track's `SourcePackage` carries its `PackageDescriptor` directly; more than
one means a `MultipleDescriptor` whose `SubDescriptorUIDs` batch names each
track's real descriptor, matched back by `LinkedTrackId` — the write side
of the exact expansion `vaco-demux-mxf::metadata::resolve_track_descriptor`
performs on read (this workspace's own prior session fixed that function
after finding it was never called at all; this crate is the first thing to
actually exercise it against a written-not-just-measured file).

**`TrackID = 1` is reserved for a timecode track — measured to matter, not
just conventional.** Every real `ffmpeg -f mxf` file this session generated
puts a `Track` -> `Sequence` -> `TimecodeComponent` chain at `TrackID = 1`
on both packages, with essence tracks starting at `2`. This looked like
free-standing convention until a real `ffmpeg -i` cross-check on an
earlier, timecode-free, single-video-track version of this crate's output
(that file's lone video track therefore held `TrackID = 1`) reported the
stream as `Data: mpeg2video` — the same `Codec type or id mismatches`
symptom the `DataDefinition` bug below also produced, and in fact the same
underlying cause was found twice from two different angles before both
were fixed. Adding the timecode track and starting essence `TrackID`s at
`2` fixed it. `vaco-demux-mxf`'s own reader does not care about `TrackID`
values at all (it recognises a timecode track by its `TimecodeComponent`
class, not by ID), which is exactly why this crate's own round-trip tests
never caught either bug — only the real reference did.

**Three `DataDefinition` labels, not two — a real transcription bug caught
the same way.** `Sequence`/`SourceClip`/`TimecodeComponent` all carry a
`DataDefinition` UL (tag `0x0201`) stating what kind of data the chain
carries. Measured directly off a real two-essence-track file's six
`Sequence` sets (one per track per package): the three values share an
11-byte prefix and differ only in bytes 11 and 12 — byte 11 distinguishes
"timecode" (`0x01`) from "essence" (`0x02`); within "essence", byte 12
distinguishes picture (`0x01`) from sound (`0x02`). An earlier version of
this crate had transcribed the *timecode* value as "picture" and the
*picture* value as "sound" (a single-video-track file only exercises one
value, so this looked plausible until the real reference disagreed) —
caught by the same `Data: mpeg2video` misreport above, and confirmed by
decoding a real three-`DataDefinition` fixture byte-for-byte
(`metadata.rs`'s `DATA_DEFINITION_TIMECODE`/`_PICTURE`/`_SOUND` doc comment
has the full account). `vaco-demux-mxf` never reads this property's value
at all, which is again why only the reference caught it.

### Essence and the Index Table Segment

`essence::track_number` assigns each essence track a Generic Container
track number (`0x15` frame-wrapped Picture, `0x16` frame-wrapped Sound,
per-media-type index starting at 1) — self-assigned, not measured against a
specific real value, since correctness here only requires internal
consistency between the essence element's own key and the matching Track's
`EssenceTrackNumber` property, which both `vaco-demux-mxf` and `ffmpeg`
confirmed reading back correctly.

One Generic Container System Item (empty value — neither reader interprets
its content) precedes every essence element, for every track, in
whatever order `write_packet` receives them. This is a **documented
simplification**, not the real multi-track interleaving convention (a real
file groups one System Item per edit unit across all tracks, not one per
essence element per track) — spec-valid and correctly read by both
`vaco-demux-mxf` and `ffmpeg`'s sequential packet reads, but not
byte-identical to a real multi-track file's layout.

Only the **video** track is indexed (`index::build`'s `SliceCount = 0`,
one essence track per `BodySID`) — matching `vaco-demux-mxf`'s own
documented scope limit ("one essence track per `BodySID`... an index table
that interleaves several tracks via `DeltaEntryArray` is not
de-interleaved"). A second essence track is fully readable sequentially
(neither demuxer consults the index for `read_packet`, only for `seek`)
but not currently seekable-to.

---

## How to change it

- **A new essence codec** (currently only `CodecId::Mpeg2video` maps to a
  `PictureEssenceCoding` UL, and `CodecId::PcmS16le` to raw PCM in an
  `AES3PCMDescriptor`): `ul::PICTURE_ESSENCE_CODING_MPEG2_LONG_GOP` and
  `metadata::build_descriptor` are the places. Measure the real UL against
  a fixture first (D6/D17) — do not transcribe from a spec table by hand;
  see the `DataDefinition` account above for what happens when a byte gets
  swapped and nothing but the reference notices.
- **A third or more essence track**: `add_stream` currently refuses a
  second video or second audio stream outright. Lifting that needs
  `essence::track_number`'s per-media-type counter (already handles it) and
  extending `metadata::build_sets`'s `MultipleDescriptor` path (already
  generic over `tracks: &[TrackPlan]`) — the real gap is the Index Table
  Segment, still single-track-only (see above).
- **D-10 / OP-Atom / a real timecode value**: out of scope here — #610
  (`FM-51c`), a distinct crate-scope decision, not a TODO left in this one.
  This crate's timecode track always states `TimecodeStart = 0`; wiring a
  real starting timecode through from the caller is unclaimed work for
  whichever package takes on OP-Atom/D-10, not blocked by anything here.

---

## Configuration

No options exposed today; `MxfOptions` is an empty placeholder type kept so
a future option (an explicit edit rate for an audio-only file, KAG
alignment) does not need a signature change. `KAGSize` is fixed at `1` (no
alignment grid; nothing downstream needs it — `vaco-demux-mxf` reads
forward by key, never trusts byte-count arithmetic).

## Dependencies

`vaco-core`, `vaco-io` (`IoWriter`, `MediaSink`), `vaco-limits`,
`vaco-time` (`unix_nanos`, D18 — the only clock access in this crate, for
UMID entropy), `vaco-packet`, `vaco-format-core` (`Muxer`, `MuxerDesc`),
`vaco-codec-core` (`CodecId`, `CodecParameters`). Dev-only:
`vaco-demux-mxf` (round-trip tests), `vaco-chlayout` (test fixtures),
`proptest`.

## Deferred work

- **Byte-identity against the reference: confirmed achievable, not
  attempted.** `-fflags +bitexact -bitexact` makes two independent
  `ffmpeg -f mxf` runs produce byte-identical output — verified directly
  this session (the UMID's material-number field is zeroed under bitexact,
  not random/time-based, which is what makes this possible; the
  coordinating dispatch's assumption that UMIDs "cannot be byte-identical
  without controlling them" undersold what `ffmpeg` itself already does).
  Matching that exactly would mean replicating `ffmpeg`'s literal partition
  count, its duplicate-metadata-in-the-footer layout, System Item placement
  per edit unit rather than per essence element, and several descriptor
  properties this crate does not yet write (`AspectRatio`, a 16-byte
  property at tag `0x320d` whose meaning was not identified) — a
  substantially larger undertaking than this package's scope, and not
  pursued. The round trip (this crate's own demuxer, and a real `ffprobe`/
  `ffmpeg`) is the bar this crate is verified against instead.
- **A two-essence-track file's descriptor parameters do not resolve under a
  real `ffmpeg -i`.** `vaco-demux-mxf`'s own `MultipleDescriptor`
  expansion (`SubDescriptorUIDs` matched by `LinkedTrackId`) correctly
  resolves both tracks' real parameters from this crate's output — the
  `a_video_and_audio_file_reports_both_streams_via_the_multiple_descriptor_expansion`
  test is byte-for-byte proof. A real `ffmpeg -i` on the same file
  correctly identifies both streams' *media type* (the `TrackID`/
  `DataDefinition` fixes above apply equally here) but logs `source track
  N: stream M, no descriptor found` and reports `codec_name=unknown`,
  `width=0`, `height=0` for both. Not root-caused: `ffmpeg`'s own
  `LinkedTrackId` matching evidently differs from the mechanism
  `vaco-demux-mxf` measured and this crate replicates, in some way not yet
  identified (a plausible candidate, untested: positional matching against
  `PackageTracks` index rather than `LinkedTrackId` value, which the
  timecode track's presence at index 0 would throw off — see
  `planning/TECH-DEBT.md`).
