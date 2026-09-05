# `vaco-demux-flv`

Layer 4. The FLV demuxer: the 11-byte tag walk, `onMetaData`, and the codec
mapping for both the legacy 4-bit codec fields and the Enhanced RTMP
(E-RTMP) FourCC extension.

---

## What it is

| Module | Contents |
|---|---|
| `amf` | AMF0 decode/encode: `AmfValue` |
| `tag` | the tag header shape, the back-pointer, both codec-id tables |
| `demux` | the tag walk, `onMetaData`, packet emission, seeking |

Written from Adobe's *Video File Format Specification, Version 10.1* and the
community-maintained *Enhanced RTMP* specification (Veovera Software
Organization, genuinely public). No FFmpeg source was consulted (D7/D15);
every table entry and byte layout below was measured against `ffmpeg 8.1`.

---

## How it works

### Stream discovery is progressive, because FLV states no stream list

Unlike AVI or MP4, FLV ships no header naming its streams. `DataOffset`
names only where the first tag starts. What this demuxer does — the same
shape `vaco-demux-mpegts` already established for a header-less format — is
watch the tag stream: the first video tag creates the video stream, the
first audio tag creates the audio stream, and each tag's own codec-id nibble
(or, for Enhanced RTMP, FourCC) names its codec directly, no bitstream
parsing required. `Demuxer::streams()` can therefore grow between calls to
`read_packet` — not a defect, the same as MPEG-TS's PAT/PMT arriving late.

`onMetaData` (tag type 18), when present, arrives first in every file this
crate has seen and supplies `width`/`height`/`duration`/`framerate` ahead of
the tags that would otherwise have to state them — `demux::PendingMeta`
caches those fields and `ensure_video_stream` applies them when the stream is
actually created.

AMF0 stores `duration` as an `f64`. The demuxer converts its shortest
round-tripping decimal spelling directly into an exact `Duration`; it does not
round the value to microseconds on ingress.

### Timestamps

Trivial by container standards: every tag states its own presentation
timestamp directly, in milliseconds, as a 24-bit value plus an 8-bit
*extension* byte appended **after** the low 24 bits — not the natural byte
order a 32-bit big-endian value would use (Adobe spec, Annex E). `dts`
differs from `pts` only for AVC/HEVC frames carrying a non-zero
`CompositionTime`.

### The codec-id tables, and the one thing that could not be measured

Legacy: a 4-bit `CodecID` (video) or `SoundFormat` (audio) nibble maps
directly to a codec — `tag::legacy_video_codec_id`/`legacy_audio_codec_id`,
each entry backed by a probe. Enhanced RTMP repurposes the top bit of the
video header (`0x80`) and, distinctly, the top *nibble* of the audio header
(`0x9_`) as an "extended header" marker, followed by a `PacketType` nibble
and a four-byte FourCC (`tag::fourcc_codec_id`). Both were measured directly:

```
$ ffmpeg -f lavfi -i testsrc=... -c:v libsvtav1 -f flv av1.flv
$ ffmpeg -f lavfi -i sine=... -c:a libopus -f flv opus.flv
```

What could **not** be confirmed byte-exactly: whether Enhanced RTMP's
`PacketType::CodedFrames` (`1`) carries a three-byte composition-time-offset
field the way legacy `AVCPacketType::Nalu` always does. The two probes above
disambiguate `CodedFramesX` (`3`, specified as never carrying one) but not
`CodedFrames` itself. `tag.rs`'s module docs record the decision: treat
`CodedFrames` identically to `CodedFramesX` (no composition time consumed),
which is exactly right for `CodedFramesX` and may misplace three leading
payload bytes as composition time for a `CodedFrames`-emitting encoder. Legacy
AVC (H.264, the common case, and what most existing FLV content uses) is
unaffected.

### Seeking

`FormatFlags::NOBINSEARCH` is set, but — unlike AVI — not because bisection is
structurally impossible: every FLV tag states its own absolute timestamp, so
landing on an arbitrary byte offset and resyncing to the next tag header
genuinely does recover a real timestamp. The flag is set defensively because
binary search is not *implemented* in this version; claiming a capability
this demuxer cannot yet perform would be a worse interface lie than declining
a case a future version can add. `resync` (used for both `SeekTarget::Byte`
and falling back from the index) scans forward for a byte position whose
next 11 bytes look like a well-formed tag header (a recognised `TagType` and
a `DataSize` that fits inside the file) — heuristic, and only reliable with
enough real tags around it to disambiguate; see `tests/roundtrip.rs`'s
`byte_seek_before_the_first_tag_never_panics` for the honest boundary case.

**Positioning convention**: every `pos` this crate hands out (`Packet::pos`,
`IndexEntry::pos`) is the **back-pointer's** start, not the tag header's —
`read_tag` and `resync`/`probe_tag_header` must agree on this, since resync's
probe starts by reading a `PreviousTagSize` field too. Getting this
inconsistent between the two was the first bug found while testing seeking,
and it is why the point is called out here rather than left implicit.

---

## What was exercised, and what was not

- **Exercised** (`tests/roundtrip.rs`): a hand-built file with `onMetaData`,
  an AVC sequence header, two AVC coded frames (one with a non-zero
  `CompositionTime`), an AAC sequence header, and one AAC raw frame — stream
  discovery with metadata applied, extradata vs. packet emission, PTS/DTS
  derivation, EOF stickiness, index-based seeking to a keyframe, and byte
  seeking both to an exact tag boundary and to a non-boundary position.
- **Structurally present, not exercised end-to-end**: Enhanced RTMP video
  (`fourcc_codec_id`/`ExPacketType` are unit-tested on their own, but no
  integration test decodes a full Enhanced RTMP video tag stream), Enhanced
  RTMP audio side-metadata packet types (recognised and skipped, not
  decoded), the `kux` Youku variant and `live_flv` (no seeking) mentioned in
  plan 18 (not implemented at all — this crate is the seekable-file case).

---

## How to change it

- **Add a codec mapping**: `tag::legacy_video_codec_id`/
  `legacy_audio_codec_id`/`fourcc_codec_id`. Follow the "probe, then add one
  match arm with the command recorded" rule the existing entries do; `None`
  for anything `vaco_codec_core::CodecId` has no variant for is correct, not
  a gap to fill with a guess.
- **Resolve the `CodedFrames` composition-time ambiguity**: needs a real FLV
  file from an encoder that uses `PacketType::CodedFrames` (not
  `CodedFramesX`) for a codec with actual B-frame reordering, decoded
  byte-for-byte to see whether a three-byte field follows the FourCC.
- **AMF0**: `amf.rs` is a complete, small AMF0 codec; a new value kind
  (Reference resolution, XML Document content) is a new arm in
  `decode_inner`/`AmfValue::encode`, not a new module.

## Configuration

None beyond `vaco_format_core::FormatOptions` (`max_streams`, `indexmem`).

## Dependencies

`vaco-core`, `vaco-io` (`IoContext`), `vaco-limits` (`Budget`, and AMF0's
depth/item caps against untrusted nesting), `vaco-packet`,
`vaco-format-core` (`Demuxer`, `DemuxerDesc`, the seek machinery),
`vaco-codec-core` (`CodecParameters`, `CodecId`). No `vaco-parse-*`
dependency (D14.1).
