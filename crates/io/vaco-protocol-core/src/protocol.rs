//! The `Protocol` trait and its descriptor.

use vaco_io::{MediaSink, MediaSource};
use vaco_opts::{Dict, Schema};

use crate::{ProtocolEnv, ProtocolError, Result, Url};

/// What an open is for.
///
/// Deliberately a struct of named booleans rather than a bitflag set: every
/// call site reads `IoFlags::read()` rather than a bit-or of constants, and a
/// flag that is not set cannot be confused with one that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "an open mode is a set of independent booleans"
)]
pub struct IoFlags {
    /// Open for reading.
    pub read: bool,
    /// Open for writing.
    pub write: bool,
    /// Append rather than truncate an existing file.
    pub append: bool,
    /// `-avioflags direct`: no buffering.
    pub direct: bool,
    /// `-listen 1`: bind and accept rather than connect.
    pub listen: bool,
}

impl IoFlags {
    /// Read-only.
    pub const READ: Self = Self {
        read: true,
        write: false,
        append: false,
        direct: false,
        listen: false,
    };
    /// Write-only, truncating.
    pub const WRITE: Self = Self {
        read: false,
        write: true,
        append: false,
        direct: false,
        listen: false,
    };
    /// Read and write.
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        append: false,
        direct: false,
        listen: false,
    };
}

/// Static facts about a protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent capability facts"
)]
pub struct ProtocolFlags {
    /// Touches the network. Used by `-protocol_whitelist` presets and by the
    /// `nonetwork` build.
    pub network: bool,
    /// Opens further URLs, so its `default_whitelist` is meaningful.
    pub nested_scheme: bool,
    /// Implements `accept`.
    pub server_capable: bool,
    /// Can be opened for reading. `-protocols` lists it under `Input:`.
    ///
    /// Not derivable from the trait: [`Protocol::open`] is required, so every
    /// implementation has one even when it returns `Unsupported` — `md5` and
    /// `tee` are output-only and still have to implement it. The reference
    /// models the same fact the same way, as a non-null `url_read` pointer
    /// rather than something inferred.
    pub readable: bool,
    /// Can be opened for writing. `-protocols` lists it under `Output:`.
    ///
    /// [`Protocol::create`] has a default implementation returning
    /// `Unsupported`, and an overridden default is not detectable at runtime,
    /// so this is stated rather than derived for the same reason as
    /// [`ProtocolFlags::readable`].
    pub writable: bool,
}

impl ProtocolFlags {
    /// A local transport that opens nothing else: `file`, `pipe`, `data`.
    pub const LOCAL: Self = Self {
        network: false,
        nested_scheme: false,
        server_capable: false,
        readable: true,
        writable: true,
    };
    /// A network transport that opens nothing else: `tcp`, `udp`.
    pub const NETWORK: Self = Self {
        network: true,
        nested_scheme: false,
        server_capable: false,
        readable: true,
        writable: true,
    };
}

/// Whether a URL can be read, written, or both. The `check` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Access {
    /// Readable.
    pub read: bool,
    /// Writable.
    pub write: bool,
}

/// What kind of thing a directory listing found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link. Never followed by a listing.
    Symlink,
    /// Anything else the platform reports.
    Other,
}

/// One entry from [`Protocol::list_dir`].
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Name relative to the listed URL.
    pub name: String,
    /// What it is.
    pub kind: EntryKind,
    /// Size in bytes, when the platform reports one.
    pub size: Option<u64>,
    /// Modification time in **microseconds since the Unix epoch**, when the
    /// platform reports one.
    ///
    /// An integer rather than `std::time::SystemTime` because this is a trait's
    /// data model: a `SystemTime` field obliges every implementer to produce an
    /// OS type, and `wasm32-unknown-unknown` has no wall clock to produce it
    /// from. The coupling would be in the *interface*, where no `cfg` can reach
    /// it. See D18 and `cargo xtask time-gate`.
    ///
    /// Signed, because filesystems really do carry pre-1970 timestamps.
    /// Microseconds because `i64` of them spans ±292,000 years, which is more
    /// range than any directory listing needs and more resolution than any
    /// caller of this will use.
    pub modified: Option<i64>,
}

/// A byte transport reachable by URL.
///
/// Implementations are stateless: `open` produces the state. That is what lets
/// [`ProtocolDesc`] hold a `&'static dyn Protocol` and lets the registry be
/// built without instantiating anything.
pub trait Protocol: Send + Sync {
    /// Open `url` for reading.
    ///
    /// The implementation must route any nested open through
    /// [`ProtocolEnv`] rather than opening a URL itself — that is the
    /// whitelist boundary, and stepping around it is what lets a hostile
    /// playlist read `/etc/passwd`.
    ///
    /// # Errors
    /// Whatever the transport reports, as [`ProtocolError`].
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>>;

    /// Open `url` for writing.
    ///
    /// # Errors
    /// [`ProtocolError::Unsupported`] by default.
    fn create(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let _ = (url, flags, opts, env);
        Err(ProtocolError::Unsupported {
            scheme: "?",
            operation: "create",
        })
    }

    /// Report whether `url` is readable, writable or both.
    ///
    /// # Errors
    /// [`ProtocolError::Unsupported`] by default.
    fn check(&self, url: &Url, env: &ProtocolEnv<'_>) -> Result<Access> {
        let _ = (url, env);
        Err(ProtocolError::Unsupported {
            scheme: "?",
            operation: "check",
        })
    }

    /// List a directory URL.
    ///
    /// Returns a `Vec` rather than an iterator because a listing is small,
    /// borrows nothing, and an iterator would have to name a lifetime the trait
    /// object cannot carry.
    ///
    /// # Errors
    /// [`ProtocolError::Unsupported`] by default.
    fn list_dir(&self, url: &Url, env: &ProtocolEnv<'_>) -> Result<Vec<DirEntry>> {
        let _ = (url, env);
        Err(ProtocolError::Unsupported {
            scheme: "?",
            operation: "list_dir",
        })
    }

    /// Delete a URL.
    ///
    /// # Errors
    /// [`ProtocolError::Unsupported`] by default.
    fn delete(&self, url: &Url, env: &ProtocolEnv<'_>) -> Result<()> {
        let _ = (url, env);
        Err(ProtocolError::Unsupported {
            scheme: "?",
            operation: "delete",
        })
    }

    /// Rename a URL.
    ///
    /// # Errors
    /// [`ProtocolError::Unsupported`] by default.
    fn rename(&self, from: &Url, to: &Url, env: &ProtocolEnv<'_>) -> Result<()> {
        let _ = (from, to, env);
        Err(ProtocolError::Unsupported {
            scheme: "?",
            operation: "rename",
        })
    }
}

/// The registry's view of a protocol: everything knowable without instantiating.
pub struct ProtocolDesc {
    /// The scheme this registers under. A CLI-stable interface fact.
    pub name: &'static str,
    /// One-line description for `-protocols`.
    pub long_name: &'static str,
    /// Capability facts.
    pub flags: ProtocolFlags,
    /// The nested schemes this protocol implicitly grants when it opens further
    /// URLs. `hls` grants http, https, tls, tcp, crypto — and deliberately not
    /// `file` (rule W3).
    pub default_whitelist: &'static [&'static str],
    /// Option schema, for `-h protocol=name`. A function pointer rather than a
    /// reference so the descriptor can be a `static`.
    pub options: Option<fn() -> &'static Schema>,
    /// The implementation.
    pub proto: &'static dyn Protocol,
}

impl std::fmt::Debug for ProtocolDesc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtocolDesc")
            .field("name", &self.name)
            .field("flags", &self.flags)
            .field("default_whitelist", &self.default_whitelist)
            .finish_non_exhaustive()
    }
}
