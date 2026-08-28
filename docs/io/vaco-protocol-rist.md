# vaco-protocol-rist

## What it is

RIST (Reliable Internet Stream Transport) Simple Profile: RTP/RTCP framing
(reused from `vaco-rtp`, layer 1), the RIST-specific RTCP messages Simple
Profile adds (bitmask/range retransmission requests, the optional RTT-echo
message), retransmitted-packet matching, and the receiver's two-section
reorder/retransmission-reassembly buffer. This is PR-11a of epic PR-11/#63
— one of three packages (#558/#559/#560). Like `vaco-protocol-srt`'s
PR-10a, there is no `rist:` `Protocol` implementation and no registry
entry here: no socket, no clock. GRE tunnelling, DTLS/PSK encryption and
authentication (Main Profile, TR-06-2) are #559; bonding, the statistics
surface, and the interop matrix are #560.

## No reference implementation on this machine

No `ffmpeg` build available here carries `librist` (`ffmpeg -protocols` /
`ffmpeg -buildconf` list no `rist` entry). Every fact in this crate comes
from `VSF TR-06-1:2020` (a freely published Technical Recommendation, CC
BY-ND 4.0 — D7/D15-clean the same way SRT's IETF draft was) rather than a
differential check. Tests are labelled the same way `vaco-protocol-srt`'s
are: **draft-derived** (checked against the spec's own worked field
layouts, tables, and Appendix A's numeric retransmission-request example)
or **self-consistency** (this crate's own two sides — a fake sender that
resends on request, a real `ReceiveBuffer` that requests — agreeing with
each other).

## Patent posture (D4)

TR-06-1's IPR notice claims a patent over §4, 5, 5.1 (excl. 5.1.2), 5.2,
5.3 and sub-sections, and 5.4 — essentially all of Simple Profile's
substantive operation — held by Video-Flow Ltd, with an assurance to
license to any implementer who asks (`planning/00-decisions.md` D4's
2026-08-28 amendment: a RAND commitment is not absence of encumbrance).
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

## What is not verified

No interop — see above. §5.1's port-assignment rules (unicast/multicast,
NAT firewall interaction) are socket/deployment concerns with nothing to
unit-test at this layer; documented, not coded. §5.3.4/§5.3.5 (burst
control, SSRC filtering) are explicitly informative in the spec itself
("details... left to the discretion of the implementer") and are not
built as a result. Appendix B's suggested buffer sizes (1000 ms total,
70 ms reorder section) are informative defaults this crate carries
forward as its own defaults (`buffer::DEFAULT_TOTAL_MS`/
`DEFAULT_REORDER_MS`), not values the spec requires.

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

## Configuration

None yet — no `Protocol`, no `-h protocol=rist` options.

## Dependencies

`vaco-core`, `vaco-limits` (bounded parsing), `vaco-protocol-core`
(`ProtocolError`/`Result`, reused ahead of any `Protocol` impl exactly as
`vaco-protocol-srt` does), `vaco-rtp` (layer 1 — RFC 3550 RTP/RTCP
framing, not duplicated), `vaco-time`.

## wasm

Builds cleanly for `wasm32-unknown-unknown` — no socket, no wall clock, no
external crate with a native dependency.
