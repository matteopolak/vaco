//! Symmetric ciphers and key derivation, behind one door.
//!
//! # What it is
//!
//! The single owner of `aes`, `ctr`, `hmac` and `pbkdf2` (**D11**). `cbc`
//! stays with `vaco-protocol-crypto` — it is still the only crate doing CBC
//! mode, so there is nothing to share yet; `aes` moved here because a
//! second consumer appeared (`vaco-protocol-rist`'s PSK encryption, AES-CTR)
//! alongside the first (`vaco-protocol-crypto`'s AES-128-CBC), the same
//! two-independent-consumers threshold that made `vaco-hash` and `vaco-rtp`
//! load-bearing rather than speculative.
//!
//! `sha2` is *not* claimed here — `vaco-hash` already owns it, and
//! [`pbkdf2_hmac_sha256`] builds its `Hmac<Sha256>` on
//! [`vaco_hash::sha2::Sha256`] (re-exported by that crate specifically for
//! this) rather than declaring a second direct `sha2` dependency. SHA-256
//! the *primitive* stays where `vaco-hash`'s own "checksum is the printed
//! output" scope already puts it; PBKDF2 the *key-derivation algorithm*
//! sits here instead, because it shares this crate's risk class, not
//! `vaco-hash`'s: a subtly wrong KDF produces a validly-shaped key that
//! silently decrypts to garbage, the same "decrypts to different bytes,
//! full stop" class already used to rule out hand-rolling AES — not
//! `vaco-hash`'s "the checksum IS the printed output" (visibly wrong)
//! class. D19 cuts on risk class here, not on "these are all hash-shaped".
//!
//! **Do not hand-roll any of this.** `aes`/`ctr`/`hmac`/`pbkdf2` are the
//! `RustCrypto` crates, pure Rust, D10-clean.
//!
//! # How it works
//!
//! - [`aes`] — re-exported whole, so a downstream crate can name concrete
//!   types (`aes::Aes128`, `aes::Aes256`) the way `vaco-protocol-crypto`'s
//!   own CBC code (measured against `ffmpeg 8.1`) does.
//! - [`ctr_apply_aes128`]/[`ctr_apply_aes192`]/[`ctr_apply_aes256`] — AES-CTR keystream generation/application
//!   (XOR, so encrypt and decrypt are the same operation). This module owns
//!   only the generic primitive: a 128-bit initial counter block, a key,
//!   and a buffer. How a protocol constructs that counter block (RFC
//!   3686's nonce/IV/counter split, or `VSF TR-06-2` §7.2's
//!   sequence-number-in-the-high-4-bytes rule) is that protocol's own
//!   concern, tested against its own spec, not this crate's.
//! - [`pbkdf2_hmac_sha256`] — PBKDF2-HMAC-SHA256 key derivation (RFC 8018
//!   §5.2's algorithm, RFC 2898's PRF choice), the mechanism
//!   `VSF TR-06-2` §7.3 names for its PSK passphrase-to-key derivation.
//! - [`hmac_sha1`] —
//!   the raw 20-byte HMAC-SHA1 tag, over [`vaco_hash::sha1::Sha1`]
//!   (re-exported by `vaco-hash` for the same reason as `sha2` above),
//!   the primitive RFC 3711 §4.2 truncates for SRTP's default
//!   authentication tag. Truncation itself is left to the caller, the
//!   same "generic primitive, protocol owns its own construction" split
//!   as [`ctr_apply_aes128`].
//!
//! # Evidence
//!
//! The `ctr_impl` module's tests are RFC-vector-derived: all nine of RFC 3686 §6's
//! own key/counter-block/plaintext/ciphertext triples, covering all three
//! AES key sizes — genuinely independent evidence, not this crate's own
//! encoder checked against its own decoder. [`pbkdf2_hmac_sha256`]'s tests
//! are two-layered: RFC 7914 §11's algorithm-level vectors (RFC 8018 itself
//! contains none — checked directly, not assumed) confirm the generic
//! PBKDF2-HMAC-SHA256 implementation,
//! and `VSF TR-06-2` Annex B's own worked passphrase/nonce example
//! (independently re-derived via Python's stdlib `hashlib.pbkdf2_hmac`
//! with the same inputs before being trusted as a test's expected value,
//! not merely read off the spec's rendered page) confirms this crate
//! reproduces the exact keys the spec itself publishes. [`hmac_sha1`]'s
//! tests are RFC-vector-derived: RFC 2202's own three HMAC-SHA1 test
//! cases, cross-checked against Python's stdlib `hmac`/`hashlib` before
//! being trusted (the same discipline as the PBKDF2 vectors above) rather
//! than recalled and taken on faith.

#![forbid(unsafe_code)]

pub use aes;
pub use ctr as ctr_crate;
pub use hmac;
pub use pbkdf2 as pbkdf2_crate;

mod ctr_impl;
mod hmac_sha1;
mod kdf;

pub use ctr_impl::{ctr_apply_aes128, ctr_apply_aes192, ctr_apply_aes256};
pub use hmac_sha1::hmac_sha1;
pub use kdf::pbkdf2_hmac_sha256;
