# `vaco-format-swf`

## What it is

SWF (ShockWave Flash) — the media tags only, not a Flash interpreter. Demux
+ mux, one crate. Registers as `swf` (muxer: extension `swf`, MIME type
`application/x-shockwave-flash`; demuxer: neither — measured, see below).

## How it works

### Scope: media tags, not a Flash player

SWF's tag vocabulary covers an entire vector-graphics/animation/ActionScript
runtime. This crate reads and writes exactly `DefineVideoStream`/
`VideoFrame` (video), `SoundStreamHead`/`SoundStreamHead2`/`SoundStreamBlock`
(audio), plus the structural `ShowFrame`/`End` tags. **Every other tag code
is skipped by its own declared length**, never interpreted — `tags.rs`'s
length encoding (10-bit code + 6-bit length, escaping to a `u32`) makes this
a plain skip, not a parse, so the huge remaining tag vocabulary costs
nothing to not support.

### The header

Fixed 8-byte header (`FWS`/`CWS`/`ZWS` signature, version, `u32` LE file
length) + a bit-packed `RECT` (5-bit `Nbits` then four `Nbits`-wide signed
fields, byte-aligned after) + `u16` frame rate (8.8 fixed) + `u16` frame
count. Only `FWS` (uncompressed) is supported — `ffmpeg -f swf`'s own muxer
never writes `CWS`/`ZWS` (checked directly), so this is a real, documented
gap rather than a silent one.

### Video and audio tags

`DefineVideoStream` (60): `CharacterID`, `NumFrames`, `Width`, `Height`,
`VideoFlags`, `CodecID`. Codec ID `2` = Sorenson H.263 (FLV1) is the only
one measured (`ffmpeg -c:v flv1`) and supported; others (screen video, VP6)
are recognised numeric values with no decode path, refused rather than
guessed at.

`SoundStreamHead2` (45, and its older sibling `SoundStreamHead`, 18):
playback/stream sound-rate/size/type nibbles, `StreamSoundSampleCount`, and
(compression == MP3 only) a `LatencySeek` field. Compression `2` = MP3 and
`0` = uncompressed PCM are supported; the standard's other compressions
(ADPCM, Nellymoser, Speex) are recognised, not decoded.

`VideoFrame` (61)/`SoundStreamBlock` (19) carry the actual compressed
bytes. Video packets get `pts` from the tag's own `FrameNum` field; audio
packets get `pts` from a running sample count this demuxer accumulates
(`SoundStreamBlock` has no frame number of its own, only a per-block sample
count).

### Muxing: two real, measured divergences from byte-identical output

**No `PlaceObject2`.** The reference muxer writes one per frame to place the
video/sound character on the display list (with a bit-packed `Matrix`
record and an ffmpeg-specific incrementing `Ratio` field). Measured
directly: stripping every `PlaceObject2` tag out of a real reference file
and re-running `ffprobe -f swf` on the result still reports the correct
codec/dimensions/sample rate/channels and the full packet count. Writing
`Matrix`/`ColorTransform` records byte-identically would be real work for
zero measured behavioural gain, so `SwfMuxer` never writes one.

**Approximate frame ordering.** The reference interleaves one `VideoFrame`,
one `SoundStreamBlock`, then `ShowFrame`, per display frame. `SwfMuxer`
writes a tag immediately for whatever packet it is handed and emits
`ShowFrame` right after every *video* packet's tag — correct for a caller
feeding packets in roughly presentation order, not guaranteed identical for
every packet ordering a caller might choose.

**Full remux round trip is therefore not byte-identical**, and is not
claimed to be. What is verified instead:
`tests/reference_files.rs::remuxing_a_real_sample_is_still_readable_by_the_reference`
demuxes a real reference file, remuxes it with `SwfMuxer`, and confirms
`ffprobe` itself reads the result back with the right codec, dimensions and
sample rate — a real reference cross-check, just not a byte comparison.

### Buffering

`SwfMuxer` builds the whole tag stream in memory (`tag_buf`) because
`FileLength`, `DefineVideoStream`'s `NumFrames` and `SoundStreamHead2`'s
total sample count are only known once every packet has been written.
`write_trailer` patches those by recorded byte offset and flushes
everything in one `MediaSink::write` — this muxer never needs the sink to
be seekable, but does hold the entire output in memory until
`write_trailer`.

## How to change it

* **`PlaceObject2`/`Matrix`/`ColorTransform` support**: would need real
  bit-packed record encoders for `Matrix` (scale/rotate/translate, each
  independently optional with a `HasX` flag and a variable bit width) and
  `ColorTransform`. Worth doing only if a consumer other than `ffprobe`
  needs the display list, since `ffprobe` demonstrably does not.
* **Compressed (`CWS`/`ZWS`) SWF**: needs a zlib/LZMA decoder wired into
  `header.rs`'s signature check; check `vaco-hash`/existing zlib usage
  elsewhere in the workspace before adding a new one (D11).
* **Screen video/VP6 video, ADPCM/Nellymoser/Speex audio**: add table
  entries in `demux.rs`'s `video_codec_from_swf`/`audio_codec_from_swf`
  only once measured against a real sample the way FLV1/MP3 were — do not
  extend the tables from the spec's numbering alone.
* **Exact `SoundStreamBlock` sample counts for MP3**: `mux.rs`'s
  `write_audio_block` currently charges the block's *byte* length to the
  informational sample-count fields, which is wrong for MP3 (a sample
  count, not a byte count) — harmless today because this crate's own
  demuxer never reads those fields back, but worth fixing with a real MP3
  frame-header sample-count read if another consumer needs it.

## Configuration

No crate-specific options; `SwfDemuxer::open`/`open_with_limits` and
`SwfMuxer::new` are the whole interface.

| Constant | Value | Meaning |
|---|---|---|
| `demux::SOUND_RATES` | `[5512, 11025, 22050, 44100]` | `StreamSoundRate`'s 2-bit table, fixed by the specification |
| `demux::MAX_REASONABLE_TAG` | 64 MiB | Second, tighter bound on top of `Budget`'s own allocation ceiling |
| `mux::SWF_VERSION` | 6 | Every measured sample used version 6 |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` (`CodecId::Flv1`/`Mp3`/`PcmS16le`), `vaco-chlayout`
(`ChannelLayout::default_for`), `vaco-bitstream` (`BitReader`/`BitWriter`
for `RECT`). No `vaco-parse-*` dependency: FLV1 and MP3 both carry their own
frame headers, so `open_demuxer` accepts a `&dyn ParserProvider` only to
satisfy `DemuxerDesc::open`'s frozen signature.

## What was and was not measured

Verified directly against real `ffmpeg 8.1 -f swf`/`-c:v flv1 -c:a mp3`
output (2026-08-27), embedded as `tests/fixtures/sample.swf` (12 video
frames + 12 audio blocks — distinguishing input: two streams, more than one
frame of each):

* The fixed header + `RECT` byte layout, field by field, against the raw
  hex of a real file.
* The tag-header short/long-length encoding, including the exact escaped
  `u32` form for a 3685-byte `VideoFrame` payload.
* `DefineVideoStream`/`SoundStreamHead2` field values against the real
  sample's own bytes (codec IDs, sample rate/channel bits).
* `probe_score=51` for a real file (`ffprobe`, no `-f` override) —
  reproduced literally as `ProbeScore(51)`, not rounded up to `CONTENT`.
* That `PlaceObject2` is not required for the reference's own demuxer to
  read a file correctly (stripped-tag test against a real file).
* The remux-then-reference-readback cross-check described above.

**Not measured, and known to be absent, not merely approximate**:

* `PlaceObject2`/`Matrix`/`ColorTransform` — never written; see "How to
  change it".
* Byte-identical remux output — not claimed; the two divergences above are
  real and documented, not oversights.
* Compressed (`CWS`/`ZWS`) SWF, screen video, VP6, ADPCM/Nellymoser/Speex
  audio, and the `avm2` (SWF/AVM2) muxer variant `ffmpeg -muxers` also
  lists separately — none of these were exercised.
