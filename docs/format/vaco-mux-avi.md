# `vaco-mux-avi`

Layer 4. The AVI muxer: `hdrl`/`strl` header, `movi` chunks, `idx1`. The
write-side companion to `vaco-demux-avi`, verified against it directly — this
crate's own tests mux a small file and demux it back with the sibling crate,
rather than hand-decoding bytes a second time.

---

## What it is

One file, `mux.rs`, implementing `vaco_format_core::Muxer` as `AviMuxer`.

---

## How it works

### AVI chunks carry no timestamp, so packets don't need one either

The demuxer recovers a timestamp from a running per-stream count (see
`vaco-demux-avi`'s docs). The consequence for the muxer: **`write_packet`
does not need `packet.pts`/`packet.dts` to decide what bytes to write** — it
only needs packets to arrive in the order the caller wants them replayed,
which the generic interleave machinery upstream already guarantees once
`stream_time_base` reports the right unit (`dwScale/dwRate`, i.e.
`1/frame_rate` for video, `1/sample_rate` for audio).

### What gets patched, and how

`dwTotalFrames`/`dwLength` and `movi`'s own declared `LIST` size are not
known until every packet has been written:

- `hdrl` (`avih` + one `strl` per stream) is fully determined by `add_stream`
  alone, so it is built in an in-memory `Vec<u8>` first and written as one
  chunk — the only way to get its `LIST` size right without requiring the
  sink to seek. While building it, the exact byte offset of `dwTotalFrames`
  and each stream's `dwLength` is recorded.
- If the sink can seek, `write_trailer` goes back and patches those offsets,
  plus `movi`'s declared size and the outer `RIFF` size.
- If it cannot, they are left at placeholder values `vaco-format-riff`'s own
  chunk reader already documents as legitimate: `0` for frame counts (a real
  decoder discovers the truth from `idx1`/EOF), `0xFFFF_FFFF` ("length
  unknown, read to EOF") for `movi`'s size.
- `idx1` itself needs no seeking and is **always** written — nothing about
  appending it after `movi` depends on the sink's ability to go backwards.
  Its `dwOffset` is written movi-relative, the convention `vaco-demux-avi`
  measured `ffmpeg 8.1`'s own writer using.

### Codec support

Video: H.264, HEVC, VP8, VP9, MJPEG, PNG — the codecs
`vaco_codec_core::CodecId` has a variant for *and* this crate has a
`biCompression` FourCC for (`video_fourcc`). Audio: the generic `Pcm` bucket
plus every little-endian PCM flavour (`PcmU8`, `PcmS16le`, `PcmS24le`,
`PcmS32le`, `PcmF32le`, `PcmF64le`, `PcmAlaw`, `PcmMulaw`) — all written with
`dwSampleSize` set to the block size, the only case AVI can express as
constant-bitrate, and with `wBitsPerSample` forced to the width the specific
flavour implies rather than trusted from the caller (see `pcm_bits_per_sample`
in `src/mux.rs`) — plus MP3 and AAC (both variable-bitrate; one chunk per
frame). The big-endian PCM flavours have no mapping and are refused, same as
any other codec `add_stream` does not recognize: a `WAVEFORMATEX` in a RIFF
file is little-endian by definition, so there is nothing correct to write.
Anything unmapped is `Error::Unsupported` from `add_stream` — never a guessed
tag a reader would misidentify.

Note: `vaco-format-riff`'s `wave_tags::codec_id` (which `vaco-demux-avi`
reuses on the read side) resolves a parsed `WAVEFORMATEX` to the *specific*
flavour, never the generic `Pcm` bucket — so a stream added here as
`CodecId::Pcm` demuxes back as e.g. `CodecId::PcmS16le`. That is intentional
and is exactly what `tests/roundtrip.rs` pins.

---

## What was exercised, and what was not

- **Exercised** (`tests/roundtrip.rs`, which demuxes with `vaco-demux-avi`):
  a two-stream (H.264 video + PCM audio) file, packet order and per-packet
  keyframe/PTS derivation on the read side, the seekable trailer-patch path.
- **Not exercised**: the non-seekable-sink path (placeholder `0`/
  `0xFFFF_FFFF` values never actually read back by anything in this crate's
  tests), MP3/AAC audio muxing, more than two streams, `strn` or `LIST INFO`
  metadata output (this crate does not write either — see below).

## How to change it

- **Add a codec**: extend `video_fourcc`/`audio_format_tag`. Only add an
  entry that `vaco-format-riff`'s read-side tables (`video_tags`,
  `wave_tags`) would map back to the *same* `CodecId` — writing a tag this
  project's own demuxer cannot demux again is worse than refusing to.
- **Write `strn`/`LIST INFO` metadata**: not implemented. `add_stream` only
  sees `CodecParameters`, which carries no stream metadata; threading title/
  artist/etc. through would need a signature change this crate's frozen
  `Muxer` trait does not have room for today (see `vaco-format-core`'s docs
  on the same gap for `vacoraw`).
- **Width/height for video `strf`**: read from `CodecParameters.video`
  directly in `add_stream` — already wired. If a caller passes a video
  stream with `width`/`height` still `0`, this crate does not reject it; the
  demuxer will simply read back `0x0`.

## Configuration

None beyond `vaco_format_core::FormatOptions`, accepted but currently unused
by this muxer (no per-file option changes its output).

## Dependencies

`vaco-core`, `vaco-io` (`IoWriter`, `MediaSink`), `vaco-packet`,
`vaco-format-core` (`Muxer`, `MuxerDesc`), `vaco-codec-core`
(`CodecParameters`, `CodecId`). Dev-only: `vaco-demux-avi`, to verify this
crate's own output demuxes as intended.
