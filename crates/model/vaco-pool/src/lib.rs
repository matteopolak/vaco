//! Aligned buffer pooling.
//!
//! Steady-state decode must not allocate: a 1080p frame is ~3 MB and allocating
//! one per frame at 60 fps dominates the profile. The pool recycles buffers whose
//! last reference has dropped.
//!
//! All buffers are aligned to 64 bytes unconditionally. `FFmpeg` derives alignment
//! from the build's widest SIMD width; taking the maximum removes a variable
//! nobody benefits from reasoning about.

use std::sync::Arc;
use vaco_core::Result;

pub const ALIGN: usize = 64;

/// Extra zero bytes past the logical end of a bitstream buffer.
///
/// Wide bitstream readers want to load a whole word at a time near the end of a
/// buffer. `FFmpeg` satisfies this by over-reading into guaranteed-zero padding,
/// which is unsound in safe Rust. We keep the padding — so the fast path stays
/// branch-free — but the reader still bounds-checks against the *logical* length,
/// making the padding an optimisation rather than a correctness requirement.
pub const BITSTREAM_PADDING: usize = 64;

/// A refcounted, aligned byte buffer.
#[derive(Debug, Clone)]
pub struct Buffer {
    inner: Arc<BufferInner>,
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "P0-03 interface freeze: fields are read once pooling is implemented"
)]
struct BufferInner {
    data: Vec<u8>,
    /// Returning to the pool on drop is what makes recycling automatic; a `Weak`
    /// so a live buffer never keeps a dead pool alive.
    pool: Option<std::sync::Weak<PoolInner>>,
}

impl Buffer {
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.inner.data
    }

    /// Mutable access if uniquely owned, cloning otherwise.
    ///
    /// This is the copy-on-write seam: a filter that only reads passes the `Arc`
    /// through untouched, and only a writer that shares pays for a copy.
    pub fn make_mut(&mut self) -> &mut [u8] {
        todo!("P0-03 freeze: Arc::make_mut over BufferInner")
    }

    #[must_use]
    pub fn is_unique(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }
}

#[derive(Debug)]
struct PoolInner;

/// A pool of same-sized buffers.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "P0-03 interface freeze: fields are read once pooling is implemented"
)]
pub struct BufferPool {
    inner: Arc<PoolInner>,
}

impl BufferPool {
    #[must_use]
    pub fn new(buffer_size: usize) -> Self {
        let _ = buffer_size;
        todo!("P0-03 freeze")
    }

    /// Take a buffer, reusing a recycled one when available.
    ///
    /// # Errors
    /// Returns [`vaco_core::Error::LimitExceeded`] when the pool's budget is spent.
    pub fn get(&self) -> Result<Buffer> {
        todo!("P0-03 freeze: pop free list, else allocate against the budget")
    }
}
