# `vaco-bsf-h2645`

Layer 4. H.264/HEVC bitstream filters. Issues #350, #353 (B-05), #354 (B-06).

---

## What it is

`h264_mp4toannexb` and `hevc_mp4toannexb`: the two filters `vaco-mux-avi` and
`vaco-mux-mpegts` were waiting on. `h264_metadata`/`hevc_metadata` (#353) are
here too, registered as the measured identity transform. `h264_redundant_pps`
and `dts2pts` (#354) are **measured but not implemented** — see below.

## The CBS write path (#353) — scaffolded, not built

`vaco-codec-cbs` has the general shape of a write path
(`CbsCodec::{read_unit, write_unit, assemble}`), but the only implementation
for either codec, `vaco_parse_hevc::cbs::HevcCbs`, can write back a raw
(undecoded) unit only — every typed variant (`Sps`/`Pps`/`Vps`/`Sei`) returns
`Error::Unsupported`. `vaco-parse-h264` has no `CbsCodec` implementation at
all. **No bit-exact H.264/HEVC parameter-set writer exists in this tree.**
That is the real, unstarted work B-05's title names.

It turns out not to be needed for `h264_metadata`/`hevc_metadata` themselves:
every option either filter exposes defaults to "leave the bitstream alone"
(`ffmpeg -h bsf=h264_metadata`/`hevc_metadata`), and gap 12 (below) means no
option can reach a filter instance regardless. Measured directly against
`ffmpeg 8.1` across five adversarial inputs each (plain, AUD-already-present,
non-16-multiple crop, explicit level + forced colour description, a longer
B-frame-bearing clip), the bare-name behaviour is byte-identical to the
input every time. Building the writer now would have no caller in this
workspace able to exercise it with anything but the default — left unbuilt.

## Gap 12 (`BsfProvider::open` has no option string) — not closed here

A trait method, not a bare fn pointer, so — mirroring the same day's
`Muxer::set_option` substitution for gaps 4/5/6 — it could plausibly be
closed by a defaulted `BitstreamFilter::set_option(&mut self, name: &str,
value: &str) -> Result<()>`. That trait lives in `vaco-codec-core`, outside
this issue owner's single-writer scope, so the shape is recorded in
`planning/INTERFACE-GAPS.md` and here rather than applied silently.

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

### `dts2pts` — measured, not implemented (#354)

Its name suggests DTS audio; `ffmpeg -h bsf=dts2pts` reports `Supported
codecs: h264 hevc` — "dts" is *decode timestamp*, not the codec. It touches
no bytes, only `Packet::pts`, so the CBS write path above is irrelevant to
it. Measured directly: assigned `pts` values are **not** a fixed
reorder-delay shift (`pts[3] == dts[3]`, no delay, while `pts[0] == dts[2]`
and `pts[1]` matches a `dts` four positions further out) — the signature of a
real picture-order-count computation (H.264 §8.2.1 / HEVC §8.3.1) over a
hierarchical B-frame structure, not a constant offset. Building that
correctly needs a decoder-adjacent reorder buffer validated against more than
one GOP shape; left unimplemented rather than shipped on an unverified guess
at the general rule.

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
