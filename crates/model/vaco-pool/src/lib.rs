//! Aligned buffer pooling.
//!
//! Steady-state decode must not allocate: a 1080p frame is ~3 MB and allocating
//! one per frame at 60 fps dominates the profile. The pool recycles buffers whose
//! last reference has dropped.
//!
//! All buffers are aligned to 64 bytes unconditionally. `FFmpeg` derives alignment
//! from the build's widest SIMD width; taking the maximum removes a variable
//! nobody benefits from reasoning about.
//!
//! # The three moving parts
//!
//! * [`Buffer`] — a refcounted, aligned, copy-on-write byte buffer. This is the
//!   storage that `vaco-frame`'s planes and `vaco-packet`'s payloads are both
//!   built on, which is why their ownership models are one design and not two.
//! * [`BufferPool`] — a free list of same-sized buffers, bounded in both bytes
//!   and count.
//! * The alignment scheme — over-allocate and sub-slice, documented at length in
//!   `aligned.rs` and in `docs/model/vaco-pool.md`. It is the non-obvious part.
//!
//! # How recycling happens
//!
//! There is no `release` method anywhere in the project, so a buffer cannot be
//! returned twice or forgotten. `Buffer` is `Arc<BufferInner>`; `BufferInner`
//! has a `Drop` that pushes its storage back onto the pool's free list, and
//! `Arc` runs that `Drop` exactly when the strong count reaches zero. The
//! back-reference is a `Weak`, so a long-lived frame never keeps a dead pool
//! alive.
//!
//! ```
//! use vaco_pool::BufferPool;
//!
//! let pool = BufferPool::new(4096);
//! let a = pool.get()?;
//! assert!(a.is_aligned());
//! drop(a);
//! let b = pool.get()?;             // same storage, no allocation
//! assert_eq!(pool.stats().allocations, 1);
//! assert_eq!(pool.stats().hits, 1);
//! # drop(b);
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Copy-on-write
//!
//! ```
//! use vaco_limits::{Budget, Limits};
//! use vaco_pool::Buffer;
//!
//! let mut budget = Budget::new(Limits::strict());
//! let mut a = Buffer::from_slice(&mut budget, b"original")?;
//! let b = a.clone();               // refcount bump, no copy
//! assert!(a.ptr_eq(&b));
//!
//! a.make_mut().fill(b'x');         // `a` was shared, so this copies
//! assert!(!a.ptr_eq(&b));
//! assert_eq!(b.as_slice(), b"original");
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

mod aligned;
mod buffer;
mod pool;

pub use buffer::Buffer;
pub use pool::{BufferPool, PoolConfig, PoolStats};

/// Alignment, in bytes, of every buffer this crate hands out.
///
/// 64 is the widest cache line and the widest SIMD register in common use, so
/// it is the alignment at which no kernel ever has to ask.
pub const ALIGN: usize = 64;

/// Extra zero bytes past the logical end of a bitstream buffer.
///
/// Wide bitstream readers want to load a whole word at a time near the end of a
/// buffer. `FFmpeg` satisfies this by over-reading into guaranteed-zero padding,
/// which is unsound in safe Rust. We keep the padding — so the fast path stays
/// branch-free — but the reader still bounds-checks against the *logical* length,
/// making the padding an optimisation rather than a correctness requirement.
///
/// Measured worth 11.7% on the short-buffer workload that dominates header
/// parsing (`vaco-bitstream`'s benchmarks), which is the v0.1 shape.
pub const BITSTREAM_PADDING: usize = 64;

/// The cross-crate contract `vaco-bitstream`'s author asked for and could not
/// write themselves: `Padded`'s typestate is only sound if the padding this
/// crate allocates is at least as wide as the padding that crate reads.
///
/// A compile error here means one of the two constants moved. Change both, or
/// hoist the constant into `vaco-core` as the bitstream corrections suggest.
const _: () = assert!(
    BITSTREAM_PADDING == vaco_bitstream::Padded::PAD,
    "vaco_pool::BITSTREAM_PADDING must equal vaco_bitstream::Padded::PAD"
);

/// The padding must also be at least as wide as one aligned block, so a padded
/// buffer's tail never shares a 64-byte line with the last logical byte.
const _: () = assert!(BITSTREAM_PADDING >= ALIGN);

const _: () = assert!(ALIGN.is_power_of_two());
