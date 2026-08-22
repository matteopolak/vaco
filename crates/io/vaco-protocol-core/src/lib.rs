//! The `Protocol` trait, the URL grammar, and the whitelist gate.
//!
//! # What it is
//!
//! Layer 2. Three things that only make sense together:
//!
//! 1. [`split_url`] — a URL splitter for a grammar that is deliberately **not**
//!    RFC 3986, because the reference tool's is not either.
//! 2. [`Protocol`] / [`ProtocolDesc`] / [`ProtocolRegistry`] — stateless
//!    transports, reachable by scheme.
//! 3. [`ProtocolEnv`] — the capability that a nested open must carry, and the
//!    gate that decides whether it is allowed.
//!
//! # The security boundary
//!
//! The third item is the reason this crate exists as a separate layer. A
//! playlist chooses its own URLs, so opening one is a privilege decision, not a
//! plumbing detail. `ProtocolEnv` is threaded down through every level of
//! nesting and never reconstructed; the gate is stated once in [`env`] and
//! applied in exactly one function, [`ProtocolRegistry::resolve`].
//!
//! CI enforces the other half of that (rule W2): no `vaco-demux-*` or
//! `vaco-mux-*` crate may depend on a concrete protocol crate, only on this
//! one. A demuxer that could construct a `FileProtocol` directly would be able
//! to skip the gate.
//!
//! # Example
//!
//! ```
//! use vaco_io::CancelToken;
//! use vaco_protocol_core::{ProtocolEnv, ProtocolRegistry, split_url};
//!
//! let registry = ProtocolRegistry::new();
//! let cancel = CancelToken::new();
//!
//! // A remote playlist grants http and https, and deliberately not file.
//! let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["http", "https"]);
//!
//! assert!(env.check_scheme("https").is_ok());
//! assert!(env.check_scheme("file").is_err());   // rule W3
//!
//! // A bare path is `file`, and only ever `file` (rule U1).
//! assert_eq!(split_url("clip.mkv").effective_scheme(), "file");
//! ```

#![forbid(unsafe_code)]

pub mod env;
pub mod error;
pub mod protocol;
pub mod registry;
pub mod url;

pub use env::{DEFAULT_RECURSION_LIMIT, ProtocolEnv};
pub use error::{DenyReason, ProtocolError, Result};
pub use protocol::{Access, DirEntry, EntryKind, IoFlags, Protocol, ProtocolDesc, ProtocolFlags};
pub use registry::ProtocolRegistry;
pub use url::{DEFAULT_SCHEME, Url, split_url};
