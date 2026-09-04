#![forbid(unsafe_code)]
//! `crypto:` — AES-128-CBC over a nested URL.
//!
//! # What the reference actually does (measured, not assumed)
//!
//! The work package this crate was built from names the protocol "AES-CTR".
//! **That is wrong.** Measured against `ffmpeg 8.1` by recovering the AES
//! input block from known plaintext/ciphertext pairs (see
//! `docs/io/vaco-protocol-crypto.md` for the full transcript): block *i*'s
//! AES input is `plaintext[i] XOR ciphertext[i-1]` (`ciphertext[-1] = iv`),
//! which is the textbook definition of **CBC**, not a counter. A name in the
//! reference's own vocabulary is evidence about what it calls the thing, not
//! about what it does; names are not specifications.
//!
//! Two more measured, load-bearing facts that follow from this:
//!
//! 1. **PKCS#7 padding is always applied on write, even when the plaintext
//!    is already block-aligned** — a 256-byte (16-block) input produces 272
//!    bytes (17 blocks): a full extra block of `0x10` padding bytes. This is
//!    why every ciphertext this protocol writes is `⌊len/16⌋·16 + 16` bytes,
//!    never merely `len` rounded up.
//! 2. **On read, there is no PKCS#7 consistency check at all.** The
//!    reference reads only the final decrypted byte, `n`, and strips exactly
//!    `n` bytes whenever `n <= 16` — even when every *other* byte of that
//!    final block disagrees with `n`. Only `n > 16` triggers a fallback,
//!    which strips a fixed 16 bytes. A first measurement pass got this
//!    wrong: corrupting the *last ciphertext block itself* looked like a
//!    consistency check, because CBC decryption of a modified ciphertext
//!    block scrambles the *entire* block (AES's avalanche effect), not just
//!    the targeted byte — so every trial happened to land outside `0..=16`
//!    and hit the fallback, which is consistent with either explanation and
//!    proves neither. A matching sample is not sufficient evidence. The
//!    correct technique is a CBC bit-flip — XOR
//!    a byte of the *second-to-last* ciphertext block to change exactly one
//!    byte of the *last* plaintext block, leaving the rest of that block
//!    (including its other padding bytes) untouched — and under that
//!    controlled test the reference trusts the single byte regardless. See
//!    [`cipher::unpad`] for the corrected rule and the full transcript of
//!    both measurement passes.
//!
//! # Security
//!
//! [`protocol::CRYPTO_PROTOCOL`] opens its nested URL through
//! [`vaco_protocol_core::ProtocolEnv`], never directly — see that module's
//! docs for why, and [`protocol`] for the measured `default_whitelist`
//! (empty: `-protocol_whitelist crypto` alone refuses the nested `file:`
//! open with `Protocol 'file' not on whitelist 'crypto'!`, exactly like
//! every other nested-opening protocol in this workspace).
//!
//! Key and IV material is never included in an error, a log line, or
//! `-h protocol=crypto` output — see [`options`]'s module docs for the one
//! place this deliberately diverges from the reference (which echoes a
//! malformed option's raw value back in `Error setting option key to value
//! …`), and why that divergence is not observable output in the D6/D17
//! sense.

pub mod cipher;
pub mod options;
pub mod protocol;
pub mod sink;
pub mod source;

pub use protocol::{CRYPTO_PROTOCOL, CryptoProtocol};
