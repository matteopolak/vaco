//! [`IpfsProtocol`]/[`IpnsProtocol`] — the `ipfs:`/`ipns:` schemes, gateway
//! discovery (the I/O side of [`crate::gateway`]'s pure resolution), and the
//! registry entries.

use vaco_io::{MediaSink, MediaSource};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

use crate::gateway;
use crate::options::IpfsOptions;

fn options(opts: &Dict) -> Result<IpfsOptions> {
    let mut parsed = IpfsOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "ipfs",
            detail: "bad option value",
        })?;
    Ok(parsed)
}

/// Read an environment variable, treating "unset" and "not valid UTF-8" the
/// same way (`None`) — this protocol only ever compares the value as text.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Discover the gateway per the measured precedence
/// (`-gateway`/`$IPFS_GATEWAY`/`$IPFS_PATH`-file/`$HOME/.ipfs`-file), doing
/// the actual environment/filesystem reads here and handing everything to
/// [`gateway::resolve`] (pure) to decide.
///
/// # Errors
/// [`ProtocolError::Malformed`] if none of the four sources yields a
/// non-empty gateway, naming `scheme` so the message matches whichever of
/// `ipfs`/`ipns` asked.
fn discover_gateway(opts: &IpfsOptions, scheme: &'static str) -> Result<String> {
    let env_gateway = env_var("IPFS_GATEWAY");
    let path_file = env_var("IPFS_PATH")
        .as_deref()
        .and_then(|p| std::fs::read_to_string(gateway::ipfs_path_gateway_file(p)).ok());
    let home_file = env_var("HOME")
        .as_deref()
        .and_then(|h| std::fs::read_to_string(gateway::home_gateway_file(h)).ok());

    gateway::resolve(
        &opts.gateway,
        env_gateway.as_deref(),
        path_file.as_deref(),
        home_file.as_deref(),
    )
    .ok_or(ProtocolError::Malformed {
        scheme,
        detail: "no IPFS gateway configured (set -gateway, $IPFS_GATEWAY, $IPFS_PATH, or $HOME/.ipfs)",
    })
}

/// Shared `open()` body for both schemes: resolve the gateway, build the
/// target HTTP(S) URL, and open it through the *same* [`ProtocolEnv`] one
/// level deeper — the same "nested open through the registry" shape as
/// `vaco-protocol-local`'s `md5:`, not a direct socket dial, since fetching
/// through a gateway is a plain single request/response with nothing duplex
/// about it (unlike `httpproxy:`/`ftp:`/`gopher:`/`icecast:` in this
/// workspace).
///
/// # Errors
/// [`ProtocolError::Malformed`] if `url.rest` (after stripping a leading
/// `//`) is empty (measured: the reference refuses with "A CID must be
/// provided." *before* even attempting gateway discovery) or if no gateway
/// can be found; propagates whatever the nested `http`/`https` open returns
/// otherwise.
fn open_generic(
    scheme: &'static str,
    kind: &'static str,
    url: &Url,
    opts: &Dict,
    env: &ProtocolEnv<'_>,
) -> Result<Box<dyn MediaSource>> {
    let rest = url.rest.strip_prefix("//").unwrap_or(&url.rest);
    if rest.is_empty() {
        return Err(ProtocolError::Malformed {
            scheme,
            detail: "a CID must be provided",
        });
    }
    let parsed = options(opts)?;
    let gw = discover_gateway(&parsed, scheme)?;
    let target = gateway::build_target(&gw, kind, &url.rest);
    // Same `env`, not a fresh one — see `md5:`'s identical reasoning: this
    // keeps depth/whitelist enforcement intact for the nested open rather
    // than bypassing it.
    env.registry.open(&target, IoFlags::READ, &Dict::new(), env)
}

/// The `ipfs:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct IpfsProtocol;

impl Protocol for IpfsProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        open_generic("ipfs", "ipfs", url, opts, env)
    }

    fn create(
        &self,
        _url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        // Input-only: `-h protocol=ipfs` marks `-gateway` `.D.` (decoding)
        // only, and `-protocols` lists `ipfs` under `Input:` alone.
        Err(ProtocolError::Unsupported {
            scheme: "ipfs",
            operation: "writing (ipfs: is an input-only protocol)",
        })
    }
}

/// The `ipns:` protocol. Identical to [`IpfsProtocol`] except the gateway
/// path prefix is `/ipns/` instead of `/ipfs/` — measured with a raw-byte
/// capture the same way.
#[derive(Debug, Clone, Copy, Default)]
pub struct IpnsProtocol;

impl Protocol for IpnsProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        open_generic("ipns", "ipns", url, opts, env)
    }

    fn create(
        &self,
        _url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        Err(ProtocolError::Unsupported {
            scheme: "ipns",
            operation: "writing (ipns: is an input-only protocol)",
        })
    }
}

fn ipfs_schema() -> &'static Schema {
    schema_of::<IpfsOptions>()
}

/// The registry entry for `ipfs:`.
pub static IPFS_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "ipfs",
    long_name: "IPFS Gateway",
    // `-protocols` lists `ipfs` under `Input:` only.
    flags: ProtocolFlags {
        network: true,
        // It opens one further URL — the gateway fetch — so a recursing
        // `-protocol_whitelist` preset needs to know that, the same as
        // `md5:`.
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: false,
    },
    // Measured: `[ipfs @ ...] No default whitelist set`.
    default_whitelist: &[],
    options: Some(ipfs_schema),
    proto: &IpfsProtocol,
};

/// The registry entry for `ipns:`.
pub static IPNS_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "ipns",
    long_name: "IPNS Gateway",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: false,
    },
    // Measured: `[ipns @ ...] No default whitelist set`.
    default_whitelist: &[],
    options: Some(ipfs_schema),
    proto: &IpnsProtocol,
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

    #[test]
    fn default_whitelists_are_empty() {
        assert!(IPFS_PROTOCOL.default_whitelist.is_empty());
        assert!(IPNS_PROTOCOL.default_whitelist.is_empty());
    }

    #[test]
    fn create_is_unsupported_for_both() {
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let url = vaco_protocol_core::split_url("ipfs://QmCid/x");
        let err = IpfsProtocol
            .create(&url, IoFlags::WRITE, &Dict::new(), &env)
            .err()
            .unwrap();
        assert!(matches!(err, ProtocolError::Unsupported { .. }));

        let url = vaco_protocol_core::split_url("ipns://example.com/x");
        let err = IpnsProtocol
            .create(&url, IoFlags::WRITE, &Dict::new(), &env)
            .err()
            .unwrap();
        assert!(matches!(err, ProtocolError::Unsupported { .. }));
    }

    #[test]
    fn empty_rest_is_refused_before_gateway_discovery() {
        // With *no* gateway configured either, this discriminates the two
        // possible orderings by which `Malformed.detail` comes back: the CID
        // message (checked first, correctly) versus "no IPFS gateway
        // configured" (which would fire first if gateway discovery ran
        // before the CID check) — matching the reference's own measured
        // order (its debug log shows "A CID must be provided." with no
        // `$IPFS_GATEWAY is empty.` line at all beforehand, i.e. gateway
        // discovery is never even attempted).
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let url = vaco_protocol_core::split_url("ipfs://");
        let err = IpfsProtocol
            .open(&url, IoFlags::READ, &Dict::new(), &env)
            .err()
            .unwrap();
        match err {
            ProtocolError::Malformed { detail, .. } => {
                assert_eq!(detail, "a CID must be provided");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_gateway_does_not_excuse_a_missing_cid() {
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let url = vaco_protocol_core::split_url("ipfs://");
        let mut opts = Dict::new();
        opts.set("gateway", "http://127.0.0.1:1");
        let err = IpfsProtocol
            .open(&url, IoFlags::READ, &opts, &env)
            .err()
            .unwrap();
        match err {
            ProtocolError::Malformed { detail, .. } => {
                assert_eq!(detail, "a CID must be provided");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
