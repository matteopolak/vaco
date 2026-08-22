//! The alignment scheme: over-allocate a plain `Vec<u8>` and sub-slice it.
//!
//! # Why this is not obvious
//!
//! Rust's global allocator only guarantees the alignment of the element type,
//! and `u8` has alignment 1. Raising it needs one of three things: a custom
//! allocator (`Allocator` is unstable), a `#[repr(align(64))]` element type plus
//! a reinterpreting cast to `[u8]` (`unsafe`, or `bytemuck::Pod`, which needs a
//! derive feature we do not have), or over-allocation.
//!
//! **We over-allocate.** A request for `len` bytes allocates `len + ALIGN - 1`
//! and the buffer starts at the first 64-byte boundary inside it:
//!
//! ```text
//!   raw:    |...|xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx|.......|
//!            ^   ^                                       ^
//!            |   offset (0..=63)          len bytes      raw.len() = len + 63
//!            base address, alignment 1
//!
//!   offset = (-base) mod 64  ==  base.wrapping_neg() & 63
//! ```
//!
//! Three properties make this sound without a line of `unsafe`:
//!
//! 1. `<*const T>::addr()` is a **safe** operation (strict provenance, stable
//!    since 1.84). We only ever read the address to compute an index; we never
//!    reconstruct a pointer from it.
//! 2. The `Vec` is never grown, shrunk or reallocated after construction —
//!    `AlignedBuf` exposes no method that could — so the address it was measured
//!    at is the address it keeps for its whole life. Moving an `AlignedBuf`
//!    moves the `Vec` header, not the heap allocation.
//! 3. `offset + len <= raw.len()` always holds because `offset <= ALIGN - 1`,
//!    so the sub-slice is in bounds by construction and the `.get(..)` calls
//!    below can never take their fallback branch.
//!
//! The cost is `ALIGN - 1` = 63 wasted bytes per allocation. Against a 3 MB
//! 1080p plane that is 0.002%, and the pool recycles the whole thing anyway.
//! Zero-length buffers still allocate the 63 bytes so that the alignment
//! invariant holds for **every** size with no special case — a zero-length
//! buffer is rare enough that the allocation does not matter, and a conditional
//! invariant is worth much less than an unconditional one.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::ALIGN;

/// A 64-byte-aligned, zero-initialised byte buffer.
///
/// Private to this crate: [`Buffer`](crate::Buffer) is the public handle, which
/// keeps the "never reallocate the `Vec`" invariant enforceable by inspection of
/// one small module.
///
/// Deliberately **not** `Default`. An empty `Vec<u8>` has a dangling pointer of
/// alignment 1, so a non-allocating value is the one `AlignedBuf` that fails
/// [`is_aligned`](Self::is_aligned). That value is still needed, exactly once,
/// to vacate `self.data` while recycling on drop — so it is spelled
/// [`placeholder`](Self::placeholder) and used with `mem::replace`, where the
/// call site has to name it, rather than handed out by a derive.
#[derive(Debug)]
pub(crate) struct AlignedBuf {
    /// The over-allocated backing store. Never resized after construction.
    raw: Vec<u8>,
    /// Distance from `raw`'s base to the first `ALIGN` boundary. `0..ALIGN`.
    offset: usize,
    /// Logical length. `offset + len <= raw.len()`.
    len: usize,
}

/// Bytes to request from the allocator for a logical length of `len`.
fn raw_len(len: usize) -> Option<usize> {
    len.checked_add(ALIGN - 1)
}

impl AlignedBuf {
    /// Wrap an over-allocated `Vec`, computing the alignment offset.
    ///
    /// `raw.len()` must be at least `len + ALIGN - 1`; anything shorter is
    /// clamped to a length that still fits, which cannot happen through either
    /// of the two constructors below.
    fn from_raw(raw: Vec<u8>, len: usize) -> Self {
        // Safe: `addr()` reads the address without materialising a pointer we
        // later dereference. The `Vec` is never reallocated afterwards.
        let offset = raw.as_ptr().addr().wrapping_neg() & (ALIGN - 1);
        let len = len.min(raw.len().saturating_sub(offset));
        Self { raw, offset, len }
    }

    /// A non-allocating stand-in, for vacating a field that is about to drop.
    ///
    /// The **only** `AlignedBuf` that is not aligned. Never hand one to a
    /// caller; `mem::replace` it into a slot whose real value you have just
    /// taken, and let it drop.
    pub(crate) const fn placeholder() -> Self {
        Self {
            raw: Vec::new(),
            offset: 0,
            len: 0,
        }
    }

    /// A zeroed buffer of `len` bytes, allocated outside any budget.
    ///
    /// Used on the pool's own path, where the pool's configured caps — not a
    /// caller's [`Budget`] — are what bound the allocation, and on the
    /// copy-on-write path, which cannot fail.
    pub(crate) fn new(len: usize) -> Self {
        let total = raw_len(len).unwrap_or(usize::MAX);
        Self::from_raw(vec![0u8; total], len)
    }

    /// A zeroed buffer of `len` bytes, charged to `budget`.
    ///
    /// The padding is charged too: it is real memory the process holds.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] if the size overflows or a cap is hit.
    pub(crate) fn alloc(budget: &mut Budget, len: usize) -> Result<Self> {
        let total = raw_len(len).ok_or(Error::LimitExceeded {
            limit: "buffer_len",
            requested: len as u64,
            cap: (usize::MAX - ALIGN + 1) as u64,
        })?;
        let raw = budget.alloc::<u8>(total)?;
        Ok(Self::from_raw(raw, len))
    }

    /// A copy of `src`, allocated outside any budget.
    pub(crate) fn copy_of(src: &[u8]) -> Self {
        let mut out = Self::new(src.len());
        out.as_mut_slice().copy_from_slice(src);
        out
    }

    /// The logical bytes.
    pub(crate) fn as_slice(&self) -> &[u8] {
        let end = self.offset.saturating_add(self.len);
        self.raw.get(self.offset..end).unwrap_or(&[])
    }

    /// The logical bytes, mutably.
    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        let end = self.offset.saturating_add(self.len);
        self.raw.get_mut(self.offset..end).unwrap_or(&mut [])
    }

    /// Logical length in bytes.
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    /// Bytes actually held by the allocation, padding included.
    ///
    /// This is what the pool accounts against its caps: the process pays for the
    /// padding whether the caller uses it or not.
    pub(crate) fn footprint(&self) -> usize {
        self.raw.len()
    }

    /// Whether row zero sits on an [`ALIGN`] boundary.
    ///
    /// True for every buffer an allocating constructor produces — exposed so the
    /// property tests can assert the invariant rather than trust it. The lone
    /// exception is [`placeholder`](Self::placeholder), which never escapes.
    pub(crate) fn is_aligned(&self) -> bool {
        self.as_slice().as_ptr().addr() & (ALIGN - 1) == 0
    }
}
