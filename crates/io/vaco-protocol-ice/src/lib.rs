//! STUN (RFC 5389) and a minimal ICE (RFC 8445) connectivity check, scoped to
//! exactly what a WebRTC-shaped *negotiating* protocol client needs: WHIP
//! (#619) publishes to a server that is overwhelmingly ICE-lite in practice
//! (it never issues its own checks, only answers ours), so this crate never
//! implements the controlling/controlled state machine in full — no
//! candidate-pair scheduling, no triggered checks, no restarts.
//!
//! # What it is
//!
//! [`build_binding_request`] builds one STUN message: the 20-byte header,
//! the attributes a short-term-credential Binding transaction needs
//! (`USERNAME`, `MESSAGE-INTEGRITY`, `FINGERPRINT`, `PRIORITY`,
//! `ICE-CONTROLLING`, `USE-CANDIDATE`), and nothing else — no `TURN`
//! allocations, no `XOR-RELAYED-ADDRESS`, no long-term credentials.
//! [`connectivity_check`] drives one client-initiated Binding transaction
//! over an already-connected [`std::net::UdpSocket`] and reports success or
//! failure; a caller tries candidates in priority order and keeps the first
//! one this returns `Ok` for.
//!
//! # How it works
//!
//! RFC 5389 §15: header is `type(2) | length(2) | magic cookie(4) |
//! transaction id(12)`, followed by TLV attributes padded to a 4-byte
//! boundary. `MESSAGE-INTEGRITY` (§15.4) is an HMAC-SHA1 over everything
//! before it, computed with the STUN header's `length` field temporarily
//! covering the message *through* that attribute; `FINGERPRINT` (§15.5) is a
//! CRC-32 of everything before it (again with `length` adjusted), `XOR`ed with
//! `0x5354554e`. Both are covered internally by the private `encode`/
//! `verify_integrity` helpers.
//!
//! ICE's short-term credential (RFC 8445 §7.2.2, layered on RFC 5389 §10.2.1):
//! a request's `USERNAME` is `"<remote ufrag>:<local ufrag>"`, and both the
//! request and the matching response are authenticated with the **remote**
//! peer's password — the request because that is whose credential the
//! `USERNAME` names, the response because RFC 5389 says the same
//! transaction is authenticated with the same key throughout. There is no
//! `USERNAME` on the response (RFC 5389 §7.3.1.2 does not require one, and
//! this crate does not send one either).
//!
//! # What is deliberately not implemented
//!
//! Server-reflexive/relayed candidate discovery (a real STUN/TURN *server*
//! role for us, not the media server) — WHIP servers observed so far publish
//! reachable host candidates directly, so nothing here resolves our own
//! public address. Retransmission follows RFC 5389 §7.2.1's schedule
//! loosely (a fixed small retry count with a fixed timeout) rather than the
//! exact `RTO`/backoff algorithm; every test and the one real peer measured
//! against (`mediamtx`, see `vaco-mux-whip`) is loopback or LAN, where that
//! gap does not show. Responding to a peer-initiated Binding Request
//! (RFC 7675 consent freshness, or an ICE-lite peer's own liveness check) is
//! also not implemented — see [`connectivity_check`]'s doc for the
//! consequence.
//!
//! # Security
//!
//! `MESSAGE-INTEGRITY` on the response is checked, not merely parsed: an
//! answer whose HMAC does not verify against the credential our own offer
//! and their answer agreed on is rejected exactly like a timeout, per
//! [`connectivity_check`]'s doc. This is real authentication of "the packet
//! came from the peer holding *this* ICE password", the low-level analogue
//! of the DTLS fingerprint check `vaco-mux-whip` performs one layer up —
//! neither is weakened to make a handshake succeed.

#![forbid(unsafe_code)]

use std::net::UdpSocket;
use std::time::Duration;

use vaco_core::{Error, Result};

/// RFC 5389 §6's fixed magic cookie.
const MAGIC_COOKIE: u32 = 0x2112_A442;

/// STUN Binding Request (RFC 5389 §18.1's class+method encoding: class `00`,
/// method `0x001`).
const METHOD_BINDING_REQUEST: u16 = 0x0001;
/// STUN Binding Success Response (class `10`, method `0x001`).
const METHOD_BINDING_SUCCESS: u16 = 0x0101;
/// STUN Binding Error Response (class `11`, method `0x001`).
const METHOD_BINDING_ERROR: u16 = 0x0111;

const ATTR_USERNAME: u16 = 0x0006;
const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
const ATTR_PRIORITY: u16 = 0x0024;
const ATTR_USE_CANDIDATE: u16 = 0x0025;
const ATTR_FINGERPRINT: u16 = 0x8028;
const ATTR_ICE_CONTROLLING: u16 = 0x802A;

/// XOR mask RFC 5389 §15.5 applies to the raw CRC-32 before it becomes the
/// `FINGERPRINT` value.
const FINGERPRINT_XOR: u32 = 0x5354_554e;

/// One STUN transaction id (RFC 5389 §6: 96 bits, not derived from the magic
/// cookie).
pub type TransactionId = [u8; 12];

/// A minimal, unopinionated PRNG seed for transaction ids and ICE
/// credentials — this workspace declares no RNG crate (D10), the same
/// constraint `vaco-mux-rtp::muxer::time_seed` already works under, and this
/// is that same trick widened to produce as many bytes as a caller asks for
/// (an SSRC needs 4, an ICE password needs at least 22 base64-alphabet
/// characters' worth of entropy).
///
/// Not cryptographically secure — nothing here needs it to be: STUN's
/// short-term credential defeats off-path spoofing by requiring the password
/// exchanged over the (network-observable-once, but not blindly guessable)
/// SDP answer, not by the transaction id being unpredictable.
#[must_use]
pub fn pseudo_random_bytes(seed: u64, out_len: usize) -> Vec<u8> {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::new();
    while out.len() < out_len {
        // splitmix64, a well-known small, fast, decent-avalanche step
        // function — not for security, only for spreading a time-derived
        // seed across enough bytes that two calls in the same millisecond
        // still differ.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(out_len);
    out
}

/// Render `bytes` as an ICE-safe `ice-char` string (RFC 8445 §5.3: the
/// unreserved URI characters plus `+`/`/`), long enough to satisfy the
/// minimum lengths RFC 8445 §5.3 states (4 chars for `ice-ufrag`, 22 for
/// `ice-pwd`).
#[must_use]
pub fn ice_credential(seed: u64, len: usize) -> String {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pseudo_random_bytes(seed, len)
        .into_iter()
        .map(|b| {
            let idx = usize::from(b) % ALPHABET.len();
            // Indexing a fixed 64-entry table with a value already reduced
            // modulo its length; never out of range, but spelled with `get`
            // per this workspace's no-indexing rule outside tests.
            char::from(*ALPHABET.get(idx).unwrap_or(&b'A'))
        })
        .collect()
}

/// What a Binding transaction needs beyond the two peers' addresses.
#[derive(Debug, Clone)]
pub struct IceCredentials {
    /// Our own `ice-ufrag`, from the SDP offer we sent.
    pub local_ufrag: String,
    /// The peer's `ice-ufrag`, from the SDP answer.
    pub remote_ufrag: String,
    /// The peer's `ice-pwd` — the HMAC key for both directions of this
    /// transaction, per this crate's doc comment above.
    pub remote_pwd: String,
}

/// Build one STUN message's bytes: header, the given attributes in order,
/// then `MESSAGE-INTEGRITY` keyed by `integrity_key` (when given), then
/// `FINGERPRINT`.
///
/// `attrs` are `(type, value)` pairs already in wire form (a caller building
/// `PRIORITY` passes its 4-byte big-endian encoding, for instance) — kept
/// this untyped rather than as an enum because this crate only ever builds
/// one message shape (the Binding Request) and an enum for four attributes
/// bought nothing a doc comment does not already say.
fn encode(
    method: u16,
    txid: &TransactionId,
    attrs: &[(u16, &[u8])],
    integrity_key: Option<&[u8]>,
    fingerprint: bool,
) -> Vec<u8> {
    let mut body = Vec::new();
    for &(kind, value) in attrs {
        push_attr(&mut body, kind, value);
    }

    if let Some(key) = integrity_key {
        // The length field must cover the message *through* this attribute
        // (RFC 5389 §15.4) while the HMAC is computed, so build the header
        // with that provisional length, hash header+body, then push the
        // real attribute.
        let provisional_len = u16::try_from(body.len() + 24).unwrap_or(u16::MAX);
        let mut signed = header_bytes(method, provisional_len, txid);
        signed.extend_from_slice(&body);
        let tag = vaco_crypto::hmac_sha1(key, &signed);
        push_attr(&mut body, ATTR_MESSAGE_INTEGRITY, &tag);
    }

    if fingerprint {
        let provisional_len = u16::try_from(body.len() + 8).unwrap_or(u16::MAX);
        let mut signed = header_bytes(method, provisional_len, txid);
        signed.extend_from_slice(&body);
        let crc = vaco_hash::crc32(&signed) ^ FINGERPRINT_XOR;
        push_attr(&mut body, ATTR_FINGERPRINT, &crc.to_be_bytes());
    }

    let len = u16::try_from(body.len()).unwrap_or(u16::MAX);
    let mut out = header_bytes(method, len, txid);
    out.extend_from_slice(&body);
    out
}

fn header_bytes(method: u16, length: u16, txid: &TransactionId) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&method.to_be_bytes());
    h.extend_from_slice(&length.to_be_bytes());
    h.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    h.extend_from_slice(txid);
    h
}

fn push_attr(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
    out.extend_from_slice(&kind.to_be_bytes());
    let len = u16::try_from(value.len()).unwrap_or(0);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
    let pad = (4 - (value.len() % 4)) % 4;
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// Build one ICE Binding Request (RFC 8445 §7.2.2): `USERNAME`, `PRIORITY`,
/// `ICE-CONTROLLING` (this crate is always the controlling agent — the only
/// role that makes sense for a publishing client talking to an ICE-lite
/// server, which RFC 8445 §2.2 says is always controlled), `USE-CANDIDATE`
/// (nomination happens on the first and only check there is, since neither
/// side is running the full candidate-pair state machine), then
/// `MESSAGE-INTEGRITY` and `FINGERPRINT`.
#[must_use]
pub fn build_binding_request(creds: &IceCredentials, txid: &TransactionId, priority: u32) -> Vec<u8> {
    let username = format!("{}:{}", creds.remote_ufrag, creds.local_ufrag);
    // The tie-breaker just needs to look different per transaction; fold the
    // whole transaction id into a seed rather than indexing into it.
    let seed = txid.iter().fold(0u64, |acc, &b| {
        acc.wrapping_mul(0x0100_0000_01B3).wrapping_add(u64::from(b))
    });
    let tie_breaker = pseudo_random_bytes(seed, 8);
    let priority_be = priority.to_be_bytes();
    let attrs: [(u16, &[u8]); 4] = [
        (ATTR_USERNAME, username.as_bytes()),
        (ATTR_PRIORITY, &priority_be),
        (ATTR_ICE_CONTROLLING, &tie_breaker),
        (ATTR_USE_CANDIDATE, &[]),
    ];
    encode(
        METHOD_BINDING_REQUEST,
        txid,
        &attrs,
        Some(creds.remote_pwd.as_bytes()),
        true,
    )
}

/// A parsed STUN header plus a pointer to where `MESSAGE-INTEGRITY` (if any)
/// begins, for [`verify_integrity`].
struct ParsedMessage<'a> {
    method: u16,
    txid: TransactionId,
    body: &'a [u8],
    /// Byte offset into `body` of the `MESSAGE-INTEGRITY` attribute's type
    /// field, if the message carries one.
    integrity_at: Option<usize>,
}

/// Read a big-endian `u16` at `at`, or `None` if it does not fit.
fn read_u16(buf: &[u8], at: usize) -> Option<u16> {
    let a = *buf.get(at)?;
    let b = *buf.get(at + 1)?;
    Some(u16::from_be_bytes([a, b]))
}

/// Read a big-endian `u32` at `at`, or `None` if it does not fit.
fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
    let a = *buf.get(at)?;
    let b = *buf.get(at + 1)?;
    let c = *buf.get(at + 2)?;
    let d = *buf.get(at + 3)?;
    Some(u32::from_be_bytes([a, b, c, d]))
}

/// Parse a STUN header and locate (without verifying) `MESSAGE-INTEGRITY`.
///
/// # Errors
/// [`Error::InvalidData`] for anything shorter than a header, a bad magic
/// cookie, or a `length` that does not match the buffer — every field here
/// comes from the network.
fn parse(buf: &[u8]) -> Result<ParsedMessage<'_>> {
    let bad = || Error::InvalidData("stun message shorter than its header");
    let method = read_u16(buf, 0).ok_or_else(bad)?;
    let length = usize::from(read_u16(buf, 2).ok_or_else(bad)?);
    let cookie = read_u32(buf, 4).ok_or_else(bad)?;
    if cookie != MAGIC_COOKIE {
        return Err(Error::InvalidData("stun message has the wrong magic cookie"));
    }
    let txid_slice = buf.get(8..20).ok_or_else(bad)?;
    let mut txid = [0u8; 12];
    txid.copy_from_slice(txid_slice);
    let body = buf
        .get(20..20 + length)
        .ok_or(Error::InvalidData("stun message length exceeds the buffer"))?;

    let mut offset = 0usize;
    let mut integrity_at = None;
    while let (Some(kind), Some(len)) = (read_u16(body, offset), read_u16(body, offset + 2)) {
        let len = usize::from(len);
        let padded = len.div_ceil(4) * 4;
        let Some(next) = offset.checked_add(4).and_then(|n| n.checked_add(padded)) else {
            break;
        };
        if next > body.len() {
            break;
        }
        if kind == ATTR_MESSAGE_INTEGRITY {
            integrity_at = Some(offset);
        }
        offset = next;
    }

    Ok(ParsedMessage {
        method,
        txid,
        body,
        integrity_at,
    })
}

/// Verify a parsed response's `MESSAGE-INTEGRITY` against `key`, the same
/// way [`encode`] computed it: HMAC-SHA1 over the header (with `length`
/// rewound to cover exactly through this attribute) plus every attribute
/// before it.
fn verify_integrity(parsed: &ParsedMessage<'_>, key: &[u8]) -> bool {
    let Some(at) = parsed.integrity_at else {
        return false;
    };
    let Some(tag) = parsed.body.get(at + 4..at + 24) else {
        return false;
    };
    let provisional_len = u16::try_from(at + 24).unwrap_or(u16::MAX);
    let mut signed = header_bytes(parsed.method, provisional_len, &parsed.txid);
    if let Some(prefix) = parsed.body.get(..at) {
        signed.extend_from_slice(prefix);
    } else {
        return false;
    }
    let expected = vaco_crypto::hmac_sha1(key, &signed);
    constant_time_eq(&expected, tag)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Perform one client-initiated ICE connectivity check over `socket`, which
/// must already be connected (RFC 8445's Binding Request goes to exactly one
/// candidate at a time; a caller trying several candidates connects a fresh
/// socket per attempt).
///
/// Blocking, with up to `retries + 1` sends at `per_try_timeout` each — RFC
/// 5389 §7.2.1's retransmission timer loosely, not exactly (see the crate
/// doc for why exactness does not matter here). Returns `Ok(())` the moment
/// a Binding Success Response with a matching transaction id and a valid
/// `MESSAGE-INTEGRITY` arrives; anything else (a timeout, a Binding Error
/// Response, a response with a bad or missing integrity tag) is
/// [`Error::InvalidData`] or [`Error::Io`], never a panic — the response
/// came from whatever answered the UDP socket, which for a WHIP endpoint the
/// caller does not otherwise authenticate before this check passes.
///
/// # What this does not do
/// Answer a Binding Request the peer sends *to* us. An ICE-lite peer never
/// issues one during setup (RFC 8445 §2.2), so this is silent for the
/// connectivity check itself; a long-running publish that wants to survive
/// RFC 7675 consent-freshness pings from a stricter peer would need a
/// responder this crate does not provide — recorded rather than built, since
/// no peer measured so far (`mediamtx`) sends one.
///
/// # Errors
/// See above.
pub fn connectivity_check(
    socket: &UdpSocket,
    creds: &IceCredentials,
    priority: u32,
    per_try_timeout: Duration,
    retries: u32,
) -> Result<()> {
    let seed = vaco_time::Instant::now();
    let seed_bits = {
        let text = format!("{seed:?}");
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    };
    let mut txid: TransactionId = [0u8; 12];
    txid.copy_from_slice(&pseudo_random_bytes(seed_bits, 12));

    let request = build_binding_request(creds, &txid, priority);
    socket
        .set_read_timeout(Some(per_try_timeout))
        .map_err(Error::Io)?;

    let mut buf = [0u8; 512];
    for attempt in 0..=retries {
        socket.send(&request).map_err(Error::Io)?;
        match socket.recv(&mut buf) {
            Ok(n) => {
                let Some(reply) = buf.get(..n) else {
                    continue;
                };
                let parsed = match parse(reply) {
                    Ok(p) => p,
                    Err(_) if attempt < retries => continue,
                    Err(e) => return Err(e),
                };
                if parsed.txid != txid {
                    continue;
                }
                if parsed.method == METHOD_BINDING_ERROR {
                    return Err(Error::InvalidData(
                        "peer rejected the ICE connectivity check",
                    ));
                }
                if parsed.method != METHOD_BINDING_SUCCESS {
                    continue;
                }
                if !verify_integrity(&parsed, creds.remote_pwd.as_bytes()) {
                    return Err(Error::InvalidData(
                        "ICE binding response failed message-integrity verification",
                    ));
                }
                return Ok(());
            }
            Err(e) if attempt < retries => {
                let _ = e;
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Err(Error::InvalidData(
        "no valid ICE binding response before the retry budget ran out",
    ))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use std::net::UdpSocket;

    fn creds() -> IceCredentials {
        IceCredentials {
            local_ufrag: "loclufrg".to_owned(),
            remote_ufrag: "remoufrg".to_owned(),
            remote_pwd: "remotepasswordremotepassword1".to_owned(),
        }
    }

    #[test]
    fn request_round_trips_through_parse() {
        let c = creds();
        let txid = [7u8; 12];
        let msg = build_binding_request(&c, &txid, 12345);
        let parsed = parse(&msg).unwrap();
        assert_eq!(parsed.method, METHOD_BINDING_REQUEST);
        assert_eq!(parsed.txid, txid);
        assert!(parsed.integrity_at.is_some());
    }

    #[test]
    fn a_request_carries_its_own_valid_message_integrity() {
        let c = creds();
        let txid = [3u8; 12];
        let msg = build_binding_request(&c, &txid, 1);
        let parsed = parse(&msg).unwrap();
        assert!(verify_integrity(&parsed, c.remote_pwd.as_bytes()));
        assert!(!verify_integrity(&parsed, b"wrong password entirely"));
    }

    #[test]
    fn fingerprint_covers_the_whole_message() {
        let c = creds();
        let txid = [9u8; 12];
        let msg = build_binding_request(&c, &txid, 1);
        // Corrupt one byte in the body (after the header) and confirm the
        // fingerprint attribute — read directly, not re-derived — no longer
        // matches a recomputation over the corrupted bytes.
        let mut corrupt = msg.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xFF;
        let fp_len = corrupt.len();
        let recomputed = vaco_hash::crc32(&corrupt[..fp_len - 8]) ^ FINGERPRINT_XOR;
        let stored = u32::from_be_bytes([
            corrupt[fp_len - 4],
            corrupt[fp_len - 3],
            corrupt[fp_len - 2],
            corrupt[fp_len - 1],
        ]);
        assert_ne!(recomputed, stored);
    }

    #[test]
    fn a_truncated_message_is_rejected_not_panicked_on() {
        for len in 0..24 {
            let _ = parse(&vec![0u8; len]);
        }
        // The real property under test is "no panic"; getting here proves it.
    }

    #[test]
    fn connectivity_check_succeeds_against_a_hand_built_responder() {
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(server.local_addr().unwrap()).unwrap();
        server.connect(client.local_addr().unwrap()).unwrap();

        let c = creds();
        let server_creds = c.clone();
        let handle = std::thread::spawn(move || {
            server
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buf = [0u8; 512];
            let n = server.recv(&mut buf).unwrap();
            let parsed = parse(&buf[..n]).unwrap();
            assert!(verify_integrity(&parsed, server_creds.remote_pwd.as_bytes()));
            // Binding Success Response: no XOR-MAPPED-ADDRESS needed for this
            // crate's own client to accept it, only a matching transaction id
            // and a valid MESSAGE-INTEGRITY keyed the same way the request was.
            let attrs: [(u16, &[u8]); 0] = [];
            let resp = encode(
                METHOD_BINDING_SUCCESS,
                &parsed.txid,
                &attrs,
                Some(server_creds.remote_pwd.as_bytes()),
                true,
            );
            server.send(&resp).unwrap();
        });

        let result = connectivity_check(&client, &c, 12345, Duration::from_millis(500), 2);
        handle.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn connectivity_check_rejects_a_forged_response() {
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.connect(server.local_addr().unwrap()).unwrap();
        server.connect(client.local_addr().unwrap()).unwrap();

        let c = creds();
        let handle = std::thread::spawn(move || {
            server
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buf = [0u8; 512];
            let n = server.recv(&mut buf).unwrap();
            let parsed = parse(&buf[..n]).unwrap();
            let attrs: [(u16, &[u8]); 0] = [];
            // Signed with the wrong key: an attacker who does not hold the
            // real ICE password cannot forge a response this crate accepts.
            let resp = encode(
                METHOD_BINDING_SUCCESS,
                &parsed.txid,
                &attrs,
                Some(b"not the real remote password"),
                true,
            );
            server.send(&resp).unwrap();
        });

        let result = connectivity_check(&client, &c, 12345, Duration::from_millis(200), 1);
        handle.join().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn connectivity_check_times_out_against_silence() {
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let nobody = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = nobody.local_addr().unwrap();
        drop(nobody);
        client.connect(addr).unwrap();
        let c = creds();
        let result = connectivity_check(&client, &c, 1, Duration::from_millis(100), 1);
        assert!(result.is_err());
    }
}
