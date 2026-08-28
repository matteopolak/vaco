# `vaco-bsf-generic`

Layer 4. Codec-agnostic bitstream filters. Issue #349.

---

## What it is

Every filter `ffmpeg -bsfs` lists that does not need per-codec NAL vocabulary:
`null`, `extract_extradata`, `noise`, `remove_extra`, `setts`, `chomp`,
`dump_extra`, `filter_units`, `trace_headers`, and — added for issue #354
(B-06) — `showinfo`. `h264_mp4toannexb`/`hevc_mp4toannexb`/`h264_metadata`/
`hevc_metadata`/`h264_redundant_pps`/`dts2pts` are `vaco-bsf-h2645`, one layer
up — see that crate's docs for the split.

The original brief's list of nine names was close but not exact against the
reference (`ffmpeg -bsfs`/`ffmpeg -h bsf=<name>`, measured 2026-08-23):
`dump_extra` (not `dump_extradata`) and `remove_extra` (not
`remove_extradata`) are the registered names — the AVOption *group* prints as
`dump_extradata bsf`/`remove_extradata AVOptions`, which is a different string
than the filter's own registered name. `trace_headers` was not in the
original list at all but is trivial to add (identity transform; see below) and
is included.

| Filter | Behaviour implemented | Notes |
|---|---|---|
| `null` | Identity | Matches the reference exactly; there is nothing else it does |
| `extract_extradata` | Synthesise extradata from in-band H.264/HEVC parameter sets | See "What is measured" in the module doc; codec coverage limited to h264/hevc |
| `dump_extra` | Prepend `extradata` onto the **first** keyframe's payload, once | `freq=all` measured but not confidently characterised; unreachable via the current seam anyway |
| `remove_extra` | Strip a leading byte-for-byte copy of `extradata` from **every** keyframe | Generic byte-prefix match; a no-op for H.264/HEVC, which don't repeat `extradata` verbatim |
| `chomp` | Trim every trailing `0x00` byte | Unambiguous from the reference's own one-line description |
| `setts`, `filter_units`, `noise`, `trace_headers` | Identity by default | Real behaviour needs options this crate's registry seam cannot pass yet (see below) |
| `showinfo` | Identity, byte-exact | No `Supported codecs:` restriction in the reference either; its only effect is a `stderr` diagnostic this crate does not reproduce (see `crate::trace_headers`'s identical call) |

## How it works

Every filter is a `vaco_bsf_core::PacketMap` wrapped in
`vaco_bsf_core::MappedFilter`. Each module exports one `pub const DESC:
vaco_bsf_core::BsfDesc`, which a `vaco-component.toml` fragment's `ctor`
names and `vaco-registry`'s `Bsfs` matches on by name (see that crate's docs
for why matching by name, not a generated typed table).

### The gap that shapes half this crate: no per-instance options

`vaco_format_core::mux::BsfProvider::open(&self, name: &str, params:
&CodecParameters)` carries no options string. Every filter here therefore
implements the reference's **bare-name** (all-default-options) behaviour
only — `noise`, `setts`, `filter_units` and `remove_extra`/`dump_extra`'s
`freq=all` mode are all meaningfully configurable in the reference and are
not reachable through this seam today. Recorded once, for every filter it
affects, rather than per filter — see `planning/INTERFACE-GAPS.md`.

`noise` additionally does not attempt byte parity with the reference at all:
measured directly, the reference's bare `-bsf:v noise` corrupts every byte
non-deterministically (no seed option exists), so there is no reference
answer to converge on. This crate's `noise` defaults to the identity
transform and carries its own deterministic corruption algorithm (xorshift64*)
for when a real options seam exists — see the module's own doc comment.

## How to change it

Add a module, implement `PacketMap`, export a `DESC`, add it to `filters()`
in `lib.rs`, and add a `[[component]]` table to `vaco-component.toml`
(`kind = "bitstream_filter"`). Run `cargo xtask gen-registry`.

## Configuration

None reachable — see "the gap that shapes half this crate" above.

## Dependencies

`vaco-bsf-core` for the driver; `vaco-format-nalu` for NAL framing and header
layout (`extract_extradata` only); `vaco-parse-h264`/`vaco-parse-hevc` for the
*meaning* of a NAL type number (`extract_extradata` only); `vaco-pool` for the
`PacketSideData::NewExtradata` payload type.
