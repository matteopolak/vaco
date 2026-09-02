//! `-h protocol=crypto`'s option surface, and key/IV resolution.
//!
//! # Names and shapes, measured against `ffmpeg 8.1`
//!
//! ```text
//! crypto AVOptions:
//!   -key               <binary>     ED......... AES encryption/decryption key
//!   -iv                <binary>     ED......... AES encryption/decryption initialization vector
//!   -decryption_key    <binary>     .D......... AES decryption key
//!   -decryption_iv     <binary>     .D......... AES decryption initialization vector
//!   -encryption_key    <binary>     E.......... AES encryption key
//!   -encryption_iv     <binary>     E.......... AES encryption key
//! ```
//!
//! `-decryption_key`/`-decryption_iv` **override** `-key`/`-iv` for reads,
//! and `-encryption_key`/`-encryption_iv` override them for writes — measured
//! by setting both a correct `-key` and a wrong `-decryption_key` and
//! confirming the wrong one wins (produces different plaintext), and
//! symmetrically for `-encryption_key` on write.
//!
//! # Security: no key material in an error, ever
//!
//! The reference itself does **not** hold to this: `-key ZZ...` (invalid hex)
//! produces `Error setting option key to value ZZ....`, echoing the raw
//! string. Per the brief this crate was built from, that shape is not
//! reproduced — [`resolve`] never puts `key`/`iv`/`decryption_key`/
//! `decryption_iv`/`encryption_key`/`encryption_iv`'s *value* into a
//! [`vaco_protocol_core::ProtocolError`], only byte counts (which are not
//! secret) and field names. This is a deliberate divergence from D17's
//! default "match the reference including its bugs" rule: D17 is about
//! *observable output* (what a differential test compares), and a debug log
//! line containing a symmetric key is a security defect in the tool being
//! measured, not a fact about the format. See the crate docs for the general
//! argument.
//!
//! Sizes the reference *does* report are reproduced exactly:
//! `invalid decryption key size (2 bytes, block size is 16)` for a 1-byte hex
//! string, `decryption key not set` when nothing at all was supplied, and the
//! encryption-side equivalents on write. See [`ResolveError`].

use vaco_opts::{Binary, Options, OptionsExt};
use vaco_protocol_core::{IoFlags, ProtocolError, Result};

use crate::cipher::BLOCK;

/// `-h protocol=crypto`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "crypto", help = "AES-128-CBC transport")]
pub struct CryptoOptions {
    /// Used for both directions unless the direction-specific option below is
    /// also set.
    #[opt(
        name = "key",
        help = "AES encryption/decryption key",
        flags(decoding, encoding)
    )]
    pub key: Binary,
    /// Used for both directions unless the direction-specific option below is
    /// also set.
    #[opt(
        name = "iv",
        help = "AES encryption/decryption initialization vector",
        flags(decoding, encoding)
    )]
    pub iv: Binary,
    /// Overrides `key` when reading.
    #[opt(name = "decryption_key", help = "AES decryption key", flags(decoding))]
    pub decryption_key: Binary,
    /// Overrides `iv` when reading.
    #[opt(
        name = "decryption_iv",
        help = "AES decryption initialization vector",
        flags(decoding)
    )]
    pub decryption_iv: Binary,
    /// Overrides `key` when writing.
    #[opt(name = "encryption_key", help = "AES encryption key", flags(encoding))]
    pub encryption_key: Binary,
    /// Overrides `iv` when writing.
    #[opt(
        name = "encryption_iv",
        help = "AES encryption initialization vector",
        flags(encoding)
    )]
    pub encryption_iv: Binary,
}

/// Parse `opts` into [`CryptoOptions`], redacting any parse failure before it
/// can carry a raw option value anywhere. Mirrors `vaco-protocol-tls`'s
/// `options()` helper exactly, for the same reason.
///
/// # Errors
/// [`ProtocolError::Malformed`] with a fixed, valueless detail string.
pub fn parse(opts: &vaco_opts::Dict) -> Result<CryptoOptions> {
    let mut parsed = CryptoOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "crypto",
            detail: "bad option value",
        })?;
    Ok(parsed)
}

/// A resolved, validated 16-byte key and IV for one direction.
///
/// Deliberately no derived `Debug`: the derived one would print the raw key
/// and IV bytes, and this type exists specifically to carry them past the
/// point where this module's no-leak guarantee applies. The manual impl below
/// redacts both fields instead.
#[derive(Clone, Copy)]
pub struct KeyMaterial {
    pub key: [u8; BLOCK],
    pub iv: [u8; BLOCK],
}

impl std::fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyMaterial").finish_non_exhaustive()
    }
}

/// Resolve `opts` for `flags.write` (write = encrypt, read = decrypt),
/// applying the measured override precedence and exact validation shapes.
///
/// # Errors
/// [`ProtocolError::Malformed`] whose `detail` is one of the reference's own
/// (byte-count-only, never value-carrying) messages: `"{direction} key not
/// set"`, `"{direction} IV not set"`, or `"invalid {direction} key/IV size
/// (N bytes, block size is 16)"`.
pub fn resolve(opts: &CryptoOptions, flags: IoFlags) -> Result<KeyMaterial> {
    let direction = if flags.write {
        "encryption"
    } else {
        "decryption"
    };
    let (key, iv) = if flags.write {
        (
            pick(&opts.encryption_key, &opts.key),
            pick(&opts.encryption_iv, &opts.iv),
        )
    } else {
        (
            pick(&opts.decryption_key, &opts.key),
            pick(&opts.decryption_iv, &opts.iv),
        )
    };

    let key = sized(key, direction, "key")?;
    let iv = sized(iv, direction, "IV")?;
    Ok(KeyMaterial { key, iv })
}

/// The specific option wins over the generic one when it was actually set.
fn pick<'a>(specific: &'a Binary, generic: &'a Binary) -> &'a [u8] {
    if specific.0.is_empty() {
        &generic.0
    } else {
        &specific.0
    }
}

/// Validate `bytes` is exactly [`BLOCK`] long, with the reference's exact
/// (value-free — only byte counts, never the bytes themselves) message
/// shapes for "missing" and "wrong size".
///
/// Routed through [`vaco_core::Error::Option`] (owned `String` fields) rather
/// than [`ProtocolError::Malformed`] (`&'static str`), because the byte-count
/// detail is genuinely dynamic and, unlike the key material itself, not
/// secret — see the module docs.
fn sized(bytes: &[u8], direction: &'static str, field: &'static str) -> Result<[u8; BLOCK]> {
    if bytes.is_empty() {
        return Err(option_error(field, format!("{direction} {field} not set")));
    }
    <[u8; BLOCK]>::try_from(bytes).map_err(|_| {
        option_error(
            field,
            format!(
                "invalid {direction} {} size ({} bytes, block size is {BLOCK})",
                field.to_ascii_lowercase(),
                bytes.len()
            ),
        )
    })
}

fn option_error(name: &str, detail: String) -> ProtocolError {
    vaco_core::Error::Option {
        name: name.to_owned(),
        detail,
    }
    .into()
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
    use vaco_opts::Dict;

    fn dict(pairs: &[(&str, &str)]) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.set(k, v);
        }
        d
    }

    const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f";
    const OTHER_HEX: &str = "ffffffffffffffffffffffffffffffff";

    #[test]
    fn decryption_key_overrides_generic_key_for_reads() {
        let opts = parse(&dict(&[
            ("key", KEY_HEX),
            ("decryption_key", OTHER_HEX),
            ("iv", KEY_HEX),
        ]))
        .unwrap();
        let resolved = resolve(&opts, IoFlags::READ).unwrap();
        assert_eq!(resolved.key, [0xff; BLOCK]);
    }

    #[test]
    fn encryption_key_overrides_generic_key_for_writes() {
        let opts = parse(&dict(&[
            ("key", KEY_HEX),
            ("encryption_key", OTHER_HEX),
            ("iv", KEY_HEX),
        ]))
        .unwrap();
        let resolved = resolve(&opts, IoFlags::WRITE).unwrap();
        assert_eq!(resolved.key, [0xff; BLOCK]);
    }

    #[test]
    fn generic_key_is_used_when_no_specific_one_is_set() {
        let opts = parse(&dict(&[("key", KEY_HEX), ("iv", KEY_HEX)])).unwrap();
        let resolved = resolve(&opts, IoFlags::READ).unwrap();
        assert_eq!(resolved.key, resolved.iv);
    }

    #[test]
    fn missing_key_is_reported_without_echoing_anything() {
        let opts = parse(&dict(&[("iv", KEY_HEX)])).unwrap();
        let err = resolve(&opts, IoFlags::READ).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains(KEY_HEX));
    }

    #[test]
    fn wrong_length_key_never_appears_in_the_error() {
        let opts = parse(&dict(&[("key", "0001"), ("iv", KEY_HEX)])).unwrap();
        let err = resolve(&opts, IoFlags::READ).unwrap_err();
        assert!(!err.to_string().contains("0001"));
    }
}
