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
`-fflags +bitexact` positioned as an **output** option:

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

### `CodecPrivate` is `extradata`, verbatim, always — except the two codecs that must never get one

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

**`V_MPEG4/ISO/AVC` and `V_MPEGH/ISO/HEVC` are the other exception**, and for
the opposite reason: their `CodecPrivate` is the ISO/IEC 14496-15 record and
their frames are length-prefixed, but an H.264/HEVC stream reaching this
crate from an encoder, or copied from MPEG-TS, AVI or raw Annex B, has its
parameter sets in band and no record at all. `flush_header_bytes` hands both
codecs' extradata to `vaco_format_nalu::length_prefixed_config`, which
returns the `avcC`/`hvcC` to write *and* whether `write_block` must reframe
every frame — one call, both halves, so the two cannot be decided apart.
This container declares no `GLOBALHEADER`, so unlike MP4 it also has to ask
for `extract_extradata` from its own `check_bitstream`; without that, an
encoded HEVC stream was refused outright at header flush.

**`V_VP8`/`V_VP9` are the measured exception, and `codec::never_carries_
extradata_str` gates the write site on it.** VP8/VP9 are self-contained
bitstreams that no real encoder or `WebM` muxer ever gives a `CodecPrivate`
at all — but an MP4-sourced VP9 stream arrives here with a real, non-empty
`extradata` (a `vpcC` record `vaco-demux-mp4` read off a real ISOBMFF file),
and "verbatim, always" would have written those ISOBMFF-shaped bytes
straight into `CodecPrivate`, which no real `WebM` reader expects (measured:
real `ffmpeg 9.0.1`'s own MP4→Matroska remux of an identical stream writes
no `CodecPrivate` child at all). See `docs/format/vaco-mux-mp4.md`'s
Gotchas — "A config-record box is only as trustworthy as knowing who owns
its byte layout" — for the general rule this is one half of: the two
containers can disagree about whether a record exists at all (this case),
disagree about its shape despite both having one, or agree exactly (AV1's
`av1C`/`CodecPrivate`, the confirming case where "verbatim, always" needed
no exception).

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
table. `Muxer::set_metadata` is the channel added for exactly this; see *Metadata, chapters, attachments*
below for how this crate uses it. `Cues` needed no such channel at all
(every field it carries comes from the packets themselves) and was
implemented in full from the start — one `CuePoint` per video keyframe,
`CueClusterPosition` relative to the first byte of `Segment`'s data per
RFC 9559 §11.8.

`SeekHead` and every Level-1 element's `CRC-32` are covered in their own
section below — *`CRC-32` and `SeekHead` (CONFORMANCE-FINDINGS 15)* — since
closing both is what makes this crate's output able to be byte-identical to
the reference's at all, not a cosmetic addition.

### `CRC-32` and `SeekHead` (CONFORMANCE-FINDINGS 15)

Two structural omissions, previously the crate's entire byte gap against the
reference (see the *Known gaps* item this replaces): every Level-1 element
lacked the `CRC-32` the reference always writes, and `SeekHead` was left out
entirely on the theory that building it needed either a second seek-patch
pass or fixed-width placeholder arithmetic. Measured directly against
`ffmpeg 8.1`, the
reference does neither.

**`CRC-32` is unconditional.** Every Level-1 element (`SeekHead`, `Info`,
`Tracks`, `Chapters`, `Attachments`, `Tags`, `Cluster`, `Cues`) opens with a
`CRC-32` element (RFC 8794 §11.3.2) as its first child: standard CRC-32
(IEEE, the same table `zlib.crc32` uses), emitted **little-endian**, over the
element's own payload excluding the `CRC-32` element itself. `ffmpeg -h
muxer=matroska` does have a real `AVOption` here — `-write_crc32 <boolean>
... (default true)` — but `Muxer` has no per-muxer option channel to turn it
off through, `-bitexact` does not touch it, and every measurement behind
this section was taken at the (default) `true` setting anyway, so this crate
writes it unconditionally. `mux::with_crc32` is the one place this happens;
`vaco_hash::crc32` supplies the algorithm (D11: `vaco-hash` is the single
owner of the `crc` crate, so this crate depends on it rather than adding a
second table). Verified against two independent elements from a real
reference file:

```
SeekHead  declared 32 30 7d 64   computed LE 32 30 7d 64
Info      declared 62 15 80 73   computed LE 62 15 80 73
```

and, as a standing regression test rather than a one-off check,
`tests/crc32_reference_fixture.rs` walks every Level-1 element of a checked-in
`ffmpeg`-written file (`tests/fixtures/ffmpeg_reference.mkv`) and recomputes
each one's `CRC-32` — six elements, not the one originally used to derive the
algorithm.

**`SeekHead` reserves a fixed budget instead of computing an exact size.**
`Info`'s, `Tracks`'s, `Chapters`'s and `Attachments`'s absolute positions are
fully known the moment their bodies are built (they sit back-to-back right
after the reservation), so they get a `Seek` entry immediately, at
`write_header` time. `Cues`'s position is not known until every `Cluster`
has been written. Measured: the reference reserves exactly
**161 bytes** (`mux::SEEKHEAD_RESERVED_BYTES`) for `SeekHead` plus the `Void`
that pads it — stable across a `SeekHead` with 3, 4, 5 and 6 `Seek` entries
and across file sizes from ~3 KB to ~300 KB, i.e. independent of both entry
count and `SeekPosition` width. `Void`'s own size field is always the full
eight-octet VINT width (not the shortest one), which is what lets the same
161-byte span be overwritten later without anything after it moving.
`mux::seekhead_and_void` builds this region; both write sites —
`write_header`'s initial commit and `write_trailer`'s later patch, via
`vaco_format_ebml::patch_known_size`'s sibling seek-and-overwrite — call it,
so the padding arithmetic lives in exactly one place. This resolved the
crate's own former objection: the reference needs neither a second seek-patch
pass (it needs exactly one, already required for `Segment`'s own size) nor
fixed-width `SeekPosition` arithmetic (it uses the reference's own
fewest-octets uinteger encoding throughout, letting `Void` absorb whatever
width difference results).

**Seekable vs. non-seekable diverge on `Cues`, not just its index entry.**
Measured with `ffmpeg -f matroska -` redirected into a plain file (the
`pipe:` protocol disables seeking regardless of what the receiving
descriptor could technically do, matching the size-field probe earlier in
this document): a **seekable** sink gets `SeekHead` rewritten in place once
`Cues`'s position is known, indexing all of `Info`/`Tracks`/`Tags`/`Cues`. A
**non-seekable** sink commits to `SeekHead` at `write_header` time with
whatever it already has (`Info`/`Tracks`/`Tags`, no `Cues` entry) and then
**omits the `Cues` element entirely** — not merely its `Seek` entry. This
crate reproduces exactly that asymmetry (`write_trailer` gates writing `Cues`
on `self.out.is_seekable()`), rather than always writing an index-less `Cues`
and only varying whether `SeekHead` points at it.

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
- **Gotcha: `Packet::duration` is the microsecond fallback**, independent of
  the stream's own time base (`vaco_core::Duration`'s own docs). When native
  packet ticks are available, `Packet::duration_ts()` is used directly;
  otherwise `write_packet` converts the fallback via `Duration::to_ticks` and
  treats `Duration::ZERO` (the field's own default) as "not stated".
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
pattern `vaco-mux-ogg` uses against `vaco-demux-ogg`; `vaco-hash` for
`vaco_hash::crc32` (D11: the single owner of the `crc` crate — every
Level-1 element's `CRC-32` goes through it rather than a second table);
`vaco-format-core`, `vaco-io`, `vaco-core`, `vaco-codec-core`,
`vaco-packet`, `vaco-chlayout`, `vaco-limits` for the rest of the container
framework.

## Fuzzing

Not applicable in the D6 sense: a muxer's input is caller-constructed
`Packet`s, not attacker-chosen bytes. Correctness is instead checked by
`tests/roundtrip_proptest.rs`, which mux a synthetic H.264 stream over an
arbitrary sequence of frame-timing deltas and demux the result with
`vaco-demux-matroska` — proving agreement with an independently developed
sibling crate, a stronger check than a fuzz target could give here.

## Metadata, chapters, attachments

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

1. ~~`SeekHead` is not written~~ — fixed (CONFORMANCE-FINDINGS 15), see
   *`CRC-32` and `SeekHead`* above. `Tags`, `Chapters` and `Attachments`
   **are** written, driven by `Muxer::set_metadata` — see the section above.
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
5. ~~`write_crc32`-equivalent behaviour ... is not implemented~~ — fixed
   (CONFORMANCE-FINDINGS 15), see *`CRC-32` and `SeekHead`* above.
6. Byte-for-byte identity with `ffmpeg 8.1`'s own output **is** now a design
   goal for the `SeekHead`/`CRC-32` scaffolding specifically (finding 15's
   scope), and `just conformance-run
   'transcode-remux-bitexact/v-mp4/output=matroska'` was used to verify it —
   but the case still fails overall, on content this crate writes elsewhere,
   not on the scaffolding. Measured directly, comparing this crate's output
   against the reference's for the exact case above with both fixes landed:
   - **`Info`**: this crate's `MuxingApp`/`WritingApp` is the literal string
     `vaco-mux-matroska`, by design (see *Configuration* — it is this
     project's own identity, not a reproduction of `ffmpeg`'s versioned
     `Lavf62.12.100`, which `-bitexact` itself shortens to plain `Lavf`).
     Separately, `Info`'s `Duration` element can **never** be written: the
     `if self.max_end_ticks > 0` check in `info_bytes` runs inside
     `write_header`, before any `write_packet` call has had a chance to grow
     `max_end_ticks` above zero — the condition is checked before it can ever
     be true. That is a real, structural bug independent of finding 15;
     fixing it needs the same reserve-then-patch shape `SeekHead` just
     adopted (`Info`'s `Duration` would need a placeholder, and patching it
     later would move every position after `Info`, including the ones this
     finding just fixed `SeekHead` to compute) and is out of this finding's
     scope.
   - **`Tracks`**: this crate's `TrackEntry` field order and `TrackUID`
     encoding width do not match the reference's (both write the same field
     set, in different orders and, for `TrackUID`, a different octet count),
     and the reference writes two small fields this crate does not.
   - **`Tags`**: this crate never forwards MP4-level container metadata
     (`major_brand`/`minor_version`/`compatible_brands`) into file-level
     `Tags`, and (already documented above) does not reproduce the
     reference's own auto `ENCODER`/`DURATION`/`HANDLER_NAME` `SimpleTag`s.
     For a file remuxed from MP4, the reference's `Tags` carries all of
     these; this crate's does not unless a caller supplies them via
     `Muxer::set_metadata` — nothing upstream of this crate currently does
     for a plain `-c copy` remux.
   None of the above three are `vaco-mux-matroska` conformance gaps this
   finding was scoped to close, and none of them existed because of
   `SeekHead`/`CRC-32` — they are pre-existing content differences that
   happen to be what the byte-identity case now hits once the structural gap
   in front of them is gone.
