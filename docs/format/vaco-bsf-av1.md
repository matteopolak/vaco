# `vaco-bsf-av1`

Layer 4. AV1 bitstream filters. Issue #351 (B-03).

---

## What it is

`ffmpeg -bsfs`/`ffmpeg -h bsf=<name>` gives three filters whose supported
codec list is `av1` alone: `av1_frame_split`, `av1_frame_merge`,
`av1_metadata`. `dovi_rpu` also names `av1` (alongside `hevc`) but is
deliberately not here — see `frame_split`'s module docs for why a dual-codec
filter does not fit this crate.

| Filter | Behaviour implemented | Notes |
|---|---|---|
| `av1_frame_split` | Split a packet with several coded frames into one packet per frame | Leading non-frame OBUs (TD, sequence header) ride with the first output packet; a single-frame packet passes through untouched |
| `av1_frame_merge` | Reassemble a Temporal-Delimiter-bounded run of packets into one per temporal unit | Refuses (`Error::InvalidData`) a stream with no TD at all — matches the reference on MP4-sourced samples |
| `av1_metadata` | Identity | Every one of its nine options defaults to "leave alone"; measured bare-invocation byte-identity |

## How it works

Same shape as `vaco-bsf-generic`: each module exports one `vaco_bsf_core::BsfDesc`,
built on `vaco_bsf_core::PacketMap` wrapped in `vaco_bsf_core::MappedFilter`.
`frame_split`/`frame_merge` use `vaco-parse-av1`'s `obu::units`/`ObuHeader` for
OBU framing — the same crate that already owns AV1 NAL-adjacent parsing, so no
second implementation of OBU boundary-finding exists (D19).

### The grouping rule, measured not assumed

`av1_frame_split`'s docs and tests record the exact measurement: an SVT-AV1
temporal unit shaped `TD, FRAME, FRAME, FRAME, FRAME, FRAME` (241 bytes)
became five packets of 109, 71, 24, 19, 18 bytes — the 109-byte first packet
is exactly the 2-byte TD plus the first 107-byte FRAME OBU. `av1_frame_merge`
was verified as a full round trip: split then merge reproduces a real
`obu`-demuxed elementary stream's native packetisation byte for byte
(`framecrc`-checked, 25/25 packets), and as a negative case — an MP4-sourced
stream (no TD ever) is refused exactly as the reference refuses it.

## How to change it

Add a module, implement `PacketMap`, export a `DESC`, add it to `filters()`
in `lib.rs`, and add a `[[component]]` table to `vaco-component.toml`.
Run `cargo xtask gen-registry`.

## Configuration

None reachable: `vaco_format_core::mux::BsfProvider::open` has no
per-instance option string (`planning/INTERFACE-GAPS.md` gap 12).

## Dependencies

`vaco-bsf-core` for the driver; `vaco-parse-av1` for OBU framing
(`obu::units`, `ObuHeader`, `ObuType`).
