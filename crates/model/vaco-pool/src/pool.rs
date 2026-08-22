//! The pool: size-uniform free lists with a hard retention bound.

use std::sync::Arc;

use parking_lot::Mutex;
use vaco_core::{Error, Result};

use crate::aligned::AlignedBuf;
use crate::buffer::{Buffer, BufferInner};

/// How much a pool may hold, and how much it may hand out.
///
/// The pool is bounded and `FFmpeg`'s is not. Unbounded pooling turns a
/// resolution-switching stream into a memory leak, and D6 names unbounded
/// allocation as a fuzz finding — so the bound is a correctness property, not a
/// tuning knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// Ceiling on bytes the pool is responsible for: outstanding buffers plus
    /// retained ones. [`BufferPool::get`] fails rather than exceeding it.
    pub max_live_bytes: usize,
    /// Ceiling on the number of buffers in flight, so a pool of zero-length
    /// buffers is bounded too.
    pub max_live_buffers: usize,
    /// How many returned buffers the free list keeps. Beyond this, a returned
    /// buffer is freed.
    pub max_retained_buffers: usize,
}

impl Default for PoolConfig {
    /// Sized for one decode or filter stage at 4K: generous enough that a
    /// well-behaved pipeline never sees the cap, tight enough that a runaway
    /// one stops.
    fn default() -> Self {
        Self {
            max_live_bytes: 1 << 30,
            max_live_buffers: 4096,
            max_retained_buffers: 32,
        }
    }
}

/// Counters, for the test that proves recycling rather than asserting it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// Buffers this pool has allocated from the system allocator.
    pub allocations: u64,
    /// Buffers served from the free list instead.
    pub hits: u64,
    /// Buffers returned to the pool on last drop.
    pub recycled: u64,
    /// Bytes the pool is currently responsible for, padding included.
    pub live_bytes: usize,
    /// Buffers outstanding plus retained.
    pub live_buffers: usize,
    /// Buffers sitting on the free list right now.
    pub retained_buffers: usize,
}

#[derive(Debug, Default)]
struct PoolState {
    free: Vec<AlignedBuf>,
    live_bytes: usize,
    live_buffers: usize,
    allocations: u64,
    hits: u64,
    recycled: u64,
}

#[derive(Debug)]
pub(crate) struct PoolInner {
    buffer_size: usize,
    config: PoolConfig,
    state: Mutex<PoolState>,
}

impl PoolInner {
    /// Pop a buffer of exactly `len`, or allocate one if the caps allow.
    ///
    /// `None` means the pool is at its bound. The allocation happens *outside*
    /// the lock: a 3 MB zeroing memset held under a mutex would serialise every
    /// other thread's free-list pop behind it.
    pub(crate) fn acquire(&self, len: usize) -> Option<AlignedBuf> {
        if len != self.buffer_size {
            return None;
        }
        let footprint = {
            let mut st = self.state.lock();
            if let Some(buf) = st.free.pop() {
                st.hits = st.hits.saturating_add(1);
                return Some(buf);
            }
            let footprint = len.checked_add(crate::ALIGN - 1)?;
            if st.live_buffers >= self.config.max_live_buffers {
                return None;
            }
            if st.live_bytes.checked_add(footprint)? > self.config.max_live_bytes {
                return None;
            }
            st.live_buffers = st.live_buffers.saturating_add(1);
            st.live_bytes = st.live_bytes.saturating_add(footprint);
            st.allocations = st.allocations.saturating_add(1);
            footprint
        };
        let buf = AlignedBuf::new(len);
        debug_assert_eq!(buf.footprint(), footprint);
        Some(buf)
    }

    /// Take a dropped buffer back, or drop it if the free list is full.
    pub(crate) fn recycle(&self, buf: AlignedBuf) {
        let footprint = buf.footprint();
        // Bind the rejected buffer outside the lock scope so it is freed after
        // the mutex is released.
        let _rejected = {
            let mut st = self.state.lock();
            if buf.len() == self.buffer_size && st.free.len() < self.config.max_retained_buffers {
                st.free.push(buf);
                st.recycled = st.recycled.saturating_add(1);
                None
            } else {
                st.live_buffers = st.live_buffers.saturating_sub(1);
                st.live_bytes = st.live_bytes.saturating_sub(footprint);
                Some(buf)
            }
        };
    }

    fn stats(&self) -> PoolStats {
        let st = self.state.lock();
        PoolStats {
            allocations: st.allocations,
            hits: st.hits,
            recycled: st.recycled,
            live_bytes: st.live_bytes,
            live_buffers: st.live_buffers,
            retained_buffers: st.free.len(),
        }
    }

    fn clear(&self) {
        let drained = {
            let mut st = self.state.lock();
            let drained: Vec<AlignedBuf> = std::mem::take(&mut st.free);
            for buf in &drained {
                st.live_buffers = st.live_buffers.saturating_sub(1);
                st.live_bytes = st.live_bytes.saturating_sub(buf.footprint());
            }
            drained
        };
        drop(drained);
    }
}

/// A pool of same-sized buffers.
///
/// Cloning a `BufferPool` shares the pool; there is no way to copy one. Every
/// buffer it hands out holds a `Weak` back-reference, so dropping the last
/// `BufferPool` handle while buffers are still alive is fine: those buffers stop
/// being recyclable and are freed normally.
#[derive(Debug, Clone)]
pub struct BufferPool {
    inner: Arc<PoolInner>,
}

impl BufferPool {
    /// A pool of `buffer_size`-byte buffers with the default caps.
    #[must_use]
    pub fn new(buffer_size: usize) -> Self {
        Self::with_config(buffer_size, PoolConfig::default())
    }

    /// A pool of `logical_len + `[`BITSTREAM_PADDING`](crate::BITSTREAM_PADDING)
    /// byte buffers, for packet payloads.
    ///
    /// Sizing the class with the padding included is what stops the padding from
    /// fragmenting the free lists.
    #[must_use]
    pub fn new_padded(logical_len: usize) -> Self {
        Self::new(logical_len.saturating_add(crate::BITSTREAM_PADDING))
    }

    /// A pool with explicit caps.
    #[must_use]
    pub fn with_config(buffer_size: usize, config: PoolConfig) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                buffer_size,
                config,
                state: Mutex::new(PoolState::default()),
            }),
        }
    }

    /// The length of every buffer this pool hands out.
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.inner.buffer_size
    }

    /// The caps this pool enforces.
    #[must_use]
    pub fn config(&self) -> PoolConfig {
        self.inner.config
    }

    /// Take a buffer, reusing a recycled one when available.
    ///
    /// The contents are **unspecified**: zero on first allocation, a previous
    /// user's bytes after a recycle. Callers that need zeros call
    /// [`BufferPool::get_zeroed`]; callers that overwrite everything — which is
    /// most of them — should not pay for a memset they do not need.
    ///
    /// # Errors
    /// Returns [`vaco_core::Error::LimitExceeded`] when the pool's budget is spent.
    pub fn get(&self) -> Result<Buffer> {
        let data = self.inner.acquire(self.inner.buffer_size).ok_or({
            Error::LimitExceeded {
                limit: "pool_live_bytes",
                requested: self.inner.buffer_size as u64,
                cap: self.inner.config.max_live_bytes as u64,
            }
        })?;
        Ok(Buffer::from_inner(BufferInner::new(
            data,
            Some(Arc::downgrade(&self.inner)),
        )))
    }

    /// Counters. `hits` rising while `allocations` stays flat is what recycling
    /// looks like.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        self.inner.stats()
    }

    /// Drop everything retained.
    ///
    /// Called when geometry changes — a resolution switch makes every cached
    /// buffer the wrong size, and keeping them is how a pool becomes a leak.
    pub fn clear(&self) {
        self.inner.clear();
    }

    /// Whether two handles refer to the same pool.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}
