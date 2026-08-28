//! Three independent surfaces of `vaco-protocol-crypto`, none of which
//! should ever panic regardless of input.
//!
//! 1. [`cipher::unpad`] directly on arbitrary bytes — must return a value no
//!    greater than the input length, for any length including zero.
//! 2. A real `encrypt` → corrupt-with-fuzzer-bytes → `decrypt_all` round
//!    trip — the corruption stands in for a hostile or truncated ciphertext
//!    file; `decrypt_all` must never panic, whatever comes back.
//! 3. `split_url` on the fuzz bytes reinterpreted as a `crypto:`/
//!    `crypto+scheme:`-prefixed string, checking [`vaco_protocol_crypto`]'s
//!    own `Url::nested_url()`-or-`rest` inner-URL construction round-trips
//!    through `split_url` again without panicking or losing bytes it
//!    shouldn't (mirrors `vaco-protocol-core::url`'s own fuzzed invariant,
//!    scoped to the `crypto:`-prefixed subset of the grammar).
//! fuzz-crate: vaco-protocol-crypto
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_core::split_url;
use vaco_protocol_crypto::cipher;

fuzz_target!(|data: &[u8]| {
    // 1. `unpad` on raw bytes.
    let n = cipher::unpad(data);
    assert!(n <= data.len());

    // 2. encrypt a small fixed plaintext, corrupt the ciphertext with the
    // fuzz bytes (XORing them in, bounded to the ciphertext's own length so
    // this never allocates more than the fuzzer's own input), decrypt.
    const KEY: [u8; cipher::BLOCK] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    ];
    const IV: [u8; cipher::BLOCK] = KEY;
    let plaintext = b"the quick brown fox jumps over the lazy dog, thirty-six bytes";
    let mut ct = cipher::encrypt(&KEY, &IV, plaintext);
    for (byte, corrupt) in ct.iter_mut().zip(data.iter()) {
        *byte ^= corrupt;
    }
    let _ = cipher::decrypt_all(&KEY, &IV, &ct);

    // 3. URL grammar: build a `crypto:`-prefixed string from the fuzz bytes
    // (lossy UTF-8 is fine — `split_url` promises to accept any string) and
    // check the round-trip invariant `vaco-protocol-core::url` documents:
    // `split_url(s).to_string() == s`.
    let tail = String::from_utf8_lossy(data);
    for prefix in ["crypto:", "crypto+file:"] {
        let s = format!("{prefix}{tail}");
        let url = split_url(&s);
        assert_eq!(url.to_string(), s, "split_url must round-trip every byte");
    }
});
