# `vaco-codec-subtitle-bitmap`

Layer 4. DVB (ETSI EN 300 743), DVD (`VobSub`) subpicture and PGS/HDMV
bitmap subtitle decode: run-length pixel decompression, CLUT/palette
resolution, and region/window/object composition.

## What it is

A standalone decode library for the three bitmap subtitle families this
workspace's demuxers already frame packets for
(`crates/format/vaco-subtitle-bitmap`) but do not decompress. It is
**not** a `vaco_codec_core::Decoder` — see "Why no registry entry" below —
so there is no `vaco-component.toml` fragment and nothing to enable via
`-c:s`. A caller wires it directly: [`dvb::DvbSubDecoder`]/
[`dvb::decode_display_set`], [`pgs::PgsDecoder`], [`vobsub::decode_spu`].

Every format converges on one output shape, [`SubtitleEvent`]: a start/end
time, a forced flag, and zero or more
[`vaco_format_subtitle_bitmap::IndexedBitmap`]s, each already carrying its
absolute canvas position. [`rgba::to_rgba`] expands one to packed RGBA8 for
pixel-level comparison against a reference decoder.

## How the registered decoders relate to the library

This crate was written before `FrameData::Subtitle` existed — `FrameData`
was a closed `Video`/`Audio` enum, so a `Decoder` impl had nowhere to put
its output (interface gap 17). That variant landed in commit `7648268`, and
`decoder.rs` is the wiring it made possible: three `SendReceive`
implementations, three `DecoderDesc`s, and one `frame_of_event` that states
the `SubtitleEvent` -> `FrameData::Subtitle` mapping in a single place.

The library functions remain the richer interface, and the split is not
vestigial — `vobsub::decode_spu` takes the `.idx` palette as a parameter and
the registered `dvdsub` decoder has no way to receive one (gaps 19/20).

## How it works

### DVB (`dvb.rs`)

[`dvb::decode_display_set`] walks a complete epoch's segments (via
`vaco_subtitle_bitmap::dvbsub::segments::iter_segments`, which that crate
already writes and fuzzes) and accumulates page/region/CLUT/object state
until an `EndOfDisplaySet` segment, then composes every referenced region
into an `IndexedBitmap` positioned at its page coordinates.
[`dvb::DvbSubDecoder`] wraps this in a buffering `push` so it works whether
a caller feeds it whole MPEG-TS PES payloads (the realistic framing — a
broadcast's `data_alignment_indicator` means one PES is one epoch) or the
registered `dvbsub` demuxer's fixed-size, segment-**un**aware raw chunks.

Region/object/CLUT parsing beyond `region_id`/width/height and the CLUT
table itself (already handled by
`vaco_subtitle_bitmap::dvbsub::segments::parse_region_composition`/
`parse_clut`) is done in this crate: the object list, fill flag/index,
depth, and the object-data segment's own header. The 2-/4-/8-bit
pixel-code run-length grammars (EN 300 743 §7.2.5.2) are a small bit-cursor
(`dvb::rle`) reading MSB-first, escalating through each format's run-length
categories exactly as tabulated in the spec's own Table 15/16/17. An
object's top and bottom interlaced fields are decoded independently, one
row per `0x10`/`0x11`/`0x12` pixel-data sub-block terminated by its own
"end of string" code and an explicit `0xF0` "end of object line" marker,
then interleaved row-by-row into the final bitmap. §10's default-CLUT
formulas (percentages scaled to `0..=255`, rounded) supply a palette when a
display set never sends a `CLUT_definition_segment`.

### PGS/HDMV (`pgs.rs`)

[`pgs::PgsDecoder`] takes one segment record per `push_segment` call —
exactly what `vaco_subtitle_bitmap::sup::PgsDemuxer::read_packet` hands
out, header bytes included. It accumulates the current epoch's composition
list (PCS), palettes (PDS, keyed by `palette_id`) and objects (ODS,
reassembled across `last_in_sequence_flag`-split fragments), and composes on
`END`. Composition objects carry absolute screen coordinates already —
window definitions (WDS) are parsed for completeness but unused, since
nothing here needs a window's own bounds to place anything. Cropping
(`object_cropped_flag`) selects a sub-rectangle of the decoded object; the
`forced_on_flag` on any composition object in the epoch sets the whole
event's `forced` flag.

### `VobSub` (`vobsub.rs`)

[`vobsub::decode_spu`] takes one complete SPU unit — the shape
`vaco_demux_mpegps` already recovers per `private_stream_1` packet after
stripping the sub-id byte (see `VobSubDemuxer::open_pair`'s own docs) and
what a `.idx` `filepos:` points at in the sibling `.sub` file. It walks the
`SP_DCSQT` control-sequence chain (self-referencing `SP_NXT_DCSQ_SA` ends
it), applying `SET_COLOR`/`SET_CONTR`/`SET_DAREA`/`SET_DSPXA` and the
start/stop/forced-start commands, then decodes the interlaced top/bottom
RLE fields with the classic 4/8/12/16-bit nibble-escalation run-length code
(`vobsub::decode_run`). The four resulting pseudo-colours are resolved
against the caller-supplied 16-entry palette and the decoded contrast
(alpha) nibbles.

The palette is a plain function parameter, not
`vaco_packet::PacketSideData::Palette` — matching
`vaco_subtitle_bitmap::vobsub::VobSubDemuxer::palette()`'s own existing
shape (a file-level accessor, not per-packet side data), which is the
convention already established for this format in this workspace.

## How to change it

Each format is one file with no shared code between them beyond
[`SubtitleEvent`] and [`rgba::to_rgba`] in `lib.rs` — the three RLE grammars
and control structures are genuinely unrelated, so there is no shared
"bitmap subtitle decoder" abstraction to preserve. A new segment/command
type starts in that format's own file; `dvb.rs`'s `rle` submodule and
`vobsub.rs`'s `decode_run`/`decode_field` are the only places bit-level
parsing happens and are the right place to add a variant of an existing
code.

## Configuration

`vaco_limits::Limits` bounds every region/object/rect size and every
decoded row's width before a pixel buffer is sized from it
(`vaco_limits::Budget::alloc`/`IndexedBitmap::allocate`). `DvbSubDecoder`
additionally caps its pending-bytes buffer
(`dvb::MAX_PENDING_BYTES`, 4 MiB) and `PgsDecoder` caps one in-progress
object's reassembled bytes (`pgs::MAX_OBJECT_BYTES`, 8 MiB) — both plain
constants, not derived from `Limits`, since neither is a "decoded pixel"
size the caller's own budget already governs.

## Dependencies

`vaco-core`/`vaco-limits` (errors, allocation bounds),
`vaco-format-subtitle-bitmap` (`Rect`/`Palette`/`IndexedBitmap`/`Rgba`/
`ycbcrt_to_rgba`), and `vaco-subtitle-bitmap` (the segment/header parsing
each format's own demuxer already has: `dvbsub::segments`, `sup`'s
`SegmentHeader`/`parse_header`/`iter_segments`). Deliberately not
`vaco-codec-core` or `vaco-frame` — see "Why no registry entry".

## Known gaps

- **DVB character-coded objects** (`object_coding_method == 0x01`, a string
  of character-table references) are rejected with `Error::Unsupported`:
  this workspace has no font/glyph renderer to turn a character code into
  pixels.
- **DVB CLUT bit-depth flags** (`2/4/8-bit_entry_CLUT_flag`) are not
  distinguished — every `CLUT_definition_segment` entry lands in one flat
  table by `CLUT_entry_id`, matching
  `vaco_subtitle_bitmap::dvbsub::segments::parse_clut`'s own existing
  simplification. A CLUT that packs more than one bit-depth family into a
  single segment (uncommon; not exercised by this crate's fixtures) would
  be misplaced.
- **DVB Disparity Signalling Segment** (§7.2.7, 3D/plano-stereoscopic
  subtitling) is skipped unread.
- **PGS `WDS`** is parsed for completeness but not applied to anything: see
  "How it works" above for why.
- **VobSub `CHG_COLCON`** (per-line/per-column colour and contrast
  override, command `0x07`) is skipped past its declared length rather than
  applied — a real but rarely-used feature of the format.
- **Nothing in this workspace parses a Matroska `S_VOBSUB` track's
  `CodecPrivate`** (the literal `.idx`-file text) into a `Palette` yet, so
  `vobsub::decode_spu`'s palette parameter has no producer for that
  container path today — tracked in `planning/TECH-DEBT.md`.
- **The registered `dvdsub` decoder paints with a fallback palette.** A DVD
  subpicture's four pseudo-colours are indices into a 16-entry table that is
  not in the SPU bytes, and `Decoder` has no extradata channel to receive one
  through (interface gap 19). Geometry and pixel indices are correct; colours
  are a documented grey ramp. A caller holding the real palette should call
  `vobsub::decode_spu` directly instead.
- **A codec-stated display window does not reach the `Frame`.** DVB's
  `page_time_out` (seconds) and VobSub's SPU start/stop delays are absolute
  durations, and converting either into `Frame::duration` needs the stream's
  time base, which `Decoder` never receives (interface gap 20).
  `Frame::pts`/`Frame::duration` are copied from the packet instead — correct
  and self-consistent, but the codec's own timing is dropped.
  `SubtitleEvent::start`/`end` still carry it for a direct caller.
- **`-c:s dvbsub` does not reach these decoders**, and the blocker is not in
  this crate. `vaco-cli`'s `check_codecs` resolves `-c:s <name>` through
  `encoder_by_name` before any decoder is looked up, and this build has no
  subtitle *encoder*. Measured, not assumed: `ffmpeg -encoders` does list
  `dvbsub` and `dvdsub` encoders, so that ordering matches the reference and
  the gap is the absent encoder rather than the dispatch.

## Testing

Every parser has unit tests against a hand-built fixture: EN 300 743's
region/CLUT/object grammar for DVB, PCS/PDS/ODS for PGS, and the SPU
header/control-sequence/RLE grammar for `VobSub`, including forced-flag and
oversized-dimension rejection cases.

`tests/fixtures/compare.py` is the differential harness: it builds the same
three hand-constructed fixtures, decodes each with this crate's own
`examples/decode_dump` binary and with `ffmpeg`'s reference decoder via
PyAV (`av.CodecContext`/`av.open`), and diffs rect geometry and raw
palette-index pixels. All three formats decode bitmap-identical to the
reference on these fixtures (D17: this probes the reference binary's
observed output, never its source). This is not the same claim as
bitmap-identical on arbitrary real-world content — the fixtures are
minimal, hand-built display sets exercising one region/object each, not a
corpus of real broadcast/Blu-ray/DVD streams.

`decoder.rs`'s own tests establish the wiring claim narrowly: a packet
entering the registered decoder comes out as a `Frame` whose
`FrameData::Subtitle` carries the *same* rects the library already produces
(asserted against `dvb::decode_display_set`'s output on the same bytes, not
re-derived), with `stride` equal to the width, the library's own pixel
indices, and `pts`/`duration` copied from the packet. A second test drives
PGS segment-by-segment and confirms exactly one frame emerges, on the `END`
segment. A third confirms each descriptor claims the `CodecId` that
`vaco_registry::decoder_for` looks it up by.

Three `cargo +nightly fuzz run` targets
(`subtitle_bitmap_dvb`/`_pgs`/`_vobsub`) cover the untrusted-input surface:
30-second local runs found zero crashes across all three (265k/25k/858k
executions respectively).
