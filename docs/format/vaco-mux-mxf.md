# `vaco-mux-mxf`

Layer 4. Three MXF (Material eXchange Format) muxers, matching three
distinct registered `ffmpeg` muxer names (`ffmpeg -muxers | grep mxf`):

| Registered as | Variant | Scope in this crate |
|---|---|---|
| `mxf` (`MUXER`) | OP1a | one video track, at most one audio track, frame-wrapped |
| `mxf_d10` (`MUXER_D10`) | D-10 (SMPTE 386M) | video-only, one of three fixed CBR bitrates (30/40/50 Mbit/s) |
| `mxf_opatom` (`MUXER_OPATOM`) | OP-Atom (SMPTE 390) | exactly one essence track per file, clip-wrapped |

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
| `klv` | Writing one Key-Length-Value triplet, plus `pad_to_kag` (KLV Alignment Grid padding via a Fill Item) |
| `ul` | The Universal Labels and key-building functions this crate writes, and `MxfVariant` (`Op1a`/`D10`/`OpAtom`) |
| `uid` | `InstanceUID`/Package UMID generation (a counter mixed with `vaco_time::unix_nanos()`, no new RNG dependency) |
| `localset` | The `Tag(u16) Length(u16) Value` item writer, batches, and the Primer Pack builder |
| `metadata` | Building the structural-metadata graph: `Preface` -> `ContentStorage` -> `Package` (Material and Source) -> `Track` -> `Sequence` -> `SourceClip`/`TimecodeComponent` -> `Descriptor`; variant-aware descriptor/essence-container/operational-pattern selection |
| `partition` | The Partition Pack's fixed-position layout, including `KAGSize` |
| `essence` | Generic Container track-number assignment and essence element keys, per variant (`track_number` for OP1a/OP-Atom, `track_number_d10` for D-10's distinct item-type byte) |
| `index` | Index Table Segment construction: `build` (VBE, OP1a/OP-Atom) and `build_cbe` (D-10) |
| `mux` | `MxfMuxer`: implements `Muxer` for all three variants; `MUXER`/`MUXER_D10`/`MUXER_OPATOM` are the registered muxer descriptors |

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

**The Random Index Pack states one entry per partition pack, each with
that partition's own real `BodySID`.** Measured against three real
fixtures (an OP1a, a D-10, an OP-Atom file): the header's own entry states
whatever `BodySID` the header partition pack itself states (`0` for
OP1a/OP-Atom, `1` for D-10, which carries essence directly — see below),
the Body Partition Pack (when written) gets its own entry stating `1`, and
the footer's own entry always states `0`. An earlier version of this crate
hardcoded exactly two entries — the header unconditionally as `BodySID =
1`, no entry at all for the Body Partition Pack — which was wrong on both
counts for OP1a/OP-Atom and only coincidentally close to right for D-10's
own no-body-partition shape. `MxfMuxer::rip_entries` now records each
partition's real `(BodySID, offset)` pair as it is written; the RIP's own
trailing restated length also now accounts for its own (minimal-width, see
below) length-prefix width rather than assuming a fixed `4` bytes. Found
while re-measuring the RIP's own BER length width, not while looking for
this specifically — see "The byte-identity matrix".
`the_random_index_pack_names_every_partition_with_its_own_body_sid` is the
regression test, and it checks this crate's own output via
`vaco_demux_mxf::partition::find_rip` rather than re-deriving the same
assertion the writer already made about itself.

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

**One `EssenceContainerData` set per file, naming the essence-carrying
`BodySID`.** `ContentStorage` carries a second batch property (tag
`0x1902`, alongside `Packages` at `0x1901`) referencing it. Identified this
session by decoding a real file's own class-`0x23` set directly and
cross-validating two independent ways: its `InstanceUID` is exactly what
`ContentStorage`'s `0x1902` batch references, and its `LinkedPackageUID`
property (tag `0x2701`, a 32-byte value under the SMPTE UMID designator
root `06 0a 2b 34...`, distinct from every other property's `06 0e 2b
34...` Universal Label root) is byte-for-byte identical to the
`SourcePackage`'s own UMID used elsewhere in the file. Alongside `BodySID`/
`IndexSID` (tags `0x3f07`/`0x3f06`, already known from the Index Table
Segment), this is exactly ST 377-1's `EssenceContainerData` class. Every
variant writes exactly one, since every variant here uses exactly one
essence-carrying `BodySID`.

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

### KLV Alignment Grid (KAG)

`KAGSize = 512` (`mux::KAG_SIZE`), matching every real `ffmpeg -f
mxf`/`-f mxf_d10`/`-f mxf_opatom` file this session measured. `klv::
pad_to_kag` writes one Fill Item KLV padding the current position out to
the next 512-byte boundary; `partition::write` writes the real `512` into
the Partition Pack's own `KAGSize` field (an earlier version hardcoded `1`,
"no alignment", there). Confirmed byte-for-byte against a real bitexact
OP1a fixture via a literal `cmp` (feeding this crate's muxer the same real
MPEG-2 frames the reference encoded): the `KAGSize` field itself, and every
byte up to the Primer Pack, now match exactly.

**Where the padding lands differs by variant** — measured directly, not
assumed to generalise from OP1a:

- **OP1a**: after the header partition pack, after the Primer Pack, after
  the structural metadata, after the Body Partition Pack, and once more
  right before the very first essence element in the whole file. Never
  between subsequent essence elements — confirmed directly (one frame's KLV
  ends and the next begins with no gap).
- **D-10**: the same header-region padding (no Body Partition Pack — see
  below), *plus* every edit unit pads its System Item KLV to a boundary
  and its essence element to a boundary independently — measured against a
  real 30 Mbit/s fixture: `EditUnitByteCount = 151040 =
  round_up_to_kag(20) [empty-value System Item] +
  round_up_to_kag(20 + 150000) [essence element]`. `mux::round_up_to_kag`
  reproduces this arithmetic to compute the value ahead of any packet
  arriving (D-10's Index Table Segment is embedded in the header — see
  below — so it must be known before `write_header` returns).
- **OP-Atom**: the same header-region padding, plus once after the single
  clip-wrapped essence element `write_trailer` writes.

### D-10 (SMPTE 386M)

Video-only in this crate (see "Deferred work" for audio). Structural
differences from OP1a, each measured directly against a real `ffmpeg -f
mxf_d10` file rather than assumed from OP1a's shape:

- **No Body Partition Pack.** The header partition states `body_sid = 1`,
  `body_offset = 0` directly, and essence follows right after the header's
  own KAG padding.
- **The header embeds a complete CBE Index Table Segment.** `IndexDuration
  = 0` (D-10 is CBR, so this is computed entirely upfront — no footer
  deferral needed the way OP1a's VBE index requires); `EditUnitByteCount`
  nonzero (`mux::MxfMuxer::build_d10_index_table`'s doc comment has the
  arithmetic); no `IndexEntryArray` at all — `vaco-demux-mxf::index::parse`
  already treats it as optional, and a CBE segment does not need one
  (`IndexTableSegment::cbe_offset` computes any edit unit's position from
  `EditUnitByteCount` alone). The real file measured also carried a short
  batch under an unidentified local tag (`0x3f09` in that file's own
  primer, shaped like `DeltaEntryArray`) that this crate does not attempt
  to reproduce — not measured with confidence.
- **`CDCIEssenceDescriptor` (class `0x28`), not `MPEGVideoDescriptor`
  (`0x51`)** — measured off a real file's structural-set class bytes in
  sequence. `vaco-demux-mxf`'s own `StructuralClass::Descriptor` already
  recognises `0x28` (it folds every descriptor subclass into one arm and
  reads by property, not by class), so this needed no read-side change.
- **`FrameLayout = 1` ("Separate Fields") halves `Stored`/`Sampled`/
  `DisplayHeight`** — `288` for a 576-line frame, measured against a real
  file; `vaco-demux-mxf::descriptor::picture_parameters` already doubles it
  back on the way in (that crate's own doc comment records the same
  measurement), so this crate's own round trip reports the real `576`.
- **A D-10-specific `EssenceContainer` UL**
  (`ul::ESSENCE_CONTAINER_D10_VIDEO`, `...0d.01.03.01.02.01.05.01`) and one
  of three fixed-bitrate `PictureEssenceCoding` labels
  (`PICTURE_ESSENCE_CODING_D10_{30,40,50}MBIT`), reused from
  `vaco-demux-mxf::descriptor::PICTURE_ESSENCE_CODING`'s own already-measured
  D-10 rows. Chosen from the caller's declared `CodecParameters::bit_rate`
  — D-10 is a constrained CBR profile, so `add_stream`/`write_header`
  require it (`build_d10_index_table` returns `Error::Unsupported` without
  one); a value outside a wide tolerance of the three rates falls back to
  30 Mbit/s rather than failing outright.
- **The essence element's own item-type byte is `0x05`, not OP1a's
  `0x15`** (`essence::track_number_d10`) — measured off a real file's
  Generic Container key. `vaco-demux-mxf`'s own reader matches essence by
  the full track number against `Track.EssenceTrackNumber`, not by
  interpreting this byte, so no read-side change was needed; confirmed by
  that same crate's own `essence.rs` module docs, which record finding a
  real D-10 file frame-wrapped (one KLV per edit unit, twenty-five of them)
  at a key byte ST 379-1's own table would call "clip-wrapped".
- **The Operational Pattern label is the same as OP1a's**, not a distinct
  D-10 label — measured against a real file, byte for byte.

Verified: `a_d10_file_round_trips_through_the_sibling_demuxer` (own
demuxer, dimensions correctly un-halved) and
`a_real_ffprobe_reports_the_correct_stream_shape_for_a_d10_file` (real
`ffprobe`). A real `ffmpeg -i`/`ffprobe -show_packets` on this crate's own
D-10 output reports three packets of 150000 bytes each at the expected
positions — the reference's own reader frames the container identically to
this crate's own demuxer.

### OP-Atom (SMPTE 390)

Exactly one essence track per file (`add_stream` rejects a second stream
outright) — the simplification the coordinating dispatch anticipated: no
`MultipleDescriptor`, no multi-track System Item bookkeeping. What turned
out to be the larger difference, found only by generating and hex-dumping a
real `ffmpeg -f mxf_opatom` file (this crate's own installed `ffmpeg 8.1`
has no clip-wrap option for `mxf`/`mxf_d10`, so this was the first
producible sample):

- **OP-Atom's essence is clip-wrapped: one Generic Container element for
  the whole file, not one per frame.** Measured directly: a single essence
  key appears exactly once, its own BER length stating the entire payload,
  and no System Item key appears anywhere in the file (OP-Atom needs no
  per-edit-unit sync marker — there is only ever one essence stream and
  nothing to interleave). `write_packet` buffers every packet's payload
  into `MxfMuxer::clip_buffer` instead of streaming it, recording each
  frame's offset within the eventual element for the (still VBE) Index
  Table Segment; `write_trailer` writes the one real element once the
  final length is known, then proceeds with the same footer/RIP logic
  OP1a uses.
- A genuine Body Partition Pack precedes the essence, same relative
  position as OP1a's (unlike D-10, which has none).
- The Operational Pattern label (`ul::OPERATIONAL_PATTERN_OP_ATOM`) is
  distinct from OP1a's/D-10's — reused from
  `vaco-demux-mxf::ul::op::OP_ATOM`, which had already measured it against
  that crate's own `opatom.mxf` corpus file, and re-confirmed this session
  byte for byte against a freshly generated fixture. The `EssenceContainer`
  label, by contrast, is byte-identical to OP1a's picture label — measured,
  not assumed.
- The descriptor stays `MPEGVideoDescriptor` (not CDCI) — OP-Atom is not
  D-10's constrained profile.

**Reading it back is where the surprise is.** This workspace's own
`vaco-demux-mxf::demux::MxfDemuxer::read_packet` reads "one KLV, one
packet" unconditionally (that crate's own `essence.rs` module docs explain
why: `clip_wrapped_spans` exists but was never wired in, for lack of a real
clip-wrapped sample to test it against before this crate started producing
one). So this crate's own demuxer reads an OP-Atom file back as a *single*
packet holding every frame concatenated. Checked directly against a real
`ffmpeg -i`/`ffprobe -show_packets` on the identical file: the reference
reports the exact same shape — one packet, the full concatenated size.
Frame-level access into OP-Atom's clip-wrapped essence is apparently a
decoder-level concern (parsing picture start codes within the one packet),
not a container-framing one; this crate's write side and both demuxers'
read sides already agree, so this is not treated as a gap.

Verified: `an_op_atom_file_round_trips_through_the_sibling_demuxer` (stream
shape, and the single-packet shape above, asserted rather than hidden) and
`adding_a_second_stream_to_an_op_atom_muxer_is_rejected`.

### `AspectRatio`: a real functional gap, not a byte-identity nicety

Tag `0x320e` (8 bytes, two 4-byte ints, the display aspect ratio) is a
property `vaco-demux-mxf::properties::PropertyId::AspectRatio` already
reads on the way in (`descriptor::picture_parameters`, into
`sample_aspect_ratio`) — this crate simply never wrote it. Confirmed
against three real fixtures across two sessions (`(5,4)` and `(4,3)` on two
different OP1a resolutions, `(5,4)` again on a D-10 file), so now written
for every video descriptor (OP1a, D-10, OP-Atom alike) whenever the
caller's `CodecParameters::video.sample_aspect_ratio` is a usable, nonzero
value (`metadata::display_aspect_ratio` does the DAR-from-SAR conversion,
the exact inverse of the read side's `display_to_sample_aspect`). Tag
`0x320d` (`VideoLineMap`, tentatively identified, see "Deferred work") is
still not written — a genuine measurement gap, not a decision to omit it.

### The byte-identity matrix

For each variant: whether it round-trips (via this crate's own demuxer and
a real `ffprobe`/`ffmpeg -i`), and where the first byte-level divergence
against a real `-fflags +bitexact` file sits (via the `cmp`
same-real-MPEG-2-frames methodology described in "Deferred work" below).

| Variant | Own demuxer | Real `ffprobe`/`ffmpeg -i` | First `cmp` divergence past `KAGSize` |
|---|---|---|---|
| OP1a | Round-trips exactly (stream shape, packet positions/sizes, `MultipleDescriptor` expansion) | Resolves correctly (single- and two-track) | The real file's header region is larger by roughly `1.5`-`2` KiB, dominated by the Primer Pack registering ~100 tags regardless of which properties this file actually uses (this crate's own primer lists only what it writes) and a fully-populated `Identification` set (real product/version strings this crate does not write) — see "Deferred work" |
| D-10 | Round-trips exactly (dimensions correctly un-halved) | Resolves correctly; packet count/positions match exactly | Same shape as OP1a's remaining gap |
| OP-Atom | Round-trips as a single concatenated packet (matches the reference's own packet count — see above) | Stream shape resolves correctly | Not independently re-measured this session; the OP1a/D-10 findings are expected to generalise |

**Two things this dispatch was specifically asked to chase, both resolved
with a definite answer:**

1. **The Primer Pack's BER width is not Primer-Pack-specific, and it is
   not universal either — it is a per-KLV-family convention, now measured
   precisely.** Walking every KLV in two real fixtures (a single-track
   file and a freshly generated two-track file) by decoding each length
   prefix directly found a consistent split: the Partition Pack family,
   the Fill Item, the System Item, essence elements, the Index Table
   Segment, and every essence *descriptor* class (`MPEGVideoDescriptor`,
   `AES3PCMDescriptor`) keep this crate's fixed-width form; the Primer
   Pack and every *other* structural set (`Preface`, `Identification`,
   `ContentStorage`, both `Package`s, `Track`, `Sequence`, `SourceClip`,
   `TimecodeComponent`, `MultipleDescriptor`) and the Random Index Pack use
   minimal-width BER instead (short form under 128, else the smallest long
   form). `ber.rs`'s own module docs have the full measurement;
   `klv::write_structural_set` is the write-side switch, keyed on the same
   class byte `ul::structural_set_key` already encodes. Fixed, with a new
   `ber::encode_minimal` and a property test asserting every `u64`
   round-trips through both encodings.
2. **The ~1536 bytes were not one thing.** Part of it is real and has been
   identified and fixed: a structural set at class byte `0x23`,
   `EssenceContainerData` (ST 377-1), one per file, naming the
   essence-carrying `BodySID`/`IndexSID` pair and linking it back to the
   `SourcePackage` by UMID. Identified with the same rigor as
   `SubDescriptorUIDs`'s real tag: decoded the set's actual bytes, then
   cross-validated two independent ways rather than pattern-matching a
   spec table — its own `InstanceUID` is exactly what `ContentStorage`'s
   previously-unnamed second batch property (tag `0x1902`) references, and
   its `LinkedPackageUID` value is byte-for-byte identical to the
   `SourcePackage`'s own UMID used elsewhere in the same file. Now
   written by every variant (see "The structural-metadata graph" above).
   But this set is only ~90 bytes — it does not come close to accounting
   for the full gap. The dominant remainder, found while re-measuring
   after the fix, is **not a missing structural set at all**: a real
   file's Primer Pack registers a fixed ~100-tag dictionary regardless of
   which properties this specific file uses (measured: `100` entries, `1808`
   bytes, even for a single-track file with a small fraction of those
   properties actually referenced), and its `Identification` set carries
   real product metadata (`CompanyName = "FFmpeg"`, `ProductName = "OP1a
   Muxer"`, `VersionString`, `Platform = "Lavf"`, a `ProductUID`, a
   `ModificationDate`, a `ToolkitVersion` — none of which this crate
   writes today, since its own `Identification` set states only an
   `InstanceUID`). Both are recorded in "Deferred work" rather than chased
   further this session: the primer-table one is a deliberate economy
   (registering only tags this crate actually uses is smaller and no less
   correct — `vaco-demux-mxf` reads either shape identically), not a bug,
   and matching it byte-for-byte would mean replicating an internal
   `ffmpeg` table with no functional payoff; the `Identification` one is a
   real, cheap enrichment worth doing, but even fully implemented it would
   not reach byte-identity on its own, since this crate's own product name
   is not literally `"FFmpeg"`.
3. **A third, unasked-for finding surfaced while measuring the Random
   Index Pack's own BER width: the RIP's entries were wrong.** A real RIP
   has one entry per partition pack actually in the file, each stating
   *that partition's own* `BodySID` — measured against three real
   fixtures (an OP1a, a D-10, an OP-Atom file). This crate's RIP hardcoded
   exactly two entries (the header unconditionally stated as `BodySID = 1`,
   the footer as `0`) and never wrote one for the Body Partition Pack at
   all — wrong on both counts for OP1a/OP-Atom (whose header actually
   states `BodySID = 0`, and whose Body Partition Pack got no entry),
   coincidentally close to correct only for D-10's own no-body-partition
   shape. Fixed: `MxfMuxer::rip_entries` now tracks each partition's real
   `(BodySID, offset)` pair as it is written, and the RIP's own restated
   total length accounts for its own (now minimal-width) length prefix
   rather than assuming a fixed 4 bytes. Regression test:
   `the_random_index_pack_names_every_partition_with_its_own_body_sid`,
   which parses this crate's own output with
   `vaco_demux_mxf::partition::find_rip` — the reference's own reading of
   this crate's RIP, not just this crate's own writer's self-report.

**`KAGSize` was the next divergence, not the whole gap, for every
variant — and it still is, one layer deeper.** The field itself and every
byte up to the Primer Pack match a real file exactly. Past it, the two
things this dispatch was asked to chase are now both *resolved as
findings* (a precise, general BER-width rule; a real, identified, and
fixed missing structural set) even though neither, once fixed, was
sufficient on its own to close the remaining gap — which turned out to be
dominated by something neither hypothesis named: a Primer Pack sized to a
static dictionary rather than actual usage, and an unpopulated
`Identification` set. **No variant reached `cmp`-identity this session.**
That is a real, honest outcome: two specifically-named divergences were
chased to ground and both turned out smaller than the total gap, which is
a more useful thing to know than either "found and fixed everything" or
"the gap didn't move."

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
- **D-10 audio**: not implemented (`add_stream` rejects an audio stream
  under `MUXER_D10` outright). The fixed 8-slot AES3 bundle
  (`4 + 1920×8×4 = 61444` bytes per edit unit at 25fps) is already measured
  on the read side (`provenance/sources.toml`'s `ffmpeg-mxf-sound-essence-probe`);
  writing it needs a `GenericSoundEssenceDescriptor` (class `0x42`,
  spec-derived and unexercised on the read side too — see
  `docs/format/vaco-demux-mxf.md`) and interleaving the fixed-size bundle
  into the same per-edit-unit KAG rhythm `write_packet`'s D-10 branch
  already applies to video.
- **A real timecode value**: this crate's timecode track always states
  `TimecodeStart = 0`; wiring a real starting timecode through from the
  caller is unclaimed work, not blocked by anything here.
- **A third or more essence track for D-10/OP-Atom**: D-10 is video-only by
  design in this crate (see above); OP-Atom is video-only by the format's
  own definition (`add_stream` enforces it). Neither has the "more than one
  essence track" question OP1a's `MultipleDescriptor` path answers.
- **Chasing byte-identity further**: the byte-identity matrix above names
  the next two concrete divergences (Primer Pack BER width, an unidentified
  structural set) — see "Deferred work" for what each would need.

---

## Configuration

No options exposed today; `MxfOptions` is an empty placeholder type kept so
a future option (an explicit edit rate for an audio-only file) does not
need a signature change. `KAGSize` is fixed at `512` (`mux::KAG_SIZE`),
matching every real file measured — not configurable, since nothing in
this crate's own scope needs a different grid. Which muxer variant runs is
selected by which `MuxerDesc` opens the file (`MUXER`/`MUXER_D10`/
`MUXER_OPATOM`), not by an `MxfOptions` field — matching `vaco-mux-asf`'s
`MUXER`/`MUXER_STREAM` pair elsewhere in this workspace. D-10 additionally
requires `CodecParameters::bit_rate` to be one of the three fixed rates
(30/40/50 Mbit/s); `add_stream`/`write_header` return `Error::Unsupported`
without one.

## Dependencies

`vaco-core`, `vaco-io` (`IoWriter`, `MediaSink`), `vaco-limits`,
`vaco-time` (`unix_nanos`, D18 — the only clock access in this crate, for
UMID entropy), `vaco-packet`, `vaco-format-core` (`Muxer`, `MuxerDesc`),
`vaco-codec-core` (`CodecId`, `CodecParameters`). Dev-only:
`vaco-demux-mxf` (round-trip tests), `vaco-chlayout` (test fixtures),
`proptest`.

## Deferred work

- **Byte-identity against the reference: confirmed achievable, `KAGSize`
  fixed, the two divergences chased this session both resolved as
  findings — see the byte-identity matrix above for the full account.**
  `-fflags +bitexact -bitexact` makes independent `ffmpeg -f mxf`/`-f
  mxf_d10` runs produce byte-identical output — verified directly, across
  three sessions now (the UMID's material-number field is zeroed under
  bitexact, not random/time-based). A literal `cmp` against real bitexact
  files, feeding this crate's muxer the *same* real MPEG-2 frames the
  reference encoded (so the essence bytes are identical and only the
  container differs), found and fixed, across three sessions: the
  Partition Pack's minor version (`3`, not `2`), the Body Partition Pack
  being unconditional for OP1a, `KAGSize`/Fill-Item alignment (`512`, not
  `1`), and this session: the BER-length-width convention (`ber::
  encode_minimal`, used via `klv::write_structural_set`/`write_minimal`
  for the Primer Pack, most structural sets, and the Random Index Pack —
  see "The byte-identity matrix" for the exact split measured), a missing
  `EssenceContainerData` set (now written by every variant), and a
  previously-wrong Random Index Pack (now one entry per real partition
  pack, each with its own real `BodySID`). None of these three fixes was,
  on its own or together, sufficient to reach `cmp`-identity: the dominant
  remaining gap, found while re-measuring after the fixes above, is a real
  file's Primer Pack registering a fixed ~100-tag dictionary regardless of
  actual usage, and a fully-populated `Identification` set carrying real
  product/version strings this crate does not write. Neither is chased
  further this session — the primer-table one is a deliberate economy
  (registering only what is used is smaller and equally correct, with no
  functional payoff to matching a static internal table byte-for-byte);
  the `Identification` one is a real, cheap enrichment (`CompanyName`,
  `ProductName`, `VersionString`, `Platform`, a `ProductUID`, a
  `ModificationDate`, a `ToolkitVersion` — measured, not guessed: see "The
  byte-identity matrix") that is worth doing but would not reach
  byte-identity even fully implemented, since this crate's own product
  name is not literally `"FFmpeg"`.
  Two real, identified-but-unwritten descriptor properties were found
  across the two sessions and are recorded here rather than guessed into
  the descriptor: tag `0x320e` is `AspectRatio` — confirmed against three
  real fixtures now (two OP1a resolutions plus one D-10 file) and **written
  as of this session** (see "`AspectRatio`" above, so this bullet now
  documents what changed, not an open gap). Tag `0x320d` (16 bytes:
  `Count=2, ItemLength=4`, then two 4-byte ints) is very likely
  `VideoLineMap` (the first active line number per field — `[46, 0]` on
  interlaced 720x576 OP1a, `[23, 336]` on D-10 720x576 (`FrameLayout = 1`),
  `[0, 0]` on progressive 320x240, consistent with `FrameLayout` across
  three data points now), but still not cross-checked with enough
  confidence to write it — `docs/format/vaco-demux-mxf.md` has no
  `PropertyId` for it either, so there is nothing to round-trip against on
  the read side yet.
- **A two-essence-track file's descriptor resolution under a real
  `ffmpeg -i`: resolved in an earlier session.** Fixed by the
  `SubDescriptorUIDs` tag correction and the per-media-type
  `EssenceContainer` fix above (see "The structural-metadata graph" and
  "Essence and the Index Table Segment").
  `a_real_ffprobe_resolves_both_tracks_of_a_multiple_descriptor_file` is
  the regression test against a real `ffprobe`.
- **D-10 audio, OP-Atom frame-level packet access**: see "How to change
  it" and the OP-Atom section above, respectively — the latter is not
  treated as a gap this crate should close, since the reference's own
  reader exhibits the identical behaviour on this crate's own output.
