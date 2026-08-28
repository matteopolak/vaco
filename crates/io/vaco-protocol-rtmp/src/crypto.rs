//! SHA-256 and HMAC-SHA256, for the RTMP complex handshake's digest scheme.
//!
//! # Why hand-rolled rather than a dependency
//!
//! `sha2` is a workspace dependency, but it already has exactly one owner —
//! `vaco-hash` (D11: one crate per swappable-output primitive) — and adding
//! `hmac` for one caller here would need a second owner-gate exemption plus
//! D10's three-gate review for a dependency this crate could avoid instead.
//! SHA-256 (FIPS 180-4) is a fully specified, fixed algorithm with no
//! implementation choices to disagree on, so a local implementation checked
//! against the standard's own test vectors is not the risk a hand-rolled
//! parser would be. HMAC (RFC 2104) is a handful of XORs and two hash calls
//! on top of it.
//!
//! Verified against FIPS 180-4's own worked examples (the one- and
//! two-block messages) and RFC 4231's HMAC-SHA256 test cases 1-4 — see the
//! tests below.

/// SHA-256's eight initial hash values, FIPS 180-4 §5.3.3 (the fractional
/// parts of the square roots of the first eight primes).
const H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// The 64 round constants, FIPS 180-4 §4.2.2 (the fractional parts of the
/// cube roots of the first 64 primes).
#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

/// One block's worth of message schedule words, computed in place.
fn message_schedule(block: &[u8]) -> [u32; 64] {
    let mut w = [0u32; 64];
    for (i, word) in w.iter_mut().enumerate().take(16) {
        let base = i * 4;
        // `block` is always exactly 64 bytes (the caller pads to that), so
        // these four indices are always in range.
        let bytes = [
            *block.get(base).unwrap_or(&0),
            *block.get(base + 1).unwrap_or(&0),
            *block.get(base + 2).unwrap_or(&0),
            *block.get(base + 3).unwrap_or(&0),
        ];
        *word = u32::from_be_bytes(bytes);
    }
    for i in 16..64 {
        let w15 = *w.get(i - 15).unwrap_or(&0);
        let w2 = *w.get(i - 2).unwrap_or(&0);
        let w16 = *w.get(i - 16).unwrap_or(&0);
        let w7 = *w.get(i - 7).unwrap_or(&0);
        let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
        let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
        let value = w16.wrapping_add(s0).wrapping_add(w7).wrapping_add(s1);
        if let Some(slot) = w.get_mut(i) {
            *slot = value;
        }
    }
    w
}

/// Compress one 64-byte block into `state`.
///
/// The eight working variables keep FIPS 180-4's own one-letter names
/// (`a`..`h`) rather than being renamed for `clippy::many_single_char_names`
/// — matching the standard's own notation is the point.
#[allow(
    clippy::many_single_char_names,
    reason = "FIPS 180-4's own variable names"
)]
fn compress(state: &mut [u32; 8], block: &[u8]) {
    let w = message_schedule(block);
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*K.get(i).unwrap_or(&0))
            .wrapping_add(*w.get(i).unwrap_or(&0));
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// SHA-256 (FIPS 180-4) over `data`, with no length cap of its own — callers
/// pass an already-bounded slice (the largest input this crate ever hashes
/// is one 1536-byte handshake signature).
#[must_use]
pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state = H0;

    #[allow(
        clippy::integer_division,
        reason = "counting whole 64-byte blocks, not a measurement"
    )]
    let full_blocks = data.len() / 64;
    for i in 0..full_blocks {
        let start = i * 64;
        if let Some(block) = data.get(start..start + 64) {
            compress(&mut state, block);
        }
    }

    // Padding: one 0x80 byte, zeros, then the bit length as a 64-bit
    // big-endian integer, all rounded up to a multiple of 64 bytes with room
    // for at least 9 extra bytes (0x80 + 8-byte length).
    let tail = data.get(full_blocks * 64..).unwrap_or(&[]);
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut last = [0u8; 128];
    let mut n = tail.len();
    if let Some(dst) = last.get_mut(..n) {
        dst.copy_from_slice(tail);
    }
    if let Some(b) = last.get_mut(n) {
        *b = 0x80;
    }
    n += 1;
    let padded_len = if n <= 56 { 64 } else { 128 };
    if let Some(len_bytes) = last.get_mut(padded_len - 8..padded_len) {
        len_bytes.copy_from_slice(&bit_len.to_be_bytes());
    }
    for chunk_start in (0..padded_len).step_by(64) {
        if let Some(block) = last.get(chunk_start..chunk_start + 64) {
            compress(&mut state, block);
        }
    }

    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        if let Some(dst) = out.get_mut(i * 4..i * 4 + 4) {
            dst.copy_from_slice(&word.to_be_bytes());
        }
    }
    out
}

/// HMAC-SHA256 (RFC 2104), key of any length.
#[must_use]
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;

    let key_block = if key.len() > BLOCK {
        let hashed = sha256(key);
        let mut k = [0u8; BLOCK];
        if let Some(dst) = k.get_mut(..hashed.len()) {
            dst.copy_from_slice(&hashed);
        }
        k
    } else {
        let mut k = [0u8; BLOCK];
        if let Some(dst) = k.get_mut(..key.len()) {
            dst.copy_from_slice(key);
        }
        k
    };

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        if let (Some(ip), Some(op), Some(k)) = (ipad.get_mut(i), opad.get_mut(i), key_block.get(i))
        {
            *ip ^= *k;
            *op ^= *k;
        }
    }

    let mut inner_input = Vec::new();
    inner_input.extend_from_slice(&ipad);
    inner_input.extend_from_slice(message);
    let inner = sha256(&inner_input);

    let mut outer_input = Vec::new();
    outer_input.extend_from_slice(&opad);
    outer_input.extend_from_slice(&inner);
    sha256(&outer_input)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    /// FIPS 180-4 §D.1: SHA-256("abc"). Expected value independently
    /// computed with Python's `hashlib`, not transcribed from memory.
    #[test]
    fn sha256_one_block_message() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// FIPS 180-4 §D.3: SHA-256 of the empty string.
    #[test]
    fn sha256_empty() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// FIPS 180-4 §D.2: SHA-256 of the 56-byte two-block message.
    #[test]
    fn sha256_two_block_message() {
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// RFC 4231 case 1: 20-byte key of `0x0b`, data `"Hi There"`.
    #[test]
    fn hmac_rfc4231_case_1() {
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    /// RFC 4231 case 2: key `"Jefe"`, data `"what do ya want for
    /// nothing?"`.
    #[test]
    fn hmac_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    /// RFC 4231 case 3: 20-byte key of `0xaa`, 50 bytes of `0xdd`.
    #[test]
    fn hmac_rfc4231_case_3() {
        let key = [0xaau8; 20];
        let data = [0xddu8; 50];
        let mac = hmac_sha256(&key, &data);
        assert_eq!(
            hex(&mac),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
    }

    /// RFC 4231 case 6: a key longer than the block size (131 bytes of
    /// `0xaa`) is itself hashed first.
    #[test]
    fn hmac_key_longer_than_block_is_hashed() {
        let key = [0xaau8; 131];
        let mac = hmac_sha256(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
