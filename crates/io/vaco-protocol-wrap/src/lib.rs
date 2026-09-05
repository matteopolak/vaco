//! The wrapping protocols: `cache:`, `subfile:`, `concat:`, `concatf:`,
//! `tee:` and `async:`.
//!
//! Layer 2: six wrappers change how another URL behaves rather than reaching a
//! transport of their own.
//!
//! | Protocol | Wraps | Changes |
//! |---|---|---|
//! | [`subfile`] | one URL | exposes only `[start, end)` |
//! | [`concat`]/[`concatf`] | several URLs | reads them as one stream |
//! | [`cache`] | one URL | makes a forward-only source seekable |
//! | [`tee`] | several URLs | writes the same bytes to all |
//! | [`asyncproto`] (`async:`) | one URL | reads it ahead of the caller |
//!
//! Every inner URL is opened through the exact [`ProtocolEnv`] supplied to the
//! outer protocol; its whitelist, root, and depth gates are the confinement.
//! Measurements against `ffmpeg 8.1` show generic wrappers grant no default
//! whitelist: `-protocol_whitelist cache -i cache:a.mkv` still refuses the
//! inner `file` open, as do `concat:`, `subfile:`, and `async:`. Each protocol
//! therefore uses `default_whitelist: &[]` and `nested_scheme: true`.
//!
//! `cache:` exposes `read_ahead_limit` via [`cache::CacheOptions`]. Other
//! protocols have no `-h` options; `subfile`'s `start`/`end` are URL arguments.
//! See the module docs for per-protocol grammar and security details.
//!
//! Dependencies are `vaco-protocol-core` for the gate, `vaco-io` for source and
//! sink traits, `vaco-limits` and `vaco-opts` for `cache:`, and standard
//! channels/threads for `async:` on non-`wasm32` targets.

#![forbid(unsafe_code)]

pub mod asyncproto;
pub mod cache;
pub mod concat;
pub mod subfile;
pub mod tee;

pub use asyncproto::{ASYNC_PROTOCOL, AsyncProtocol, AsyncSource};
pub use cache::{CACHE_PROTOCOL, CacheOptions, CacheProtocol, CacheSource};
pub use concat::{
    CONCAT_PROTOCOL, CONCATF_PROTOCOL, ConcatFProtocol, ConcatProtocol, ConcatSource,
};
pub use subfile::{Range, SUBFILE_PROTOCOL, SubfileProtocol, SubfileSource};
pub use tee::{TEE_PROTOCOL, TeeProtocol, TeeSink};

use vaco_protocol_core::ProtocolRegistry;

/// Register all six protocols.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&SUBFILE_PROTOCOL);
    registry.register(&CONCAT_PROTOCOL);
    registry.register(&CONCATF_PROTOCOL);
    registry.register(&CACHE_PROTOCOL);
    registry.register(&TEE_PROTOCOL);
    registry.register(&ASYNC_PROTOCOL);
}
