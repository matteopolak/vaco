//! Raw AES-128-CBC block chaining, and the measured padding rules.
//!
//! # Why this is not just `cbc::Encryptor`/`Decryptor` end to end
//!
//! Encryption is exactly `cbc::Encryptor<Aes128>` with PKCS#7 padding — the
//! crate does this correctly and there is no reason to reimplement it.
//! Decryption is different: the `RustCrypto` `cbc` crate's own
//! `decrypt_padded*` **errors** on invalid padding, but the reference does
//! not — it strips the last byte's value unconditionally when that value is
//! `<= BLOCK`, with no check that the preceding bytes agree (see [`unpad`]).
//! Matching that means decrypting raw blocks ourselves and applying [`unpad`]
//! by hand.
//!
//! `BLOCK` is `aes::Aes128`'s block size, 16 bytes, for both operations —
//! this crate does not support AES-192/256 (see [`crate::options`]; the
//! reference itself rejects any key that is not exactly 16 bytes).

use aes::Aes128;
use aes::cipher::{Array, BlockCipherDecrypt, BlockModeEncrypt, KeyInit, KeyIvInit, block_padding::Pkcs7};

/// AES's (and this protocol's) block size. Every key and IV this protocol
/// accepts is exactly one block, and every ciphertext this protocol produces
/// is a whole number of blocks — see the crate docs on why a whole extra
/// block is added even when the plaintext is already aligned.
pub const BLOCK: usize = 16;

/// `BLOCK`'s power-of-two shift, so block-index arithmetic in
/// [`crate::source::CryptoSource::seek`] can use `>>`/`&` instead of `/`/`%`
/// — exact either way, but this sidesteps `clippy::integer_division`'s
/// general "may lose precision" warning, which has no way to know a divisor
/// is a compile-time power of two.
pub const BLOCK_SHIFT: u32 = BLOCK.trailing_zeros();

type Aes128CbcEnc = cbc::Encryptor<Aes128>;

/// Encrypt `plaintext` under CBC with PKCS#7 padding, `key`/`iv` each exactly
/// [`BLOCK`] bytes (the caller validates this — see
/// [`crate::options::resolve`]).
///
/// Always returns `⌊plaintext.len() / BLOCK⌋ · BLOCK + BLOCK` bytes: PKCS#7
/// adds a full dummy block when the input is already aligned, matching the
/// reference exactly (measured: a 256-byte input encrypts to 272 bytes).
///
/// This is the one place [`cbc::Encryptor`] does the whole job — encryption's
/// padding rule (always PKCS#7, always erroring never observed) matches the
/// crate's `encrypt_padded_vec` exactly, unlike decryption (see
/// [`decrypt_all`]'s docs for why that path is hand-rolled instead).
#[must_use]
pub fn encrypt(key: &[u8; BLOCK], iv: &[u8; BLOCK], plaintext: &[u8]) -> Vec<u8> {
    Aes128CbcEnc::new(&Array::from(*key), &Array::from(*iv)).encrypt_padded_vec::<Pkcs7>(plaintext)
}

/// One block's worth of CBC decryption: `D(key, block) XOR chain`, returning
/// the plaintext block and updating `chain` to `block` (the ciphertext just
/// consumed), ready for the next call. `chain` starts as the IV.
///
/// Built on the raw [`Aes128`] block cipher rather than `cbc::Decryptor`
/// because the streaming reader in [`crate::source`] needs each block's
/// plaintext the moment it has that block's ciphertext and the *previous*
/// block — never the whole buffer — and because [`decrypt_all`]'s padding
/// fallback needs the raw (unpadded) plaintext blocks, which
/// `cbc::Decryptor`'s own padded API does not expose (it errors on invalid
/// padding instead of returning the bytes). This is the unit both
/// [`crate::source::CryptoSource`] (streaming, one block behind) and
/// [`decrypt_all`] (whole-buffer, for tests and the fuzz target) build on.
#[must_use]
pub fn decrypt_block(key: &[u8; BLOCK], chain: &mut [u8; BLOCK], block: &[u8; BLOCK]) -> [u8; BLOCK] {
    let cipher = Aes128::new(&Array::from(*key));
    let mut work = Array::from(*block);
    cipher.decrypt_block(&mut work);
    let mut out = [0u8; BLOCK];
    // `clippy::indexing_slicing` is denied workspace-wide, so the XOR is
    // written through `zip` rather than `work[i] ^ chain[i]`.
    for ((o, w), c) in out.iter_mut().zip(work.iter()).zip(chain.iter()) {
        *o = w ^ c;
    }
    *chain = *block;
    out
}

/// Decrypt a whole, already-assembled ciphertext (a multiple of [`BLOCK`]
/// bytes) and strip the trailing padding per the measured fallback rule.
///
/// Used by tests and the fuzz target; the streaming reader in
/// [`crate::source`] uses [`decrypt_block`] directly so it never needs the
/// whole file in memory.
///
/// Returns [`None`] if `ciphertext` is empty or not a whole number of
/// [`BLOCK`]-byte blocks — the reference's behaviour on a malformed
/// (non-block-aligned) ciphertext file is untested for lack of a way to
/// produce one through its own encoder; see the crate docs.
#[must_use]
pub fn decrypt_all(key: &[u8; BLOCK], iv: &[u8; BLOCK], ciphertext: &[u8]) -> Option<Vec<u8>> {
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(BLOCK) {
        return None;
    }
    let mut chain = *iv;
    // Not `Vec::with_capacity`: `clippy::disallowed_methods` requires sizing
    // an allocation through `vaco_limits::Budget` instead, and this function
    // is only ever called with a buffer the caller already holds in memory
    // (tests and the fuzz target) — the streaming path in `crate::source`
    // never assembles the whole ciphertext at once. `extend_from_slice`
    // reallocates as it goes, exactly like any other unsized `Vec::new()`.
    let mut plain = Vec::new();
    for raw in ciphertext.chunks_exact(BLOCK) {
        let block = <[u8; BLOCK]>::try_from(raw).ok()?;
        plain.extend_from_slice(&decrypt_block(key, &mut chain, &block));
    }
    let n = unpad(&plain);
    plain.truncate(n);
    Some(plain)
}

/// The measured padding-removal rule.
///
/// **There is no PKCS#7 consistency check at all.** The reference reads only
/// the final byte, `n`, and strips exactly `n` bytes whenever `n <= BLOCK`
/// (`n == 0` is a legal "no padding" answer, stripping nothing); when
/// `n > BLOCK` it falls back to stripping a fixed [`BLOCK`] bytes. The
/// `N - 1` preceding bytes are never inspected — a padding value can be
/// wrong everywhere except the last byte and the reference still trusts it.
///
/// This was measured incorrectly on a first pass: corrupting the *last
/// ciphertext block itself* (`0x00, 0x01, 0x10, 0x11, 0xff` at the final
/// byte) looked like a byte-consistency check, because CBC decryption of a
/// modified ciphertext block scrambles *the entire block* via AES's
/// avalanche effect, not just the targeted byte — so every one of those five
/// trials happened to land outside `0..=16` and hit the fallback,
/// coincidentally consistent with a validation rule that was never actually
/// being exercised (`planning/AGENT-CONSTRAINTS.md`: "one matching sample is
/// not a passing test"). The correct technique is a **CBC bit-flip**: XOR a
/// byte of the *second-to-last* ciphertext block, which XORs the
/// corresponding byte of the *last plaintext block* directly and leaves
/// every other byte of that block — including the other 15 padding bytes —
/// untouched. Under that controlled test, setting the final decrypted byte
/// to `0, 1, 5, 8, 15` while every other byte of the block stayed the
/// (invalid, non-matching) original `0x10` still stripped exactly `0, 1, 5,
/// 8, 15` bytes; `16, 17, 20, 100, 255` all stripped exactly 16. See
/// `docs/io/vaco-protocol-crypto.md` for the full transcript of both passes.
///
/// Returns the number of bytes to *keep* (never more than `plaintext.len()`,
/// never underflowing when `plaintext.len() < BLOCK`, which cannot happen
/// through [`decrypt_all`] but is defended here anyway since this function is
/// also exercised directly by the fuzz target).
#[must_use]
pub fn unpad(plaintext: &[u8]) -> usize {
    let len = plaintext.len();
    let Some(&last) = plaintext.last() else {
        return 0;
    };
    let n = usize::from(last);
    if n <= BLOCK && n <= len {
        len - n
    } else {
        len.saturating_sub(BLOCK)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    const KEY: [u8; BLOCK] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const IV: [u8; BLOCK] = KEY;

    /// Measured against `ffmpeg 8.1`: `AES_encrypt(key, iv)` for this
    /// key/iv pair, recovered from an all-zero-plaintext ciphertext's first
    /// block (`plaintext XOR ciphertext == keystream` when plaintext is
    /// zero, and CBC's own definition makes that keystream `E(key, iv)`
    /// exactly when the true plaintext block is also zero).
    const BLOCK0_OF_ZERO_PLAINTEXT: [u8; BLOCK] = [
        0x0a, 0x94, 0x0b, 0xb5, 0x41, 0x6e, 0xf0, 0x45, 0xf1, 0xc3, 0x94, 0x58, 0xc6, 0x53, 0xea,
        0x5a,
    ];

    #[test]
    fn block_zero_of_an_all_zero_plaintext_matches_the_reference() {
        let ct = encrypt(&KEY, &IV, &[0u8; 256]);
        assert_eq!(ct.len(), 272, "256-byte aligned input pads to a full extra block");
        assert_eq!(ct.get(..BLOCK).unwrap(), &BLOCK0_OF_ZERO_PLAINTEXT);
    }

    #[test]
    fn aligned_input_still_grows_by_one_block() {
        // Measured: 640 bytes (40 blocks) of zero plaintext -> 656 bytes.
        let ct = encrypt(&KEY, &IV, &[0u8; 640]);
        assert_eq!(ct.len(), 656);
    }

    #[test]
    fn non_aligned_input_pads_only_to_the_boundary() {
        // Measured: 8-byte remainder -> 8 bytes of padding, not a full block.
        let plaintext = vec![0u8; 88_200 % BLOCK + 88_192];
        let ct = encrypt(&KEY, &IV, &plaintext);
        assert_eq!(ct.len() % BLOCK, 0);
    }

    #[test]
    fn round_trip_recovers_the_exact_plaintext_for_every_remainder() {
        for len in 0..=64 {
            let pt: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let ct = encrypt(&KEY, &IV, &pt);
            let back = decrypt_all(&KEY, &IV, &ct).unwrap();
            assert_eq!(back, pt, "len={len}");
        }
    }

    #[test]
    fn counter_crossing_analog_a_multi_kilobyte_plaintext_round_trips() {
        // CBC has no counter to overflow, but a long multi-block plaintext is
        // still the test that would have caught a chaining bug a short one
        // cannot: every block after the first depends on the one before it.
        let pt: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let ct = encrypt(&KEY, &IV, &pt);
        let back = decrypt_all(&KEY, &IV, &ct).unwrap();
        assert_eq!(back, pt);
    }

    #[test]
    fn out_of_range_last_byte_falls_back_to_stripping_one_block() {
        // Measured (second pass, CBC bit-flip controlled): n in 17..=255
        // strips exactly BLOCK bytes regardless of what the other 15 bytes
        // of the block hold.
        for n in [17u8, 20, 100, 255] {
            let mut padded = vec![0u8; 32];
            *padded.last_mut().unwrap() = n;
            assert_eq!(unpad(&padded), 32 - BLOCK, "n={n}");
        }
    }

    #[test]
    fn any_in_range_last_byte_is_trusted_with_no_consistency_check() {
        // Measured (second pass, CBC bit-flip controlled): the reference
        // strips exactly `n` bytes for every `n` in `0..=BLOCK`, even when
        // every *other* byte of the final block is left at an unrelated,
        // non-matching value (`0x10` here) — there is no PKCS#7-style check
        // that the preceding `n - 1` bytes also equal `n`. See the crate
        // docs on `unpad` for the full transcript and the flawed first-pass
        // measurement this corrects.
        for n in 0u8..=16 {
            let mut padded = vec![0x10u8; 32];
            *padded.last_mut().unwrap() = n;
            assert_eq!(
                unpad(&padded),
                32 - usize::from(n),
                "n={n}, even though bytes 16..31 are all 0x10, not {n}"
            );
        }
    }

    #[test]
    fn a_genuine_pkcs7_tail_still_strips_correctly() {
        // The common, valid case: an 8-byte real PKCS#7 tail (8 copies of
        // the value 8) following 8 bytes of real data.
        let mut padded = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        padded.extend(std::iter::repeat_n(8u8, 8));
        assert_eq!(unpad(&padded), 8);
    }

    /// End-to-end version of the CBC bit-flip measurement, through
    /// [`decrypt_all`] rather than [`unpad`] directly — proves the whole
    /// pipeline (raw block decrypt, chaining, padding removal) reproduces
    /// the reference's byte-for-byte, not just the isolated `unpad` unit.
    #[test]
    fn bit_flipping_the_second_to_last_block_controls_only_the_final_byte() {
        let ct = encrypt(&KEY, &IV, &[0u8; 256]); // -> 17 blocks, last is pure 0x10 padding
        let second_to_last = ct.len() - 32;
        let last_byte_idx = 15;

        for (new_n, expect_stripped) in [(0u8, 0usize), (1, 1), (8, 8), (16, 16), (100, 16)] {
            let mut corrupt = ct.clone();
            let flip = 0x10 ^ new_n;
            let pos = second_to_last + last_byte_idx;
            let byte = corrupt.get_mut(pos).unwrap();
            *byte ^= flip;
            let plain = decrypt_all(&KEY, &IV, &corrupt).unwrap();
            // `ct` is 272 bytes (17 blocks): 256 real bytes in blocks 0..15,
            // unaffected by the flip, plus one padding-only block whose
            // contribution changes with `new_n`.
            assert_eq!(plain.len(), ct.len() - expect_stripped, "n={new_n}");
        }
    }

    #[test]
    fn unpad_never_panics_on_adversarial_input() {
        for len in 0..40 {
            let buf = vec![0xffu8; len];
            let n = unpad(&buf);
            assert!(n <= len);
        }
        assert_eq!(unpad(&[]), 0);
    }
}
