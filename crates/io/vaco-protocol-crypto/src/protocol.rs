//! [`CryptoProtocol`] — the `crypto:` scheme, and the registry entry.
//!
//! # Direction: `-protocols` lists `crypto` under both `Input:` and `Output:`
//!
//! ```text
//! $ ffmpeg -hide_banner -protocols
//! Input:
//!   ...
//!   crypto
//!   ...
//! Output:
//!   crypto
//!   ...
//! ```
//!
//! so [`ProtocolFlags::readable`] and [`ProtocolFlags::writable`] are both
//! `true` — this crate implements both [`Protocol::open`] and
//! [`Protocol::create`].
//!
//! # `default_whitelist` is empty — measured, not assumed
//!
//! `crypto:` opens a nested URL, so its `default_whitelist` is the grant a
//! caller gets "for free" without an explicit `-protocol_whitelist` entry
//! naming the inner scheme. Measured against `ffmpeg 8.1`:
//!
//! ```text
//! $ ffmpeg -v debug -i "crypto:file:x" -key <hex> -iv <hex> -f null -
//! [crypto @ ...] No default whitelist set
//!
//! $ ffmpeg -protocol_whitelist crypto -decryption_key <hex> -decryption_iv <hex> \
//!     -f u8 -i "crypto:file:x" -f u8 -
//! [file @ ...] Protocol 'file' not on whitelist 'crypto'!
//! ```
//!
//! An explicit whitelist naming only `crypto` does **not** implicitly grant
//! `file` — the caller must list both. So `default_whitelist: &[]`, the same
//! as `tls`, `data`, and every other nested-opening protocol this workspace
//! has measured so far that does not itself hard-code a fixed inner
//! transport.

use vaco_io::{MediaSink, MediaSource};
use vaco_opts::{Dict, Schema, schema_of};
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolFlags, Result, Url,
};

use crate::options::{self, CryptoOptions};
use crate::sink::CryptoSink;
use crate::source::CryptoSource;

/// The inner URL `crypto:` (or `crypto+scheme:`) delegates to.
///
/// Handles both grammars `vaco-protocol-core::url` provides for exactly this
/// case: `crypto:file:x` (`url.rest == "file:x"`, used as-is — including the
/// bare-path form `crypto:x`, which resolves to `file:x` through rule U1 the
/// same way a bare top-level path does) and `crypto+file:x`
/// (`url.nested_url()` reassembles `"file:x"`). Measured: both forms produce
/// byte-identical ciphertext against the reference.
fn inner_url(url: &Url) -> String {
    url.nested_url().unwrap_or_else(|| url.rest.clone())
}

fn options(opts: &Dict) -> Result<CryptoOptions> {
    options::parse(opts)
}

/// The `crypto:` protocol: AES-128-CBC over a nested URL.
#[derive(Debug, Clone, Copy, Default)]
pub struct CryptoProtocol;

impl Protocol for CryptoProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let parsed = options(opts)?;
        // Option validation happens before the nested open — measured: a
        // missing `-decryption_key` reports "decryption key not set" even
        // when the inner URL would otherwise be denied by the whitelist, so
        // the same ordering is reproduced here rather than opening first.
        let material = options::resolve(&parsed, flags)?;
        let inner = env.registry.open(&inner_url(url), IoFlags::READ, opts, env)?;
        Ok(Box::new(CryptoSource::new(inner, material)))
    }

    fn create(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let parsed = options(opts)?;
        let material = options::resolve(&parsed, flags)?;
        let inner = env
            .registry
            .create(&inner_url(url), IoFlags::WRITE, opts, env)?;
        Ok(Box::new(CryptoSink::new(inner, material)))
    }
}

fn crypto_schema() -> &'static Schema {
    schema_of::<CryptoOptions>()
}

/// The registry entry for `crypto:`.
pub static CRYPTO_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "crypto",
    long_name: "AES-128-CBC encryption/decryption",
    // `-protocols` lists `crypto` under both `Input:` and `Output:` — see the
    // module docs.
    flags: ProtocolFlags {
        network: false,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: true,
    },
    // Measured empty — see the module docs for the exact transcript.
    default_whitelist: &[],
    options: Some(crypto_schema),
    proto: &CryptoProtocol,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;
    use vaco_io::CancelToken;
    use vaco_protocol_core::{ProtocolRegistry, split_url};

    fn env<'a>(registry: &'a ProtocolRegistry, cancel: &'a CancelToken) -> ProtocolEnv<'a> {
        ProtocolEnv::new(registry, cancel)
    }

    fn registry_with_file_and_crypto() -> ProtocolRegistry {
        let mut r = ProtocolRegistry::new();
        r.register(&CRYPTO_PROTOCOL);
        r.register(&vaco_protocol_file::FILE_PROTOCOL);
        r
    }

    const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f";

    #[test]
    fn round_trip_through_a_real_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.bin");
        let path_str = path.to_str().unwrap();

        let registry = registry_with_file_and_crypto();
        let cancel = CancelToken::new();
        let e = env(&registry, &cancel);

        let mut opts = Dict::new();
        opts.set("key", KEY_HEX);
        opts.set("iv", KEY_HEX);

        let url = split_url(&format!("crypto:{path_str}"));
        let plaintext: Vec<u8> = (0..500u32).map(|i| (i % 251) as u8).collect();

        {
            let mut sink = CRYPTO_PROTOCOL
                .proto
                .create(&url, IoFlags::WRITE, &opts, &e)
                .unwrap();
            sink.write(&plaintext).unwrap();
            sink.flush().unwrap();
        }

        let mut source = CRYPTO_PROTOCOL
            .proto
            .open(&url, IoFlags::READ, &opts, &e)
            .unwrap();
        let mut back = Vec::new();
        let mut buf = [0u8; 37]; // deliberately not block-aligned
        loop {
            let n = source.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            back.extend_from_slice(buf.get(..n).unwrap());
        }
        assert_eq!(back, plaintext);
    }

    #[test]
    fn seeking_mid_stream_lands_on_the_correct_plaintext_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.bin");
        let path_str = path.to_str().unwrap();

        let registry = registry_with_file_and_crypto();
        let cancel = CancelToken::new();
        let e = env(&registry, &cancel);
        let mut opts = Dict::new();
        opts.set("key", KEY_HEX);
        opts.set("iv", KEY_HEX);

        let url = split_url(&format!("crypto:{path_str}"));
        // Long enough to span several blocks, and not a multiple of BLOCK,
        // exercising the padded final block too.
        let plaintext: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();

        {
            let mut sink = CRYPTO_PROTOCOL
                .proto
                .create(&url, IoFlags::WRITE, &opts, &e)
                .unwrap();
            sink.write(&plaintext).unwrap();
            sink.flush().unwrap();
        }

        let mut source = CRYPTO_PROTOCOL
            .proto
            .open(&url, IoFlags::READ, &opts, &e)
            .unwrap();

        // Seek to a handful of positions, including inside the final
        // (padded) block, and check the byte stream from there matches.
        for &pos in &[0u64, 1, 15, 16, 17, 500, 991, 999] {
            source.seek(pos).unwrap();
            let mut buf = [0u8; 5];
            let n = source.read(&mut buf).unwrap();
            let expected = plaintext.get(pos as usize..).unwrap_or(&[]);
            let expected = expected.get(..n).unwrap_or(expected);
            assert_eq!(buf.get(..n).unwrap(), expected, "seek to {pos}");
        }
    }

    #[test]
    fn the_plus_form_and_the_colon_form_agree() {
        let url_colon = split_url("crypto:file:secret.bin");
        let url_plus = split_url("crypto+file:secret.bin");
        assert_eq!(inner_url(&url_colon), inner_url(&url_plus));
    }

    #[test]
    fn default_whitelist_is_empty() {
        assert!(CRYPTO_PROTOCOL.default_whitelist.is_empty());
    }

    #[test]
    fn a_whitelist_naming_only_crypto_still_refuses_the_nested_file_open() {
        let registry = registry_with_file_and_crypto();
        let cancel = CancelToken::new();
        let e = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["crypto"]);

        let mut opts = Dict::new();
        opts.set("key", KEY_HEX);
        opts.set("iv", KEY_HEX);
        let url = split_url("crypto:file:nonexistent.bin");
        let err = CRYPTO_PROTOCOL
            .proto
            .open(&url, IoFlags::READ, &opts, &e)
            .err().unwrap();
        assert!(matches!(
            err,
            vaco_protocol_core::ProtocolError::Denied { .. }
        ));
    }

    #[test]
    fn missing_key_is_reported_before_the_nested_open_is_attempted() {
        // Measured ordering: option validation happens first, so a missing
        // key is reported even for a nested URL that does not exist and even
        // under a whitelist that would otherwise deny it.
        let registry = registry_with_file_and_crypto();
        let cancel = CancelToken::new();
        let e = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["crypto"]);
        let mut opts = Dict::new();
        opts.set("iv", KEY_HEX);
        let url = split_url("crypto:file:nonexistent.bin");
        let err = CRYPTO_PROTOCOL
            .proto
            .open(&url, IoFlags::READ, &opts, &e)
            .err().unwrap();
        // Not `Denied` (the whitelist), and not an I/O error about a missing
        // file — the option error, reported without ever reaching either.
        assert!(!matches!(
            err,
            vaco_protocol_core::ProtocolError::Denied { .. }
        ));
    }
}
