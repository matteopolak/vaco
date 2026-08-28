# vaco-crypto

## What it is

The single owner of `aes`, `ctr`, `hmac` and `pbkdf2` (**D11**). `cbc` stays
with `vaco-protocol-crypto` — it is still the only crate doing CBC mode, so
there is nothing to share yet.

Two independent consumers made this load-bearing rather than speculative:
`vaco-protocol-crypto`'s AES-128-CBC (which needs the raw `Aes128` block
cipher) and `vaco-protocol-rist`'s PSK encryption (AES-CTR, keyed by a
PBKDF2-HMAC-SHA256-derived key). Before this crate, `aes` was declared
directly by `vaco-protocol-crypto` alone — fine with one consumer, not fine
with two, the same threshold `vaco-hash` and `vaco-rtp` were extracted at.

## Why PBKDF2 lives here, not in `vaco-hash`

`vaco-hash`'s scope is justified by "the checksum **is** the printed
output" — a *visible*-wrong-answer risk class. PBKDF2 does not share it: a
subtly wrong KDF produces a validly-shaped key that silently decrypts to
garbage, the same "decrypts to different bytes, full stop" class already
used to rule out hand-rolling AES. That risk class belongs with the cipher,
not the checksum crate — D19 cuts on risk class here, not on "these are all
hash-adjacent".

SHA-256 the *primitive* stays owned by `vaco-hash` unchanged; `vaco-hash`
re-exports `sha2` (`pub use sha2;`) specifically so this crate can build
`Hmac<Sha256>` for [`pbkdf2_hmac_sha256`] without declaring a second direct
`sha2` dependency — one D11 claim on `sha2`, composed from two crates.

## How it works

- `aes` (re-exported whole) — `vaco-protocol-crypto`'s CBC code reaches
  `Aes128` through `vaco_crypto::aes::Aes128` rather than a direct `aes`
  dependency; nothing about its own measured-against-`ffmpeg` behaviour
  changed, only the import path.
- `ctr_apply_aes128`/`_aes192`/`_aes256` — AES-CTR keystream
  generation/application (XOR, so encrypt and decrypt are the same call).
  Takes the 128-bit initial counter block as a plain `[u8; 16]` and
  increments the *whole* 128 bits per block (`ctr::Ctr128BE`, textbook
  CTR) — building that counter block from a nonce or sequence number is
  each protocol's own concern (RFC 3686's nonce‖IV‖32-bit-counter split for
  IPsec; `VSF TR-06-2` §7.2's sequence-number-in-the-high-4-bytes rule for
  RIST — the two schemes only diverge after 2^32 blocks, unreachable by
  either protocol's own packets).
- `pbkdf2_hmac_sha256` — PBKDF2-HMAC-SHA256 (RFC 8018 §5.2's algorithm).

## Evidence

`ctr_apply_*`'s tests are RFC-vector-derived: all nine of RFC 3686 §6's own
key/counter-block/plaintext/ciphertext triples (all three AES key sizes) —
genuinely independent evidence, not this crate's own encoder checked
against its own decoder.

`pbkdf2_hmac_sha256`'s tests are two-layered. RFC 8018 itself has **no**
test vectors — checked directly against the fetched RFC text (its table of
contents lists only Appendices A–E, none titled "Test Vectors"; a full-text
search for "Test Vector" and for "6070" both return zero matches), not
assumed from the RFC's reputation. RFC 7914 (`scrypt`) §11 gives genuine
PBKDF2-HMAC-SHA256 vectors instead (algorithm-level, not RIST-specific).
`VSF TR-06-2` Annex B's own worked passphrase/nonce example is also used —
independently re-derived via Python's stdlib `hashlib.pbkdf2_hmac` with the
same inputs before being trusted as a test's expected value, not merely
read off the spec's rendered page.

## How to change it

Adding a cipher mode or a KDF that a second real consumer needs goes here.
Adding `cbc` here too is legitimate the day a second CBC consumer appears
(it currently has exactly one, `vaco-protocol-crypto`) — until then it
stays where it is; moving it here without a second consumer would be
speculative, not load-bearing.

## Configuration

None.

## Dependencies

`vaco-core`, `vaco-hash` (for the `sha2::Sha256` re-export), `aes`, `ctr`,
`hmac`, `pbkdf2`.

## wasm

Builds cleanly for `wasm32-unknown-unknown` — pure Rust, no native
dependency.
