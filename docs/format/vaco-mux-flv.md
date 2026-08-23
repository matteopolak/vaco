# `vaco-mux-flv`

Layer 4. The FLV muxer: file header, `onMetaData`, and per-tag codec framing
for both legacy and Enhanced RTMP codec signalling. The write-side companion
to `vaco-demux-flv`, and the one crate in this family that has a genuine
source-level dependency on its sibling: `vaco_demux_flv::AmfValue` is this
format's one AMF0 codec (D19), reused verbatim rather than re-implemented.

---

## What it is

One file, `mux.rs`, implementing `vaco_format_core::Muxer` as `FlvMuxer`.

---

## How it works

### Timestamps, the other way round from AVI

An FLV tag's timestamp is exactly what gets written — there is no clock to
derive the way `vaco-mux-avi` has to. `stream_time_base` reports
milliseconds for every stream (FLV has no per-stream time base to choose),
so by the time a packet reaches `write_packet` its `pts`/`dts` are already in
the unit this format states directly; `CompositionTime` for legacy AVC is
`pts - dts`.

### Framing

`Framing::LegacyVideoAvc` for H.264, `Framing::Enhanced(fourcc)` for
HEVC/AV1/VP9 video and Opus/FLAC audio, `Framing::LegacyAudio(format)` for
AAC/MP3/PCM. Enhanced video frames are written as `PacketType::CodedFramesX`
(`3`), never `CodedFrames` (`1`) — deliberately, to avoid the composition-time
ambiguity `vaco-demux-flv::tag`'s module docs describe; as the writer, this
crate can simply not create the ambiguous case.

A codec whose `CodecParameters.extradata` is set gets a sequence-header tag
written at timestamp `0`, immediately after `onMetaData` and before any real
frame — the order every FLV reader relies on.

### `onMetaData` and the one thing that gets patched

`onMetaData` is written at `write_header` time with `duration` set to `0.0`
and `videocodecid`/`audiocodecid` filled from the framing decided in
`add_stream`. The `duration` value's absolute byte position is computed
during encoding (an AMF0 `Number` is a fixed 8 bytes with no length prefix,
so this is safe) and, if the sink can seek, `write_trailer` seeks back and
overwrites it with the highest timestamp actually written. On a non-seekable
sink, `duration` stays `0.0` — genuinely unknown at header-write time for a
live/streamed encode, the same limitation real FLV encoders have.

`width`/`height`/`framerate` are **not** written to `onMetaData` in this
version — `CodecParameters` carries them, but nothing here forwards them yet.
See *How to change it*.

---

## What was exercised, and what was not

- **Exercised** (`tests/roundtrip.rs`): muxing an H.264+AAC pair and demuxing
  the result with `vaco-demux-flv`, checking stream discovery, extradata,
  packet order, and PTS/DTS/composition-time round-tripping through both
  crates' independent understanding of the byte layout.
- **Not exercised**: Enhanced RTMP video/audio muxing (`HEVC`/`AV1`/`VP9`/
  `Opus`/`FLAC` framing is implemented per the spec but has no integration
  test decoding it back), the seekable-sink `duration` patch path, PCM/MP3
  audio.

## How to change it

- **Write `width`/`height`/`framerate` into `onMetaData`**: extend
  `write_metadata_tag` the same way `duration` is handled — these do *not*
  need a patch-after-the-fact, since a video stream's dimensions are known at
  `add_stream` time, before `write_header` runs.
- **Add a codec**: extend `framing_for`. An Enhanced RTMP entry only needs a
  FourCC; a legacy entry needs the right nibble value and, for AAC
  specifically, `AACPacketType` handling (`write_packet`'s `LegacyAudio`
  arm already special-cases `format == 10`).
- **Multitrack / more than one video or audio stream**: `add_stream` rejects
  a second stream of either media type outright. Real Enhanced RTMP
  multitrack extends the tag framing further (per-track FourCC data within
  one tag) — this is a different wire shape, not a relaxed limit, and would
  need its own framing variant.

## Configuration

None beyond `vaco_format_core::FormatOptions`, accepted but currently unused.

## Dependencies

`vaco-core`, `vaco-io` (`IoWriter`, `MediaSink`), `vaco-packet`,
`vaco-format-core` (`Muxer`, `MuxerDesc`), `vaco-codec-core`
(`CodecParameters`, `CodecId`), `vaco-demux-flv` (`AmfValue` — the one
cross-sibling dependency in this set of four crates, and load-bearing: it is
what keeps AMF0 a single definition rather than two).
