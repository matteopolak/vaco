# `vaco-mux-matroska`

Layer 4. The Matroska/`WebM` muxer (`matroska`, `webm`), plus `webm_chunk`.
One implementation, `mux::MatroskaMuxer`, behind the first two `MuxerDesc`
constants; `webm_chunk::WebmChunkMuxer` wraps it for the third.

Issue #575 (FM-25, Matroska mux), epic #20.

## What it is

| Module | Contents |
|---|---|
| `codec` | `CodecId` → Matroska `CodecID` string, and the `webm` codec allow-list |
| `block` | `SimpleBlock`/`BlockGroup` encoding and the three lacings |
| `mux` | `MatroskaMuxer` — the shared implementation behind `matroska` and `webm` |
| `webm_chunk` | `WebmChunkMuxer` — `Cluster`-boundary-aligned segmented output |

## A note on the fourth registration the brief named

The brief this crate was built from names four registrations: `matroska`,
`webm`, `matroska_audio`, `webm_chunk`. Measured directly rather than
trusted (D17): `ffmpeg -hide_banner -muxers` on `ffmpeg 8.1` lists exactly
`matroska`, `webm`, `webm_chunk` and `webm_dash_manifest` (the last one is
issue #570, a different crate's scope) in the Matroska family, and
`ffmpeg -h muxer=matroska_audio` answers `Unknown format 'matroska_audio'.`
There is no such muxer. This crate registers the three that exist and does
not invent a fourth; see the closing comment on issue #575 for the same
note.

## How it works

### The size-field problem, resolved by two measurements

`Segment`'s length is not known until every packet has been written.
Measured against `ffmpeg 8.1` — `-f matroska out.mkv` (a seekable file) vs.
`-f matroska -` piped into a non-seekable sink, both under
`-fflags +bitexact` positioned as an **output** option (it must come after
`-i`; before it, it sets the flag on the input instead, and the muxer keeps
writing a fresh random `SegmentUID` on every run — the exact trap
`planning/AGENT-CONSTRAINTS.md` calls out):

```
$ ffmpeg -f lavfi -i testsrc=... -c:v libx264 -fflags +bitexact -f matroska seek.mkv
$ xxd -l 48 seek.mkv | tail -2
00000020: 4287 8104 4285 8102 1853 8067 0100 0000  B...B....S.g....
00000030: 0000 0925 ...

$ ffmpeg -f lavfi -i testsrc=... -c:v libx264 -fflags +bitexact -f matroska - > pipe.mkv
$ xxd -l 48 pipe.mkv | tail -2
00000020: 4287 8104 4285 8102 1853 8067 01ff ffff  B...B....S.g....
00000030: ffff ffff ...
```

The seekable run's `Segment` size field (`0100000000000925`) is a real,
patched value; the piped run's (`01ffffffffffffff`) is the RFC 8794 §6.2
all-ones "unknown size" marker, kept forever. `mux::MatroskaMuxer::write_trailer`
is exactly that branch: it always reserves the eight-octet unknown-size
marker at `write_header` (`vaco_format_ebml::vint_unknown(8)`), and only
overwrites it — via `IoWriter::seek` and `vaco_format_ebml::patch_known_size`
— when `self.out.is_seekable()`.

**`Cluster` needed no such branch**, and that was also a measurement, not an
assumption: the same two runs' `Cluster` elements both carry the *shortest*
VINT size (`47 3e` = 2 octets, value 1854), identical bytes in both files.
That is only possible if the whole `Cluster` is assembled before its header
is written, on both a seekable and a non-seekable sink — so
`mux::Cluster` is a plain in-memory `Vec<u8>` that grows with every
`SimpleBlock`/`BlockGroup` and is written as one complete
`vaco_format_ebml::write_element(CLUSTER, body)` call, no seeking involved.

### `DocTypeVersion`, measured per `DocType` and per codec

Four probes, same method as above, each just `xxd -l 40` on the output:

| Container | Codec | `DocTypeVersion` |
|---|---|---|
| `matroska` | H.264 | 4 |
| `matroska` | PCM | 4 |
| `matroska` | VP9 | 4 |
| `webm` | VP8 | 2 |
| `webm` | VP9 | 2 |
| `webm` | Opus | 4 |
| `webm` | VP9 + Opus | 4 |

`matroska` is always 4, independent of codec. `webm` starts at 2 and is
bumped to 4 the moment an Opus track is added (Opus needs `CodecDelay`/
`SeekPreRoll`, version-4 features) and stays there even when another track
is VP9. `MatroskaMuxer::needs_doctype_v4` is exactly this: initialised to
`!variant.is_webm`, and flipped to `true` in `add_stream` the moment
`codec_id == CodecId::Opus`. `DocTypeReadVersion` was 2 in every probe and
is hard-coded as such.

### `SimpleBlock` vs. `BlockGroup`

RFC 9559 §10.3 gives `SimpleBlock` no `BlockDuration` and no
`ReferenceBlock` — those exist only on the `Block` inside a `BlockGroup`.
`write_packet` picks the long form when either is actually needed: a
packet whose duration is present and does not match the track's
`DefaultDuration`, or a packet on a stream that reorders frames
(`VideoParameters::has_b_frames > 0`) whose `pts != dts`. Everything else —
audio, and video with no B-frames — is a `SimpleBlock`. `ReferenceBlock` is
computed as `prev_dts - dts` against the same track's previous block, the
common "reference the immediately preceding frame in decode order"
convention; this is not a claim that it names the *specific* frame a real
encoder's reference picture list would, since `Packet` carries no such
information — see *Known gaps*.

### Lacing is implemented but never chosen here

`block::lace` implements Xiph, EBML and fixed-size lacing in full, and
round-trips through `vaco-demux-matroska::block`'s decoder in this crate's
own tests. `MatroskaMuxer` never calls it: `vaco-demux-matroska`'s own
module docs record that "`ffmpeg`'s Matroska muxer writes `FlagLacing=0`
and never laces", and this crate matches that — every `TrackEntry` writes
`FlagLacing=0` and every block carries exactly one frame. The lacing code
exists because the deliverable asks for it and because a caller assembling
frames some other way (or wanting a smaller file for many tiny frames) can
still reach it directly.

### `CodecPrivate` is `extradata`, verbatim, always

D14.1 forbids a `vaco-mux-*` crate depending on a `vaco-parse-*` one, so
this crate cannot parse a raw bitstream's extradata into an
`AVCDecoderConfigurationRecord` or an Xiph-laced Vorbis header set itself.
It does not need to: `vaco-demux-matroska`'s own `codec::private_is_extradata`
already documents that its demuxer stores `CodecPrivate` **verbatim** into
`CodecParameters::extradata` for every codec this crate maps (AVC, HEVC,
Opus, Vorbis, FLAC). Writing `extradata` back out unchanged as
`CodecPrivate` is therefore not a simplification of the real rule — it *is*
the real rule, for any stream whose extradata already has Matroska's
expected shape (which includes every stream this crate's own sibling
demuxer produced). A stream whose extradata came from a different
container's packaging convention for the same codec is a bitstream-filter
problem, not a muxer one, and is out of scope here as it would be in the
reference too.

### `webm`'s codec allow-list, and its exact rejection message

Measured: `ffmpeg -f lavfi -i testsrc... -c:v libx264 -f webm bad.webm`
fails `write_header` with

```
Only VP8 or VP9 or AV1 video and Vorbis or Opus audio and WebVTT subtitles are supported for WebM.
```

`codec::WEBM_REJECTION` is exactly this string, and `MatroskaMuxer::add_stream`
returns it as `Error::Unsupported` for any `webm` stream outside
`codec::webm_allows_video`/`webm_allows_audio`. `V_AV1` is on the video
allow-list per the current `WebM` Project container guidelines, which added
AV1 after the original VP8/VP9-only text the error message itself still
quotes.

### What `Muxer` carries now, and what still needs no channel

`vaco_format_core::Muxer::add_stream` still takes only `CodecParameters` —
nothing in *that* method carries a file title, a tag list, or a chapter
table. `Muxer::set_metadata` is the channel added for exactly this (M30,
`planning/INTERFACE-GAPS.md` gap 1); see *Metadata, chapters, attachments*
below for how this crate uses it. `Cues` needed no such channel at all
(every field it carries comes from the packets themselves) and was
implemented in full from the start — one `CuePoint` per video keyframe,
`CueClusterPosition` relative to the first byte of `Segment`'s data per
RFC 9559 §11.8.

`SeekHead` is a separate, deliberate omission, not a trait limitation: it is
RFC 9559's optional fast-locate index, and `vaco-demux-matroska` itself
falls back to a linear scan for `Info`/`Tracks` when it is absent — every
reader has to. Writing it correctly needs either patching through a second
seek pass or fixed-width placeholder arithmetic for `SeekPosition`, for no
behavioural gain over the `Cues`-only index already written, so it is
deferred.

### `webm_chunk`: what a one-sink trait can and cannot do

Measured (`ffmpeg -h muxer=webm_chunk`): the real muxer writes one file per
chunk, plus a separate `-header` file for the initialization segment,
driven by `-chunk_start_index` (default 0) and `-audio_chunk_duration`
(default 5000 ms). `MuxerDesc::open`'s signature —
`fn(Box<dyn MediaSink>) -> Result<Box<dyn Muxer>>` — hands a muxer exactly
one already-opened sink and no channel for muxer-private options at all;
there is no way to open a second file from inside this trait, and no way
for `-chunk_start_index` to reach a `Muxer` built through the registry.

So `WebmChunkMuxer` is `MatroskaMuxer` configured for `webm` with
`max_cluster_ms` set to the chunk duration (making a `Cluster` boundary and
a chunk boundary the same byte offset), writing the whole thing as one
continuous stream — exactly the bytes the numbered chunk files would
contain if concatenated in order. `WebmChunkMuxer::chunk_boundaries()`
reports `(chunk_index, byte_offset)` pairs so a caller that *does* have
multi-file capability (a future CLI segmenter) can cut the single stream
into the real files without this crate growing one. The registry's own
`open_webm_chunk` uses `ffmpeg`'s measured defaults (`chunk_start_index=0`,
`audio_chunk_duration=5000`); a caller wanting different values constructs
`WebmChunkMuxer::new` directly.

## How to change it

- **Adding a codec** is one row in `codec::codec_id_str` plus, if it is
  video or audio for `webm`, one arm in `webm_allows_video`/
  `webm_allows_audio`. `codec::tests::every_mapped_codec_id_round_trips_through_the_demuxers_table`
  will fail if the string does not read back as the same `CodecId` through
  `vaco-demux-matroska::codec::map` — that test is the guard against the two
  tables silently drifting apart.
- **Changing the cluster-splitting policy** is `MatroskaMuxer::write_packet`'s
  `needs_new_cluster` computation and the `max_cluster_ms` field it reads
  (settable via `set_max_cluster_ms`, which is what `webm_chunk` uses).
  Nothing else depends on where a `Cluster` boundary falls except `Cues`
  (one entry per video-keyframe-opened cluster) and `webm_chunk`'s chunk
  boundaries, both already keyed off the same decision point.
- **Gotcha: `Packet::duration` is always microseconds**, independent of the
  stream's own time base (`vaco_core::Duration`'s own docs) — unlike `pts`/
  `dts`, it does not go through the `MuxWriter` rescale chain. `write_packet`
  converts it to `TimestampScale` ticks itself via `Duration::to_ticks`, and
  treats `Duration::ZERO` (the field's own default) as "not stated" rather
  than a real zero-length block.
- **Gotcha: `ReferenceBlock`'s sign.** RFC 9559 §10.3.1's convention is a
  signed delta from *this* block's timestamp to the frame referenced,
  negative for a past reference — `prev_dts - dts`, not `dts - prev_dts`.
  Swapping the operands still produces a value that looks plausible and
  silently reverses every reference.

## Configuration

`FormatOptions`, read once at construction (`MatroskaMuxer::new`):
`fflags=+bitexact` suppresses `DateUTC` entirely, matching the reference
measured the same way as the size-field probe above; `start_time_realtime`
(Unix microseconds, the same field `vaco-format-core`'s own `MuxWriter`
reads), when set and not bitexact, becomes `DateUTC` (nanoseconds since the
Matroska epoch, 2001-01-01). Neither path calls `vaco_time` or any system
clock — the wall-clock value, when written at all, always comes from the
caller, which is what keeps this crate buildable for `wasm32-unknown-unknown`
without a `web` feature.

## Dependencies

`vaco-format-ebml` for the EBML layer (D19); `vaco-demux-matroska` for the
Matroska element schema and its lacing decoder, reused by this crate's own
tests to prove round-trip agreement rather than re-tabulated — the same
pattern `vaco-mux-ogg` uses against `vaco-demux-ogg`; `vaco-format-core`,
`vaco-io`, `vaco-core`, `vaco-codec-core`, `vaco-packet`, `vaco-chlayout`,
`vaco-limits` for the rest of the container framework.

## Fuzzing

Not applicable in the D6 sense: a muxer's input is caller-constructed
`Packet`s, not attacker-chosen bytes. Correctness is instead checked by
`tests/roundtrip_proptest.rs`, which mux a synthetic H.264 stream over an
arbitrary sequence of frame-timing deltas and demux the result with
`vaco-demux-matroska` — proving agreement with an independently developed
sibling crate, a stronger check than a fuzz target could give here.

## Metadata, chapters, attachments (CL-16, `planning/INTERFACE-GAPS.md` gap 1)

`Muxer::set_metadata` stores whatever `vaco_format_core::metadata::MuxMetadata`
it is handed; every field it drives — `Info > Title`, per-track `Name`/
`Language`, `Tags`, `Chapters`, `Attachments` — is resolved **lazily**, inside
`write_header`, not eagerly inside `set_metadata` itself. That is deliberate:
`vaco-cli`'s scheduler drives a raw `dyn Muxer` and has no guaranteed point at
which `set_metadata` runs relative to `add_stream` (see that crate's
`exec.rs` module docs), so any field this crate resolves by reading
`self.tracks` has to do it at a point both calls are guaranteed to have
already happened — `write_header` is that point; `set_metadata` itself is not.
`mux::tests::set_metadata_before_add_stream_still_resolves_per_stream_fields`
is the regression test for this.

Element order (`Info`, `Tracks`, `Chapters`, `Attachments`, `Tags`, then the
first `Cluster`) and the exact key routing (`title`→`Info > Title` file-level,
`title`/`language`→`TrackEntry` per stream, everything else→`SimpleTag` with
an **uppercased** `TagName`) were measured against `ffmpeg 8.1` by
byte-inspecting `ffmpeg -metadata title=... -metadata:s:v:0 language=eng ...
-f matroska -`/`-f webm -` output with a small Python EBML walker — see
`crate::mux`'s module docs for the full table. `FileMimeType` (`0x4660`) is
not in `vaco-demux-matroska::ebml::schema` (that crate has no attachment
reader), so it is a local constant in `crate::mux` rather than an edit to a
crate this one only reads from (D19).

Not reproduced: the reference's own auto `ENCODER`/`DURATION` `SimpleTag`s
(they stamp the reference's own build identity and a duration this trait
cannot see ahead of write time) and a random `FileUID` (this crate derives
one deterministically from an attachment's position and filename instead —
neither a clock nor an RNG is reachable from `wasm32`, and a random value
would make output non-reproducible under `-fflags +bitexact`, the same
failure mode already documented for `DateUTC` below).

## Known gaps (say plainly what is not done)

1. `SeekHead` is not written — see *SeekHead* above for why. `Tags`,
   `Chapters` and `Attachments` **are** now written, driven by
   `Muxer::set_metadata` — see the section above.
2. `ReferenceBlock` always points at the immediately preceding frame on the
   same track in decode order. This is correct for the common single-past-
   reference case and is **not** a general reference-picture-list encoder;
   a stream with a real multi-frame reference structure will still produce
   a playable file (every frame still has *a* valid backward reference) but
   not necessarily the one an original encoder chose.
3. `webm_chunk` does not write numbered files or a separate header file —
   see its own section above for the trait-level reason and what
   `chunk_boundaries()` offers instead.
4. `-chunk_start_index`/`-audio_chunk_duration` cannot be threaded through
   the registry's `open` function at all (no options channel exists there
   for muxer-private `AVOption`s); `webm_chunk`'s registered defaults match
   `ffmpeg 8.1`'s own (0 and 5000 ms), and a caller wanting different values
   must construct `WebmChunkMuxer` directly rather than through the
   registry.
5. `write_crc32`-equivalent behaviour (the reference writes a `CRC-32`
   element inside every Level-1 element by default) is not implemented;
   every element this crate writes is CRC-unprotected. Legal per RFC 9559
   (CRC-32 is optional) and verified not to break round-tripping through
   `vaco-demux-matroska`, which treats an absent `CRC-32` as no check to
   perform.
6. Byte-for-byte identity with `ffmpeg 8.1`'s own output is not a design
   goal and is not claimed anywhere above; every measurement in this
   document is used to match a *structural* decision (unknown-vs-patched
   size, `DocTypeVersion`, the rejection message), not to reproduce every
   field's exact bytes.
