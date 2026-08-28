# `vaco-protocol-crypto`

Layer 2. `crypto:` — and the one crate in this workspace that declares
`cbc` (D11); `aes` moved to `vaco-crypto` on 2026-08-28.

## What it is

`crypto:<inner-url>` (or `crypto+scheme:rest`) wraps a nested URL with
symmetric encryption: writing through it encrypts before handing bytes to the
inner sink, reading through it decrypts after reading from the inner source.
`-key`/`-iv` (or the direction-specific `-decryption_key`/`-decryption_iv`,
`-encryption_key`/`-encryption_iv`) supply the key material.

**The work package this crate was built from calls the algorithm "AES-CTR".
That is wrong.** Measured against `ffmpeg 8.1` (method below): the algorithm
is **AES-128-CBC with PKCS#7 padding**. `planning/AGENT-CONSTRAINTS.md`'s "a
name in the reference is not a specification" applies directly — the
reference's own CLI/help text never states an algorithm name at all (`-h
protocol=crypto` just says "AES encryption/decryption key"), so "CTR" was
never actually observed; it was assumed from the work package title.

## How it works

### Establishing CBC, not CTR

Encrypt a known plaintext under a known key/IV, both through the reference and
through a hand-rolled Python CTR implementation (`pycryptodome`), and diff:

```
$ ffmpeg -y -key 000102030405060708090a0b0c0d0e0f -iv 000102030405060708090a0b0c0d0e0f \
    -f u8 -i plain.bin -f u8 crypto:file:enc.bin
```

The first block matched a textbook `AES_encrypt(key, iv)` — consistent with
*either* CTR or CBC, since both start the same way. The second block is where
they diverge: recovering the AES input block from known plaintext/ciphertext
(`ciphertext XOR plaintext = keystream`, `AES_decrypt(key, keystream) =
AES-input`) showed block *i*'s AES input equals block *(i-1)*'s **ciphertext**,
not `iv + i`. That is CBC's definition (`AES_input[i] = plaintext[i] XOR
ciphertext[i-1]`), not a counter's.

### Padding is always added, even when already block-aligned

A 256-byte (exactly 16-block) plaintext encrypts to **272 bytes** — a full
extra block of PKCS#7 padding (16 copies of `0x10`), not zero. General rule,
confirmed at several sizes: ciphertext length is always
`⌊plaintext_len / 16⌋·16 + 16`, i.e. `⌊len/16⌋ + 1` blocks, never merely `len`
rounded up.

### Reading back: no PKCS#7 consistency check at all

**First measurement pass (wrong).** Corrupting only the final byte of the
*last ciphertext block* to five different values (`0x00, 0x01, 0x10, 0x11,
0xff`) always produced `original_len - 16`, which looked like "invalid padding
falls back to stripping one block" — consistent with a real PKCS#7 check that
rejects all five. It is not: CBC-decrypting a *modified* ciphertext block
scrambles the **entire** plaintext block (AES's avalanche effect), not just
the targeted byte, so all five trials just happened to decrypt to some byte
`> 16` and hit whatever the true fallback is. Five samples, all landing on the
same wrong side of a boundary, looked like confirmation and was not — exactly
`planning/AGENT-CONSTRAINTS.md`'s "one matching sample is not a passing test".

**Second pass (CBC bit-flip, controlled).** To change exactly one byte of the
*last* plaintext block while leaving every other byte of that block —
including its other 15 padding bytes — untouched, flip a byte of the
*second-to-last* ciphertext block instead (classic CBC malleability:
`plaintext[i] = D(ciphertext[i]) XOR ciphertext[i-1]`, so XORing byte `j` of
`ciphertext[i-1]` XORs byte `j` of `plaintext[i]` directly, with zero effect
on the rest of that block). Holding the other 15 bytes of the final block at
the *wrong*, non-matching value `0x10` and setting only the last byte to `n`:

| `n` (last byte) | bytes stripped |
|---|---|
| 0 | 0 |
| 1 | 1 |
| 5 | 5 |
| 8 | 8 |
| 15 | 15 |
| 16 | 16 |
| 17, 20, 100, 255 | 16 (fallback) |

**Conclusion: the reference reads only the final byte `n` and strips exactly
`n` bytes whenever `n <= 16`; anything larger falls back to stripping a fixed
16.** It never checks that the preceding `n - 1` bytes also equal `n`. See
[`cipher::unpad`](../../crates/io/vaco-protocol-crypto/src/cipher.rs) for the
implementation and both sets of tests (the flawed methodology is kept as a
named historical test/comment, not deleted, since the correction is the
point).

### Key/IV validation and override precedence

`-h protocol=crypto` (ffmpeg 8.1):

```
crypto AVOptions:
  -key               <binary>     ED......... AES encryption/decryption key
  -iv                <binary>     ED......... AES encryption/decryption initialization vector
  -decryption_key    <binary>     .D......... AES decryption key
  -decryption_iv     <binary>     .D......... AES decryption initialization vector
  -encryption_key    <binary>     E.......... AES encryption key
  -encryption_iv     <binary>     E.......... AES encryption key
```

Measured: `-decryption_key`/`-decryption_iv` override `-key`/`-iv` on reads;
`-encryption_key`/`-encryption_iv` override them on writes (set a correct
generic key and a wrong direction-specific one; the wrong one wins).

Key and IV must each be **exactly 16 bytes** (AES-128 only; no AES-192/256):

```
$ ffmpeg ... -key 0001 -iv <hex> -f u8 -i crypto:file:x -f u8 -
[crypto @ ...] invalid decryption key size (2 bytes, block size is 16)

$ ffmpeg ... -iv <hex> -f u8 -i crypto:file:x -f u8 -    # no -key at all
[crypto @ ...] decryption key not set
```

Both message shapes (`invalid {direction} key/IV size (N bytes, block size is
16)` and `{direction} key/IV not set`) are reproduced exactly, with `N` (a
byte count, never secret) but never the key/IV value itself — see the next
section.

### Wrong key: garbage output, never an error

There is no authentication (no MAC) — a wrong key or IV decrypts to garbage
bytes silently, same length rules as above. Confirmed with a round-trip using
a 1-bit-different key: no error, output simply differs from the original
plaintext.

### Direction: `Input:`/`Output:`

`ffmpeg -hide_banner -protocols` lists `crypto` under **both** `Input:` and
`Output:`, so `ProtocolFlags { readable: true, writable: true, .. }`.

### `default_whitelist`: measured empty

```
$ ffmpeg -v debug -key <hex> -iv <hex> -f u8 -i crypto:file:x -f u8 -
[crypto @ ...] No default whitelist set

$ ffmpeg -protocol_whitelist crypto -decryption_key <hex> -decryption_iv <hex> \
    -f u8 -i crypto:file:x -f u8 -
[file @ ...] Protocol 'file' not on whitelist 'crypto'!
```

An explicit whitelist naming only `crypto` does not implicitly grant `file`;
the caller must list both. `default_whitelist: &[]`, matching every other
nested-opening protocol measured so far in this workspace (`tls`, `data`) that
does not hard-code one fixed inner transport.

### Both URL grammars work identically

`crypto:file:x` (bare inner URL as `rest`) and `crypto+file:x` (the `+`-split
form, reassembled via `Url::nested_url`) produce byte-identical ciphertext.
`crypto:x` (no inner scheme at all) resolves `x` as `file:x` via rule U1, the
same as any other bare path.

## How to change it

- `src/cipher.rs` — the CBC engine and [`cipher::unpad`]'s padding rule. If a
  future measurement finds the padding rule wrong again, **falsify it the way
  this file documents**: a CBC bit-flip on the second-to-last ciphertext
  block, not a corruption of the final block itself.
- `src/options.rs` — key/IV parsing and the encryption/decryption override
  precedence. Every error path here is checked (`missing_key_is_reported_...`,
  `wrong_length_key_never_appears_in_the_error`) to never carry the raw key or
  IV bytes into a `Display`ed error — see "Security" below before adding a new
  error path.
- `src/source.rs`/`src/sink.rs` — the streaming `MediaSource`/`MediaSink`
  adapters. The source holds exactly one decrypted-but-unreleased block
  (`held`) so it can tell "not last" (release as-is) from "last" (unpad) —
  see the module docs before changing the buffering shape.
- `src/protocol.rs` — the `Protocol` impl and `CRYPTO_PROTOCOL` descriptor.

## Configuration

`-key`, `-iv`, `-decryption_key`, `-decryption_iv`, `-encryption_key`,
`-encryption_iv` — all hex-encoded, all exactly 16 bytes when present.

## Dependencies

`cbc` (RustCrypto) — this crate is its sole owner (`cargo xtask
owner-gate`). `aes` moved to `vaco-crypto` on 2026-08-28 once a second
consumer (`vaco-protocol-rist`'s PSK/AES-CTR) appeared alongside this
crate's own CBC use — see `docs/core/vaco-crypto.md`. This crate now
reaches `Aes128` through `vaco_crypto::aes::Aes128` rather than declaring
`aes` itself; nothing about the measured-against-`ffmpeg` behaviour below
changed, only the import path (verified: all 24 of this crate's own tests,
unchanged, still pass byte-for-byte).

`cbc`'s declared workspace version (`"0.4"`) did not exist on crates.io — the
highest published version is `0.2.x`. Corrected to `"0.2"` as part of landing
this crate (`Cargo.toml`, one line). `aes`'s declared version (`"0.8"`) also
needed bumping to `"0.9"`, since `cbc 0.2` depends on `cipher 0.5` and `aes
0.8` depends on the incompatible `cipher 0.4` — the two must be upgraded
together. **`ctr`'s own pre-declared version (`"0.9"`) had the identical
problem, undiscovered until `vaco-crypto` actually tried to compose `aes`
and `ctr` together**: `ctr 0.9` depends on `cipher 0.4`, incompatible with
`aes 0.9.2`/`cbc 0.2`'s `cipher 0.5` — invisible as long as nothing used
`ctr` for real. Corrected to `"0.10"` in `vaco-crypto`'s own landing
commit; unlike `aes`, `ctr` still had no consumer here (this crate stays
CBC-only), so nothing in this crate needed to change for that fix.

No `unsafe` in this crate's own code (`#![forbid(unsafe_code)]`, matching
D2); `aes`'s runtime-dispatched AES-NI/ARMv8-crypto backends use `unsafe`
internally, weighed rather than vetoed per D10 — the same treatment already
given to `rustls-rustcrypto` in `vaco-protocol-tls`.

## Testing — what is measured, and what is not

Every algorithmic claim in "How it works" above has a unit test using the
exact key/IV/plaintext from the measurement transcript, so a regression in
`cipher.rs` fails a test with the same shape as the original measurement:

- `block_zero_of_an_all_zero_plaintext_matches_the_reference` — the recovered
  `AES_encrypt(key, iv)` value.
- `aligned_input_still_grows_by_one_block`,
  `non_aligned_input_pads_only_to_the_boundary` — the padding-length rule.
- `any_in_range_last_byte_is_trusted_with_no_consistency_check`,
  `out_of_range_last_byte_falls_back_to_stripping_one_block`,
  `bit_flipping_the_second_to_last_block_controls_only_the_final_byte` — the
  corrected padding-removal rule, via the CBC bit-flip technique.
- `round_trip_recovers_the_exact_plaintext_for_every_remainder` (every length
  0..=64) and `counter_crossing_analog_a_multi_kilobyte_plaintext_round_trips`
  (70,000 bytes) — the distinguishing inputs the brief for this crate asked
  for: longer than one block, not a multiple of the block size, and — CBC has
  no counter to overflow, so the nearest analog to "a counter crossing a
  16-bit boundary" is a plaintext long enough that a single-block chaining
  bug could not survive it (4,375 blocks).
- `options::tests::*` — override precedence and the no-leak guarantee on
  every option-parse error path.
- `protocol::tests::round_trip_through_a_real_temp_file`,
  `seeking_mid_stream_lands_on_the_correct_plaintext_byte` — end to end
  through the real `Protocol` trait and a real `file:` backing store,
  including a seek that lands inside the final (padded) block.
- `a_whitelist_naming_only_crypto_still_refuses_the_nested_file_open` — the
  whitelist boundary.

**Untested, and why:** `crypto:`'s nested transport in every test here is
`file:`, which is local and needs no network — there is nothing this crate
does differently for a network-backed inner transport (it never inspects the
scheme), so no server-dependent behaviour was skipped. What genuinely is
untested: the reference's behaviour when the underlying ciphertext file is
*not* a whole number of AES blocks (this crate returns
[`vaco_core::Error::InvalidData`] rather than guessing; no way to produce such
a file through the reference's own encoder was found), and whether the
reference itself pre-computes an exact `avio_size()` for `crypto:` reads by
peeking at the final block on open (this crate's `CryptoSource::size()` is
deliberately always `None` — see its doc comment — because every measurement
here used sequential reads to true EOF, which do not exercise `size()` at
all).

## Fuzzing

`fuzz/fuzz_targets/protocol_crypto.rs` feeds arbitrary bytes to three
independent surfaces: `cipher::unpad` directly (must never panic or return a
value greater than the input length), a full `encrypt` → corrupt →
`decrypt_all` round trip (must never panic regardless of corruption), and
`vaco_protocol_core::split_url` on `crypto:`/`crypto+scheme:`-prefixed
strings (must never panic, and `inner_url` must reconstruct a URL that
round-trips through `split_url` again). 30 seconds, exit 0, `fuzz/artifacts`
empty.
