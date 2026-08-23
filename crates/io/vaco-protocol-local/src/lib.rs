//! `data:` and `md5:`, plus the base64 codec `data:` needs.
//!
//! # What it is
//!
//! Layer 2. Two small, unrelated local protocols that share this crate because
//! neither is large enough to be its own and neither wraps another URL — see
//! `vaco-protocol-wrap` for the ones that do.
//!
//! * [`data`] — RFC 2397 data URLs, with the reference's own measured
//!   divergences from the RFC (no percent-decoding, a case-sensitive `base64`
//!   flag, a stricter content-type rule). Read-only.
//! * [`md5`] — an output protocol that discards every byte written to it and
//!   emits an MD5 digest of the whole stream when writing finishes.
//!   Write-only, and **not** the same thing as the `-f md5` muxer — see the
//!   module docs for the measured difference.
//!
//! # `fd:` is not here, deliberately
//!
//! Plan 18 §2.4 originally scoped an `fd:` protocol into this PR. **D16**
//! (`planning/00-decisions.md`) later found that estimate assumed an `unsafe`
//! escape hatch D2 does not grant: turning an integer into an owned file
//! descriptor needs `FromRawFd::from_raw_fd`, and nothing proves the integer
//! names a descriptor this process actually owns. D16's decision is `fd:` is
//! not implemented at all, full stop — not even restricted to a small safe
//! subset the way `pipe:` is restricted to descriptors 0/1/2 in
//! `vaco-protocol-file`. This crate implements exactly what D16 leaves in
//! scope: `data:` and `md5:`.
//!
//! # Security
//!
//! Neither protocol opens a further URL on its own initiative from untrusted
//! input: `data:`'s entire content is the URL string itself, and `md5:`'s one
//! nested open (its destination) is a path the *caller* wrote, not something
//! read out of a document `md5:` parsed. Both still route through
//! [`ProtocolEnv`] rather than the OS directly, so a caller that *does* want
//! to confine them (rule U2) or gate them (the whitelist) can.
//!
//! # Example
//!
//! ```
//! use vaco_io::CancelToken;
//! use vaco_opts::Dict;
//! use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};
//!
//! let mut registry = ProtocolRegistry::new();
//! vaco_protocol_local::register(&mut registry);
//!
//! let cancel = CancelToken::new();
//! let env = ProtocolEnv::new(&registry, &cancel);
//! let mut src = registry.open("data:,hello", IoFlags::READ, &Dict::new(), &env)?;
//! let mut buf = [0u8; 5];
//! src.read_exact(&mut buf)?;
//! assert_eq!(&buf, b"hello");
//! # Ok::<(), vaco_protocol_core::ProtocolError>(())
//! ```

#![forbid(unsafe_code)]

pub mod base64;
pub mod data;
pub mod md5;

pub use data::{DATA_PROTOCOL, DataProtocol};
pub use md5::{MD5_PROTOCOL, Md5Protocol, Md5Sink};

use vaco_protocol_core::ProtocolRegistry;

/// Register both protocols.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&DATA_PROTOCOL);
    registry.register(&MD5_PROTOCOL);
}
