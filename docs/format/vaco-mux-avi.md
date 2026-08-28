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

### Video sits on a fixed 600 Hz grid; audio does not

AVI has no per-packet timestamp field. A frame's presentation time is its
ordinal position, so a video stream's `stream_time_base` is a fixed
`GRID_RATE` of `1/600` regardless of the source's own frame rate — measured
constant across every frame rate tried. `write_packet` places each real
video packet at the slot its (rescaled) `dts`/`pts` rounds to, and
`AviMuxer::backfill_grid_slots` fills every slot skipped since the last real
one with a zero-length placeholder chunk. `strh.dwLength`/`avih.dwTotalFrames`
fall out of this for free: a video `StreamOut::count` is the next unfilled
slot throughout, which is already the right value once the file is done.

Because AVI has no absolute-time field either, the very first video packet's
own timestamp becomes slot zero (`StreamOut::grid_origin`) — a source clock
that does not start there (routine for MPEG-TS) is rebased against it,
rather than pushing every slot out by however far that clock had already
run. The gap a video timestamp implies is attacker-controlled the ordinary
way (it comes straight from the input container), so `backfill_grid_slots`
checks it against a `Budget` before writing or indexing anything — see
`grid_budget`'s doc comment for the numbers.

Audio has no such grid: one chunk is one frame (VBR) or a fixed number of
samples (CBR PCM), and `write_packet` still only needs packets to arrive in
the order the caller wants them replayed. A source whose compressed-audio
timeline itself has gaps (dropped frames, not just a nonzero start) is not
covered by this — see *What was exercised, and what was not* below.

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

### `dwMicroSecPerFrame` and `vprp` come from the source, not the grid

`avih.dwMicroSecPerFrame` tracks the *source* time base (`1e6 × num / den`,
truncating) rather than `GRID_RATE` — internally inconsistent with `strh`'s
own `dwRate`, but that is what a real writer states. The source time base is
only available through `Muxer::add_stream_with`'s `StreamSpec`, so
`AviMuxer` overrides that method to capture it into `StreamOut::source_time_base`;
a caller that drives `add_stream` directly (every test in this crate) never
supplies one, and the field stays `0`.

Each video `strl` also gets a `vprp` (`OpenDML` video-properties) chunk,
built from `CodecParameters` alone: dimensions, aspect ratio (`1:1` when the
source declared none), and a single progressive `VIDEO_FIELD_DESC`.
Interlaced sources get the same single-field shape, which is a known gap —
this crate has no interlace-aware `vprp` layout yet.

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
  keyframe/PTS derivation on the read side, the seekable trailer-patch path,
  the 600 Hz grid's placeholder backfill and origin rebasing, an implausible
  grid gap being rejected, and `dwMicroSecPerFrame` when driven through
  `MuxBuilder`. Verified directly against `ffmpeg 8.1 -c copy -f avi` on
  three fixtures (an MP4 with B-frame-free H.264+AAC, a video-only MP4, and
  an MPEG-TS with B-frames and a non-zero start time): the decoded video is
  byte-identical to both the source and the reference's own output in every
  case.
- **Not exercised**: the non-seekable-sink path (placeholder `0`/
  `0xFFFF_FFFF` values never actually read back by anything in this crate's
  tests), MP3/AAC audio muxing, more than two streams, `strn` or `LIST INFO`
  metadata output (this crate does not write either — see below), and
  interlaced `vprp`.
- **Known gap, not fixed here**: on the one fixture measured with compressed
  (AAC) audio, the reference also writes a handful of zero-length audio
  placeholder chunks that this crate does not — the same "position is time"
  principle the video grid applies, extended to audio's own per-frame
  duration, but not yet measured across enough fixtures to implement with
  confidence. The `JUNK` padding chunks `hdrl` reserves around each `strl`
  and before `movi` are also not written; their exact sizing rule was not
  determined from the one fixture available, and they carry no semantic
  content a reader depends on.

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
`nal_length_size` toggled by an input bit and a `pts`/`dts` gap (bounded to
`MAX_GRID_GAP` slots, so one iteration cannot spend its time writing an
unbounded run of placeholder chunks) derived from two more input bytes,
asserting output growth stays within a bound that accounts for both the
Annex-B conversion and the grid backfill. 30-second run: `exit=0`,
`execs≈1,660,000` (varies run to run), `find fuzz/artifacts -type f` empty.
The budget-rejection path for a genuinely implausible gap is covered by a
unit test instead (`an_implausible_grid_gap_is_rejected_not_looped_forever`),
where an exact input is worth more than a slow corpus entry.
