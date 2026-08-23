# `vaco-bsf-vpx`

Layer 4. VP9 bitstream filters. Issue #351 (B-03).

---

## What it is

`ffmpeg -bsfs` has exactly four filters reporting a VP-family codec, and
every one of them is `Supported codecs: vp9` — **there is no VP8 bitstream
filter in the reference at all**, so "VPx" in the issue title overstates it.

| Filter | Behaviour implemented | Notes |
|---|---|---|
| `vp9_metadata` | Identity | Both options default to "leave alone"; measured bare-invocation byte-identity against a real `libvpx-vp9` stream |
| `vp9_superframe_split` | Split a superframe (trailing index) into its constituent frames | Index format measured directly off a real two-pass `-auto-alt-ref` encode |
| `vp9_superframe` | Buffer invisible (`show_frame=0`) frames, flush with the next shown frame as one superframe | `show_frame` read from the VP9 uncompressed header's first few bits, verified against real bytes (see below) |
| `vp9_raw_reorder` | **Not implemented** | No oracle input in this environment — see below |

### Why `vp9_raw_reorder` is missing

Every VP9 stream this environment could produce that contains anything worth
reordering (alt-ref/hidden frames) packs them as superframes, and the
reference's own `vp9_raw_reorder` refuses superframe input outright
(`Input in superframes is not supported.`, measured directly). That leaves
no input that both exercises the filter and is not just ordinary,
already-ordered frames — implementing its reordering rule from a
description alone, with nothing to falsify it against, would be exactly the
false-confidence trap this project's conformance findings warn about
repeatedly. Left out and flagged for whoever has real non-superframe raw
VP9 material with hidden frames to test against.

## How it works

Same shape as the other `vaco-bsf-*` crates. `vp9_superframe`'s grouping
decision reads a handful of bits with `vaco_bitstream::BitReader` directly —
no VP9 NAL-level parser crate exists in this workspace to depend on instead,
and the fields needed (`frame_marker`, `profile`, `show_existing_frame`,
`frame_type`, `show_frame`) are cheap enough that building one would be
disproportionate to the need.

### The superframe index, measured

```text
last byte:  c9  =  1100_1001
            marker(3)=110, magbytes-1(2)=01, nframes-1(3)=001
         -> magbytes=2, nframes=2
index:      c9 85 34 98 02 c9   (6 bytes = 2 + nframes*magbytes)
```

`13445 + 664 + 6 == 14115` — the declared sizes cover only the constituent
frames, the index is on top. Confirmed on all six superframes a real
two-pass `-auto-alt-ref 1` encode produced, not just one.

### The `show_frame` bit layout, measured

Decoding `frame_marker` (must be `0b10`) at the hypothesised bit offset on
both halves of a real measured superframe gives `0b10` on both — a wrong
offset would hit that only by chance. The larger half decoded to
`show_frame=0`, the smaller to `show_frame=1`, consistent with an alt-ref
(never displayed) followed by the frame that is.

## How to change it

Add a module, implement `PacketMap`, export a `DESC`, add it to `filters()`
in `lib.rs`, and add a `[[component]]` table to `vaco-component.toml`.
Run `cargo xtask gen-registry`.

## Configuration

None reachable: `vaco_format_core::mux::BsfProvider::open` has no
per-instance option string (`planning/INTERFACE-GAPS.md` gap 12).

## Dependencies

`vaco-bsf-core` for the driver; `vaco-bitstream` for the raw bit reads
`vp9_superframe` needs.
