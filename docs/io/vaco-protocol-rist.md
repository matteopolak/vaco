# vaco-protocol-rist

## What it is

RIST (Reliable Internet Stream Transport) Simple and Main Profile:
RTP/RTCP framing (reused from `vaco-rtp`, layer 1), the RIST-specific
RTCP messages Simple Profile adds (bitmask/range retransmission requests,
the optional RTT-echo message), retransmitted-packet matching, the
receiver's two-section reorder/retransmission-reassembly buffer, GRE
tunnelling, DTLS integration, Pre-Shared Key encryption, Annex D
EAP-SHA256-SRP6a authentication, bonding/multi-link support, and a
statistics surface. Like `vaco-protocol-srt`, there is no `rist:`
`Protocol` implementation and no registry entry here: the crate owns no
socket and reads no clock.

## No reference implementation on this machine

No `ffmpeg` build available here carries `librist` (`ffmpeg -protocols` /
`ffmpeg -buildconf` list no `rist` entry). Every fact in this crate comes
from `VSF TR-06-1:2020`, `TR-06-2:2022`, and the corrected
`TR-06-2:2024` Annex D (freely published Technical
Recommendations, CC BY-ND 4.0 — D7/D15-clean the same way SRT's IETF
draft was) rather than a differential check. Tests carry three
evidence-class labels:

- **RFC-vector-derived** — checked against a published RFC's own numeric
  test vectors, genuinely independent of this crate's own code. Lives in
  `vaco-crypto` (RFC 3686's AES-CTR vectors, RFC 7914's PBKDF2-HMAC-SHA256
  vectors); this crate's `gre`/`keepalive`/`psk` modules build on those
  without re-deriving them.
- **draft-derived** — checked against the spec's own worked field
  layouts, tables, and numeric examples: Appendix A's retransmission-
  request scenario (#558), Appendix B's PBKDF2 key-derivation example
  (#559, independently re-derived via Python's stdlib
  `hashlib.pbkdf2_hmac` before being trusted, not merely read off the
  page — see `vaco-crypto`'s own docs). Annex D authentication checks
  every intermediate in the corrected 2024 D.9 example. The 2023
  revision corrected the 2022 D.2 `M1` formula and incorrectly calculated
  D.9 values; do not restore the obsolete 2022 outputs.
- **self-consistency** — this crate's own two sides agreeing (a fake
  sender/receiver pair completing a session, in both directions for
  #559's PSK work; #560's bonded-receiver tests feeding the same
  sequence stream through two links and checking nothing is lost).

## Patent posture (D4)

TR-06-1's IPR notice claims a patent over §4, 5, 5.1 (excl. 5.1.2), 5.2,
5.3 and sub-sections, and 5.4 — essentially all of Simple Profile's
substantive operation — held by Video-Flow Ltd, with an assurance to
license to any implementer who asks.
This crate is not "in the published build" today — no
`vaco-component.toml` fragment, nothing links it into `vaco-cli` — but
**the moment it is registered, the fragment must set `encumbered = true`
behind `patent-encumbered-rist`, `default = false`.**

## How it works

- `rtcp` — the RIST-specific RTCP messages TR-06-1 §5.2/§5.3 add:
  - `RttEcho` — §5.2.6's optional RTT Echo Request/Response, an `APP`
    message (`PT=204`) with `Subtype` 2 (request) or 3 (response), the
    fixed `"RIST"` name field, a 64-bit timestamp echoed verbatim, and a
    32-bit processing-delay field.
  - `GenericNack` — §5.3.2.1's bitmask-based retransmission request, RFC
    4585's own Generic NACK (`FMT=1`, `PT=205`) reused unchanged: a `PID`
    naming one lost packet plus a 16-bit `BLP` bitmask for up to 16
    following packets.
  - `RangeNack` — §5.3.2.2's range-based retransmission request, a
    RIST-specific `APP` message (`Subtype=0`) the spec itself calls an
    interim measure pending an IANA-allocated feedback `FMT`.
  - Plain SR/RR/SDES/BYE need no new types: §5.2.2-§5.2.5 constrain field
    *values* (`RC=0` for an empty RR, exactly one report block for a real
    RR, one CNAME chunk for SDES) rather than the wire shape, so they are
    `vaco_rtp::rtcp::RtcpPacket` used directly.
  - `vaco_rtp::rtcp::RtcpPacket::Other` gained a `count_or_fmt` field
    alongside this crate (`vaco-rtp`'s own 2026-08-28 change) — RIST is
    the first consumer that needs the `APP` `Subtype`/feedback `FMT` bits
    that field carries, which `vaco-rtp` itself has no reason to
    interpret.
- `retransmit` — §5.3.3's SSRC-LSB tag (`Origin::Original`/
  `Origin::Retransmission`) and `flow_id`, the 31 bits that stay the same
  between an original packet and its retransmission.
- `buffer` — §5.3.1's two-section receiver buffer (Figure 1). Sans-io,
  the same shape as `vaco_protocol_srt::arq`: `ReceiveBuffer::on_packet`/
  `on_tick` both take an explicit `now_ms`. A gap is detected the moment
  something arrives *ahead* of the next expected sequence number (Figure
  1's "Packet Loss Detected Here", at the reorder/reassembly boundary);
  the packet that opened it is given up on (`BufferEvent::Dropped`) once
  `total_ms` has passed since — Figure 1's "No Recovery After This
  Point". **A loss with nothing ever arriving after it cannot be
  detected by sequence-number discontinuity at all** — the same
  limitation a real RIST deployment has (there is no way to distinguish
  "the next packet was lost" from "the sender has not sent it yet"
  without something later arriving) — this is stated in the module docs
  and exercised directly by
  `a_permanently_lost_packet_is_eventually_given_up_on_while_the_rest_recover`,
  which relies on the deadline timeout rather than gap detection for that
  one packet.
- `gre` (#559) — the GRE-over-UDP tunnel header (`TR-06-2` §5.1
  `Fig. 1/2`: RFC 8086/2890's `C`/`K`/`S` flags plus the RIST-specific `H`
  (PSK key length) and `RV` (RIST version) bits carved out of
  `Reserved0`), the VSF Packet Header (§5.2 `Fig. 3`) and Reduced Overhead
  Mode's own 4-byte header (§5.3.2 `Fig. 5`). Full Datagram Mode's
  payload (a full layer-3 IP packet) and the Keep-Alive message's JSON
  payload are both carried as opaque bytes on purpose — parsing either
  would need a dependency (an IP-packet parser or a JSON crate) this
  module has no reason to adopt for framing alone.
- `keepalive` (#559) — the Keep-Alive message (§5.6.3/§5.6.4 `Fig. 8`):
  48-bit MAC address plus the thirteen named capability flags (`X` through
  `F`), including `D`/`T` which double as Disconnect/Reconnect signals on
  the same message type (§5.6.5/§5.6.6). The JSON payload is opaque bytes
  — see `gre`'s docs for why.
- `psk` (#559) — §7.1-7.4's Pre-Shared Key encryption. Key derivation
  (§7.3: PBKDF2-HMAC-SHA256, 1024 iterations, the GRE Key/Nonce field's 4
  bytes as salt) and the IV construction (§7.2: the sequence number as
  the counter block's top 4 bytes, 12 zero bytes following) both build
  directly on `vaco-crypto`. §7.4's on-the-fly passphrase rotation and
  §7.6's Future Nonce Announcement message are not built — the rotation
  *policy* is a session concern above this crate's framing layer.
- `eap`, `srp`, and `auth` — `TR-06-2:2024` Annex D's bounded EAPOL wire
  codec, fixed-group SHA256-SRP6a calculations, and client/server sans-I/O
  state machines. Authentication travels as cleartext GRE Protocol Type
  `0x888E`; it is never passed through PSK encryption. The server stores a
  salt and verifier rather than a password, requests use four consecutive
  wrapping identifiers, and retransmissions reuse the exact encoded request
  and ephemeral values. Both peers expose a data gate and a borrowed
  32-byte session key only after mutual proof validation. A failed
  re-authentication closes the existing gate and clears the old key.
- `bonding` (#560) — §5.4 (Simple Profile bonding across raw network
  connections) and §5.5 (Main Profile tunnel-level multi-path over GRE
  paths) are one mechanism at this crate's level of abstraction:
  `BondedReceiver` wraps `buffer::ReceiveBuffer` and adds only what the
  buffer does not already track — which link an arrival came in on
  (`LinkStats::packets_received` per link ID). Deduplication of
  replicated copies needs no new logic: it falls straight out of
  `ReceiveBuffer`'s existing sequence-number keying, per §5.4's own
  requirement that replicated copies "shall have the same RTP sequence
  number and timestamp". Exercised directly against #560's own
  Acceptance Criterion by
  `losing_one_of_two_replicated_links_loses_no_packets` and its symmetric
  counterpart for the other link, plus a combining-mode (split-traffic)
  test.
- `stats` (#560) — a small statistics surface; neither profile names a
  required statistics API, so this is this crate's own choice of what to
  expose, not a spec-mandated shape. `SessionStats::total_accounted_for`
  is **independently-computed** (checked in its own test against a total
  the test derives separately from the packets it feeds in);
  `SessionStats::packets_delivered`/`packets_dropped` are
  **merely-reported** (read straight off `buffer::BufferEvent`) — the
  same distinction `vaco-protocol-srt`'s PR-10c stats module drew.
  `link_reports` surfaces per-link `LinkStats` from a `BondedReceiver`.

## What is not verified

No interop — see above, and #560's interop-matrix clause specifically:
no `librist` build exists on this machine, so it is named unreachable
rather than attempted, with the Acceptance Criterion itself (bonded
two-link survival) built and tested as the replacement bar instead.
§5.1's port-assignment rules (unicast/multicast, NAT firewall
interaction) are socket/deployment concerns with nothing to unit-test at
this layer; documented, not coded. §5.3.4/§5.3.5 (burst control, SSRC
filtering) are explicitly informative in the spec itself ("details...
left to the discretion of the implementer") and are not built as a
result. Appendix B's suggested buffer sizes (1000 ms total, 70 ms reorder
section) are informative defaults this crate carries forward as its own
defaults (`buffer::DEFAULT_TOTAL_MS`/`DEFAULT_REORDER_MS`), not values
the spec requires. Annex D has spec-vector, wire-layout, and internal
client/server evidence, but no external RIST peer is installed, so network
interoperability remains unmeasured.

## How to change it

- Adding the actual `Protocol` implementation (socket ownership, the
  `rist:` scheme, a `vaco-component.toml` fragment) is future work — when
  it lands, the fragment **must** be `encumbered = true` behind
  `patent-encumbered-rist`; see the Patent posture section above.
- `buffer::ReceiveBuffer` currently treats "reorder" and "reassembly" as
  one undifferentiated waiting period (`total_ms`) rather than giving the
  reorder section its own, shorter window ahead of the reassembly
  section's longer one, as Figure 1's two-section picture suggests. That
  split is future work once a concrete deployment's timing needs justify
  the extra complexity — see `buffer.rs`'s own module docs.
- The `always_lose` parameter in `tests/lossy_link.rs::simulate` exists
  specifically to test the give-up path deterministically, separate from
  the random-loss recovery path — keep that separation when extending the
  simulation rather than relying on a lucky PRNG seed.
- Extend authentication messages in `eap.rs`, arithmetic in `srp.rs`, and
  transitions in `auth.rs`. Production arithmetic deliberately accepts
  only Annex D's default 2048-bit group; the weaker D.9 group exists only
  inside the numeric-vector test. Keep retries byte-stable and keep private
  exponents, passwords, and session keys zeroized when adding states.

## Configuration

`AuthenticationConfig` sets packet/identity/password limits, the server
display name, initial wrapping identifier, retry timeout and count,
unknown-identity policy, and whether this peer advertises the derived key for
its outbound PSK passphrase. The client's and server's `U` bits are directional
and are exposed separately for inbound/outbound traffic. Defaults
are 4,096-byte packets, 1,024-byte identities/passwords, 3,000 ms, three
retries, privacy-preserving fake-record work, and no PSK-key request.
Re-authentication has a fixed 60,000 ms minimum interval. One state-machine
value represents one peer; the embedding application must bound its global
peer map.

## Dependencies

`vaco-core`, `vaco-crypto` (layer 0 — AES-CTR and PBKDF2-HMAC-SHA256, not
duplicated), `vaco-hash` (SHA-256), `crypto-bigint 0.7.5` (constant-time
fixed-width Montgomery arithmetic, with only `subtle` and `zeroize`
workspace features), `vaco-limits` (bounded parsing), `vaco-protocol-core`
(`ProtocolError`/`Result`, reused ahead of any `Protocol` impl exactly as
`vaco-protocol-srt` does), `vaco-rtp` (layer 1 — RFC 3550 RTP/RTCP
framing, not duplicated), `vaco-time`.

## wasm

The state machines own no socket or wall clock. Native builds expose
`SystemSecretSource` through `crypto-bigint`'s target-specific `getrandom`
feature; wasm callers inject `SecretSource`, so the common library does not
gain a platform entropy dependency. The OpenSSL-backed `dtls` module is native
only; it was already unusable on `wasm32-unknown-unknown` because its socket and
OpenSSL dependencies do not support that target.
