//! The wrapping protocols: `cache:`, `subfile:`, `concat:`, `concatf:`,
//! `tee:` and `async:`.
//!
//! # What it is
//!
//! Layer 2. Six protocols that share a crate because they share a shape: each
//! one's whole job is to change how *another* URL behaves, rather than to
//! reach a transport of its own.
//!
//! | Protocol | Wraps | Changes |
//! |---|---|---|
//! | [`subfile`] | one URL | exposes only `[start, end)` of it |
//! | [`concat`]/[`concatf`] | several URLs | reads them back to back as one stream |
//! | [`cache`] | one URL | buffers it, so a forward-only source becomes seekable |
//! | [`tee`] | several URLs | writes the same bytes to all of them |
//! | [`asyncproto`] (`async:`) | one URL | reads it ahead of the caller |
//!
//! # The inner-URL security rule
//!
//! **Every inner URL is opened through the exact [`ProtocolEnv`] this crate's
//! protocol was itself given — never a fresh, unrestricted one.** That is the
//! whole of the rule; there is no additional confinement layered on top of it
//! here, because [`ProtocolEnv`]'s whitelist/blacklist/root/depth gate already
//! is the confinement, and reconstructing it would be exactly the reset
//! privilege check `vaco-protocol-core`'s own docs warn against.
//!
//! The concrete consequence, **measured** against `ffmpeg 8.1` rather than
//! assumed, is what the rest of the project (HLS, DASH, and anything else that
//! opens a URL out of a document another party wrote) needs to know:
//!
//! > **None of these six protocols grants a default whitelist to what they
//! > open.** `ffmpeg -protocol_whitelist cache -i "cache:a.mkv"` is refused
//! > with `Protocol 'file' not on whitelist 'cache'!` — the caller must name
//! > `file` explicitly too, even though `cache:` plainly cannot do anything
//! > without opening its inner URL. The same was measured for `concat:`
//! > (`-protocol_whitelist concat` still refuses the inner `file` open),
//! > `subfile:`, and `async:`. This is the *opposite* of `hls`'s own
//! > preset (documented in `vaco-protocol-file`'s module docs as granting
//! > `http`, `https`, `tls`, `tcp`, `crypto` and deliberately excluding
//! > `file` — rule W3): a playlist protocol grants a curated set because it
//! > *knows* what kind of URL it is about to open, while a generic wrapper
//! > like `cache:` or `concat:` does not, and the reference's answer to that
//! > is to grant nothing at all rather than guess.
//!
//! Every [`vaco_protocol_core::ProtocolDesc`] in this crate therefore sets
//! `default_whitelist: &[]`. `nested_scheme: true` is still set on all of
//! them (they do each open at least one further URL), because that flag only
//! affects whether a `-protocol_whitelist`-style preset *recurses into* this
//! protocol's own grants — and an empty grant is still a real, checkable
//! grant, distinct from not having one to check at all.
//!
//! # `fd:`
//!
//! Not here. See `vaco-protocol-local`'s crate docs: D16 removed it from
//! scope project-wide, not just from that crate.
//!
//! # Configuration
//!
//! * `cache:`'s `read_ahead_limit` — see [`cache::CacheOptions`].
//! * Every other protocol here has no options, matching `ffmpeg -h
//!   protocol=<name>` for `subfile`, `concat`/`concatf`, `tee` and `async`
//!   (measured: `subfile` has `start`/`end`, which live in the URL's own
//!   comma-args rather than as `-h`-visible options; the rest print nothing).
//!
//! # Dependencies
//!
//! `vaco-protocol-core` for the trait and the gate (not ours to change),
//! `vaco-io` for the source/sink traits, `vaco-limits` for `cache:`'s
//! budget-bounded history buffer, `vaco-opts` for `cache:`'s option schema.
//! `async:` uses `std::sync::mpsc` and, on every non-`wasm32` target,
//! `std::thread::spawn` directly — see [`asyncproto`]'s module docs for why
//! that is not routed through `vaco-time` despite what an earlier brief for
//! this crate assumed.

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
