//! Byte sources and sinks.
//!
//! Deliberately not `std::io::Read + Seek`. A media source must answer questions
//! std's traits cannot express — is seeking cheap or does it cost a network round
//! trip; is the total size known; can we read a prefix without consuming it — and
//! demuxers make very different choices depending on the answers.
//!
//! # The three layers
//!
//! | Layer | Type | Job |
//! |---|---|---|
//! | transport | [`RawSource`] | one thin call per syscall: read, seek, size |
//! | source | [`MediaSource`] / [`MediaSink`] | the frozen object-safe interface every protocol produces |
//! | context | [`IoContext`] / [`IoWriter`] | buffering, byte-order readers, short seeks, checksums |
//!
//! A protocol crate implements [`RawSource`] and wraps it in [`PeekSource`],
//! which supplies the peek buffer that [`MediaSource::peek`] requires. Demuxers
//! never see a bare source: they get an [`IoContext`], which owns the real
//! 32 KiB buffer and all the typed readers.
//!
//! # Why `peek` is a trait method
//!
//! Format probing has to look at a prefix without consuming it, **and it has to
//! work on a pipe**. Seek-read-seek cannot do that, so peeking is a capability
//! of the source rather than a pattern callers implement. Both [`MediaSource`]
//! and [`IoContext`] guarantee that a `peek` leaves the position untouched, and
//! that guarantee is checked by a property test against a forward-only source.
//!
//! # Allocation
//!
//! Every buffer in this crate is sized through a [`vaco_limits::Budget`]. That
//! includes the read buffer (whose size can come from a URL option), the peek
//! window (whose size comes from `probesize`) and [`DynBuf`] (which a muxer
//! grows from packet payloads). `clippy.toml` bans raw `Vec::with_capacity` to
//! force exactly this.
//!
//! # Example
//!
//! ```
//! use vaco_io::{IoContext, IoOptions, MemorySource};
//!
//! let src = MemorySource::forward_only(b"RIFF\x24\x00\x00\x00WAVE".to_vec());
//! let mut io = IoContext::new(Box::new(src), &IoOptions::default())?;
//!
//! // Probing a non-seekable source: look, then decide.
//! assert_eq!(&io.peek(4)?[..4], b"RIFF");
//! assert_eq!(io.pos(), 0);              // peek consumed nothing
//! assert_eq!(&io.tag()?, b"RIFF");
//! assert_eq!(io.rl32()?, 0x24);
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

mod cancel;
mod checksum;
mod ctx;
mod dynbuf;
mod memory;
mod raw;
mod writer;

pub use cancel::CancelToken;
pub use checksum::{Checksum, ChecksumKind};
pub use ctx::{DEFAULT_BLOCK_SIZE, IoContext, IoOptions};
pub use dynbuf::{DynBuf, SharedDynBuf};
pub use memory::MemorySource;
pub use raw::{PeekSource, RawSource, ReaderSource, WriterSink};
pub use writer::{DataMarker, IoWriter};

use vaco_core::{Error, Result};

/// Where a source's bytes come from, which determines what a demuxer may assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seekability {
    /// Seeking is a local operation. Index building and two-pass reads are fine.
    Cheap,
    /// Seeking works but costs a round trip. Prefer sequential access.
    Expensive,
    /// Forward-only: a pipe or a live stream. The demuxer must work in one pass.
    None,
}

/// A readable media source.
pub trait MediaSource: Send {
    /// Read into `buf`, returning the number of bytes read. Zero means EOF.
    ///
    /// # Errors
    /// Propagates transport failure.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Fill `buf` completely.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if the source ends first.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut n = 0;
        while n < buf.len() {
            let Some(rest) = buf.get_mut(n..) else {
                return Err(Error::UnexpectedEof);
            };
            match self.read(rest)? {
                0 => return Err(Error::UnexpectedEof),
                k => n += k,
            }
        }
        Ok(())
    }

    /// # Errors
    /// [`Error::NotSeekable`] when [`MediaSource::seekability`] is `None`.
    fn seek(&mut self, pos: u64) -> Result<u64>;

    fn position(&self) -> u64;

    fn size(&self) -> Option<u64> {
        None
    }

    fn seekability(&self) -> Seekability;

    /// Peek up to `len` bytes without advancing the position.
    ///
    /// Format probing needs this, and it must work even on a pipe — which is why
    /// it is a first-class method rather than seek-read-seek.
    ///
    /// # Errors
    /// Propagates transport failure.
    fn peek(&mut self, len: usize) -> Result<&[u8]>;
}

/// A writable media sink.
pub trait MediaSink: Send {
    /// # Errors
    /// Propagates transport failure.
    fn write(&mut self, buf: &[u8]) -> Result<()>;

    /// # Errors
    /// [`Error::NotSeekable`] for non-seekable output.
    ///
    /// Muxers use this to rewrite a header once final sizes are known; a muxer
    /// that must support non-seekable output has to be structured to avoid it.
    fn seek(&mut self, pos: u64) -> Result<u64>;

    fn position(&self) -> u64;

    fn is_seekable(&self) -> bool;

    /// # Errors
    /// Propagates transport failure.
    fn flush(&mut self) -> Result<()>;
}

/// Reconstruct an error so a sticky failure can be replayed to every later call.
///
/// [`Error`] is not `Clone` — it wraps `std::io::Error` — so a context that must
/// keep returning the same failure re-manufactures an equivalent value. The kind
/// and message survive; the original `source()` chain does not.
pub(crate) fn replay(e: &Error) -> Error {
    match e {
        Error::InvalidData(w) => Error::InvalidData(w),
        Error::UnexpectedEof => Error::UnexpectedEof,
        Error::Eof => Error::Eof,
        Error::NeedMoreInput => Error::NeedMoreInput,
        Error::OutputPending => Error::OutputPending,
        Error::Unsupported(w) => Error::Unsupported(w),
        Error::LimitExceeded {
            limit,
            requested,
            cap,
        } => Error::LimitExceeded {
            limit,
            requested: *requested,
            cap: *cap,
        },
        Error::Option { name, detail } => Error::Option {
            name: name.clone(),
            detail: detail.clone(),
        },
        Error::Io(e) => Error::Io(std::io::Error::new(e.kind(), e.to_string())),
        Error::NotSeekable => Error::NotSeekable,
        other => Error::Io(std::io::Error::other(other.to_string())),
    }
}
