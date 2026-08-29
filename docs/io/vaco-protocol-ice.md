# `vaco-protocol-ice`

Layer 2. STUN (RFC 5389) message codec and a minimal ICE (RFC 8445)
connectivity check, built for #619 (WHIP).

## What it is

Not a `Protocol` registration (no `ice:`/`stun:` URL scheme exists) — a
plain library a negotiating muxer like `vaco-mux-whip` links directly. It
covers exactly what a WebRTC-shaped publishing client needs:

- Building and parsing one STUN Binding transaction with the short-term
  credential attributes ICE uses (`USERNAME`, `MESSAGE-INTEGRITY`,
  `FINGERPRINT`, `PRIORITY`, `ICE-CONTROLLING`, `USE-CANDIDATE`).
- Running our own connectivity check against a candidate (`connectivity_check`).
- Answering a Binding Request a peer sends *to* us (`respond_to_binding_request`).

No TURN, no candidate-pair state machine, no long-term credentials.

## How it works

Everything lives in `src/lib.rs` (small enough that a module split would
cost more than it clarified): message encode/decode (`encode`/`parse`),
`MESSAGE-INTEGRITY`/`FINGERPRINT` computation (`verify_integrity`, both
covered internally by `encode`), the outbound check (`connectivity_check`),
and the inbound responder (`respond_to_binding_request` plus the RFC 7983
demux helper `looks_like_stun`).

## The two-directional design, and why it exists

A first implementation only built the outbound check, on the assumption
that a WHIP media server runs ICE-lite (RFC 8445 §2.2: never issues its own
checks, only answers ours). That assumption was measured, not verified —
and it was wrong for a real peer: `mediamtx` 1.20.1 runs a full ICE agent
and keeps sending Binding Requests to the publisher throughout the DTLS
handshake window. Without `respond_to_binding_request`, those requests just
piled up unanswered and the peer's own connectivity never confirmed, so
the DTLS handshake `vaco-mux-whip` was driving over the same socket never
received anything back — silence, not an error. See
`docs/format/vaco-mux-whip.md` for the full trace.

## How to change it

- Adding a new STUN attribute: extend the `ATTR_*` constants and the
  `encode`/`parse` pair. `encode` takes untyped `(u16, &[u8])` pairs
  deliberately — this crate only ever builds one message shape, so a richer
  attribute enum would not pay for itself yet.
- The retry/timeout shape in `connectivity_check` is a fixed count and
  fixed per-try timeout, not RFC 5389 §7.2.1's real `RTO` backoff — every
  peer measured so far is loopback/LAN, where the difference does not show.
  Widen this if a WAN deployment needs it.
- `pseudo_random_bytes`/`ice_credential` are a from-scratch splitmix64-based
  generator, not cryptographically secure and not meant to be — this
  workspace declares no RNG crate (D10) and STUN's security comes from the
  exchanged password, not from unpredictable transaction ids.

## Configuration

None — no `-h protocol=` surface, since this is not a registered protocol.
A caller supplies `IceCredentials` and timeouts directly.

## Dependencies

`vaco-core` (errors), `vaco-crypto` (`hmac_sha1`, for `MESSAGE-INTEGRITY`),
`vaco-hash` (`crc32`, for `FINGERPRINT`), `vaco-time` (seeding). No `unsafe`,
no new external crate.
