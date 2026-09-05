//! Byte sources and sinks, deliberately separate from `std::io::Read + Seek`:
//! media demuxers need seekability, size, and non-consuming prefix reads.
//!
//! # The layers
//!
//! [`RawSource`] is one thin call per transport syscall. [`PeekSource`] adds the
//! prefix window, and [`MediaSource`] / [`MediaSink`] are the object-safe
//! protocol interfaces. [`IoContext`] / [`IoWriter`] own buffering, typed
//! readers, short seeks, and checksums; demuxers never receive a bare source.
//!
//! # Why `peek` is a trait method
//!
//! Probing must inspect a prefix without consuming it, including on a pipe.
//! Seek-read-seek cannot do that, so [`MediaSource::peek`] is a capability. Both
//! [`MediaSource`] and [`IoContext`] preserve position across peeks, verified by
//! a property test against a forward-only source.
//!
//! # Allocation
//!
//! Every buffer uses [`vaco_limits::Budget`], including the read buffer, peek
//! window, and [`DynBuf`]; `clippy.toml` bans raw `Vec::with_capacity`.
//!
//! # Example
//!
//! ```
//! use vaco_io::{IoContext, IoOptions, MemorySource};
//! let src = MemorySource::forward_only(b"RIFF\x24\x00\x00\x00WAVE".to_vec());
//! let mut io = IoContext::new(Box::new(src), &IoOptions::default())?;
//! assert_eq!(&io.peek(4)?[..4], b"RIFF");
//! assert_eq!(io.pos(), 0);
//! assert_eq!(&io.tag()?, b"RIFF");
//! assert_eq!(io.rl32()?, 0x24);
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

mod checksum;
mod ctx;
mod dynbuf;
mod memory;
mod raw;
mod writer;

pub use checksum::{Checksum, ChecksumKind};
pub use ctx::{DEFAULT_BLOCK_SIZE, IoContext, IoOptions};
pub use dynbuf::{DynBuf, SharedDynBuf};
pub use memory::MemorySource;
pub use raw::{PeekSource, RawSource, ReaderSource, WriterSink};
/// Re-exported from `vaco-core`, where it now lives.
///
/// This crate defined its own until D19's duplicate audit: `vaco-codec-core`
/// had a byte-for-byte identical one, and a transcode holding both meant
/// "stop" cancelled whichever half the caller reached for. The re-export keeps
/// `vaco_io::CancelToken` spelling the same thing it always did.
pub use vaco_core::CancelToken;
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
