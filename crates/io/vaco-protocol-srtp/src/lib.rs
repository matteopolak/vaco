//! SRTP (RFC 3711) — RTP payload encryption and authentication.
//!
//! # Scope
//!
//! RTP payload encryption and authentication follow RFC 3711 §4.3.1 key
//! derivation, §4.1.1 keystream IV construction, and §4.2 authentication tags.
//! [`session::SrtpContext`] combines them with a rollover counter for one SSRC.
//! AES-CTR and HMAC-SHA1 come from `vaco-crypto`.
//!
//! Only SRTP media is supported, not SRTCP. The supported profile is
//! AES-128-CTR/HMAC-SHA1-80, and `key_derivation_rate = 0`; AES-256 key
//! derivation is available separately. [`session::RolloverTracker`] implements
//! RFC 3711 Appendix A.3's high-to-low sequence-jump rule.
//!
//! # Provenance and interop
//!
//! RFC 3711 publishes no numeric vectors, so KDF tests cover its stated
//! labels/layout and session tests cover round trips and tamper rejection.
//! Independent validation against a real DTLS-SRTP handshake (`pion/srtp` with
//! `mediamtx` 1.20.1) and byte-for-byte `libsrtp` known answers established the
//! label position in [`kdf::derivation_counter_block`]: index 7. The known-answer
//! test preserves that measured result; RFC 3711 §4.3.1 remains the specification.

#![forbid(unsafe_code)]

pub mod kdf;
pub mod session;

pub use kdf::{Label, SessionKeys, derive_session_keys_aes128, derive_session_keys_aes256};
pub use session::{DEFAULT_TAG_LEN, RolloverTracker, SrtpContext, build_iv, compute_auth_tag};
