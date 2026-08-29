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

## 1b. Measured H.264 capability (corrected 2026-08-29, real binary)

**A real H.264 stream now decodes end to end.** Measured with a scripted
harness against `libx264`-encoded `testsrc2`, after the deblocking fix in
`e63c09f`:

| Input | Result |
|---|---|
| Main, `-bf 0 -refs 1` | **FULL 25/25 frames** |
| Main, `-bf 0` (refs 3) | 2/25 — CABAC desync, needs multiple references |
| Main, default (B-frames) | 2/25 — `CABAC B-slice mb_type/sub_mb_type` |
| High, any (**x264's own default**) | 0/25 — `transform_size_8x8_flag`/Intra_8x8 |
| Baseline | 0/25 — CAVLC reconstruction unimplemented |

**An earlier version of this section was wrong** and is corrected here. It
claimed Main with `-bf 0` reached only 2 frames and blamed a CABAC desync on
plain P slices. Two errors: `-bf 0` alone still leaves `-refs 3`, and the
measuring harness silently reported `0/25` for every row because a partial
write met an exact-size comparison. The desync is real but is **multi-reference
only** — single-reference Main content decodes completely.

Remaining work, ordered by how many real files each blocks:

1. **Intra_8x8 / `transform_size_8x8_flag`** — High profile is x264's default,
   so this alone excludes most files in existence.
2. **CABAC B slices** — x264 emits B-frames by default at every profile.
3. **The multi-reference CABAC desync** — x264 defaults to `-refs 3`. Sharpest
   known repro: slice 4 of `cabac_ip_multiref.264` stops at 35 of 36
   macroblocks. `ref_idx_lX` binarization has been ruled out against clause
   9.3.3.1.1.6.
4. **CAVLC reconstruction** — the entropy layer verifies bit consumption and
   discards its coefficients and motion vectors.

All four are unfinished implementation over a decoder now proven correct on a
real stream, not design problems.

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
