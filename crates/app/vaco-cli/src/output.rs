//! Opening an output: URL conventions, then the protocol layer.
//!
//! Mirrors `input.rs`'s protocol setup on the write side — same registry, same
//! per-call [`vaco_protocol_core::ProtocolEnv`] — so `-protocol_whitelist`-class
//! decisions are made the same way on both sides of a run, once an output side
//! has options to carry them (`OutputSpec` does not yet; see
//! `docs/app/vaco-cli.md`).
//!
//! # `-` is stdout, not a file named `-`
//!
//! [`vaco_protocol_core::url::split_url`]'s rule U1 sends a bare, unqualified
//! name through the `file` scheme — deliberately, so no configuration can turn
//! an unqualified name into a network fetch. `-` is exactly such a bare name,
//! so left alone it would create a file literally called `-` in the working
//! directory. The reference special-cases `-` to its pipe protocol before the
//! generic URL machinery ever sees it, and [`normalize`] is that same
//! special case, at the same boundary, for output only (`input.rs` will need
//! the mirror image whenever an input `-` is exercised end to end).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vaco_core::{Error, Result};
use vaco_io::{CancelToken, MediaSink};
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolError, ProtocolRegistry};

/// `-` means stdout for an output URL. Anything else passes through unchanged.
#[must_use]
pub fn normalize(url: &str) -> &str {
    if url == "-" { "pipe:1" } else { url }
}

/// Open `url` for writing: `file:` and `pipe:` registered, same as
/// [`crate::input::open`]'s read side.
///
/// # Errors
///
/// Whatever the protocol reported, unwrapped so the caller can read the
/// `io::ErrorKind` the way [`crate::exit::AvError::of`] expects — `ENOENT` for
/// a missing directory, `EACCES` for a read-only one, and so on.
pub fn create(url: &str) -> Result<Box<dyn MediaSink>> {
    let mut protocols = vaco_registry::protocol_registry();
    vaco_protocol_file::register(&mut protocols);
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&protocols, &cancel);
    create_with(&protocols, &env, url)
}

fn create_with(
    protocols: &ProtocolRegistry,
    env: &ProtocolEnv<'_>,
    url: &str,
) -> Result<Box<dyn MediaSink>> {
    protocols
        .create(normalize(url), IoFlags::WRITE, &Dict::new(), env)
        .map_err(|e| match e {
            ProtocolError::Io(inner) => inner,
            other => Error::Option {
                name: "output".to_owned(),
                detail: other.to_string(),
            },
        })
}

/// Wraps a real sink to record the furthest position ever written.
///
/// This is what the reference's `muxing overhead` line divides by (measured
/// against `ffmpeg 8.1`; see `exec::summary_line`'s docs) — and it has to be
/// the *high-water mark*, not [`MediaSink::position`] read once at the end.
/// A muxer that seeks back to patch a header (every seekable-output container
/// this workspace writes does this at least once, for its size or duration
/// fields) leaves the final position wherever that last patch landed, which
/// can be well before the last byte the file actually contains.
pub struct HighWaterSink {
    inner: Box<dyn MediaSink>,
    high_water: Arc<AtomicU64>,
}

impl core::fmt::Debug for HighWaterSink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HighWaterSink")
            .field("high_water", &self.high_water.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl HighWaterSink {
    #[must_use]
    pub fn new(inner: Box<dyn MediaSink>) -> Self {
        Self {
            inner,
            high_water: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A handle to the running high-water mark, read after the muxer — which
    /// by then owns this sink — has finished with it.
    #[must_use]
    pub fn high_water(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.high_water)
    }
}

impl MediaSink for HighWaterSink {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.inner.write(buf)?;
        let pos = self.inner.position();
        self.high_water.fetch_max(pos, Ordering::Relaxed);
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        self.inner.seek(pos)
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_bare_dash_becomes_stdout() {
        assert_eq!(normalize("-"), "pipe:1");
        assert_eq!(normalize("out.mkv"), "out.mkv");
        assert_eq!(normalize("pipe:2"), "pipe:2");
    }

    #[test]
    fn the_high_water_mark_survives_a_seek_back() {
        let sink = vaco_format_core::vacoraw::MemorySink::new();
        let mut counting = HighWaterSink::new(Box::new(sink));
        let hw = counting.high_water();
        counting.write(&[0u8; 16]).unwrap();
        assert_eq!(hw.load(Ordering::Relaxed), 16);
        // A header patch: seek back, rewrite a few bytes, and stop there
        // without returning to the end — exactly what a size field rewrite
        // looks like.
        counting.seek(0).unwrap();
        counting.write(&[0u8; 4]).unwrap();
        assert_eq!(
            hw.load(Ordering::Relaxed),
            16,
            "the mark must not fall back to the patch's position"
        );
    }
}
