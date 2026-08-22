//! The buffered writer every muxer is handed.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::{IoOptions, MediaSink, replay};

/// Where a segmenting muxer is allowed to cut the byte stream.
///
/// The frozen [`MediaSink`] does not carry this — see the crate docs — so it is
/// recorded on the writer and made available to sinks that care through
/// [`IoWriter::last_marker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataMarker {
    /// Container header.
    Header,
    /// Container trailer.
    Trailer,
    /// A keyframe boundary: a new segment may start here.
    SyncPoint,
    /// A weaker boundary: a segment may end here but not necessarily start.
    BoundaryPoint,
    /// Everything else.
    #[default]
    Unknown,
    /// The muxer has flushed; nothing is buffered behind this point.
    FlushPoint,
}

/// Buffered, byte-order-aware writing.
///
/// Split from [`IoContext`](crate::IoContext) rather than sharing one type with
/// a `write_flag`, which is how the reference implementation does it. A single
/// type makes "read from a write context" a runtime error on every one of forty
/// methods; two types make it a compile error and delete the check.
pub struct IoWriter {
    sink: Box<dyn MediaSink>,
    buf: Vec<u8>,
    len: usize,
    /// Logical offset of `buf[0]`.
    base: u64,
    err: Option<Error>,
    bytes_written: u64,
    direct: bool,
    marker: DataMarker,
}

impl std::fmt::Debug for IoWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoWriter")
            .field("pos", &self.pos())
            .field("buffered", &self.len)
            .field("error", &self.err)
            .finish_non_exhaustive()
    }
}

impl IoWriter {
    /// Wrap `sink` with a buffer sized by `opts`.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if `opts.limits` will not permit the buffer.
    pub fn new(sink: Box<dyn MediaSink>, opts: &IoOptions) -> Result<Self> {
        let block = opts.block_size.clamp(64, 16 * 1024 * 1024);
        let mut budget = Budget::new(opts.limits.clone());
        let buf = budget.alloc::<u8>(block)?;
        let base = sink.position();
        Ok(Self {
            sink,
            buf,
            len: 0,
            base,
            err: None,
            bytes_written: 0,
            direct: opts.direct,
            marker: DataMarker::Unknown,
        })
    }

    /// The offset the next byte will land at.
    #[must_use]
    pub const fn pos(&self) -> u64 {
        self.base.saturating_add(self.len as u64)
    }

    /// Bytes handed to the sink so far. Unlike [`IoWriter::pos`], a seek never
    /// rewinds this.
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Whether the sink supports [`IoWriter::seek`].
    #[must_use]
    pub fn is_seekable(&self) -> bool {
        self.sink.is_seekable()
    }

    /// The sticky error, if the writer has failed.
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        self.err.as_ref()
    }

    /// The marker most recently passed to [`IoWriter::write_marked`].
    #[must_use]
    pub const fn last_marker(&self) -> DataMarker {
        self.marker
    }

    fn fail(&mut self, e: Error) -> Error {
        let out = replay(&e);
        if self.err.is_none() {
            self.err = Some(e);
        }
        out
    }

    /// Hand the buffer to the sink.
    ///
    /// # Errors
    /// Propagates transport failure and replays a sticky one.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(e) = &self.err {
            return Err(replay(e));
        }
        if self.len > 0 {
            let Some(region) = self.buf.get(..self.len) else {
                return Err(Error::UnexpectedEof);
            };
            // `write` on the sink is all-or-nothing, so a copy is needed only
            // because the borrow checker cannot see that `sink` and `buf` are
            // disjoint fields through a `Box<dyn _>` call.
            let owned = region.to_vec();
            if let Err(e) = self.sink.write(&owned) {
                return Err(self.fail(e));
            }
            self.bytes_written = self.bytes_written.saturating_add(self.len as u64);
            self.base = self.base.saturating_add(self.len as u64);
            self.len = 0;
        }
        if let Err(e) = self.sink.flush() {
            return Err(self.fail(e));
        }
        Ok(())
    }

    /// Write `data`.
    ///
    /// # Errors
    /// Propagates transport failure and replays a sticky one.
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if let Some(e) = &self.err {
            return Err(replay(e));
        }
        // A write at least as large as the buffer, or `-avioflags direct`, has
        // nothing to gain from a copy.
        if self.direct || data.len() >= self.buf.len() {
            self.flush()?;
            if let Err(e) = self.sink.write(data) {
                return Err(self.fail(e));
            }
            self.bytes_written = self.bytes_written.saturating_add(data.len() as u64);
            self.base = self.base.saturating_add(data.len() as u64);
            if self.direct
                && let Err(e) = self.sink.flush()
            {
                return Err(self.fail(e));
            }
            return Ok(());
        }
        if self.len + data.len() > self.buf.len() {
            self.flush()?;
        }
        let end = self.len + data.len();
        let Some(dst) = self.buf.get_mut(self.len..end) else {
            return Err(Error::UnexpectedEof);
        };
        dst.copy_from_slice(data);
        self.len = end;
        Ok(())
    }

    /// Write `data`, telling the sink what kind of boundary it is.
    ///
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn write_marked(&mut self, data: &[u8], marker: DataMarker) -> Result<()> {
        self.marker = marker;
        self.write(data)
    }

    /// Move to absolute offset `pos`, flushing first.
    ///
    /// # Errors
    /// [`Error::NotSeekable`] on a non-seekable sink, otherwise transport failure.
    pub fn seek(&mut self, pos: u64) -> Result<u64> {
        self.flush()?;
        match self.sink.seek(pos) {
            Ok(p) => {
                self.base = p;
                Ok(p)
            }
            Err(e) => Err(self.fail(e)),
        }
    }

    // -------------------------------------------------------------- byte order

    /// # Errors
    /// As [`IoWriter::write`].
    pub fn w8(&mut self, v: u8) -> Result<()> {
        self.write(&[v])
    }
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wb16(&mut self, v: u16) -> Result<()> {
        self.write(&v.to_be_bytes())
    }
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wl16(&mut self, v: u16) -> Result<()> {
        self.write(&v.to_le_bytes())
    }
    /// Write the low 24 bits of `v`, big-endian.
    ///
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wb24(&mut self, v: u32) -> Result<()> {
        let b = v.to_be_bytes();
        self.write(&[b[1], b[2], b[3]])
    }
    /// Write the low 24 bits of `v`, little-endian.
    ///
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wl24(&mut self, v: u32) -> Result<()> {
        let b = v.to_le_bytes();
        self.write(&[b[0], b[1], b[2]])
    }
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wb32(&mut self, v: u32) -> Result<()> {
        self.write(&v.to_be_bytes())
    }
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wl32(&mut self, v: u32) -> Result<()> {
        self.write(&v.to_le_bytes())
    }
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wb64(&mut self, v: u64) -> Result<()> {
        self.write(&v.to_be_bytes())
    }
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn wl64(&mut self, v: u64) -> Result<()> {
        self.write(&v.to_le_bytes())
    }
    /// Write a four-character code.
    ///
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn write_tag(&mut self, tag: &[u8; 4]) -> Result<()> {
        self.write(tag)
    }
    /// Write `s` followed by a NUL.
    ///
    /// # Errors
    /// As [`IoWriter::write`].
    pub fn write_cstr(&mut self, s: &str) -> Result<()> {
        self.write(s.as_bytes())?;
        self.w8(0)
    }
}

impl Drop for IoWriter {
    fn drop(&mut self) {
        // Best effort: a muxer is expected to flush explicitly and check the
        // result. This exists so a buffer is not silently lost on an early
        // return path, not as a substitute for that.
        let _ = self.flush();
    }
}
