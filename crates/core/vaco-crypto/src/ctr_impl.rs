//! Generic AES-CTR keystream application — RFC 3686-vector-tested.
//!
//! CTR mode is symmetric: encryption and decryption are the same XOR
//! operation, so there is one function per key size rather than separate
//! `encrypt`/`decrypt` names.
//!
//! # Scope: the counter block, not how a protocol builds it
//!
//! This module takes the 128-bit initial counter block as a plain
//! `[u8; 16]` and increments the *whole* 128 bits per block (`ctr::Ctr128BE`
//! — textbook CTR). RFC 3686 itself only defines a 32-bit counter field
//! (nonce ‖ IV ‖ 32-bit counter, incrementing just the low 32 bits), but for
//! every one of its own §6 test vectors (at most 3 blocks) incrementing the
//! full 128 bits and incrementing only the low 32 bits produce byte-identical
//! keystreams — the two schemes only diverge after 2^32 blocks (64 GiB),
//! unreachable by any test here or any real packet. `VSF TR-06-2` §7.2's own
//! IV construction (sequence number in the high 4 bytes, 12 zero bytes) has
//! no protected nonce region at all, so it needs the full-128-bit increment
//! this module actually implements, not RFC 3686's narrower one. Building
//! the initial counter block from a nonce or a sequence number is each
//! protocol's own concern, not this module's.

use ctr::Ctr128BE;
use ctr::cipher::{KeyIvInit, StreamCipher};

/// Apply the AES-128-CTR keystream to `data` in place.
pub fn ctr_apply_aes128(key: &[u8; 16], counter_block: &[u8; 16], data: &mut [u8]) {
    Ctr128BE::<aes::Aes128>::new(key.into(), counter_block.into()).apply_keystream(data);
}

/// Apply the AES-192-CTR keystream to `data` in place.
pub fn ctr_apply_aes192(key: &[u8; 24], counter_block: &[u8; 16], data: &mut [u8]) {
    Ctr128BE::<aes::Aes192>::new(key.into(), counter_block.into()).apply_keystream(data);
}

/// Apply the AES-256-CTR keystream to `data` in place.
pub fn ctr_apply_aes256(key: &[u8; 32], counter_block: &[u8; 16], data: &mut [u8]) {
    Ctr128BE::<aes::Aes256>::new(key.into(), counter_block.into()).apply_keystream(data);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A `nonce ‖ iv ‖ counter=1` triple, RFC 3686 §4's `CTRBLK` layout,
    /// assembled into the 16-byte initial counter block this module takes.
    fn counter_block(nonce: &str, iv: &str) -> [u8; 16] {
        let mut block = [0u8; 16];
        let n = hex(nonce);
        let v = hex(iv);
        block[..4].copy_from_slice(&n);
        block[4..12].copy_from_slice(&v);
        block[15] = 1; // big-endian 32-bit counter, starts at 1
        block
    }

    // --- RFC-vector-derived: RFC 3686 §6's own nine test vectors, all
    // three AES key sizes, key/nonce/IV/plaintext/ciphertext quoted exactly
    // (whitespace-normalized only; every byte is the RFC's own).

    #[test]
    fn rfc3686_vector_1_aes128() {
        let key: [u8; 16] = hex("AE6852F8121067CC4BF7A5765577F39E").try_into().unwrap();
        let block = counter_block("00000030", "0000000000000000");
        let mut data = hex("53696E676C6520626C6F636B206D7367"); // "Single block msg"
        ctr_apply_aes128(&key, &block, &mut data);
        assert_eq!(data, hex("E4095D4FB7A7B3792D6175A3261311B8"));
    }

    #[test]
    fn rfc3686_vector_2_aes128() {
        let key: [u8; 16] = hex("7E24067817FAE0D743D6CE1F32539163").try_into().unwrap();
        let block = counter_block("006CB6DB", "C0543B59DA48D90B");
        let mut data = hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F");
        ctr_apply_aes128(&key, &block, &mut data);
        assert_eq!(
            data,
            hex("5104A106168A72D9790D41EE8EDAD388EB2E1EFC46DA57C8FCE630DF9141BE28")
        );
    }

    #[test]
    fn rfc3686_vector_3_aes128() {
        let key: [u8; 16] = hex("7691BE035E5020A8AC6E618529F9A0DC").try_into().unwrap();
        let block = counter_block("00E0017B", "27777F3F4A1786F0");
        let mut data =
            hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F20212223");
        ctr_apply_aes128(&key, &block, &mut data);
        assert_eq!(
            data,
            hex("C1CF48A89F2FFDD9CF4652E9EFDB72D74540A42BDE6D7836D59A5CEAAEF3105325B2072F")
        );
    }

    #[test]
    fn rfc3686_vector_4_aes192() {
        let key: [u8; 24] = hex("16AF5B145FC9F579C175F93E3BFB0EED863D06CCFDB78515").try_into().unwrap();
        let block = counter_block("00000048", "36733C147D6D93CB");
        let mut data = hex("53696E676C6520626C6F636B206D7367");
        ctr_apply_aes192(&key, &block, &mut data);
        assert_eq!(data, hex("4B55384FE259C9C84E7935A003CBE928"));
    }

    #[test]
    fn rfc3686_vector_5_aes192() {
        let key: [u8; 24] = hex("7C5CB2401B3DC33C19E7340819E0F69C678C3DB8E6F6A91A").try_into().unwrap();
        let block = counter_block("0096B03B", "020C6EADC2CB500D");
        let mut data = hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F");
        ctr_apply_aes192(&key, &block, &mut data);
        assert_eq!(
            data,
            hex("453243FC609B23327EDFAAFA7131CD9F8490701C5AD4A79CFC1FE0FF42F4FB00")
        );
    }

    #[test]
    fn rfc3686_vector_6_aes192() {
        let key: [u8; 24] = hex("02BF391EE8ECB159B959617B0965279BF59B60A786D3E0FE").try_into().unwrap();
        let block = counter_block("0007BDFD", "5CBD60278DCC0912");
        let mut data =
            hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F20212223");
        ctr_apply_aes192(&key, &block, &mut data);
        assert_eq!(
            data,
            hex("96893FC55E5C722F540B7DD1DDF7E758D288BC95C69165884536C811662F2188ABEE0935")
        );
    }

    #[test]
    fn rfc3686_vector_7_aes256() {
        let key: [u8; 32] =
            hex("776BEFF2851DB06F4C8A0542C8696F6C6A81AF1EEC96B4D37FC1D689E6C1C104").try_into().unwrap();
        let block = counter_block("00000060", "DB5672C97AA8F0B2");
        let mut data = hex("53696E676C6520626C6F636B206D7367");
        ctr_apply_aes256(&key, &block, &mut data);
        assert_eq!(data, hex("145AD01DBF824EC7560863DC71E3E0C0"));
    }

    #[test]
    fn rfc3686_vector_8_aes256() {
        let key: [u8; 32] =
            hex("F6D66D6BD52D59BB0796365879EFF886C66DD51A5B6A99744B50590C87A23884").try_into().unwrap();
        let block = counter_block("00FAAC24", "C1585EF15A43D875");
        let mut data = hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F");
        ctr_apply_aes256(&key, &block, &mut data);
        assert_eq!(
            data,
            hex("F05E231B3894612C49EE000B804EB2A9B8306B508F839D6A5530831D9344AF1C")
        );
    }

    #[test]
    fn rfc3686_vector_9_aes256() {
        let key: [u8; 32] =
            hex("FF7A617CE69148E4F1726E2F43581DE2AA62D9F805532EDFF1EED687FB54153D").try_into().unwrap();
        let block = counter_block("001CC5B7", "51A51D70A1C11148");
        let mut data =
            hex("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F20212223");
        ctr_apply_aes256(&key, &block, &mut data);
        assert_eq!(
            data,
            hex("EB6C52821D0BBBF7CE7594462ACA4FAAB407DF866569FD07F48CC0B583D6071F1EC0E6B8")
        );
    }

    // --- self-consistency: CTR's own textbook property, applying the
    // keystream twice returns the original (XOR with the same keystream
    // cancels out) -- not spec evidence, just confirming the symmetric-XOR
    // shape this module's single-function-per-key-size API relies on.

    #[test]
    fn applying_the_keystream_twice_recovers_the_plaintext() {
        let key = [0x11u8; 16];
        let block = [0x22u8; 16];
        let original = b"a lossy link is not a reason to give up on delivery".to_vec();
        let mut data = original.clone();
        ctr_apply_aes128(&key, &block, &mut data);
        assert_ne!(data, original);
        ctr_apply_aes128(&key, &block, &mut data);
        assert_eq!(data, original);
    }
}
