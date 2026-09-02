//! Fetching a [`crate::lock::LockEntry`] into the [`crate::store::Store`].
//!
//! # Network policy
//!
//! Fetching a real conformance corpus needs the internet, and this project's
//! CI and offline builds must not depend on it (see the batch brief's
//! "Network caution"). So a fetch only ever reaches the network when the
//! caller explicitly opts in — either by passing [`NetworkPolicy::Allowed`]
//! directly, or via [`NetworkPolicy::from_env`], which is `Allowed` only when
//! `VACO_CORPUS_NETWORK=1` is set in the environment. Every other case is
//! [`NetworkPolicy::CacheOnly`]: a cache hit still succeeds, and a cache miss
//! fails with [`FetchError::NetworkDisabled`] rather than silently trying the
//! socket anyway.
//!
//! # How it fetches
//!
//! Through [`vaco_protocol_http`], the crate that already owns `ureq` +
//! `rustls` in this workspace (D11 — `cargo xtask owner-gate` fails the build
//! the moment a second crate declares either). This crate never depends on
//! `ureq` directly; it builds a `vaco_protocol_core::ProtocolRegistry` with
//! only `http`/`https` registered and opens the URL through that, the same
//! seam a demuxer's nested protocol opens use.
//!
//! # Verification is fatal, not advisory
//!
//! Plan 13 §2.5.2: "verification failure is fatal — a corpus is a security
//! boundary." A hash mismatch is [`FetchError::HashMismatch`], and the bytes
//! are never adopted into the store under any name (see
//! [`crate::store::Store::put_verified`]).

use std::time::Duration;

use vaco_core::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

use crate::lock::LockEntry;
use crate::store::{ObjectId, Store, StoreError};

/// Whether a fetch may reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// Only ever satisfy a fetch from the local object store.
    CacheOnly,
    /// Reach the network on a cache miss.
    Allowed,
}

impl NetworkPolicy {
    /// `Allowed` iff `VACO_CORPUS_NETWORK=1`; `CacheOnly` otherwise
    /// (including when the variable is unset or set to anything else — this
    /// is opt-in, not opt-out).
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_var(std::env::var("VACO_CORPUS_NETWORK").ok().as_deref())
    }

    /// The pure decision behind [`NetworkPolicy::from_env`], taking the
    /// variable's value directly so it is testable without mutating process
    /// environment (which is `unsafe` to do from safe Rust under the 2024
    /// edition, and this crate forbids `unsafe` unconditionally).
    #[must_use]
    pub fn from_var(value: Option<&str>) -> Self {
        if value == Some("1") {
            Self::Allowed
        } else {
            Self::CacheOnly
        }
    }
}

/// A cap on how many bytes a single fetch will read, independent of any
/// `Content-Length` the server claims — the same "never trust a declared
/// size" discipline `vaco-limits` applies to parsed input applies here to a
/// server response. 512 MiB is far larger than any entry this catalogue
/// names today and small enough to fail fast on a runaway stream.
pub const MAX_FETCH_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug)]
pub enum FetchError {
    /// The entry names no URL or no expected hash (a documented gap; see
    /// `suites.toml`).
    NotFetchable {
        name: String,
    },
    /// A cache miss and the caller did not opt into the network.
    NetworkDisabled {
        name: String,
    },
    /// The response exceeded [`MAX_FETCH_BYTES`].
    TooLarge {
        name: String,
        limit: u64,
    },
    Protocol(vaco_protocol_core::ProtocolError),
    Io(vaco_core::Error),
    Store(StoreError),
    /// `entry.member` named a file [`crate::zip::extract`] could not pull out
    /// of the fetched archive.
    Zip(crate::zip::ZipError),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFetchable { name } => {
                write!(f, "{name}: no url/sha256 on record (documented gap)")
            }
            Self::NetworkDisabled { name } => write!(
                f,
                "{name}: not cached and network fetch is disabled \
                 (set VACO_CORPUS_NETWORK=1 to allow it)"
            ),
            Self::TooLarge { name, limit } => {
                write!(f, "{name}: response exceeded the {limit}-byte fetch cap")
            }
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Store(e) => write!(f, "{e}"),
            Self::Zip(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<StoreError> for FetchError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// A registry with just `http`/`https` — everything a corpus fetch should
/// ever need to reach. Building this fresh per call is cheap: the registry
/// is a `Vec` of `&'static` descriptors (see `vaco-protocol-core`'s own
/// docs), not a connection.
fn http_only_registry() -> ProtocolRegistry {
    let mut registry = ProtocolRegistry::new();
    vaco_protocol_http::register(&mut registry);
    registry
}

/// [`fetch`], then — when `entry.member` names one — extract that path out
/// of the fetched bytes as a ZIP archive member ([`crate::zip::extract`]).
///
/// This is the entry point a conformance case actually wants: `entry` names
/// the *archive* (a JVT/JCT-VC conformance ZIP), verified and cached whole
/// by content hash exactly like every other corpus asset, while `member`
/// says which file inside it is the bitstream a case should be pointed at.
/// An entry with no `member` behaves exactly like [`fetch`] — every
/// pre-existing suite (`pngsuite`, `vp8`/`vp9-test-vectors`,
/// `flac-test-files`) is a bare file, not an archive, and takes this branch.
///
/// # Errors
/// [`FetchError`] as [`fetch`], plus [`FetchError::Zip`] if `entry.member`
/// is set and the fetched bytes are not a well-formed ZIP containing it.
pub fn fetch_asset(
    entry: &LockEntry,
    store: &Store,
    policy: NetworkPolicy,
) -> Result<Vec<u8>, FetchError> {
    let bytes = fetch(entry, store, policy)?;
    match &entry.member {
        Some(member) => crate::zip::extract(&bytes, member).map_err(FetchError::Zip),
        None => Ok(bytes),
    }
}

/// Fetch one entry, using `store` as both the cache and the destination.
///
/// Returns the verified bytes. A cache hit never touches the network
/// regardless of `policy` — the policy only governs what happens on a miss.
///
/// # Errors
/// See [`FetchError`].
pub fn fetch(
    entry: &LockEntry,
    store: &Store,
    policy: NetworkPolicy,
) -> Result<Vec<u8>, FetchError> {
    let (Some(url), Some(expected)) = (&entry.url, &entry.sha256) else {
        return Err(FetchError::NotFetchable {
            name: entry.name.clone(),
        });
    };

    if let Some(bytes) = store.get(expected)? {
        return Ok(bytes);
    }

    if policy == NetworkPolicy::CacheOnly {
        return Err(FetchError::NetworkDisabled {
            name: entry.name.clone(),
        });
    }

    let bytes = download(url, &entry.name)?;
    store.put_verified(expected, &bytes)?;
    Ok(bytes)
}

/// Download `url` in full, bounded by [`MAX_FETCH_BYTES`]. Does not touch the
/// store — callers that need verification-then-store should use [`fetch`];
/// this is exposed separately for callers that want to see what a URL
/// actually serves without an existing lock entry to check it against (e.g.
/// while adding a new catalogue entry).
///
/// # Errors
/// [`FetchError::Protocol`], [`FetchError::Io`], or [`FetchError::TooLarge`].
pub fn download(url: &str, label: &str) -> Result<Vec<u8>, FetchError> {
    let registry = http_only_registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel).with_rw_timeout(Duration::from_secs(30));
    let opts = Dict::new();

    let mut source = registry
        .open(url, IoFlags::READ, &opts, &env)
        .map_err(FetchError::Protocol)?;

    let mut buf = vec![0_u8; 64 * 1024];
    let mut out = Vec::new();
    loop {
        let n = source.read(&mut buf).map_err(FetchError::Io)?;
        if n == 0 {
            break;
        }
        let Some(chunk) = buf.get(..n) else {
            break;
        };
        out.extend_from_slice(chunk);
        if out.len() as u64 > MAX_FETCH_BYTES {
            return Err(FetchError::TooLarge {
                name: label.to_owned(),
                limit: MAX_FETCH_BYTES,
            });
        }
    }
    Ok(out)
}

/// Verify that `bytes` matches `id`, without touching the store. Used by
/// [`fetch`] internally and exposed for callers that already have bytes from
/// somewhere else (a test fixture, a previously downloaded file) and just
/// want the same check.
#[must_use]
pub fn verify(id: &ObjectId, bytes: &[u8]) -> bool {
    ObjectId::of(bytes) == *id
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::{FetchError, NetworkPolicy, fetch, verify};
    use crate::lock::LockEntry;
    use crate::store::{ObjectId, Store};

    fn entry(url: Option<&str>, sha: Option<&str>) -> LockEntry {
        LockEntry {
            name: "t".to_owned(),
            suite: "test".to_owned(),
            url: url.map(str::to_owned),
            sha256: sha.and_then(ObjectId::parse),
            size: None,
            license: "test".to_owned(),
            source: "test".to_owned(),
            targets: vec![],
            member: None,
        }
    }

    #[test]
    fn network_policy_from_var_is_opt_in() {
        assert_eq!(NetworkPolicy::from_var(None), NetworkPolicy::CacheOnly);
        assert_eq!(NetworkPolicy::from_var(Some("1")), NetworkPolicy::Allowed);
        assert_eq!(
            NetworkPolicy::from_var(Some("yes")),
            NetworkPolicy::CacheOnly
        );
        assert_eq!(NetworkPolicy::from_var(Some("")), NetworkPolicy::CacheOnly);
    }

    #[test]
    fn not_fetchable_without_url_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let e = entry(None, None);
        let err = fetch(&e, &store, NetworkPolicy::Allowed).unwrap_err();
        assert!(matches!(err, FetchError::NotFetchable { .. }));
    }

    #[test]
    fn cache_hit_short_circuits_before_any_network_decision() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let id = store.put(b"cached bytes").unwrap();
        let e = entry(Some("http://example.invalid/x"), Some(id.as_str()));
        // CacheOnly would normally refuse a network fetch, but this is a hit.
        let got = fetch(&e, &store, NetworkPolicy::CacheOnly).unwrap();
        assert_eq!(got, b"cached bytes");
    }

    #[test]
    fn cache_miss_under_cache_only_policy_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let id = ObjectId::of(b"not yet fetched");
        let e = entry(Some("http://example.invalid/x"), Some(id.as_str()));
        let err = fetch(&e, &store, NetworkPolicy::CacheOnly).unwrap_err();
        assert!(matches!(err, FetchError::NetworkDisabled { .. }));
    }

    #[test]
    fn verify_matches_and_mismatches() {
        let id = ObjectId::of(b"content");
        assert!(verify(&id, b"content"));
        assert!(!verify(&id, b"different content"));
    }
}
