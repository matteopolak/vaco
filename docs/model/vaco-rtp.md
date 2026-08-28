# `vaco-rtp`

Layer 1. The RTP/RTCP wire-format model — RFC 3550 header parsing and
building, with nothing above it: no SDP, no payload-type table, no
depacketisers.

## What it is

* `rtp` — RFC 3550 §5.1 header parse/build (`RtpPacket`, `RtpHeader`,
  `RTP_VERSION`).
* `rtcp` — RFC 3550 §6 sender/receiver reports, `SDES`, `BYE` (`RtcpPacket`,
  `ReportBlock`, `SenderInfo`, `SdesItem`, plus compound-packet iteration).

## How it works

Every parser takes attacker-controlled bytes and returns `Result` rather
than indexing blindly — an RTP packet claiming 15 CSRCs in a 12-byte buffer
is refused, not indexed into; an RTCP report-block count that would overrun
the buffer is refused the same way. See `rtp.rs`/`rtcp.rs`'s own module
docs for the exact shapes these are written against.

## Why this crate exists (2026-08-28 extraction)

Originally `rtp.rs`/`rtcp.rs` lived in `vaco-format-rtp` (layer 4), which
was fine while the only consumers were format-layer crates
(`vaco-demux-rtsp`, `vaco-mux-rtp`). It stopped being fine once
`vaco-protocol-rist` (layer 2, epic #63) needed the same framing — RIST's
Simple Profile (VSF TR-06-1) *is* RTP/RTCP on the wire — and
`xtask/src/layers.rs`'s `layer-check` forbids a layer-2 crate depending on a
layer-4 one (every edge must point downward). Reimplementing the same
RFC 3550 structs a second time inside the protocol crate was rejected:
`dup-check` (D19, one definition per concept) would have forced either a
name collision, a renamed-but-identical duplicate, or a `DISTINCT`
exception hiding real duplication rather than resolving it.

So the wire-format types moved down to their own layer-1 crate — the same
shape as the `vaco-hash` extraction (one owner, at the lowest layer that
needs it, everyone else depends downward). The move was a pure relocation:
both modules only ever depended on `vaco-core::{Error, Result}`, nothing
`vaco-format-rtp`-specific, so nothing downstream (`vaco-demux-rtsp`,
`vaco-mux-rtp`) had to change — `vaco-format-rtp` re-exports `rtp`/`rtcp`
from here unchanged (`pub use vaco_rtp::{rtp, rtcp};`).

## How to change it

Both modules are self-contained; the only external dependency is
`vaco-core`. Extending the RTP header extension parsing or adding an RTCP
packet type (e.g. `APP`, needed by RIST's RTT-echo mechanism) belongs here,
not in `vaco-format-rtp` or `vaco-protocol-rist` — this is the one place
either crate should read RFC 3550 framing from.

## Configuration

None.

## Dependencies

`vaco-core` only.

## wasm

Builds cleanly for `wasm32-unknown-unknown` — no socket, no wall clock, no
external crate with a native dependency.
