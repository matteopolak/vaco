# `vaco-bsf-h2645`

Layer 4. H.264/HEVC bitstream filters. Issue #350.

---

## What it is

`h264_mp4toannexb` and `hevc_mp4toannexb`: the two filters `vaco-mux-avi` and
`vaco-mux-mpegts` were waiting on. `h264_redundant_pps` is **measured but not
implemented** — see below.

## How it works

Framing (length-prefixed to Annex B) is
`vaco_format_nalu::convert::length_prefixed_to_annexb`, not reimplemented
here — that crate's own module docs name this crate as the intended home for
"everything else": parameter-set splicing. Parameter sets come from
`vaco_parse_h264::AvcDecoderConfigurationRecord`/
`vaco_parse_hevc::HevcDecoderConfigurationRecord`, parsed once at
construction from `CodecParameters::extradata`. Splicing is a NAL-unit
insertion located with `vaco_format_nalu::units`, not hand-rolled start-code
scanning.

Measured against `ffmpeg 8.1` (recipe in each module's doc comment): every
access unit whose first VCL NAL is an IDR (H.264) or IRAP (HEVC) gets the
record's parameter sets spliced in immediately before that unit, after any
leading non-VCL unit already present (a leading SEI stayed in front in every
case tested). A stream with no usable extradata, or already Annex B, is a
pass-through.

**One disclosed divergence**: the reference writes the unit immediately
following an insertion with a 3-byte Annex B start code (H.264 only — HEVC
uses 4 throughout, checked on the identical experiment). This crate always
writes 4, matching `vaco_format_nalu::convert`'s own established convention
("four is what every producer writes ... and the difference is not worth a
knob") rather than reopening that decision for one cosmetic byte. Everything
else — unit boundaries, content, order — matches exactly; see
`h264_mp4toannexb`'s own test, which byte-compares against real `ffmpeg 8.1`
output captured from a genuine MP4 packet (not a self-comparison).

### `h264_redundant_pps` — measured, not implemented

Measured against `ffmpeg 8.1` on a stream with `repeat-headers=1` (two PPS
occurrences per keyframe): the edit is not a clean unit deletion. A
`SequenceMatcher` diff starts inside the surviving PPS's own RBSP (a few bits
shorter) and continues as small, recurring, non-byte-aligned differences
through the following slice's CABAC-coded data — the signature of a bit width
changing mid-stream (most likely `pic_parameter_set_id`'s `ue(v)` encoding),
not of a unit being dropped.

Reproducing that needs a CABAC-safe, bit-precise PPS rewrite and slice-header
renumbering — the same class of problem `vaco_parse_hevc::cbs::HevcCbs`'s own
docs call "not yet supported," for the identical reason: a writer that is not
bit-exact silently corrupts a stream rather than failing. `vaco-parse-h264`
has no bit-writer layer at all (unlike HEVC's `cbs` module). Left out rather
than landed wrong.

## How to change it

Add a module, implement `PacketMap`, export a `DESC`, add it to `filters()`,
and add a `[[component]]` table to `vaco-component.toml`.

## Configuration

None — see `vaco-bsf-generic`'s docs for why (`BsfProvider::open` carries no
option string).

## Dependencies

`vaco-bsf-core` for the driver; `vaco-format-nalu` for framing and NAL
headers; `vaco-parse-h264`/`vaco-parse-hevc` for decoder-configuration-record
parsing.
