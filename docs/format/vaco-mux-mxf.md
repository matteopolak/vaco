# `vaco-mux-mxf`

Layer 4. The MXF (Material eXchange Format) muxer: OP1a, one video track
and at most one audio track, frame-wrapped. Registered as `mxf`.

Written clean-room (D7/D15): this crate does not read `ffmpeg` source. It
reuses its sibling `vaco-demux-mxf`'s own already-published, clean-room
measurement of the KLV/BER wrapper, the Partition Pack, the
structural-metadata graph, and Universal Labels and property tags
(`provenance/sources.toml`'s `ffmpeg-mxf-probe` family), plus a handful of
local-tag *numbers* and structural details this crate measured fresh
against real headers this session (`ffmpeg-mxf-mux-header-probe`) — the
demux crate only ever needed the resolved UL, never which conventional tag
carries it, or details (partition count, System Item placement) it never
had to reproduce on write. Every claim below is cross-checked against both
`vaco-demux-mxf` (a dev-dependency, used the same way `vaco-mux-mp4`
depends on `vaco-demux-mp4`) and a real `ffprobe`/`ffmpeg` on the
development machine.

**The two demuxer says nothing, only the reference does.** Three separate
real bugs in this crate were invisible to `vaco-demux-mxf` round-trip
testing and caught only by a real `ffmpeg -i`: `TrackID` reservation,
`DataDefinition`'s three values, and `SubDescriptorUIDs`'s real local tag
(see "The structural-metadata graph" below). `vaco-demux-mxf` does not read
any of the three properties involved. This is the concrete argument for
round-tripping through the reference rather than through this workspace's
own understanding of the format alone.

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

### Partition layout

A "closed, complete" header partition (full structural metadata, generated
once at `init()`, `body_sid = 0` — it carries no essence itself), then a
genuine Body Partition Pack (`body_sid = 1`, essence follows directly after
it), then a "closed, complete" footer partition, then a Random Index Pack.

**The Body Partition Pack is not optional, for any track count.** An
earlier version of this crate omitted it for a single essence track,
reasoning from `vaco-demux-mxf`'s own D-10 corpus (a real `ffmpeg -f
mxf_d10` file's header partition carries essence directly, no separate
body pack). A literal byte-for-byte `cmp` against a real single-track
`ffmpeg -f mxf -fflags +bitexact` file found a Body Partition Pack there
too, at the same relative position as a two-track file's — D-10's
single-partition shape is real for `-f mxf_d10` specifically, not for
OP1a's `-f mxf`, and this crate targets OP1a. Checking a claim about "what
a real file does" against a *second* real file, not just re-reading the
first one's own conclusion, is what caught this.

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
outright was cheaper and is not a loss, since nothing needs it. This is
also the reason full byte-identity with the reference is not attempted —
see "Deferred work".

**The `FooterPartition` backpatch covers every partition pack, not just the
header's.** `vaco-demux-mxf::demux::MxfDemuxer::open` only ever checks the
header's `FooterPartition` to find the footer, so backpatching only that
one field would have been sufficient for this crate's own reader — but a
real `ffmpeg -f mxf` file backpatches the Body Partition Pack's copy too,
so `MxfMuxer` tracks every partition pack's `FooterPartition` field
position (`footer_field_positions`) and overwrites all of them on a
seekable sink. On a non-seekable sink every field stays `0` and the footer
is present but not reachable by a reader that trusts `FooterPartition == 0`
to mean "no footer" — an honest degradation, not a silent one
(`a_non_seekable_sink_still_produces_a_sequentially_readable_file` is the
regression test).

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

**`SubDescriptorUIDs`'s real local tag is `0x3f01`, not an invented
`0x0603` — the multi-track descriptor-resolution bug, and the most
user-visible one found this session.** A two-essence-track file
round-tripped cleanly through `vaco-demux-mxf` (which resolves properties
by UL through the primer, so the tag number this crate chose for a
property genuinely should not matter) but a real `ffmpeg -i` logged
`source track N: stream M, no descriptor found` for **both** tracks and
reported `codec_name=unknown`, zeroed dimensions/rate for both — `ffmpeg`'s
own resolution of this one property evidently does not go through the
general per-file primer/UL matching every other property here does.
Decoding a real two-track file's actual primer confirmed the UL this crate
already had registered for `SubDescriptorUIDs` was correct — only the
*tag number* (measured this session directly, not previously) differed.
Changing it to `0x3f01` made a real `ffmpeg -i` resolve both tracks
completely. Two smaller bugs surfaced by the same investigation: this crate
had been writing the *video* essence-container UL onto the audio
descriptor too (see "Essence and the Index Table Segment" below), and the
`MultipleDescriptor`/`Preface`/Partition-Pack `EssenceContainers` lists did
not distinguish "one media type", "the other", and "more than one" the way
a real file's three-entry list does.

### Essence and the Index Table Segment

`essence::track_number` assigns each essence track a Generic Container
track number (`0x15` frame-wrapped Picture, `0x16` frame-wrapped Sound,
per-media-type index starting at 1) — self-assigned, not measured against a
specific real value, since correctness here only requires internal
consistency between the essence element's own key and the matching Track's
`EssenceTrackNumber` property, which both `vaco-demux-mxf` and `ffmpeg`
confirmed reading back correctly.

**Each essence kind states its own `EssenceContainer` label** —
`ul::ESSENCE_CONTAINER_MPEG_FRAME_WRAPPED` for picture,
`ESSENCE_CONTAINER_SOUND_FRAME_WRAPPED` for sound (measured off a real
`AES3PCMDescriptor` this session), and `ESSENCE_CONTAINER_MULTIPLE_WRAPPINGS`
on a `MultipleDescriptor` itself and in `Preface`/both partition packs'
`EssenceContainers` batch whenever more than one essence kind is present
(`metadata::essence_containers_used` builds the exact three-entry list a
real two-track file states, in the same order). Getting this wrong —an
earlier version reused the picture label for the audio track's own
property — did not stop `vaco-demux-mxf` from reading the file (that crate
never interprets this property's value) but made a real `ffmpeg -i` guess
`mp2` instead of `pcm_s16le` for the audio stream, even after the
dimensions/rate had already resolved correctly.

**One Generic Container System Item per edit unit, shared across every
track** — corrected this session from an earlier "one per essence element,
per track" simplification, after decoding a real two-track file's exact
KLV sequence (`SystemItem, Video, Audio, SystemItem, Video, Audio, ...`,
never `SystemItem, Video, SystemItem, Audio`). `MxfMuxer` tracks the edit
unit (`Packet::pts`) the last-written System Item covers
(`last_system_item_pts`) and only writes a new one when a packet's own
edit unit differs — correct only because every track shares one edit rate
(this crate's own documented scope, see below). The System Item's value is
still empty; neither reader interprets its content, only recognises the
key.

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
  see the `DataDefinition` and `SubDescriptorUIDs` accounts above for what
  happens when a byte or a tag number gets guessed and nothing but the
  reference notices.
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
- **Chasing byte-identity further**: `partition::write`'s hardcoded
  `KAGSize` (currently `1`, real files use `512` with Fill Item padding to
  match) is the next concrete, well-understood divergence — see "Deferred
  work".

---

## Configuration

No options exposed today; `MxfOptions` is an empty placeholder type kept so
a future option (an explicit edit rate for an audio-only file, real KAG
alignment) does not need a signature change. `KAGSize` is fixed at `1` (no
alignment grid; nothing downstream needs it for correctness —
`vaco-demux-mxf` reads forward by key, never trusts byte-count arithmetic —
but it is a real, measured divergence from a real file's `512`; see
"Deferred work").

## Dependencies

`vaco-core`, `vaco-io` (`IoWriter`, `MediaSink`), `vaco-limits`,
`vaco-time` (`unix_nanos`, D18 — the only clock access in this crate, for
UMID entropy), `vaco-packet`, `vaco-format-core` (`Muxer`, `MuxerDesc`),
`vaco-codec-core` (`CodecId`, `CodecParameters`). Dev-only:
`vaco-demux-mxf` (round-trip tests), `vaco-chlayout` (test fixtures),
`proptest`.

## Deferred work

- **Byte-identity against the reference: confirmed achievable, partially
  chased, deliberately bounded.** `-fflags +bitexact -bitexact` makes two
  independent `ffmpeg -f mxf` runs produce byte-identical output — verified
  directly this session (the UMID's material-number field is zeroed under
  bitexact, not random/time-based, which is what makes this possible; the
  coordinating dispatch's assumption that UMIDs "cannot be byte-identical
  without controlling them" undersold what `ffmpeg` itself already does).
  A literal `cmp` against a real single-track bitexact file, feeding this
  crate's muxer the *same* real MPEG-2 frames the reference encoded (so the
  essence bytes are identical and only the container differs), found and
  fixed two further structural divergences this session: the Partition
  Pack's minor version (`3`, this crate had `2`) and the Body Partition
  Pack being unconditional (see "Partition layout" above). After both
  fixes, the first remaining byte-level divergence is `KAGSize` (`1` here,
  `512` in a real file, which also pads structures with Fill Items to that
  boundary) — a genuine, well-understood, and still-open structural gap.
  Beyond `KAGSize`/Fill-Item alignment, the dominant remaining difference
  is still the deliberately-dropped duplicate-footer-metadata layout (see
  "Partition layout" above): the file-size gap this leaves (several KiB)
  swamps a byte-level `cmp` past that point, which is why this was
  bounded here rather than chased to zero — restating the footer would
  reopen the `Multiple primer packs`/media-type-misreport bug this session
  already spent real effort fixing, and the two goals (byte-identity,
  correctness under a real reference reader) are in real tension at that
  specific point, not just a matter of more time.
  Two real, identified-but-unwritten descriptor properties were found
  along the way and are recorded here rather than guessed into the
  descriptor: tag `0x320e` (8 bytes, two 4-byte ints) is `AspectRatio`
  (confirmed against two real fixtures: `(5,4)` on a 720x576 file, `(4,3)`
  on a 320x240 one — both correct display aspect ratios); tag `0x320d` (16
  bytes: `Count=2, ItemLength=4`, then two 4-byte ints) is very likely
  `VideoLineMap` (a batch of the first active line number per field —
  `[46, 0]` on the interlaced 720x576 fixture, `[0, 0]` on the progressive
  320x240 one, consistent with `FrameLayout`), but this was not
  cross-checked against a third fixture and is reported as a strong
  inference, not a confirmed measurement.
- **A two-essence-track file's descriptor resolution under a real
  `ffmpeg -i`: resolved this session.** Fixed by the `SubDescriptorUIDs`
  tag correction and the per-media-type `EssenceContainer` fix above (see
  "The structural-metadata graph" and "Essence and the Index Table
  Segment"). `a_real_ffprobe_resolves_both_tracks_of_a_multiple_descriptor_file`
  is the regression test against a real `ffprobe`.
