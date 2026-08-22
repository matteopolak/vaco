//! Byte sources and sinks.
//!
//! Deliberately not `std::io::Read + Seek`. A media source must answer questions
//! std's traits cannot express — is seeking cheap or does it cost a network round
//! trip; is the total size known; can we read a prefix without consuming it — and
//! demuxers make very different choices depending on the answers.

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
