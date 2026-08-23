# `vaco-mux-rtp`

Layer 4. The RTP packetisers (the registered `rtp` muxer) and `rtp_mpegts`.

## What it is

`RtpMuxer` (`MUXER`, registered name `rtp`): one stream, packetised per
`Packetizer` and written to whatever `vaco_io::MediaSink` the caller already
opened. This crate never opens a socket itself — `MuxerDesc::open`'s
signature is "here is your sink", and whatever opened that sink already went
through `vaco-protocol-core`'s whitelist gate. There is no
transport-security surface in this crate.

## `rtp_mpegts` — a scope decision, not a hidden gap

`ffmpeg`'s `rtp_mpegts` muxer runs a private MPEG-TS muxer internally and
packs its output into RTP. `vaco_format_core::Muxer` has no seam for one
muxer to depend on another's implementation — there is no `MuxerProvider`
the way `ParserProvider` exists on the demux side, and inventing one is out
of this crate's scope (a `vaco-format-core`/`vaco-registry` change this
crate does not own).

**What is actually implemented**: `rtp_mpegts::pack` — the RFC 2250 §2
RTP-framing half, splitting a run of already-muxed, 188-byte-aligned MPEG-TS
packets into MTU-sized RTP payloads (a whole number of TS packets per
payload, never a partial one). `RtpMpegtsMuxer` (`MUXER_RTP_MPEGTS`,
registered name `rtp_mpegts`) is a real, working, registered muxer built on
it — but it expects **already-muxed TS bytes** as its one stream's packet
payload. A caller wanting the same result as `ffmpeg -f rtp_mpegts` runs
`vaco-mux-mpegts` itself and feeds this muxer its output, rather than this
muxer running a nested one.

## Packetiser coverage

`registry::packetizer_for` is the lookup point. Implemented: PCMU, PCMA, L16
and Opus (`raw::RawPacketizer` — no RTP-visible framing, split at MTU
boundaries), H.264 (`h264::H264Packetizer` — RFC 6184 single-NAL/FU-A, no
STAP-A aggregation), AAC/`MPEG4-GENERIC` (`aac::AacPacketizer` — RFC 3640,
one access unit per packet, no fragmentation across packets). Every
packetiser has a round-trip test against its `vaco-format-rtp` depacketiser
counterpart.

## How to change it

* **Adding a packetiser**: implement `Packetizer`, add a match arm in
  `registry::packetizer_for`. Write the round-trip test against the
  matching `vaco_format_rtp::depacket` module first — that is what actually
  catches a framing mismatch, not either half alone.
* **RTCP sender reports**: `vaco_format_rtp::rtcp::build_sr` exists and is
  ready to use, but `RtpMuxer` does not call it — `MuxerDesc::open` hands
  this crate exactly one `MediaSink` (the RTP one), and RTCP needs a second
  connection this trait has no parameter for. A future pass wiring RTCP
  needs either a second sink parameter (an interface change to
  `vaco-format-core`) or a caller-supplied side channel.

## Configuration

None yet — `RtpMuxer`'s MTU (1200 bytes, this crate's own conservative
choice, not an observed reference default) and SSRC/sequence-number seeding
(time-derived, not cryptographically random — this workspace declares no
RNG crate, D10) are not exposed as options. `ffmpeg -h muxer=rtp`'s
`payload_type`/`ssrc`/`cname`/`seq`/`rtpflags` are not implemented.

## Dependencies

`vaco-format-core`, `vaco-format-rtp`, `vaco-codec-core`, `vaco-packet`,
`vaco-bitstream` (Annex-B NAL splitting for the H.264 packetiser),
`vaco-time` (the SSRC/sequence-number seed — never `std::time`, D18).

## wasm

Builds cleanly for `wasm32-unknown-unknown` (`cargo xtask wasm-check`) — no
socket, no wall clock, no external crate with a native dependency.
