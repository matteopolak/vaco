# End-to-end gaps, measured 2026-08-29

Measured by building `vaco`/`vaco-probe` and running real invocations against
real ffmpeg-produced media. **None of these are missing codecs.** Every one is
integration glue between pieces that already exist and already work.

## What already works

- `vaco-probe` on mp4, mkv, mp3, wav — including `-print_format json -show_streams`.
- Stream-copy remux: mp4→mkv, mp4→mp4, mp4→ts, wav→wav.
- Audio filtering: `-af volume=0.5` through to a real wav.

## 1. H.264 decode is unreachable from the binary — the top blocker

`H264Decoder::send_packet` resolves the slice header's `pic_parameter_set_id`,
reads `entropy_coding_mode_flag` off the PPS, and **stops**. Its own module doc
says so. It has never decoded a pixel.

Meanwhile `reconstruct_picture_with_inter` and `reconstruct_picture_luma` decode
`cabac_ip_simple.264` **100% byte-exact against ffmpeg, all 25 frames,
0/102400 luma samples differing** — and both are `pub(crate)` with **no caller
anywhere in `src/`**. The only thing that drives them is a `#[cfg(test)]` module
inside `reconstruct.rs`.

So the entire H.264 decoder is reachable only from its own tests. `vaco -i
any.mp4` cannot decode video. This is the inverse of the "an API with no caller
is invisible to every test you will write" rule: here the *implementation* has
no production caller, and the tests hid that by driving it directly.

Needs: an access-unit driver in `H264Decoder` (reusing
`H264Parser::push_access_unit` rather than duplicating it), DPB/output ordering,
and AVCC-vs-Annex-B handling. MP4 stores length-prefixed AVCC; the decoder wants
Annex-B; `h264_mp4toannexb` exists and is registered but **nothing in the decode
path ever applies it**.

## 1b. Measured H.264 capability after wiring (2026-08-29, real binary)

`H264Decoder` is now wired to the real reconstruction (`a81e2d2`) and genuinely
decodes through the CLI. Measured against `libx264`-encoded `testsrc2`:

| Input | Result |
|---|---|
| Main profile, `-bf 0` | **2 of 25 frames**, then a CABAC desync |
| Main profile, default (B-frames) | 2 frames, then `CABAC B-slice mb_type/sub_mb_type` |
| High profile (x264's **default**) | 0 frames — `transform_size_8x8_flag`/Intra_8x8 unimplemented |
| Baseline / Constrained Baseline | 0 frames — CAVLC reconstruction unimplemented |

So the wiring is correct and the first frames are byte-exact, but no real-world
file decodes to completion yet. In priority order, what stands between here and
ordinary MP4 playback:

1. **The CABAC desync on P slices** — `end_of_slice_flag` fires before every
   macroblock is decoded. Pre-existing, tracked by ignored tests in
   `tests/macroblock_layer_cabac.rs`, several prior investigation rounds, root
   cause unisolated. This is the blocker: it stops even the simplest real
   stream at frame 3.
2. **Intra_8x8 / `transform_size_8x8_flag`** — x264 defaults to High profile,
   so most files in the world start here.
3. **CABAC B slices** — x264 emits B-frames by default at every profile.
4. **CAVLC reconstruction** — the entropy layer verifies bit consumption and
   discards its coefficients and motion vectors.

Nothing here is a design problem; all four are unfinished implementation with a
correct decoder underneath.

## 2. Matroska rejects codecs it should map

`the muxer refused a stream: unsupported: matroska: codec has no CodecID
mapping` for `ffv1` and `flac` — both of which we implement. A table gap.

## 3. No automatic sample-format conversion for muxer constraints

`wav: planar sample formats are not supported`. Decoding AAC yields planar
float; wav needs packed. ffmpeg inserts the conversion automatically; we refuse
the stream instead. `vaco-codec-dsp-fmtconvert` and `vaco-resample` both exist.

## 4. `-f null` has no default encoder

`Default encoder for format null (codec none) is probably disabled`. `-f null -`
is one of the most common invocations there is (decode-and-discard timing runs).

## 5. mkv → mp4 stream copy fails on timestamps

`non-monotonic dts: this container requires strictly increasing timestamps`.
Matroska carries DTS the mp4 muxer then rejects.

## The pattern

Four separate defects today were "registered but not actually reachable" — the
bitstream-filter crates, two empty `FormatFlags` declarations, the Ogg/Theora
extradata gap, the `.jls` extension gap. This list is the same class one level
up: **the pieces work; the wiring between them does not.** Verification that
drives internals directly cannot see any of it. Only running the binary can.
