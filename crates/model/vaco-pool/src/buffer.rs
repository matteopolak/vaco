//! The refcounted, pool-aware, copy-on-write byte buffer.

use std::sync::{Arc, Weak};

use vaco_bitstream::Padded;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::aligned::AlignedBuf;
use crate::pool::PoolInner;
use crate::{BITSTREAM_PADDING, BufferPool};

/// A refcounted, 64-byte-aligned byte buffer with copy-on-write semantics.
///
/// This is the storage every [`Frame`] plane and every [`Packet`] payload is
/// built on. Cloning is a refcount bump; writing through [`Buffer::make_mut`]
/// copies only if the buffer is shared. When the last clone of a *pooled* buffer
/// drops, the storage goes back to its pool instead of to the allocator.
///
/// [`Frame`]: https://docs.rs/vaco-frame
/// [`Packet`]: https://docs.rs/vaco-packet
#[derive(Debug, Clone)]
pub struct Buffer {
    inner: Arc<BufferInner>,
}

#[derive(Debug)]
pub(crate) struct BufferInner {
    data: AlignedBuf,
    /// Returning to the pool on drop is what makes recycling automatic; a `Weak`
    /// so a live buffer never keeps a dead pool alive.
    pool: Option<Weak<PoolInner>>,
}

impl BufferInner {
    pub(crate) const fn new(data: AlignedBuf, pool: Option<Weak<PoolInner>>) -> Self {
        Self { data, pool }
    }
}

/// Invoked by [`Arc::make_mut`] when the buffer is shared — the copy half of
/// copy-on-write.
///
/// The copy is drawn from the *same pool* when one is alive and has room, so a
/// steady-state copy-on-write is also allocation-free. When the pool is gone or
/// at its cap the copy falls out of the pool rather than failing, because
/// `Clone` cannot report an error.
impl Clone for BufferInner {
    fn clone(&self) -> Self {
        let pooled = self
            .pool
            .as_ref()
            .and_then(Weak::upgrade)
            .and_then(|p| p.acquire(self.data.len()).map(|b| (b, Arc::downgrade(&p))));
        match pooled {
            Some((mut data, pool)) => {
                data.as_mut_slice().copy_from_slice(self.data.as_slice());
                Self {
                    data,
                    pool: Some(pool),
                }
            }
            None => Self {
                data: AlignedBuf::copy_of(self.data.as_slice()),
                pool: None,
            },
        }
    }
}

/// Runs when the LAST `Arc` to this buffer drops — exactly the "returns to the
/// pool when the refcount hits zero" behaviour, with no manual refcount and no
/// explicit release call anywhere in the project.
impl Drop for BufferInner {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.as_ref().and_then(Weak::upgrade) {
            let data = std::mem::replace(&mut self.data, AlignedBuf::placeholder());
            pool.recycle(data);
        }
    }
}

impl Buffer {
    pub(crate) fn from_inner(inner: BufferInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// A zeroed buffer of `len` bytes, charged to `budget`.
    ///
    /// The general-purpose public constructor. Not pooled: use
    /// [`BufferPool::get`] on any path that runs per frame or per packet.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] if `len` overflows or a budget cap is hit.
    pub fn alloc(budget: &mut Budget, len: usize) -> Result<Self> {
        Ok(Self::from_inner(BufferInner::new(
            AlignedBuf::alloc(budget, len)?,
            None,
        )))
    }

    /// A buffer holding a copy of `src`.
    ///
    /// # Errors
    ///
    /// As [`Buffer::alloc`].
    pub fn from_slice(budget: &mut Budget, src: &[u8]) -> Result<Self> {
        let mut buf = Self::alloc(budget, src.len())?;
        buf.make_mut().copy_from_slice(src);
        Ok(buf)
    }

    /// A buffer of `logical_len + `[`BITSTREAM_PADDING`] zero bytes.
    ///
    /// The bitstream fast path (plan 11 F9): a reader built from
    /// [`Buffer::padded`] can load eight bytes at any position up to
    /// `logical_len` without a per-read bounds check, because the padding is
    /// real memory we own.
    ///
    /// # Errors
    ///
    /// As [`Buffer::alloc`].
    pub fn alloc_padded(budget: &mut Budget, logical_len: usize) -> Result<Self> {
        let total = logical_len
            .checked_add(BITSTREAM_PADDING)
            .ok_or(Error::LimitExceeded {
                limit: "buffer_len",
                requested: logical_len as u64,
                cap: (usize::MAX - BITSTREAM_PADDING) as u64,
            })?;
        Self::alloc(budget, total)
    }

    /// `src` copied into a buffer that carries the bitstream padding.
    ///
    /// # Errors
    ///
    /// As [`Buffer::alloc`].
    pub fn from_slice_padded(budget: &mut Budget, src: &[u8]) -> Result<Self> {
        let mut buf = Self::alloc_padded(budget, src.len())?;
        let bytes = buf.make_mut();
        if let Some(head) = bytes.get_mut(..src.len()) {
            head.copy_from_slice(src);
        }
        Ok(buf)
    }

    /// An empty buffer.
    ///
    /// Cheap enough for a placeholder field, but it does allocate — see the
    /// module docs on why zero-length buffers still take the aligned path.
    #[must_use]
    pub fn empty() -> Self {
        Self::from_inner(BufferInner::new(AlignedBuf::new(0), None))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.inner.data.as_slice()
    }

    /// Mutable access if uniquely owned, cloning otherwise.
    ///
    /// This is the copy-on-write seam: a filter that only reads passes the `Arc`
    /// through untouched, and only a writer that shares pays for a copy.
    pub fn make_mut(&mut self) -> &mut [u8] {
        Arc::make_mut(&mut self.inner).data.as_mut_slice()
    }

    /// Pay the copy-on-write cost now, at a point of the caller's choosing,
    /// rather than inside a hot loop.
    pub fn make_writable(&mut self) {
        let _unique: &mut BufferInner = Arc::make_mut(&mut self.inner);
    }

    #[must_use]
    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }

    /// Length in bytes, padding included when the buffer carries any.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.data.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether two handles refer to the same storage.
    ///
    /// The cheap way to check that a filter really did pass a plane through
    /// without copying it.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Whether byte zero sits on a [`crate::ALIGN`] boundary. Always true.
    #[must_use]
    pub fn is_aligned(&self) -> bool {
        self.inner.data.is_aligned()
    }

    /// Whether this buffer will be recycled rather than freed on last drop.
    #[must_use]
    pub fn is_pooled(&self) -> bool {
        self.inner.pool.is_some()
    }

    /// The padded view `vaco-bitstream` wants, if this buffer carries the
    /// padding for `logical_len`.
    ///
    /// Returns `None` unless `len() >= logical_len + `[`BITSTREAM_PADDING`] and
    /// those padding bytes are all zero — the invariant
    /// [`BitReader::new_padded`] relies on. A buffer that fails the check is
    /// simply not padded and the caller falls back to `BitReader::new`.
    ///
    /// [`BitReader::new_padded`]: vaco_bitstream::BitReader::new_padded
    #[must_use]
    pub fn padded(&self, logical_len: usize) -> Option<Padded<'_>> {
        Padded::new(self.as_slice(), logical_len)
    }
}

impl BufferPool {
    /// Take a buffer and fill it with zeros.
    ///
    /// [`BufferPool::get`] hands back whatever a previous user left behind; this
    /// is for the callers who need a clean slate.
    ///
    /// # Errors
    ///
    /// As [`BufferPool::get`].
    pub fn get_zeroed(&self) -> Result<Buffer> {
        let mut buf = self.get()?;
        buf.make_mut().fill(0);
        Ok(buf)
    }
}
