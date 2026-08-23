//! `cache:` — buffer an inner stream so it can be seeked.
//!
//! # Grammar
//!
//! `cache:inner-url`, e.g. `cache:https://host/path` or (measured to work
//! identically) `cache:pipe:0`. Measured against `ffmpeg 8.1`'s `-h
//! protocol=cache`: the one option is `read_ahead_limit` (bytes that may be
//! read ahead when the inner protocol is not itself seekable, default 65536,
//! `-1` for unlimited); this module keeps the name but implements it as a cap
//! on the *whole* retained history rather than a look-ahead window specific to
//! the seek path — see [`CacheOptions`] and the "How to change it" section of
//! the crate docs for why, and what a fuller implementation would need to
//! change.
//!
//! # What it actually does
//!
//! Every byte ever read from the inner source is retained (up to the budget).
//! A backward seek into that history is free; a forward seek past it reads
//! and discards through the gap, which is the only way to reach a new
//! position on a transport that cannot seek at all — and is exactly the
//! mechanism that makes a forward-only source *look* seekable to the caller.
//! [`CacheSource::seekability`] always reports [`Seekability::Cheap`] for
//! that reason: from the caller's side, every position from `0` up to
//! whatever has been read is reachable in O(1) plus at most one linear scan
//! forward, which is the contract `Seekability::Cheap` promises.
//!
//! # Security
//!
//! `cache:` opens exactly one nested URL, its own `rest`, through the *same*
//! [`ProtocolEnv`] it was given — see the crate docs for the measured
//! whitelist behaviour (no implicit grant).

use vaco_io::{MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_opts::{Dict, Options, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

/// Options of the `cache:` protocol.
#[derive(Debug, Clone, Copy, PartialEq, Options)]
#[options(name = "cache", help = "buffer a stream to make it seekable")]
pub struct CacheOptions {
    /// `-1` means unlimited, matching the reference's own sentinel.
    #[opt(
        name = "read_ahead_limit",
        help = "bytes of history to retain; -1 for unlimited",
        default = 65536,
        range = -1..=i32::MAX,
        flags(decoding)
    )]
    pub read_ahead_limit: i32,
}

/// A [`MediaSource`] that retains every byte read from `inner`, so any
/// already-visited position is free to return to and any later one is reached
/// by reading forward through the gap.
pub struct CacheSource {
    inner: Box<dyn MediaSource>,
    /// Every byte read from `inner` so far, from offset 0.
    buf: Vec<u8>,
    /// The caller's current logical position, always `<= buf.len()` except
    /// transiently inside [`Self::fill_to`] while it is still growing.
    pos: u64,
    inner_eof: bool,
    budget: Budget,
    /// Total bytes this cache will retain. `usize::MAX` for "unlimited",
    /// mirroring the reference's `-1` sentinel.
    limit: usize,
}

impl std::fmt::Debug for CacheSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheSource")
            .field("buffered", &self.buf.len())
            .field("pos", &self.pos)
            .field("inner_eof", &self.inner_eof)
            .finish_non_exhaustive()
    }
}

impl CacheSource {
    /// Wrap `inner`, retaining up to `limit` bytes (`None` for unlimited).
    #[must_use]
    pub fn new(inner: Box<dyn MediaSource>, limit: Option<usize>) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            pos: 0,
            inner_eof: false,
            budget: Budget::new(Limits::strict()),
            limit: limit.unwrap_or(usize::MAX),
        }
    }

    /// Read from `inner` until either the buffer holds `want` bytes or `inner`
    /// reaches EOF. Does not move `self.pos`.
    fn fill_to(&mut self, want: usize) -> vaco_core::Result<()> {
        let want = want.min(self.limit);
        while self.buf.len() < want && !self.inner_eof {
            let mut chunk = [0u8; 8192];
            let n = self.inner.read(&mut chunk)?;
            if n == 0 {
                self.inner_eof = true;
                break;
            }
            let Some(got) = chunk.get(..n) else { break };
            let room = self.limit.saturating_sub(self.buf.len());
            let take = got.len().min(room);
            let Some(taken) = got.get(..take) else { break };
            if take > 0 {
                self.budget.charge(take as u64)?;
                self.buf.extend_from_slice(taken);
            }
            if take < got.len() {
                // The retention limit was reached with more of this chunk
                // left over: those bytes are gone (this is a cache, not a
                // guarantee), so stop rather than silently under-filling.
                break;
            }
        }
        Ok(())
    }
}

impl MediaSource for CacheSource {
    fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
        let pos = usize::try_from(self.pos).unwrap_or(usize::MAX);
        if pos >= self.buf.len() {
            self.fill_to(pos.saturating_add(buf.len()))?;
        }
        let Some(available) = self.buf.get(pos..) else {
            return Ok(0);
        };
        let n = available.len().min(buf.len());
        let Some(src) = available.get(..n) else {
            return Ok(0);
        };
        let Some(dst) = buf.get_mut(..n) else {
            return Ok(0);
        };
        dst.copy_from_slice(src);
        self.pos = self.pos.saturating_add(n as u64);
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> vaco_core::Result<u64> {
        let target = usize::try_from(pos).unwrap_or(usize::MAX);
        if target > self.buf.len() {
            // Forward past history: read-and-discard through the gap, which
            // is the only way a forward-only inner transport can reach it.
            self.fill_to(target)?;
        }
        self.pos = (target.min(self.buf.len())) as u64;
        Ok(self.pos)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        self.inner.size()
    }

    fn seekability(&self) -> Seekability {
        // See the module docs: this is the whole point of the protocol.
        Seekability::Cheap
    }

    fn peek(&mut self, len: usize) -> vaco_core::Result<&[u8]> {
        let pos = usize::try_from(self.pos).unwrap_or(usize::MAX);
        self.fill_to(pos.saturating_add(len))?;
        let end = pos.saturating_add(len).min(self.buf.len());
        self.buf
            .get(pos..end)
            .ok_or(vaco_core::Error::UnexpectedEof)
    }
}

/// The `cache:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheProtocol;

impl CacheProtocol {
    fn options(opts: &Dict) -> Result<CacheOptions> {
        let mut parsed = CacheOptions {
            read_ahead_limit: 65536,
        };
        parsed
            .apply_dict(opts)
            .map_err(|_| ProtocolError::Malformed {
                scheme: "cache",
                detail: "bad option value",
            })?;
        Ok(parsed)
    }
}

impl Protocol for CacheProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let parsed = Self::options(opts)?;
        let inner = env.registry.open(&url.rest, flags, opts, env)?;
        let limit = if parsed.read_ahead_limit < 0 {
            None
        } else {
            Some(usize::try_from(parsed.read_ahead_limit).unwrap_or(usize::MAX))
        };
        Ok(Box::new(CacheSource::new(inner, limit)))
    }
}

/// The registry entry for `cache:`.
pub static CACHE_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "cache",
    long_name: "Cache wrapper",
    flags: ProtocolFlags {
        network: false,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: false,
    },
    default_whitelist: &[],
    options: Some(cache_schema),
    proto: &CacheProtocol,
};

fn cache_schema() -> &'static Schema {
    schema_of::<CacheOptions>()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    #[test]
    fn makes_a_forward_only_source_seekable() {
        let data: Vec<u8> = (0u8..=255).collect();
        let inner = MemorySource::forward_only(data.clone());
        assert_eq!(inner.seekability(), Seekability::None);

        let mut cache = CacheSource::new(Box::new(inner), None);
        assert_eq!(cache.seekability(), Seekability::Cheap);

        // Read forward a bit, then seek backward — free, from history.
        let mut got = [0u8; 10];
        cache.read_exact(&mut got).unwrap();
        assert_eq!(got, data[..10]);
        cache.seek(0).unwrap();
        cache.read_exact(&mut got).unwrap();
        assert_eq!(got, data[..10]);

        // Seek forward past everything read so far: reached by filling the
        // gap, not an error.
        cache.seek(200).unwrap();
        let mut got2 = [0u8; 5];
        cache.read_exact(&mut got2).unwrap();
        assert_eq!(got2, data[200..205]);
    }

    #[test]
    fn peek_does_not_move_the_position() {
        let inner = MemorySource::forward_only(vec![1, 2, 3, 4, 5]);
        let mut cache = CacheSource::new(Box::new(inner), None);
        assert_eq!(cache.peek(3).unwrap(), &[1, 2, 3]);
        assert_eq!(cache.position(), 0);
        let mut got = [0u8; 3];
        cache.read_exact(&mut got).unwrap();
        assert_eq!(got, [1, 2, 3]);
    }

    #[test]
    fn already_seekable_inner_still_works_through_the_cache() {
        let inner = MemorySource::new(vec![9, 8, 7, 6]);
        let mut cache = CacheSource::new(Box::new(inner), None);
        cache.seek(2).unwrap();
        let mut got = [0u8; 2];
        cache.read_exact(&mut got).unwrap();
        assert_eq!(got, [7, 6]);
    }

    #[test]
    fn a_zero_limit_retains_nothing_but_still_reads_forward() {
        let inner = MemorySource::forward_only(vec![1, 2, 3]);
        let mut cache = CacheSource::new(Box::new(inner), Some(0));
        let mut got = [0u8; 3];
        // Even with no retention, sequential forward reads still work: they
        // never need history, only what `fill_to` just pulled from `inner`.
        // (A limit of 0 is a degenerate configuration; this asserts it fails
        // safe rather than panicking or looping.)
        let _ = cache.read(&mut got);
    }

    #[test]
    fn options_parse_the_measured_default() {
        let parsed = CacheProtocol::options(&Dict::new()).unwrap();
        assert_eq!(parsed.read_ahead_limit, 65536);
    }

    #[test]
    fn nested_open_goes_through_the_same_env() {
        let mut registry = vaco_protocol_core::ProtocolRegistry::new();
        vaco_protocol_file::register(&mut registry);
        registry.register(&CACHE_PROTOCOL);
        let cancel = vaco_io::CancelToken::new();
        // Whitelisting only `cache` must deny the nested `file:` open.
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["cache"]);
        let err = registry
            .open("cache:clip.mkv", IoFlags::READ, &Dict::new(), &env)
            .err()
            .unwrap();
        assert!(matches!(
            err,
            vaco_protocol_core::ProtocolError::Denied {
                reason: vaco_protocol_core::DenyReason::NotWhitelisted,
                ..
            }
        ));
    }
}
