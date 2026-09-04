//! SRTP (RFC 3711) — RTP payload encryption and authentication.
//!
//! # What this package is
//!
//! RTP payload encryption and authentication: §4.3.1's key derivation
//! ([`kdf`]), §4.1.1's per-packet keystream IV construction and §4.2's
//! authentication tag ([`session`]), and a small [`session::SrtpContext`]
//! that ties both to a rollover counter for one SSRC. Built entirely on
//! `vaco-crypto`'s AES-CTR (added [`vaco_crypto::hmac_sha1`] to that
//! crate alongside this one, D11's single-owner rule, rather than a
//! second `hmac` wiring here).
//!
//! **Scope, stated up front:**
//! - SRTP (media) only — not SRTCP. The three SRTCP key-derivation labels
//!   and SRTCP's own packet-index/`E`-bit framing are not built; see
//!   [`kdf`]'s own docs.
//! - `key_derivation_rate = 0` only (derive once, the common real-world
//!   configuration) — periodic re-derivation is not built.
//! - AES-128-CTR / HMAC-SHA1-80 (`AES_CM_128_HMAC_SHA1_80`, the profile
//!   named in RFC 3711 itself as SRTP's default) is the profile actually
//!   wired end-to-end through [`session::SrtpContext::protect`]/
//!   `unprotect`; AES-256 key derivation exists ([`kdf::derive_session_keys_aes256`])
//!   but [`session::SrtpContext`]'s own encrypt/decrypt path is not yet
//!   generic over key size — see that function's own doc comment.
//! - [`session::RolloverTracker`] implements RFC 3711 Appendix A.3's
//!   simple "wrap detected on a high-to-low sequence jump" rule, not
//!   Appendix A's fuller out-of-order-across-a-rollover guessing
//!   algorithm.
//!
//! # Provenance, and a real interop pass (2026-08-29)
//!
//! RFC 3711 publishes no numeric test vectors of its own (see
//! `provenance/sources.toml`'s `rfc-3711` entry), so [`kdf`]'s tests were
//! originally self-consistency plus draft-derived field-layout checks, not
//! RFC-vector-derived; [`session`]'s `protect`/`unprotect` round-trip and
//! tamper-rejection tests were the same. [`vaco_crypto::hmac_sha1`] itself,
//! underneath both, *is* RFC-vector-derived (RFC 2202's own HMAC-SHA1 test
//! cases, cross-checked against Python's stdlib `hmac`/`hashlib` before
//! being trusted).
//!
//! That gap — no independent peer to disagree with — hid a real bug, and
//! then hid it a *second* time: [`kdf::derivation_counter_block`] `XOR`ed
//! the key-derivation label into the wrong octet not once but twice (index
//! 0, then index 8 after a first fix; the correct answer is index 7 — see
//! that function's own doc for exactly why). Every self-consistency and
//! field-layout test here passed throughout both wrong versions, because
//! both kinds check the code against its own restated claim, not against
//! an independently computed key. The first wrongness surfaced once
//! `vaco-mux-whip` (#619) completed a real DTLS-SRTP handshake against
//! `mediamtx` 1.20.1 — an independent implementation (`pion/srtp`) — and
//! every resulting SRTP packet was silently dropped: connection and
//! handshake both genuinely succeeded, keys were derived and used exactly
//! as designed, and every one of them was simply wrong. The *second*
//! wrongness (the off-by-one at index 8) was caught only by cross-checking
//! a hand-built packet against `libsrtp` itself (the reference C
//! implementation, via its `pylibsrtp` binding — D17 applied to
//! open-source, non-`FFmpeg` code, so no clean-room concern) and comparing
//! ciphertext byte for byte. [`session`]'s `protect_matches_a_real_libsrtp_known_answer`
//! pins that exact answer permanently, which is what RFC 3711 itself
//! cannot do (it publishes no numeric vectors) — the whole reason a wrong
//! placement survived twice.

#![forbid(unsafe_code)]

pub mod kdf;
pub mod session;

pub use kdf::{Label, SessionKeys, derive_session_keys_aes128, derive_session_keys_aes256};
pub use session::{DEFAULT_TAG_LEN, RolloverTracker, SrtpContext, build_iv, compute_auth_tag};
