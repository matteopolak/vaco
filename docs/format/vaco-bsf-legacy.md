# `vaco-bsf-legacy`

Layer 4. Legacy/professional-format bitstream filters. Issues #353 (B-05),
#354 (B-06).

---

## What it is

`mpeg2_metadata` and `prores_metadata` — the two `*_metadata` filters left
over from #353 once `h264_metadata`/`hevc_metadata` claimed `vaco-bsf-h2645`
and `av1_metadata`/`vp9_metadata`/`opus_metadata` were already homed
elsewhere. Both are the measured identity transform, for the same reason as
their siblings: every option either exposes defaults to "leave the
bitstream alone" (`ffmpeg -h bsf=mpeg2_metadata`/`prores_metadata`), verified
against a real `mpeg2video` elementary stream (`cmp`, byte-identical) and a
real `prores_ks` stream (`framemd5`-identical, checked independently of `mov`
container overhead).

## What was measured and left out

Checked against `ffmpeg 8.1` and left unregistered, each for its own reason
rather than a blanket "too old":

| Filter | Why it is not here |
|---|---|
| `mjpeg2jpeg` | No options at all, so no default to fall back on. It both inserts the ITU-T T.81 Annex K.3 standard Huffman tables **and** rewrites the leading `APP0`'s JFIF version/density fields — the DHT insertion is spec-derived and safe, but the JFIF rewrite was only ever seen on one sample, with no way in this environment to vary the source density independently and tell "always overwrite with these constants" apart from "derive from the input". Left out rather than generalised from one data point. |
| `mjpegadump` | Inserts a 40-byte `APP1` marker with an ASCII `mjpg` tag and two repeated 4-byte fields — again exactly one sample, with no way to vary what those fields encode. |
| `imxdump` | Targets Sony XDCAM IMX/D-10 streams specifically; this environment's `mpeg2video` encoder has no IMX/D-10 profile option, and running it on an ordinary stream is not a no-op but also not evidence of *correct* IMX behaviour. |
| `dovi_rpu` | Needs a real Dolby Vision RPU-bearing stream; no encoder here can produce one. |
| `dv_error_marker` | Needs damaged/concealed DV footage to exercise its 18-value error-status flag set; none available. |
| `evc_frame_merge` | This `ffmpeg` build has an EVC decoder but no encoder — no oracle input. |
| `hapqa_extract`, `media100_to_mjpegb` | Name codecs (`hap`, `media100`) with no `CodecId` in this workspace — unreachable. |
| `apv_metadata`, `lcevc_metadata` | No `CodecId` for APV or LCEVC — unreachable. |
| `vvc_metadata`, `vvc_mp4toannexb` | `CodecId::Vvc` exists, but this `ffmpeg` build has a VVC decoder only (no encoder) and no VVC sample was available — never actually measured against a real bitstream. |

`h264_redundant_pps` is `vaco-bsf-h2645`'s exclusion, not this crate's.

## How it works

Same shape as every `vaco-bsf-*` crate: one `BsfDesc` per module on
`PacketMap`/`MappedFilter`. Both filters here are pure identity, gated only
on `codec_id` — no codec-specific parsing crate needed.

## How to change it

Add a module, implement `PacketMap`, export a `DESC`, add it to `filters()`,
and add a `[[component]]` table to `vaco-component.toml`.

## Configuration

None reachable — see `vaco-bsf-h2645`'s docs for the fuller account of gap 12
 and why that does not stop these filters from
being worth registering: the bare-name behaviour this interface limits us to
is also the measured-correct one.

## Dependencies

`vaco-bsf-core` for the driver; `vaco-codec-core` for `CodecId`/
`CodecParameters`. Nothing else.
