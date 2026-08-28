# vaco-protocol-srtp

## What it is

SRTP (RFC 3711), issue #551 (PR-08). RTP payload encryption and
authentication: §4.3.1's key derivation (`kdf`), §4.1.1's per-packet
keystream IV construction and §4.2's authentication tag (`session`), and
a small `session::SrtpContext` tying both to a rollover counter for one
SSRC. Built entirely on `vaco-crypto`'s AES-CTR; `vaco_crypto::hmac_sha1`
was added alongside this crate rather than wiring a second `hmac`
dependency here (D11).

## Scope, stated up front

- **SRTP (media) only — not SRTCP.** The three SRTCP key-derivation
  labels and SRTCP's own packet-index/`E`-bit framing are not built.
- **`key_derivation_rate = 0` only** (derive once — the common real-world
  configuration). Periodic re-derivation is not built.
- **`AES_CM_128_HMAC_SHA1_80` is the profile wired end-to-end** through
  `SrtpContext::protect`/`unprotect`. AES-256 key derivation exists
  (`kdf::derive_session_keys_aes256`) but the encrypt/decrypt path is not
  yet generic over key size.
- **`RolloverTracker` implements RFC 3711 Appendix A.3's simple rule**
  ("a high-to-low sequence jump means the counter wrapped"), not Appendix
  A's fuller out-of-order-across-a-rollover guessing algorithm.

## How it works

- `kdf::derive_session_keys_aes128`/`_aes256` — §4.3.1's KDF: build a
  16-byte AES-CTR counter block by XORing a one-byte label
  (`0x00`/`0x01`/`0x02` for encryption/authentication/salting keys) into
  the most significant octet of the (zero-padded) master salt, then take
  AES-CTR keystream bytes from that block as the derived key material.
- `session::build_iv` — §4.1.1's `IV = (salt * 2^16) XOR (SSRC * 2^64)
  XOR (index * 2^16)`, implemented as explicit byte offsets into a
  16-byte big-endian block (salt in bytes 0-13, SSRC XORed into bytes
  4-7, the 48-bit packet index XORed into bytes 8-13).
- `session::compute_auth_tag` — §4.2: HMAC-SHA1 over the packet (up to
  but not including the tag) with the 4-byte ROC appended (the ROC is
  never transmitted on the wire — this is exactly why a receiver has to
  track it out of band), truncated to the requested tag length.
- `session::RolloverTracker` — the ROC-wrap detection described above,
  producing the 48-bit packet index `protect`/`unprotect` need.
- `session::SrtpContext` — `protect` encrypts the payload region in
  place (the header itself is authenticated but never encrypted, §4.1)
  and appends the tag; `unprotect` verifies the tag with a
  non-short-circuiting byte comparison before decrypting, returning
  `None` rather than plaintext on any authentication failure (including a
  single flipped ciphertext bit, which normal CTR-mode malleability would
  otherwise let through silently without the tag check).

## No reference peer on this machine

No `openssl`/`libsrtp`-backed peer was available to interoperate against
in this batch, so every fact comes from RFC 3711's own text (freely
published IETF RFC, D7/D15-clean) rather than a differential check.

## Evidence

RFC 3711 publishes no numeric test vectors of its own (checked directly
against the fetched RFC text). `kdf`'s tests are therefore self-consistency
plus draft-derived field-layout checks (the label byte's position, not an
independent numeric answer), and so are `session`'s `protect`/`unprotect`
round-trip and tamper-rejection tests. `vaco_crypto::hmac_sha1` itself,
underneath both, *is* RFC-vector-derived (RFC 2202's own HMAC-SHA1 test
cases, cross-checked against Python's stdlib `hmac`/`hashlib`).

## How to change it

SRTCP support would add three more KDF labels (`0x03`-`0x05`) and its own
packet-index/`E`-bit framing — a real second unit of work, not a small
extension of this crate's existing `protect`/`unprotect`. Generalising
`SrtpContext` over AES-256 needs `apply_keystream` to dispatch on
`cipher_key.len()` rather than hard-assuming 16 bytes — see that
function's own comment. `RolloverTracker`'s simple wrap rule would need
replacing with RFC 3711 Appendix A's full algorithm before this crate
could handle heavy reordering across a rollover boundary correctly.

## Configuration

None yet — no `Protocol`, no `-h protocol=srtp` options (same pre-registry
stage as `vaco-protocol-rtp`/`vaco-protocol-srt`/`vaco-protocol-rist`).

## Dependencies

`vaco-core`, `vaco-crypto` (layer 0 — AES-CTR and HMAC-SHA1, not
duplicated), `vaco-limits`, `vaco-protocol-core`, `vaco-rtp`.

## wasm

Builds cleanly for `wasm32-unknown-unknown` — pure Rust, no native
dependency.

## Fuzzing

`srtp_unprotect` (20s+ smoke run, no crashes): a fixed fuzzer-owned key
derives a real `SrtpContext`, then arbitrary bytes are fed to `unprotect`
as if they had arrived off the network. Property: never panics, on any
input.
