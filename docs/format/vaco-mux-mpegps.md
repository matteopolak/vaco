# `vaco-mux-mpegps`

## What it is

The MPEG program stream muxer family: five registry entries (`mpeg`, `vcd`,
`vob`, `svcd`, `dvd`) sharing one implementation, `mux::PsMuxer`, that
differ only in `mux::MuxProfile` — which PES envelope (MPEG-1 or MPEG-2), which
pack-header syntax, and whether packs are padded to a fixed size.

| Muxer | Reference long name | PES syntax | Pack syntax | Fixed pack size |
|---|---|---|---|---|
| `mpeg` | MPEG-1 Systems / MPEG program stream | MPEG-1 | MPEG-1 | none |
| `vcd` | MPEG-1 Systems / MPEG program stream (VCD) | MPEG-1 | MPEG-1 | 2048 B |
| `vob` | MPEG-2 PS (VOB) | MPEG-2 | MPEG-2 | 2048 B |
| `svcd` | MPEG-2 PS (SVCD) | MPEG-2 | MPEG-2 | 2048 B |
| `dvd` | MPEG-2 PS (DVD VOB) | MPEG-2 | MPEG-2 | 2048 B |

The 2048-byte fixed pack size for `vcd`/`svcd`/`dvd`/`vob` is **measured**
against `ffmpeg 8.1` output, not the White Book's nominal 2324-byte CD-ROM/
XA sector size — the reference does not use that size for any of these four
muxers today (checked directly: all four wrote packs exactly 2048 bytes
apart on a synthetic clip). Recorded as a `MuxProfile::fixed_pack_size` comment
so nobody "fixes" it back to the textbook value later without re-measuring.

## How it works

### Layout

| Module | Contents |
|---|---|
| `pack` | pack header and system header **encoding**, both syntaxes |
| `pes` | PES packet header **encoding**, both syntaxes, plus padding-stream framing |
| `mux` | `MuxProfile`, `PsMuxer`, the five profile constants |

### One PES packet per pack

`PsMuxer::write_packet` writes one pack header, the system header (only on
the very first pack — every sample measured has it immediately follow the
first pack header, never standalone), the packet's PES envelope, and — for
a fixed-pack-size profile — padding-stream bytes up to the nominal size,
per call. **This crate does not split an oversized PES payload across pack
boundaries** the way the reference's VOB/SVCD/DVD profiles do; a payload
larger than the fixed pack size simply makes that one pack larger than
nominal. That keeps every pack independently self-describing and trivially
round-trippable, at the cost of not reproducing the reference's exact
sector alignment on large frames. See "What was and was not measured".

### Stream id assignment

`add_stream` assigns sequential ids by media type: video gets
`0xE0..=0xEF`, audio gets `0xC0..=0xDF`. A caller wanting a
`private_stream_1` substream (AC-3/DTS/LPCM/subpicture) sets
`CodecParameters::codec_tag` to one of this crate's own placeholder byte
tags — `b"AC-3"`, `b"DTS "`, `b"LPCM"`, `b"dvsp"` — because
`vaco_codec_core::CodecId` has no variant for any of those codecs yet
(surveyed 2026-08-23; see `vaco-demux-mpegps`'s docs for the same gap on the
read side). Each kind gets its own sub-id counter within its DVD-authoring
range (`mux::PsMuxer::next_ac3`/`next_dts`/`next_lpcm`/`next_subpicture`).

## How to change it

* **Real substream selection**: once `vaco-codec-core` gains `CodecId`
  variants for AC-3/DTS/LPCM/DVD-subpicture, replace
  `PsMuxer::substream_range`'s `codec_tag` matching with a match on
  `params.codec_id` — the sub-id assignment machinery underneath does not
  need to change.
* **Pack splitting for byte-exact VOB/SVCD/DVD output**: `write_pack_and_pes`
  is the one place that would need it — split `pes` across multiple pack
  headers with a fresh SCR each, carrying over the remaining bytes as a
  continuation. Not implemented; see "What was and was not measured".
* **A new muxer profile** (e.g. a stricter `dvd-video` with mandatory
  audio/video presence checks): add a `MuxProfile` constant and a
  `MuxerDesc`/`open_*` pair in `lib.rs`, following the existing five.

### Why this crate does not share code with `vaco-demux-mpegps`

Same reasoning as `vaco-demux-mpegps`'s docs file, mirrored: plan 18 §8.3
names `vaco-format-mpeg-common` as the intended shared home for PES/pack
encode-and-decode and the timestamp codec, and it does not exist (out of
this brief's scope to create). This crate's `pack.rs`/`pes.rs` are
independent **encoders**, verified against the sibling crate's independent
**decoders** via a cross-crate property test
(`pack::tests::mpeg{1,2}_scr_round_trips_through_the_demux_crate`, using
`vaco-demux-mpegps` as a dev-dependency only — the production dependency
graph does not include it). That property test is exactly the kind of
regression a shared crate would make structurally impossible to violate; see
`vaco-demux-mpegps`'s docs for what moving to one would take.

## Configuration

No crate-specific options; `Muxer::add_stream`/`write_packet`/
`write_trailer` are the whole interface, per the frozen `Muxer` trait.

| Constant | Value | Meaning |
|---|---|---|
| `mux::PsMuxer::scr_step` | 900 (10 ms at 90 kHz) | Nominal SCR advance per pack when a packet's own duration is not consulted |
| Per-profile `fixed_pack_size` | `None` (`mpeg`) or `Some(2048)` (others) | See the table above |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` for the production build. `vaco-demux-mpegps` is a
**dev-dependency only**, used exclusively by `tests/roundtrip.rs` and the
cross-crate property tests in `src/pack.rs` — not part of the shipped
dependency graph.

## What was and was not measured

Verified directly:

* Pack header and system header **encoding** bit layouts, cross-checked
  against `vaco-demux-mpegps`'s independently-derived **decoding** formulas
  via a `proptest` property test over the full 33-bit SCR range, for both
  pack syntaxes.
* End-to-end round trip: mux two packets (video + audio) through each of
  `mpeg`/`vob`/`dvd`, demux the result with `vaco-demux-mpegps`, and recover
  the same stream count, payload bytes, and PTS/DTS values
  (`tests/roundtrip.rs`).
* Fixed-pack-size padding produces output at least `2 * 2048` bytes for two
  small packets on the `dvd` profile.

**Not measured, and known to diverge from the reference**:

* **Byte-exact output.** This muxer's pack boundaries, SCR step and
  interleaving policy are this crate's own reasonable choices, not a
  reproduction of the reference's multiplexing algorithm. Plan 18 §0 calls
  muxing "a pure function of (packets, options)" and therefore the
  subsystem where byte-identical output is most achievable — that remains
  true in principle, but reaching it here would need probing the
  reference's actual pack-fill/SCR-step/interleave decisions field by
  field, which this pass did not do.
* **Oversized-payload pack splitting** (see "How it works" above) — not
  implemented, so a payload larger than 2048 bytes on a fixed-pack-size
  profile produces a structurally valid but non-reference-shaped pack.
* System header `rate_bound`/buffer-bound values are plausible constants
  (`mux::PsMuxer::system_header`), not derived from the actual stream
  bitrates the way the reference computes them.
