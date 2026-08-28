# `vaco-format-rtp`

Layer 4. The RFC 3551 static payload-type table, the RTP depacketisers, and
the SDP parser. `rtp`/`rtcp` are re-exported unchanged from `vaco-rtp`
(layer 1) — see below.

## What it is

Everything `vaco-demux-rtsp` and `vaco-mux-rtp` both need and neither should
duplicate:

* `rtp` — RFC 3550 §5.1 header parse/build (`RtpPacket`, `RtpHeader`).
  **Moved to `vaco-rtp` on 2026-08-28** and re-exported here as
  `pub use vaco_rtp::rtp;` — nothing that already wrote `vaco_format_rtp::rtp`
  or `vaco_format_rtp::RtpPacket` needed to change. It moved because RIST's
  Simple Profile (VSF TR-06-1, `vaco-protocol-rist`, layer 2) needs the same
  RFC 3550 framing, and `xtask/src/layers.rs` forbids a layer-2 crate
  depending on a layer-4 one. See `docs/model/vaco-rtp.md`.
* `rtcp` — RFC 3550 §6 sender/receiver reports, `SDES`, `BYE` (`RtcpPacket`,
  `build_sr`/`build_rr`/`build_bye`). Moved to `vaco-rtp` at the same time,
  for the same reason, re-exported the same way.
* `payload` — the RFC 3551 static payload-type table (`STATIC_PAYLOADS`, 35
  rows including the RFC's own reserved/unassigned entries).
* `sdp` — RFC 4566 session descriptions (`SessionDescription`,
  `MediaDescription`, `parse`).
* `depacket` — the RTP depacketisers. See `src/depacket/mod.rs`'s module
  docs for the full, exact count and why it is not 26 — that comment is the
  source of truth and this file does not repeat numbers that can drift.

## How it works

Every parser here takes attacker-controlled bytes (an RTSP server's own
packets) and returns `Result` rather than indexing blindly — see
`rtp.rs`/`rtcp.rs`'s module docs for the specific "15 CSRCs in a 12-byte
buffer" shape this is written against. `Depacketizer::push` is the one
trait every codec module implements: feed it one RTP payload (plus the
marker bit and timestamp), get back `Ok(Some(bytes))` when a complete
access unit is ready.

`depacket::registry::for_encoding` is the single lookup point
`vaco-demux-rtsp` calls to turn an SDP `a=rtpmap` encoding name into a
`(CodecId, DepacketizerFactory)` pair.

## How to change it

* **Adding a depacketiser**: add a module under `src/depacket/`, add a match
  arm in `registry::for_encoding`, and update the count/table in
  `depacket/mod.rs`'s module docs — that comment is read as ground truth by
  anyone auditing the count, so keep it in sync with `for_encoding`.
* **A missing `CodecId`**: `vaco-codec-core::CodecId` is `#[non_exhaustive]`
  and only that crate may add a variant. `depacket/mod.rs`'s "Missing
  `CodecId` variants" table lists exactly what is blocked and why (GSM,
  G722, G728, G729, QCELP, DVI4, CelB, DV, iLBC).
* **JPEG's default tables** (`depacket/jpeg.rs`): transcribed from RFC 2435
  Appendices A/B, structurally tested (Huffman code-length counts sum to the
  value-table length) but **not verified byte-for-byte against a real JPEG
  decoder's output** — flagged there and here rather than claimed more
  solid than it is.

## Configuration

None — this crate has no `-h`-style option surface of its own; every option
that affects RTP/SDP behaviour (transport mode, port range, timeouts) lives
in `vaco-demux-rtsp::RtspOptions`.

## Dependencies

`vaco-core`, `vaco-limits`, `vaco-packet`, `vaco-codec-core`, `vaco-rtp`
(the RTP/RTCP wire format, layer 1) — no protocol or I/O crate. This crate
never opens a socket; see `vaco-demux-rtsp`'s docs for where the
transport-security boundary actually lives.

## wasm

Builds cleanly for `wasm32-unknown-unknown` (`cargo xtask wasm-check`) — no
socket, no wall clock, no external crate with a native dependency.
