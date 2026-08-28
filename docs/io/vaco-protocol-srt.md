# vaco-protocol-srt

## What it is

SRT packet framing, the handshake state machine across all three modes
(caller, listener, rendezvous), and the KM (key material) message *shape*.
This is PR-10a of epic PR-10/#62 — one of three packages (see #555/#556/
#557). Like `vaco-protocol-rtmp`'s own PR-09a, there is no `srt:`/`srts:`
`Protocol` implementation and no registry entry here: no socket, no clock,
no cipher. Those land once `planning/INTERFACE-GAPS.md` gap 28's
worker-thread seam and the AES/CTR ownership question are both settled.

## No reference implementation on this machine

No `ffmpeg` build available here carries `libsrt` (`ffmpeg -protocols`
lists no `srt` entry, only the unrelated `srtp` and a `srt` *subtitle*
format). Every fact in this crate comes from `draft-sharabayko-srt-01` (an
IETF Internet-Draft; freely published, no clean-room blocker) rather than a
differential check.

## How it works

- `packet` — the common 16-byte header (the one `F` bit that decides
  everything else) and the data/control packet framings built on it
  (`draft` §3). `ControlPacket` keeps `subtype_or_reserved`/`type_specific`
  raw rather than reinterpreting them per control type — that
  reinterpretation (ACK's own Acknowledgement Number, DropReq's Message
  Number, etc.) is #556/#557's job, not this package's; this package only
  needs every control packet to parse and re-serialize without loss.
- `handshake` — the Handshake control packet's fixed 48-byte CIF, its
  extension-block walker, the HSREQ/HSRSP body, and the numeric constants
  for handshake type, encryption field, extension type and rejection
  reason (`draft` §3.2.1, Tables 4-7).
- `km` — the Key Material message *shape* (`draft` §3.2.2): every field
  including the wrapped-key blob is parsed and re-serialized, but the blob
  itself is never unwrapped — see the module's own docs for the deferred
  AES-CTR ownership question.
- `cookie` — the rendezvous cookie: MD5 (via `vaco-hash`, D11 — no second
  `md-5` dependency) over a locally-chosen preimage, and the strictly-
  greater-wins contest rule. The exact preimage byte layout is this
  module's own documented choice, not something the fetched draft text
  specifies or that a reference peer could confirm here — see that
  module's docs.
- `session` — the three handshake state machines, sans-io: each takes "a
  packet arrived" plus a caller-supplied timestamp and returns "send these
  bytes" / "connected" / "rejected". `RendezvousHandshake` is one type
  driven by both peers (unlike `CallerHandshake`/`ListenerHandshake`,
  which are different types) — a genuinely different state machine from
  caller/listener, not one with a flag flipped, matching how `draft` §4.3.2
  describes it.
- `ack` (PR-10b / #556) — the ACK/NAK CIF shapes (`draft` §3.2.4/§3.2.5).
  Field layout is draft-derived; the ACK stats fields (RTT/RTTVar/the
  three rate estimates) have no stated formula and are always zero
  (`AckStats::placeholder`) rather than a guessed smoothing constant. NAK
  range compression is not implemented — every NAK this crate builds names
  each lost sequence number as its own single-entry word; see the module's
  own docs for why.
- `arq` (PR-10b / #556) — `SendWindow` (retransmission buffer, NAK- and
  RTO-triggered resend) and `ReceiveWindow` (loss detection, in-order
  delivery, TSBPD-ish too-late drop), sans-io via an explicit
  `on_tick(now_ms)` rather than owning a socket or clock
  (`planning/INTERFACE-GAPS.md` gap 28's addendum — the standard
  sans-io answer for a timer-driven protocol, the same shape QUIC
  implementations use, rather than forcing the worker-thread seam into
  existence here). Every constant this module needed a number for and
  could not get from the draft (RTO, the too-late-drop latency threshold,
  NAK re-announcement policy) is marked `IMPLEMENTATION-DEFINED` at its
  declaration with its own reasoning. **No congestion control or rate
  limiting is implemented**: `draft` §5.1/§5.2 name LiveCC/FileCC but do
  not give their algorithms in the fetched text — checked directly, across
  two independently-worded fetches that agreed, before concluding there is
  no specification and no instrument for this at all, not merely an
  unreachable reference.

## How to change it

Wiring this to a real socket (a later package, once gap 28's seam is
built): a worker thread owns the raw UDP socket
(`vaco-protocol-socket::udp`'s existing datagram-boundary-preserving
primitives) and drives one of these three state machines, then the actual
data-transfer/ARQ/congestion-control loop (`#556`) once `HandshakeOutcome::Connected`
arrives. `session.rs`'s `HandshakeParams`/`ConnectedInfo` are the seam
between this package and that one.

Adding the cipher (once the D11 ownership question resolves): `km::KmMessage`
already has every field the unwrap needs (`cipher`, `salt`, `wrapped_key`,
`key_flags`) — the unwrap itself is an RFC 3394 AES key-unwrap over
`wrapped_key`, keyed by a KEK derived from the passphrase (not yet
represented anywhere in this crate; the passphrase itself is a `Protocol`
option, `#557`'s job).

## Configuration

None — no `Protocol`, no `-h protocol=srt` options (yet).

## Dependencies

`vaco-core`, `vaco-limits` (`Budget`-bounded parsing), `vaco-protocol-core`
(`ProtocolError`/`Result`, reused ahead of any `Protocol` impl, exactly as
`vaco-protocol-rtmp` does), `vaco-time` (present for a later package's
clock; not yet called from this one — every timestamp in this crate's own
API is caller-supplied, per its sans-io design), `vaco-hash` (MD5 for the
rendezvous cookie — an internal, multi-consumer Vaco crate, not a second
external `md-5` dependency, D11). No crypto crate dependency: `aes`/`ctr`
ownership is deferred (`planning/INTERFACE-GAPS.md` gap 28's crypto-
ownership note) rather than claimed directly or hand-rolled.

## Testing and what is unverified against a real peer

Every module has unit tests, split explicitly by evidence class in each
test's own doc comment: **draft-derived** (checked against
`draft-sharabayko-srt-01`'s own worked field layouts and numeric tables,
independent of this crate's encoder — e.g. `packet::tests::
data_packet_matches_the_drafts_own_field_layout`, every numeric-table test
in `handshake`, `km::tests::km_message_matches_the_drafts_own_field_layout`)
versus **self-consistency** (this crate's own encoder and decoder agree —
every `proptest` round-trip, `cookie`'s determinism tests, and both
`tests/loopback.rs` simulations). Self-consistency is real evidence of
internal consistency, not evidence of matching the draft: a shared
misreading passes both sides of a round trip identically, which is why the
draft-derived tests exist independently rather than being inferred from
round-trip success.

One fuzz target, `srt_packet`, covers `packet::SrtPacket::parse`,
`handshake::HandshakeCif::parse`/`parse_extensions`, and `km::KmMessage::parse`
— every parser here that reads attacker-controlled length fields ahead of a
socket existing. A multi-hour background run replaced the original "24h
against a reference peer" Acc (permanently unreachable here, no `ffmpeg`
build on this machine carries `libsrt`) with an explicit substitute bar;
see the issue thread for the actual number of hours/executions reached and
`fuzz/artifacts`' state at that point, rather than restating a number here
that would go stale the moment a longer run replaces it.

`tests/lossy_link.rs` is `arq`'s own replacement-bar evidence for #556's
"recovers to the same delivered-packet set as the reference peer": a
deterministic (fixed-seed `xorshift32`, no new dependency) simulated lossy
link between this crate's own `SendWindow`/`ReceiveWindow` pair at 5% and
20% loss, checking that every packet is either delivered — in strictly
increasing order, with the exact payload sent — or explicitly recorded as
dropped past the latency window, never silently lost. **Self-consistency,
with a bounded weakness stated in the test's own docs**: both sides are
this crate's own code, so a shared misreading would pass identically, the
same weakness the handshake loopback test has — bounded here because
*functional* delivery under loss is close to a property of the pair's own
internal consistency, which is what this test actually checks, not
interop or the IMPLEMENTATION-DEFINED constants' real-world tuning.

**What is not verified, and cannot be from here**:

- **Nothing in this crate has completed a handshake against a real SRT
  peer.** No `libsrt`-carrying `ffmpeg` build, and no other SRT
  implementation, is reachable in this environment. Every test either
  round-trips this crate's own encoder against its own decoder, checks a
  hand-built byte sequence against the fetched draft text directly, or (the
  loopback tests) runs this crate's own two handshake implementations
  against each other.
- **The rendezvous cookie's exact preimage byte layout** is this crate's
  own documented choice (see `cookie.rs`), not something `draft-
  sharabayko-srt-01`'s own text (as fetched) specifies precisely enough to
  reproduce a real peer's cookie value. A real peer computing a different
  cookie for the same host/port/minute would still resolve the same
  Initiator/Responder role only by coincidence.
- **The rendezvous state machine's retry/timeout/re-WAVEAHAND behaviour**
  under packet loss or near-simultaneous starts is not implemented — only
  the single successful pass through Waving → Conclusion → Agreement.
  Flagged explicitly as the highest-risk area of this package, per this
  dispatch's own instruction, since there is no reference to catch a subtle
  divergence here.
- **HSREQ/HSRSP's `SRT Flags`/TSBPD delay values this crate sends** are
  this crate's own placeholders (`SRT_VERSION`, fixed 120ms delays), not
  values a real deployment has confirmed acceptable.
- **`arq`'s `IMPLEMENTATION-DEFINED` constants** (`DEFAULT_RTO_MS`,
  `DEFAULT_LATENCY_MS`, the every-tick NAK re-announcement policy) are not
  tuned against anything — no formula for any of them exists in the
  fetched draft text (checked directly, across two independently-worded
  fetches that agreed), and there is no reference peer's own behaviour to
  measure them against either. `tests/lossy_link.rs` confirms this crate's
  own ARQ recovers from loss with *these* constants; it says nothing about
  whether they would perform well, or even correctly interoperate, against
  a real SRT peer's own timing.
- **No congestion control or rate limiting**: not implemented, not
  attempted. `draft` §5.1/§5.2 name LiveCC/FileCC without giving their
  algorithms in the fetched text — the same "no specification, no
  instrument" conclusion as WHIP's DTLS gap, reached the same way (checked
  directly before building, not assumed), and named rather than
  approximated with an invented rate-control formula.
