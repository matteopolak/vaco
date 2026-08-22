//! The transport layer, and the adapter that turns it into a [`MediaSource`].
//!
//! A protocol has no business implementing a peek buffer. It implements
//! [`RawSource`] — one method per syscall — and wraps the result in
//! [`PeekSource`], which supplies the small unread-bytes window that
//! [`MediaSource::peek`] needs and nothing else. The real 32 KiB read buffer
//! lives one level up, in [`IoContext`](crate::IoContext), so a source and a
//! context do not each hold a copy of the same bytes.

use std::io::{Read, Write};

use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};

use crate::{MediaSink, MediaSource, Seekability};

/// One thin call per transport operation.
///
/// Everything defaults to the most restrictive answer, so a forward-only
/// transport implements exactly one method.
pub trait RawSource: Send {
    /// Read into `buf`. `Ok(0)` is EOF; short reads are normal.
    ///
    /// # Errors
    /// Propagates transport failure.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Move to absolute byte offset `pos`, returning the position reached.
    ///
    /// # Errors
    /// [`Error::NotSeekable`] by default.
    fn seek(&mut self, pos: u64) -> Result<u64> {
        let _ = pos;
        Err(Error::NotSeekable)
    }

    /// Total size when it is genuinely known. Never a guess.
    fn size(&self) -> Option<u64> {
        None
    }

    /// What this transport can do. Answered from what the protocol knows, never
    /// by attempting a seek and seeing what happens.
    fn seekability(&self) -> Seekability {
        Seekability::None
    }
}

/// Wraps any `Read` as a forward-only [`RawSource`].
///
/// This is how `pipe:`, stdin and an [`std::io::PipeReader`] become media
/// sources without any of them re-implementing the trait.
pub struct ReaderSource<R> {
    inner: R,
}

impl<R: Read + Send> ReaderSource<R> {
    /// Wrap `inner`.
    pub const fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Recover the wrapped reader.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R> std::fmt::Debug for ReaderSource<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReaderSource").finish_non_exhaustive()
    }
}

impl<R: Read + Send> RawSource for ReaderSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            return match self.inner.read(buf) {
                Ok(n) => Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(Error::from(e)),
            };
        }
    }
}

/// Wraps any `Write` as a non-seekable [`MediaSink`].
pub struct WriterSink<W> {
    inner: W,
    pos: u64,
}

impl<W: Write + Send> WriterSink<W> {
    /// Wrap `inner`, treating the current offset as zero.
    pub const fn new(inner: W) -> Self {
        Self { inner, pos: 0 }
    }

    /// Recover the wrapped writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W> std::fmt::Debug for WriterSink<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterSink")
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl<W: Write + Send> MediaSink for WriterSink<W> {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.inner.write_all(buf)?;
        self.pos = self.pos.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        let _ = pos;
        Err(Error::NotSeekable)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        false
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()?;
        Ok(())
    }
}

/// Adds the peek window a [`MediaSource`] must provide to any [`RawSource`].
///
/// The window holds only bytes that were read from the transport but not yet
/// consumed by the caller. It is empty in steady state: after probing finishes,
/// reads go straight through, so wrapping costs nothing per byte.
///
/// The window is sized through a [`Budget`], because the peek length comes from
/// `probesize`, which comes from the command line or a URL.
pub struct PeekSource<R> {
    raw: R,
    /// Bytes read ahead. `window[head..]` is unconsumed.
    window: Vec<u8>,
    head: usize,
    /// Logical position of `window[head]`, i.e. what the caller sees.
    pos: u64,
    eof: bool,
    budget: Budget,
    max_peek: usize,
}

impl<R> std::fmt::Debug for PeekSource<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeekSource")
            .field("pos", &self.pos)
            .field("buffered", &(self.window.len() - self.head))
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl<R: RawSource> PeekSource<R> {
    /// Wrap `raw` with a peek window bounded by [`Limits::strict`].
    pub fn new(raw: R) -> Self {
        Self::with_limits(raw, Limits::strict())
    }

    /// Wrap `raw`, bounding the peek window by `limits.max_probe_bytes`.
    pub fn with_limits(raw: R, limits: Limits) -> Self {
        let max_peek = usize::try_from(limits.max_probe_bytes).unwrap_or(usize::MAX);
        Self {
            raw,
            window: Vec::new(),
            head: 0,
            pos: 0,
            eof: false,
            budget: Budget::new(limits),
            max_peek,
        }
    }

    /// Start the logical position at `pos` rather than zero. Used by protocols
    /// that open a byte range.
    #[must_use]
    pub const fn with_start(mut self, pos: u64) -> Self {
        self.pos = pos;
        self
    }

    /// The wrapped transport.
    pub const fn get_ref(&self) -> &R {
        &self.raw
    }

    fn buffered(&self) -> usize {
        self.window.len().saturating_sub(self.head)
    }

    /// Drop consumed bytes from the front of the window.
    fn compact(&mut self) {
        if self.head == 0 {
            return;
        }
        if self.head >= self.window.len() {
            self.window.clear();
        } else {
            self.window.drain(..self.head);
        }
        self.head = 0;
    }

    /// Grow the window to hold `want` unconsumed bytes, charging the budget.
    fn grow_to(&mut self, want: usize) -> Result<()> {
        if want <= self.window.capacity() {
            return Ok(());
        }
        let extra = want.saturating_sub(self.window.capacity());
        self.budget.charge(extra as u64)?;
        self.window
            .try_reserve(want.saturating_sub(self.window.len()))
            .map_err(|_| Error::LimitExceeded {
                limit: "peek_window",
                requested: want as u64,
                cap: self.max_peek as u64,
            })?;
        Ok(())
    }
}

impl<R: RawSource> MediaSource for PeekSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let have = self.buffered();
        if have > 0 {
            let n = have.min(buf.len());
            let src = self
                .window
                .get(self.head..self.head + n)
                .ok_or(Error::UnexpectedEof)?;
            let dst = buf.get_mut(..n).ok_or(Error::UnexpectedEof)?;
            dst.copy_from_slice(src);
            self.head += n;
            self.pos = self.pos.saturating_add(n as u64);
            if self.head >= self.window.len() {
                self.window.clear();
                self.head = 0;
            }
            return Ok(n);
        }
        let n = self.raw.read(buf)?;
        if n == 0 {
            self.eof = true;
        }
        self.pos = self.pos.saturating_add(n as u64);
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        // A seek inside the window is free, and every box parser overshoots.
        let start = self.pos;
        let end = start.saturating_add(self.buffered() as u64);
        if pos >= start && pos <= end {
            let delta = (pos - start) as usize;
            self.head += delta;
            self.pos = pos;
            if self.head >= self.window.len() {
                self.window.clear();
                self.head = 0;
            }
            return Ok(pos);
        }
        if self.raw.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let actual = self.raw.seek(pos)?;
        self.window.clear();
        self.head = 0;
        self.eof = false;
        self.pos = actual;
        Ok(actual)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        self.raw.size()
    }

    fn seekability(&self) -> Seekability {
        self.raw.seekability()
    }

    fn peek(&mut self, len: usize) -> Result<&[u8]> {
        if len > self.max_peek {
            return Err(Error::LimitExceeded {
                limit: "max_probe_bytes",
                requested: len as u64,
                cap: self.max_peek as u64,
            });
        }
        if self.buffered() < len {
            self.compact();
            self.grow_to(len)?;
            while self.window.len() < len && !self.eof {
                let base = self.window.len();
                self.window.resize(len, 0);
                let dst = self.window.get_mut(base..len).ok_or(Error::UnexpectedEof)?;
                match self.raw.read(dst) {
                    Ok(0) => {
                        self.eof = true;
                        self.window.truncate(base);
                    }
                    Ok(n) => self.window.truncate(base + n),
                    Err(e) => {
                        self.window.truncate(base);
                        return Err(e);
                    }
                }
            }
        }
        let end = (self.head + len).min(self.window.len());
        self.window.get(self.head..end).ok_or(Error::UnexpectedEof)
    }
}
