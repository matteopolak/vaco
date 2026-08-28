# vaco-protocol-sctp

## What it is

SCTP (RFC 4960), issue #561 (PR-12a). Framing (`packet`, `chunk`) and a
sans-io association state machine (`association`) for RFC 4960's
four-way handshake and basic reliable data transfer. Same "framing/state
library, no `Protocol` yet" stage `vaco-protocol-srt`/`vaco-protocol-rist`/
`vaco-protocol-rtmp` all started at.

## Scope, stated up front

- Twelve chunk types are built (`DATA`/`INIT`/`INIT ACK`/`SACK`/
  `HEARTBEAT`/`HEARTBEAT ACK`/`ABORT`/`SHUTDOWN`/`SHUTDOWN ACK`/`ERROR`/
  `COOKIE ECHO`/`COOKIE ACK`/`SHUTDOWN COMPLETE`) — their fixed fields,
  not their optional variable-length parameters, except INIT ACK's
  mandatory State Cookie (the handshake cannot work without it).
- `Association` drives the four-way handshake and cumulative-TSN-only
  `DATA`/`SACK` acknowledgement — no gap-ack-block tracking for
  out-of-order arrivals, even though `SackChunk` itself can carry them.
- **Not built:** multi-homing, PR-SCTP partial reliability, congestion
  control, an authenticated state cookie (§5.1.3's own recommendation is
  an HMAC-authenticated cookie so an attacker cannot replay/forge one to
  exhaust server resources — this crate's cookie is unauthenticated peer
  tag/TSN bytes, a real production gap, named rather than hidden), and
  the shutdown sequence's timers/retransmission.

## No reference peer, and no 24-hour fuzz run, from here

#561's own Acceptance Criterion ("a session round-trips against a
reference peer and the fuzz target is green for 24 h") names two things
unreachable from this environment: no SCTP-speaking reference peer is
installed here, and this batch has no 24-hour window to run a fuzz target
in. Both are named rather than silently skipped, per the owner's own
ruling on replacement bars (`planning/AGENT-CONSTRAINTS.md`, `705779d`):
demonstrably-not-broken, structurally-correct, deviation-named is the
bar. The substitute actually built: `association::tests::
four_way_handshake_reaches_established_on_both_sides` and
`data_sent_after_the_handshake_is_received_and_acknowledged` drive two
`Association`s (client and server, this crate's own code on both sides)
through the full handshake to `Established`, then a `DATA`/`SACK`
exchange. The fuzz target (`protocol_sctp_parse`) ran clean for a short
smoke window instead of 24 hours — disclosed as exactly that.

## How it works

- `packet` — RFC 4960 §3.1's 12-byte common header (source/destination
  port, verification tag, checksum) and Appendix B's CRC32c checksum
  (`vaco_hash::crc32c`, D11 — not a new `crc` dependency here).
  `compute_checksum`/`build_with_checksum`/`verify_checksum` implement
  §6.8's own procedure: zero the checksum field, CRC32c the whole packet,
  write the result back.
- `chunk` — the generic chunk header (§3.2: type/flags/length) plus the
  twelve typed chunks above. `pad_to_4` implements §3.2's own
  padding-not-counted-in-length rule; `parse_one` returns how many bytes
  were consumed *including* padding, so a caller can walk a packet's
  whole chunk area in a loop. An unrecognised chunk type is preserved as
  `Chunk::Unknown` rather than rejected, per §3.2's own handling
  requirement.
- `association` — `Association::new_client`/`new_server`, `initiate`
  (builds `INIT`, enters `CookieWait`), `on_packet` (drives the state
  machine, returns zero or more packets to send back), `send_data`/
  `received_data` for the `Established` data path. The state cookie
  carries the two verification tags and two initial TSNs so the server
  side of `on_packet` can be a plain match on `(role, state, chunk)`
  rather than needing separate stored half-open-association state.

## How to change it

Gap-ack-block tracking for out-of-order `DATA` arrivals would extend
`ReceiveState` (currently cumulative-TSN-only) with a small set of
received-but-not-yet-contiguous TSNs, folded into the cumulative ack as
gaps close — `SackChunk::gap_ack_blocks` already exists on the wire
format and needs no format change, only an `association.rs` change.
Cookie authentication (an HMAC over the tag/TSN fields, verified before
trusting a `COOKIE ECHO`) is the other concrete next step named above.

## Configuration

None yet — no `Protocol`, no `-h protocol=sctp` options.

## Dependencies

`vaco-core`, `vaco-hash` (CRC32c, not duplicated), `vaco-limits`,
`vaco-protocol-core`.

## wasm

Builds cleanly for `wasm32-unknown-unknown` — no socket, no wall clock.

## Fuzzing

`protocol_sctp_parse`: whole-buffer `CommonHeader::parse` +
`chunk::parse_one` looped over the chunk area, plus a live server-role
`Association::on_packet` call on the same bytes (so a fuzzer-found input
that reaches deep into the handshake state machine is exercised, not just
`Closed`). Short smoke run only — see "No reference peer" above for why
this is not the 24-hour run #561 names.
