//! The `file:` and `pipe:` protocols.
//!
//! # What it is
//!
//! The two protocols every other part of the system assumes exist: a local file
//! (seekable, sized, the reference case for `Seekability::Cheap`) and a
//! standard stream (forward-only, unsized, the reference case for
//! `Seekability::None`). Between them they exercise both ends of the
//! seekability model, which is why they are built together.
//!
//! # How it works
//!
//! Neither type implements [`MediaSource`](vaco_io::MediaSource) directly. Both
//! implement the thin [`RawSource`](vaco_io::RawSource) — one call per syscall —
//! and are wrapped in [`PeekSource`](vaco_io::PeekSource), which supplies the
//! peek window. Buffering happens once more, higher up, in
//! [`IoContext`](vaco_io::IoContext). Three layers, each with one job, and no
//! byte copied twice in the steady state.
//!
//! # Security
//!
//! [`file::FileProtocol`] honours [`ProtocolEnv::root`](vaco_protocol_core::ProtocolEnv)
//! (rule U2): when a caller opens a URL it did not write — a `concat` list, a
//! local HLS playlist — it names a root, and the open is refused if the
//! *canonical* target falls outside it. Canonical, so a symlink out of the root
//! is refused rather than followed.
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
