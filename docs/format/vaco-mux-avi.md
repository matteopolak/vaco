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

### Length-prefixed H.264/HEVC is converted to Annex B

AVI has no out-of-band configuration record the way MP4's `avcC`/`hvcC` do,
so it expects H.264/HEVC as Annex B: start-code-delimited NAL units, SPS/PPS
in-band. A stream sourced from a length-prefixed container (typically MP4,
via `-c copy`) arrives with 4-byte length prefixes instead, which used to be
written straight into the `movi` chunk verbatim — finding 16
(`planning/CONFORMANCE-FINDINGS.md`)'s measured 3.4× size gap against the
reference, and a structural one: the two files are not different lengths of
the same bytes, they are laid out differently.

`add_stream` now records a `LengthSize` on the `StreamOut` whenever the video
codec is H.264/HEVC and `VideoParameters::nal_length_size` says the source is
length-prefixed; `AviMuxer::maybe_convert` runs
`vaco_format_nalu::convert::length_prefixed_to_annexb` over the payload in
`write_packet` before anything else touches it — mirroring
`vaco-mux-mpegts::MpegTsMuxer::maybe_convert`, which solves the identical
problem for the identical codecs (that is also *why* this crate now depends
on `vaco-format-nalu`, which it did not before this fix). Verified against
the reference directly (`ffmpeg -i <mp4-source> -c copy -f avi`): per-packet
sizes now match exactly, byte for byte — the *remaining* total-file-size gap
against the reference is a separate, still-open difference (an ~192-byte gap
appears between consecutive `movi` chunks in the reference's own output for
reasons not yet understood; not a bitstream-framing issue, since the payload
bytes and their per-packet lengths already match).

**AAC arriving in ADTS framing (no `AudioSpecificConfig` in `extradata`) is
refused, not silently written**: AVI's `WAVE_FORMAT_AAC` entry expects raw,
config-out-of-band-framed AAC, and MPEG-TS's own AAC convention is ADTS
(config repeated per frame, no separate config blob) — measured directly
(`ffmpeg -i <mpegts-source> -c copy -f avi` refuses at `write_header` with
"ADTS is only supported with codec tag 0x1610"). `add_stream` refuses AAC
with empty/absent `extradata` for the same reason finding 19
(`planning/CONFORMANCE-FINDINGS.md`) named this "silent success": writing the
chunk anyway produces a technically-malformed audio stream no real AVI reader
expects.

**A packet with no PTS at all is refused, not silently muxed with a
`pts=dts=0` chunk timestamp** — also finding 19, measured the same way
(`ffmpeg -i <avi-source-with-no-pts> -c copy -f {mpegts,flv}` refuses; AVI
itself has no PTS field to lose, so this specific check lives in
`vaco-mux-mpegts`/`vaco-mux-flv` rather than here, but is documented in both
places since it is one finding).

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
(`CodecParameters`, `CodecId`), `vaco-format-nalu`
(`convert::length_prefixed_to_annexb`, `LengthSize` — added for finding 16;
see *Length-prefixed H.264/HEVC is converted to Annex B* above),
`vaco-limits` (`Budget`, for that same conversion's bound). Dev-only:
`vaco-demux-avi`, to verify this crate's own output demuxes as intended.

## Tests and fuzzing

`tests/roundtrip.rs` covers the length-prefixed-to-Annex-B conversion
directly (`a_length_prefixed_h264_sample_is_rewritten_to_annex_b`) and the
finding-19 refusals (`adts_framed_aac_with_no_extradata_is_rejected`,
`raw_aac_with_extradata_is_accepted`), alongside the pre-existing shape/order/
trailer-patch tests.

`fuzz/fuzz_targets/avi_mux_packet.rs` mirrors `vaco-mux-mpegts`'s own
`mpegts_mux_packet` target: arbitrary bytes through `write_packet`, with
`nal_length_size` toggled by an input bit, asserting output growth stays
within a generous bound. 30-second run: `exit=0`, `execs≈1,753,000` (varies
run to run), `find fuzz/artifacts -type f` empty.
