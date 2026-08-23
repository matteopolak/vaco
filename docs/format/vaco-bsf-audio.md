# `vaco-bsf-audio`

Layer 4. Audio bitstream filters. Issue #352 (B-04).

---

## What it is

Membership was derived from `ffmpeg -bsfs`/`ffmpeg -h bsf=<name>`, not the
issue title: `aac_adtstoasc` (the priority filter — "the one a real
MP4-from-TS remux needs"), `opus_metadata`, `pcm_rechunk`.

| Filter | Behaviour implemented | Notes |
|---|---|---|
| `aac_adtstoasc` | Strip ADTS framing, synthesise `AudioSpecificConfig` extradata | ISO/IEC 13818-7 Annex B + ISO/IEC 14496-3 §1.6.2.1, cross-checked against a real encode; single-block ADTS only |
| `opus_metadata` | Identity | `gain` defaults to `0` (no-op); measured bare-invocation byte-identity |
| `pcm_rechunk` | Re-slice interleaved PCM into fixed 1024-sample-frame chunks, zero-padding the tail | Byte-for-byte re-slice, no resampling; covers the `CodecId` PCM variants this workspace has |

### Two names that sound like they belong here and do not

* **`dts2pts`** — its name suggests DTS audio; `ffmpeg -h bsf=dts2pts`
  reports `Supported codecs: h264 hevc`. "dts" is *decode timestamp*, not
  the codec. Not audio, not in this crate.
* **`ahx_to_mp2`** — genuinely audio (`Supported codecs: ahx`), but AHX has
  no `CodecId` variant anywhere in this workspace, so there is no way to
  even construct a `CodecParameters` this filter could claim. Unreachable,
  not merely unimplemented.

### Also deliberately absent: `dca_core`, `eac3_core`, `truehd_core`

Every DTS/E-AC-3/TrueHD encoder available in this environment produces
core-only output (no extension or dependent substream), so every sample
this environment can generate makes these three filters look like the
identity transform whether or not that is true in general. Implementing
their real substream-stripping logic with nothing extension-bearing to
falsify it against would be presenting a guess as a measurement. Left out,
flagged for whoever has real DTS-HD/Atmos/JOC material.

## How it works

Same shape as every other `vaco-bsf-*` crate: one `vaco_bsf_core::BsfDesc`
per module, `PacketMap` wrapped in `MappedFilter`. `aac_adtstoasc` attaches
its synthesised config the same way `vaco-bsf-generic::extract_extradata`
does — `PacketSideData::NewExtradata`, emitted once per change, not on
every packet.

### What is measured, not assumed (`aac_adtstoasc`)

A real `libavcodec` `aac` ADTS encode: `profile=1` (AAC-LC),
`sampling_frequency_index=4` (44100 Hz), `channel_configuration=1` (mono)
produced the `AudioSpecificConfig` `12 08` — read out of the MP4 `esds` box
by properly walking the nested MPEG-4 descriptor length varints, not by
eyeballing a byte offset (which produced a value 8 off from the real one on
the first attempt — a real lesson in why the length is a varint). `12 08`
matches this crate's formula exactly:
`(audioObjectType << 11) | (samplingFrequencyIndex << 7) | (channelConfiguration << 3)`.

## How to change it

Add a module, implement `PacketMap`, export a `DESC`, add it to `filters()`
in `lib.rs`, and add a `[[component]]` table to `vaco-component.toml`.
Run `cargo xtask gen-registry`.

## Configuration

None reachable: `vaco_format_core::mux::BsfProvider::open` has no
per-instance option string (`planning/INTERFACE-GAPS.md` gap 12).
`opus_metadata`'s `gain` and `pcm_rechunk`'s three options all default to
what this crate already implements.

## Dependencies

`vaco-bsf-core` for the driver; `vaco-pool` for the
`PacketSideData::NewExtradata` payload type; `vaco-chlayout` for
`pcm_rechunk`'s channel count.
