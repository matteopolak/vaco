//! The `file:` and `pipe:` protocols.
//!
//! [`FileProtocol`] provides a seekable, sized local file, while [`PipeProtocol`]
//! provides a forward-only standard stream. Together they exercise both ends of
//! the `Seekability` model. Both implement [`RawSource`](vaco_io::RawSource),
//! are wrapped in [`PeekSource`](vaco_io::PeekSource), and are buffered by
//! [`IoContext`](vaco_io::IoContext).
//!
//! [`file::FileProtocol`] honours [`ProtocolEnv::root`](vaco_protocol_core::ProtocolEnv)
//! (rule U2): an untrusted URL is opened only when its *canonical* target stays
//! inside the configured root, so a symlink cannot escape it.
//!
//! # Example
//!
//! ```no_run
//! use vaco_io::CancelToken;
//! use vaco_opts::Dict;
//! use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};
//!
//! let mut registry = ProtocolRegistry::new();
//! vaco_protocol_file::register(&mut registry);
//!
//! let cancel = CancelToken::new();
//! let env = ProtocolEnv::new(&registry, &cancel);
//! let src = registry.open("clip.mkv", IoFlags::READ, &Dict::new(), &env)?;
//! # Ok::<(), vaco_protocol_core::ProtocolError>(())
//! ```

#![forbid(unsafe_code)]

pub mod file;
pub mod path;
pub mod pipe;

pub use file::{FILE_PROTOCOL, FileOptions, FileProtocol, FileSink, FileSource};
pub use path::{confine, url_to_path};
pub use pipe::{PIPE_PROTOCOL, PipeProtocol};

use vaco_protocol_core::ProtocolRegistry;

/// Register both protocols.
///
/// `vaco-registry` calls this; so does every test that needs a real file open.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&FILE_PROTOCOL);
    registry.register(&PIPE_PROTOCOL);
}
