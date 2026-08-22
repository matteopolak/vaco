//! Scheme lookup and dispatch.

use vaco_io::{MediaSink, MediaSource};
use vaco_opts::Dict;

use crate::{
    Access, DirEntry, IoFlags, ProtocolDesc, ProtocolEnv, ProtocolError, Result, Url, split_url,
};

/// The set of protocols this build can reach.
///
/// Descriptors are `&'static` because a protocol is stateless — `open` produces
/// the state — so the registry is a list of pointers to constants and costs
/// nothing to clone into a scheduler thread.
#[derive(Debug, Default, Clone)]
pub struct ProtocolRegistry {
    entries: Vec<&'static ProtocolDesc>,
}

impl ProtocolRegistry {
    /// An empty registry. `vaco-registry` fills it; a test fills it with one
    /// protocol and proves the gate refuses everything else.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add `desc`, replacing any earlier registration of the same name.
    pub fn register(&mut self, desc: &'static ProtocolDesc) {
        if let Some(slot) = self.entries.iter_mut().find(|d| d.name == desc.name) {
            *slot = desc;
        } else {
            self.entries.push(desc);
        }
    }

    /// Look a scheme up. Case-insensitive: schemes are ASCII and URLs get
    /// upper-cased by copy-paste more often than anyone would like.
    #[must_use]
    pub fn find(&self, scheme: &str) -> Option<&'static ProtocolDesc> {
        self.entries
            .iter()
            .copied()
            .find(|d| d.name.eq_ignore_ascii_case(scheme))
    }

    /// Every registered name, in registration order. Backs `-protocols`.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|d| d.name)
    }

    /// How many protocols are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolve a scheme through the gate, returning the descriptor and the
    /// environment its implementation must use for nested opens.
    ///
    /// Every entry point goes through here, so there is exactly one place the
    /// whitelist can be bypassed and it is this function.
    ///
    /// # Errors
    /// [`ProtocolError::Denied`] when the gate refuses; [`ProtocolError::Unknown`]
    /// when nothing is registered under the scheme.
    pub fn resolve<'e>(
        &self,
        url: &Url,
        env: &ProtocolEnv<'e>,
    ) -> Result<(&'static ProtocolDesc, ProtocolEnv<'e>)> {
        let scheme = url.effective_scheme();
        // Gate first: an unknown scheme that is also blacklisted must report
        // the denial, not the absence, so probing the registry through error
        // messages tells an attacker nothing.
        env.check_scheme(scheme)?;
        let desc = self.find(scheme).ok_or_else(|| ProtocolError::Unknown {
            scheme: scheme.to_owned(),
        })?;
        Ok((desc, env.descend(desc)))
    }

    /// Split, gate and open `url` for reading.
    ///
    /// # Errors
    /// As [`ProtocolRegistry::resolve`], plus whatever the protocol reports.
    pub fn open(
        &self,
        url: &str,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let parsed = split_url(url);
        self.open_parsed(&parsed, flags, opts, env)
    }

    /// [`ProtocolRegistry::open`] for an already-split URL.
    ///
    /// # Errors
    /// As [`ProtocolRegistry::open`].
    pub fn open_parsed(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let (desc, child) = self.resolve(url, env)?;
        desc.proto.open(url, flags, opts, &child)
    }

    /// Split, gate and open `url` for writing.
    ///
    /// # Errors
    /// As [`ProtocolRegistry::open`].
    pub fn create(
        &self,
        url: &str,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let parsed = split_url(url);
        let (desc, child) = self.resolve(&parsed, env)?;
        desc.proto.create(&parsed, flags, opts, &child)
    }

    /// Report access rights for `url`.
    ///
    /// # Errors
    /// As [`ProtocolRegistry::open`].
    pub fn check(&self, url: &str, env: &ProtocolEnv<'_>) -> Result<Access> {
        let parsed = split_url(url);
        let (desc, child) = self.resolve(&parsed, env)?;
        desc.proto.check(&parsed, &child)
    }

    /// List a directory URL.
    ///
    /// # Errors
    /// As [`ProtocolRegistry::open`].
    pub fn list_dir(&self, url: &str, env: &ProtocolEnv<'_>) -> Result<Vec<DirEntry>> {
        let parsed = split_url(url);
        let (desc, child) = self.resolve(&parsed, env)?;
        desc.proto.list_dir(&parsed, &child)
    }

    /// Delete a URL.
    ///
    /// # Errors
    /// As [`ProtocolRegistry::open`].
    pub fn delete(&self, url: &str, env: &ProtocolEnv<'_>) -> Result<()> {
        let parsed = split_url(url);
        let (desc, child) = self.resolve(&parsed, env)?;
        desc.proto.delete(&parsed, &child)
    }

    /// Rename a URL.
    ///
    /// # Errors
    /// As [`ProtocolRegistry::open`].
    pub fn rename(&self, from: &str, to: &str, env: &ProtocolEnv<'_>) -> Result<()> {
        let a = split_url(from);
        let b = split_url(to);
        let (desc, child) = self.resolve(&a, env)?;
        // Both ends of a rename are opens; the destination is gated too.
        env.check_scheme(b.effective_scheme())?;
        desc.proto.rename(&a, &b, &child)
    }
}
