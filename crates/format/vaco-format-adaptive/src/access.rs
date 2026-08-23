//! Owned protocol access: the pieces `HlsDemuxer`/`DashDemuxer` need to open
//! a segment, a sub-playlist, or an MPD `Period`'s next manifest by string,
//! kept alive for the whole demuxer's lifetime rather than borrowed for one
//! call.
//!
//! Originally written inside `vaco-demux-hls` and moved here once
//! `vaco-demux-dash` needed the identical thing: opening more URLs after the
//! top-level manifest, under the same whitelist, for the life of the
//! demuxer, is not an HLS-specific need at all. See [`crate::provider`] for
//! the sibling case (a nested container demuxer/muxer) this crate already
//! generalised the same way.
//!
//! [`vaco_protocol_core::ProtocolEnv`] is deliberately borrow-shaped — built
//! fresh at the top of a call and threaded down (see its own docs on why a
//! reconstructed environment would be a reset privilege check). A demuxer
//! that has to *store* the capability to open more URLs across many
//! `read_packet` calls therefore needs an owned equivalent to rebuild an
//! `env` from each time; that is what [`RemoteAccess`] is.

use vaco_core::{CancelToken, Result};
use vaco_io::MediaSource;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry, Url, split_url};

/// Everything an HLS/DASH demuxer needs, owned, to keep opening nested URLs
/// after the manifest itself was opened.
#[derive(Debug, Clone)]
pub struct RemoteAccess {
    pub registry: ProtocolRegistry,
    /// `-protocol_whitelist`. Rule W3: a manifest fetched over a network
    /// protocol should not carry this as `None` (unrestricted) — the caller
    /// assembling this is responsible for that policy; this struct enforces
    /// whatever it is given, not a specific default.
    pub whitelist: Option<Vec<String>>,
    pub blacklist: Option<Vec<String>>,
    pub root: Option<std::path::PathBuf>,
    pub recursion_limit: u32,
    pub cancel: CancelToken,
    /// How deep this demuxer's own nesting is beneath whatever opened the
    /// manifest itself (0 for a top-level `-i playlist.m3u8`).
    pub depth: u32,
}

impl RemoteAccess {
    /// The most permissive configuration: everything the registry knows, no
    /// whitelist, unrestricted `file` root. Fine for a URL the user typed
    /// directly (the CLI's own top-level default) and for tests; **wrong**
    /// for a manifest fetched from the network, which must call
    /// [`RemoteAccess::for_remote_manifest`] instead (rule W3).
    #[must_use]
    pub fn unrestricted(registry: ProtocolRegistry) -> Self {
        Self {
            registry,
            whitelist: None,
            blacklist: None,
            root: None,
            recursion_limit: vaco_protocol_core::DEFAULT_RECURSION_LIMIT,
            cancel: CancelToken::new(),
            depth: 0,
        }
    }

    /// The right default for a manifest reached over `http`/`https`: grants
    /// exactly the schemes a nested HLS/DASH open legitimately needs and
    /// excludes `file` (rule W3), so a hostile playlist cannot read the local
    /// filesystem of whatever process opened it.
    #[must_use]
    pub fn for_remote_manifest(registry: ProtocolRegistry) -> Self {
        Self {
            whitelist: Some(
                ["http", "https", "tls", "tcp", "crypto"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            ),
            ..Self::unrestricted(registry)
        }
    }

    fn env(&self) -> (Vec<&str>, Vec<&str>) {
        let w = self
            .whitelist
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let b = self
            .blacklist
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();
        (w, b)
    }

    /// Open `url_str` for reading, through the whitelist gate.
    ///
    /// # Errors
    /// Whatever [`vaco_protocol_core::ProtocolRegistry::resolve`] or
    /// [`vaco_protocol_core::Protocol::open`] report — most importantly
    /// [`vaco_protocol_core::ProtocolError::Denied`] when the gate refuses,
    /// converted through the blanket `From` into [`vaco_core::Error`].
    pub fn open(&self, url_str: &str) -> Result<Box<dyn MediaSource>> {
        let url: Url = split_url(url_str);
        let (whitelist, blacklist) = self.env();
        let mut env = ProtocolEnv::new(&self.registry, &self.cancel);
        if self.whitelist.is_some() {
            env = env.with_whitelist(&whitelist);
        }
        if self.blacklist.is_some() {
            env = env.with_blacklist(&blacklist);
        }
        if let Some(root) = &self.root {
            env = env.with_root(root);
        }
        env.recursion_limit = self.recursion_limit;
        env.depth = self.depth;
        let (desc, next_env) = self.registry.resolve(&url, &env)?;
        Ok(desc
            .proto
            .open(&url, IoFlags::READ, &Dict::new(), &next_env)?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_file_url_opens_when_whitelisted_and_registered() {
        let mut registry = ProtocolRegistry::new();
        registry.register(&vaco_protocol_file::FILE_PROTOCOL);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.txt");
        std::fs::write(&path, b"hello").unwrap();
        let access = RemoteAccess::unrestricted(registry);
        let mut src = access.open(path.to_str().unwrap()).unwrap();
        let mut buf = [0u8; 5];
        src.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn a_remote_manifests_default_whitelist_excludes_file() {
        let mut registry = ProtocolRegistry::new();
        registry.register(&vaco_protocol_file::FILE_PROTOCOL);
        let access = RemoteAccess::for_remote_manifest(registry);
        let result = access.open("/etc/passwd");
        // W3: file is not on {http,https,tls,tcp,crypto}. `Box<dyn MediaSource>`
        // is not `Debug`, so match rather than `unwrap_err`.
        let Err(err) = result else {
            panic!("expected the file open to be denied");
        };
        assert!(format!("{err}").to_lowercase().contains("unsupported"));
    }
}
