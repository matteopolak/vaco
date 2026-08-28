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

### `onMetaData` and the two things that get patched

`onMetaData` is written at `write_header` time with `duration` and
`filesize` set to `0.0` placeholders and everything else filled from the
`OnMetaFields` each stream's `add_stream` call captured (see below) plus
`videocodecid`/`audiocodecid` from the framing decided there too. Both
placeholders' absolute byte positions are found by searching the encoded tag
body for the field's own key-plus-type-marker bytes (`number_value_offset`)
rather than computed from a fixed layout — necessary once the field set
varies with which streams exist (video-only vs. video+audio write different
keys). If the sink can seek, `write_trailer` seeks back and overwrites
`duration` with the highest timestamp actually written and `filesize` with
the file's true final byte count (nothing is written after that patch, so
the position captured just before it already *is* the final size). On a
non-seekable sink, both stay `0.0` — genuinely unknown at header-write time
for a live/streamed encode, the same limitation real FLV encoders have.

**`width`/`height`/`videodatarate`/`framerate`/`audiodatarate`/
`audiosamplerate`/`audiosamplesize`/`stereo` are now written too** — finding
18 (`planning/CONFORMANCE-FINDINGS.md`): `add_stream` used to discard
`CodecParameters` entirely after pulling `extradata` out of it, so
`onMetaData` had nowhere to forward these from even though the caller's
stream carried them all along. `OnMetaFields` (captured per-stream, in
`add_stream`) is what survives instead. `videodatarate`/`audiodatarate` are
written only when `CodecParameters::bit_rate` states one — omitted rather
than fabricated when the source did not say, which is the honest reading of
finding 25's "field-presence, not field-value" distinction applied here.
Order and key set are measured, not guessed: `-c copy -f flv` on an H.264(+
AAC) MP4 source writes `duration width height videodatarate framerate
videocodecid`, then (audio streams only) `audiodatarate audiosamplerate
audiosamplesize stereo audiocodecid`, then `filesize` — `major_brand`/
`minor_version`/`compatible_brands`/`encoder` also appear in that
measurement but are MP4-`ftyp`-sourced format tags and an encoder identity
string this crate has no channel for (finding 22), so they are left out
rather than guessed at.

---

## What was exercised, and what was not

- **Exercised** (`tests/roundtrip.rs`): muxing an H.264+AAC pair and demuxing
  the result with `vaco-demux-flv`, checking stream discovery, extradata,
  packet order, and PTS/DTS/composition-time round-tripping through both
  crates' independent understanding of the byte layout;
  `on_meta_data_carries_the_streams_own_video_and_audio_properties` decodes
  the raw `onMetaData` AMF0 body directly (via `vaco_demux_flv::amf::decode`)
  and checks each field against the `CodecParameters` it should have come
  from, per finding 18; `a_packet_with_no_pts_is_refused` covers finding 19.
- **Not exercised**: Enhanced RTMP video/audio muxing (`HEVC`/`AV1`/`VP9`/
  `Opus`/`FLAC` framing is implemented per the spec but has no integration
  test decoding it back), the seekable-sink `filesize`/`duration` patch path
  end to end against a real reader, PCM/MP3 audio, and the Enhanced RTMP
  end-of-sequence tag (see below — only the legacy AVC case was measured).
  No fuzz target exists yet for this crate — `write_packet` takes an
  arbitrary payload but does no byte-level parsing of it (unlike
  `vaco-mux-avi`/`vaco-mux-mpegts`'s length-prefix-to-Annex-B conversion),
  so D6's "parses untrusted input" trigger has not fired for it so far.
- Verified directly against `ffmpeg 8.1 -c copy -f flv`: the output is the
  same size as the reference's, byte-identical in `onMetaData`'s forwarded
  keys and the end-of-sequence tag, and the decoded video and audio both
  match the reference's own FLV output exactly (audio does not match the
  *source* — an inherent FLV/AAC limitation present in the reference too,
  since FLV has no edit-list mechanism to trim encoder priming samples).

### Container-level tags reach `onMetaData` too

`major_brand`/`minor_version`/`compatible_brands` (typically MP4's `ftyp`
fields) now appear in `onMetaData`, right before `filesize`, as AMF0
*strings* — `minor_version` included, despite being numeric in the source.
`Muxer::set_metadata` stores whatever `MuxMetadata::tags` it is given in
`container_tags`; `write_metadata_tag` forwards the specific keys it knows
`onMetaData` wants. This was a channel problem, not an FLV-specific gap:
`-map_metadata`'s default already copies these into `MuxMetadata::tags`
upstream of this crate (`vaco-demux-mp4::meta::file_type_tags`), but nothing
here ever read them until `set_metadata` was implemented.

### The stream ends with an AVC end-of-sequence tag

`write_trailer` now appends a 5-byte video tag (`17 02 00 00 00`: keyframe,
codec `7`/AVC, `AVCPacketType = 2` — end of sequence) at the same timestamp
as the last real video tag, for a `Framing::LegacyVideoAvc` stream. Without
it, a reader that trusts the terminator to know the sequence is complete
sees a truncated file. Enhanced RTMP's analogous `PacketTypeSequenceEnd` for
HEVC/AV1/VP9 video is not implemented — unverified against the reference,
since no such fixture was available to measure.

### A packet with no PTS is refused

Finding 19 (`planning/CONFORMANCE-FINDINGS.md`), measured directly
(`ffmpeg -i <avi-source> -c copy -f flv` refuses with "Packet is missing
PTS" and a nonzero exit; AVI is the concrete source, since it has no native
per-packet PTS field): `write_packet` used to default a missing `pts` to
`0` silently. It now refuses the packet outright — unlike
`vaco-mux-mpegts`'s version of this same check, this one is not limited to
a stream's first packet, since the reference's own message carries no
"first" qualifier.

## How to change it

- **Add another `onMetaData` key**: extend `OnMetaFields` (captured in
  `add_stream`, from the stream's own `CodecParameters`) and
  `write_metadata_tag`'s `pairs.push` sequence, in the position the
  reference's own measured order puts it — see *`onMetaData` and the two
  things that get patched* above.
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
what keeps AMF0 a single definition rather than two). Dev-only:
`vaco-chlayout` (`ChannelLayout::STEREO`, for the `onMetaData` test above).
