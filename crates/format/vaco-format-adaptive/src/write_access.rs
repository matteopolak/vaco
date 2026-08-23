//! Owned protocol write-access: the mux-side mirror of [`crate::access::RemoteAccess`].
//!
//! An HLS or DASH muxer writes many files — the manifest, each segment, an
//! optional master/companion playlist, an optional fMP4 init segment — so it
//! needs the same "keep the capability alive across many calls" shape the
//! demux side needs for reading. See [`crate::access`]'s docs for why this
//! cannot be `ProtocolEnv` itself, and why it moved here.

use vaco_core::{CancelToken, Result};
use vaco_io::MediaSink;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry, Url, split_url};

/// Everything an HLS/DASH muxer needs, owned, to open and finalise output
/// files after the primary sink was opened by its caller.
#[derive(Debug, Clone)]
pub struct WriteAccess {
    pub registry: ProtocolRegistry,
    pub whitelist: Option<Vec<String>>,
    pub blacklist: Option<Vec<String>>,
    pub root: Option<std::path::PathBuf>,
    pub recursion_limit: u32,
    pub cancel: CancelToken,
}

impl WriteAccess {
    /// Everything the registry knows, no whitelist — the right default for
    /// local output (the CLI's own top-level case) and for tests.
    #[must_use]
    pub fn unrestricted(registry: ProtocolRegistry) -> Self {
        Self {
            registry,
            whitelist: None,
            blacklist: None,
            root: None,
            recursion_limit: vaco_protocol_core::DEFAULT_RECURSION_LIMIT,
            cancel: CancelToken::new(),
        }
    }

    fn lists(&self) -> (Vec<&str>, Vec<&str>) {
        (
            self.whitelist
                .as_ref()
                .map(|v| v.iter().map(String::as_str).collect())
                .unwrap_or_default(),
            self.blacklist
                .as_ref()
                .map(|v| v.iter().map(String::as_str).collect())
                .unwrap_or_default(),
        )
    }

    fn env<'a>(&'a self, whitelist: &'a [&'a str], blacklist: &'a [&'a str]) -> ProtocolEnv<'a> {
        let mut env = ProtocolEnv::new(&self.registry, &self.cancel);
        if self.whitelist.is_some() {
            env = env.with_whitelist(whitelist);
        }
        if self.blacklist.is_some() {
            env = env.with_blacklist(blacklist);
        }
        if let Some(root) = &self.root {
            env = env.with_root(root);
        }
        env.recursion_limit = self.recursion_limit;
        env
    }

    /// Create `url_str` for writing, truncating any existing content.
    ///
    /// # Errors
    /// Whatever `vaco_protocol_core::ProtocolRegistry::resolve` or
    /// `vaco_protocol_core::Protocol::create` report.
    pub fn create(&self, url_str: &str) -> Result<Box<dyn MediaSink>> {
        let url: Url = split_url(url_str);
        let (whitelist, blacklist) = self.lists();
        let env = self.env(&whitelist, &blacklist);
        let (desc, next_env) = self.registry.resolve(&url, &env)?;
        Ok(desc
            .proto
            .create(&url, IoFlags::WRITE, &Dict::new(), &next_env)?)
    }

    /// Rename `from` to `to` — used for `hls_flags temp_file`.
    ///
    /// # Errors
    /// As [`WriteAccess::create`]; falls back to a no-op failure the caller
    /// can choose to ignore when the protocol has no rename (e.g. `http`).
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from_url = split_url(from);
        let to_url = split_url(to);
        let (whitelist, blacklist) = self.lists();
        let env = self.env(&whitelist, &blacklist);
        let (desc, next_env) = self.registry.resolve(&from_url, &env)?;
        Ok(desc.proto.rename(&from_url, &to_url, &next_env)?)
    }

    /// Delete `url_str` — used for `hls_flags delete_segments`. Best-effort:
    /// a caller wanting `delete_segments` to be advisory rather than fatal
    /// should not propagate this with `?`.
    ///
    /// # Errors
    /// As [`WriteAccess::create`].
    pub fn delete(&self, url_str: &str) -> Result<()> {
        let url = split_url(url_str);
        let (whitelist, blacklist) = self.lists();
        let env = self.env(&whitelist, &blacklist);
        let (desc, next_env) = self.registry.resolve(&url, &env)?;
        Ok(desc.proto.delete(&url, &next_env)?)
    }

    /// Read `url_str` back — used for `hls_flags append_list` to recover an
    /// existing playlist's numbering. Best-effort: a missing file is a
    /// legitimate "nothing to append to" and this returns `None` for it
    /// rather than propagating.
    #[must_use]
    pub fn read_to_string(&self, url_str: &str) -> Option<String> {
        let url = split_url(url_str);
        let (whitelist, blacklist) = self.lists();
        let env = self.env(&whitelist, &blacklist);
        let (desc, next_env) = self.registry.resolve(&url, &env).ok()?;
        let mut src = desc
            .proto
            .open(&url, IoFlags::READ, &Dict::new(), &next_env)
            .ok()?;
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let bytes = crate::read_all_bounded(&mut *src, &mut budget, 16 << 20).ok()?;
        String::from_utf8(bytes).ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creates_and_overwrites_a_local_file() {
        let mut registry = ProtocolRegistry::new();
        registry.register(&vaco_protocol_file::FILE_PROTOCOL);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let access = WriteAccess::unrestricted(registry);
        let mut sink = access.create(path.to_str().unwrap()).unwrap();
        sink.write(b"hello").unwrap();
        sink.flush().unwrap();
        drop(sink);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");

        let mut sink = access.create(path.to_str().unwrap()).unwrap();
        sink.write(b"hi").unwrap();
        sink.flush().unwrap();
        drop(sink);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"hi",
            "create must truncate, not append"
        );
    }

    #[test]
    fn read_to_string_recovers_what_was_written() {
        let mut registry = ProtocolRegistry::new();
        registry.register(&vaco_protocol_file::FILE_PROTOCOL);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        std::fs::write(&path, "existing content").unwrap();
        let access = WriteAccess::unrestricted(registry);
        assert_eq!(
            access.read_to_string(path.to_str().unwrap()),
            Some("existing content".to_owned())
        );
        assert_eq!(access.read_to_string("/nonexistent/path/x"), None);
    }
}
