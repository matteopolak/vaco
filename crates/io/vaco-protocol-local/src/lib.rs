//! `data:` and `md5:`, plus the base64 codec `data:` needs.
//!
//! [`data`] implements RFC 2397 with measured strictness: no percent-decoding,
//! a case-sensitive `base64` flag, and a stricter content-type rule. [`md5`]
//! discards written bytes and emits a digest; it is a protocol, not the `-f md5`
//! muxer.
//!
//! The `fd:` protocol is intentionally absent. Turning an integer into an
//! owned descriptor would require an unsafe ownership assumption; unlike
//! `pipe:`, there is no safe restricted subset to expose here.
//!
//! # Security
//!
//! Neither protocol opens a URL from untrusted document content. `md5:`'s
//! caller-supplied destination still goes through [`ProtocolEnv`], so its
//! confinement and whitelist checks apply.
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
