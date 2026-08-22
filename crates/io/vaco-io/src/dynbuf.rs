//! The `avio_open_dyn_buf` role: a growable in-memory sink.

use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};

use crate::MediaSink;

/// A growable, seekable in-memory [`MediaSink`].
///
/// Every muxer that must know an element's size before writing its header needs
/// one: MP4 `moov` and `moof`, Matroska master elements, NUT syncpoints, and the
/// general write-measure-patch pattern. It is seekable precisely so the patch
/// step works.
///
/// Growth is charged to a [`Budget`], and [`DynBuf::set_limit`] caps the total
/// length, so a hostile input cannot make a muxer allocate without bound.
#[derive(Debug)]
pub struct DynBuf {
    buf: Vec<u8>,
    pos: usize,
    limit: usize,
    budget: Budget,
    charged: u64,
}

impl Default for DynBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl DynBuf {
    /// An empty buffer bounded by [`Limits::strict`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(Limits::strict())
    }

    /// An empty buffer bounded by `limits`.
    #[must_use]
    pub fn with_limits(limits: Limits) -> Self {
        let limit = usize::try_from(limits.max_alloc_single).unwrap_or(usize::MAX);
        Self {
            buf: Vec::new(),
            pos: 0,
            limit,
            budget: Budget::new(limits),
            charged: 0,
        }
    }

    /// Cap the total length. Lowering it below the current length does not
    /// truncate; it only refuses further growth.
    pub const fn set_limit(&mut self, bytes: usize) {
        self.limit = bytes;
    }

    /// The bytes written so far.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been written.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Take the bytes, consuming the buffer.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    /// Drop the contents and rewind, keeping the allocation and the budget
    /// charge, so a muxer can reuse one buffer per element.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.pos = 0;
    }

    /// Bytes charged to the internal budget.
    #[must_use]
    pub const fn charged(&self) -> u64 {
        self.charged
    }

    /// Grow the backing store to `want` bytes, charging the budget.
    fn reserve_to(&mut self, want: usize) -> Result<()> {
        if want > self.limit {
            return Err(Error::LimitExceeded {
                limit: "dynbuf",
                requested: want as u64,
                cap: self.limit as u64,
            });
        }
        if want <= self.buf.capacity() {
            return Ok(());
        }
        // Geometric, so appending byte at a time is not quadratic, but never
        // past the cap the caller set.
        let target = want
            .max(self.buf.capacity().saturating_mul(2))
            .min(self.limit);
        let extra = target.saturating_sub(self.buf.capacity());
        self.budget.charge(extra as u64)?;
        self.charged = self.charged.saturating_add(extra as u64);
        self.buf
            .try_reserve(target.saturating_sub(self.buf.len()))
            .map_err(|_| Error::LimitExceeded {
                limit: "allocator",
                requested: target as u64,
                cap: self.limit as u64,
            })?;
        Ok(())
    }
}

impl MediaSink for DynBuf {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        let end = self
            .pos
            .checked_add(buf.len())
            .ok_or(Error::LimitExceeded {
                limit: "dynbuf",
                requested: u64::MAX,
                cap: self.limit as u64,
            })?;
        if end > self.buf.len() {
            self.reserve_to(end)?;
            // A seek past the end followed by a write leaves a zero hole, which
            // is what a file does.
            self.buf.resize(end, 0);
        }
        let Some(dst) = self.buf.get_mut(self.pos..end) else {
            return Err(Error::UnexpectedEof);
        };
        dst.copy_from_slice(buf);
        self.pos = end;
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        let p = usize::try_from(pos).map_err(|_| Error::LimitExceeded {
            limit: "dynbuf",
            requested: pos,
            cap: self.limit as u64,
        })?;
        if p > self.limit {
            return Err(Error::LimitExceeded {
                limit: "dynbuf",
                requested: pos,
                cap: self.limit as u64,
            });
        }
        self.pos = p;
        Ok(pos)
    }

    fn position(&self) -> u64 {
        self.pos as u64
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// A [`DynBuf`] that can be written through an [`IoWriter`](crate::IoWriter)
/// and read back afterwards.
///
/// [`IoWriter`](crate::IoWriter) takes ownership of its sink, which is right for
/// a file but wrong for the write-measure-embed pattern: the muxer has to get
/// the element back. Cloning a `SharedDynBuf` shares the underlying buffer, so
/// one clone goes into the writer and one stays with the muxer.
///
/// ```
/// use vaco_io::{IoOptions, IoWriter, SharedDynBuf};
///
/// let element = SharedDynBuf::new();
/// let mut w = IoWriter::new(Box::new(element.clone()), &IoOptions::default())?;
/// w.write_tag(b"mvhd")?;
/// w.wb32(0x0100)?;
/// w.flush()?;
/// assert_eq!(element.len(), 8);
/// let bytes = element.take();
/// # Ok::<(), vaco_core::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct SharedDynBuf(std::sync::Arc<std::sync::Mutex<DynBuf>>);

impl Default for SharedDynBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedDynBuf {
    /// An empty buffer bounded by [`Limits::strict`].
    #[must_use]
    pub fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(DynBuf::new())))
    }

    /// An empty buffer bounded by `limits`.
    #[must_use]
    pub fn with_limits(limits: Limits) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            DynBuf::with_limits(limits),
        )))
    }

    /// A poisoned lock means another thread panicked while writing; the bytes
    /// are still exactly as consistent as they were, so we take them anyway
    /// rather than turning someone else's panic into ours.
    fn lock(&self) -> std::sync::MutexGuard<'_, DynBuf> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Bytes written so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Copy the contents out, leaving the buffer intact.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.lock().as_slice().to_vec()
    }

    /// Take the contents, leaving the buffer empty and rewound.
    #[must_use]
    pub fn take(&self) -> Vec<u8> {
        let mut g = self.lock();
        let out = g.as_slice().to_vec();
        g.clear();
        out
    }

    /// Cap the total length.
    pub fn set_limit(&self, bytes: usize) {
        self.lock().set_limit(bytes);
    }
}

impl MediaSink for SharedDynBuf {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.lock().write(buf)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        self.lock().seek(pos)
    }

    fn position(&self) -> u64 {
        self.lock().position()
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
