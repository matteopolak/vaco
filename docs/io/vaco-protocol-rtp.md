# vaco-protocol-rtp

## What it is

The `rtp:`/`rtcp:` protocol scheme (#551, PR-08). A thin protocol-layer
wrapper around already-framed RTP/RTCP bytes (`vaco-rtp`, layer 1): RFC
5761 §4 multiplexing/demultiplexing (deciding which stream a buffer
belongs to when RTP and RTCP share one transport), and a sans-io sending
session that owns the small piece of per-source state (the sequence
counter) a pure parse/build module has no reason to hold.

**No URL option table.** `vaco-protocol-srt`'s own `options.rs` names the
reason this crate follows: without a `librtp`-carrying `ffmpeg` build to
measure `-h protocol=rtp` against, reconstructing option names (`ttl`,
`buffer_size`, `localrtpport`, ...) from general knowledge would smuggle
implementation-specific detail in under a clean-room-derived label.
Nothing here needed one.

## How it works

- `mux::is_rtcp_packet_type`/`mux::classify` — RFC 5761 §4's rule, applied
  to the **raw second octet** (marker bit included), not a masked 7-bit
  payload type: RTCP packet types occupy 192-223; RTP's second octet
  (`marker<<7 | payload_type`) can only collide with that range if a
  *dynamic* payload type in 64-95 is negotiated with the marker bit set —
  RFC 5761 resolves the ambiguity by directing implementations not to
  negotiate a dynamic PT in that range when muxed, not by any smarter
  inspection. This function implements exactly that convention on the raw
  byte.
- `demux::demux` — classifies and parses one buffer in a single call. The
  RTCP arm returns every packet `vaco_rtp::rtcp::iter_compound` finds
  (RFC 3550 §6.1: RTCP is almost always sent as a compound packet, SR/RR
  first), not just one.
- `session::SendSession` — sequence-number bookkeeping for one SSRC. No
  socket, no clock: the caller supplies the RTP timestamp and reads the
  built bytes back out — the same shape `vaco-protocol-srt`/
  `vaco-protocol-rist` use at this layer. The initial sequence number is a
  caller choice (RFC 3550 §5.1 requires it be random, which is not this
  sans-io crate's concern to generate).

## What is not built (`prompeg`)

`prompeg` (Pro-MPEG Code of Practice #3 FEC) is named in #551's own scope
but **not built**: no D7/D15-clean primary source for the COP3 FEC
header's exact bit layout could be located and cross-checked in the time
this batch allowed. Noted here rather than silently dropped; see #551's
closing comment for the same note.

## Evidence

RFC 5761 states a reserved-range rule, not a worked numeric example, so
`mux`'s tests are draft-derived (checked directly against the range
stated in the RFC text, boundaries included) rather than
RFC-vector-derived. `session`'s tests are self-consistency (this crate's
own packetizer output re-parsed by `vaco_rtp::rtp::RtpPacket`).

## How to change it

A URL option table becomes buildable the day a `librtp`-carrying `ffmpeg`
build is available to measure `-h protocol=rtp` against — do not
reconstruct one from general `rtpproto.c` knowledge before then. `prompeg`
needs a located, citable Pro-MPEG CoP#3 (or SMPTE 2022-1) source before
any FEC header parsing is added to this crate or a sibling.

## Configuration

None yet — no `Protocol`, no `-h protocol=rtp` options (see above).

## Dependencies

`vaco-core`, `vaco-limits`, `vaco-protocol-core` (unused today beyond the
convention of depending on it, matching `vaco-protocol-srt`/
`vaco-protocol-rist` at this pre-`Protocol` stage), `vaco-rtp` (layer 1).

## wasm

Builds cleanly for `wasm32-unknown-unknown` — no socket, no wall clock.

## Fuzzing

`rtp_demux` (30s+ smoke run, no crashes): whole-buffer `demux` over
arbitrary bytes, checking only that it never panics.
