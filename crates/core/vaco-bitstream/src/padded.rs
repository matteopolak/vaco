//! The padded-buffer typestate that keeps the reader's fast path honest.

/// A byte slice whose backing allocation carries at least [`Padded::PAD`] zero
/// bytes past its logical end.
///
/// # Why this exists
///
/// `FFmpeg` guarantees `AV_INPUT_BUFFER_PADDING_SIZE` zero bytes after every
/// bitstream buffer so a 64-bit refill can load eight bytes without asking
/// whether eight bytes remain. We cannot over-read an allocation in safe Rust —
/// but we do not have to. **Put the padding inside the allocation and slice it.**
/// The refill then loads eight real bytes we own; the check that they are in
/// bounds is one comparison against a slice length, not a bet on what the
/// allocator put next door.
///
/// The payoff is not that the check disappears — it is that the check happens
/// once per *eight bytes* instead of once per *read*, and that the reader's
/// byte-at-a-time tail path starts 64 bytes past the logical end, so real
/// parsers never enter it at all.
///
/// # The invariant, and who establishes it
///
/// The padding must be **zero**, not merely present: past the logical end the
/// reader returns whatever the padding holds, and every caller — parsers,
/// `mark`/`restore`, the padded-vs-unpadded equivalence property — depends on
/// that being zero.
///
/// [`Padded::new`] verifies it in `PAD` byte comparisons, once per buffer. That
/// is cheap enough that `vaco-pool` and `vaco-packet`, which allocate with the
/// guarantee already in hand, pay nothing meaningful for it, and it means the
/// type needs no `unsafe`, no sealed trait, and no cross-crate privacy trick to
/// be sound. A buffer that fails the check is simply not `Padded`; the caller
/// falls back to [`BitReader::new`](crate::BitReader::new), which is correct and
/// only slightly slower near the end.
#[derive(Debug, Clone, Copy)]
pub struct Padded<'a> {
    bytes: &'a [u8],
    logical_len: usize,
}

impl<'a> Padded<'a> {
    /// Zero bytes required past the logical end.
    ///
    /// Matches `vaco_pool::BITSTREAM_PADDING`; the two are asserted equal in
    /// that crate's tests. 64 rather than 8 because it is also the pool's
    /// alignment, and because it puts the tail path far enough away that a
    /// header parser reading a few hundred bits never reaches it.
    pub const PAD: usize = 64;

    /// Wrap a buffer that already carries the padding.
    ///
    /// Returns `None` unless `bytes.len() >= logical_len + PAD` and the `PAD`
    /// bytes starting at `logical_len` are all zero.
    #[must_use]
    pub fn new(bytes: &'a [u8], logical_len: usize) -> Option<Self> {
        let pad = bytes.get(logical_len..)?.get(..Self::PAD)?;
        if pad.iter().any(|&b| b != 0) {
            return None;
        }
        Some(Self { bytes, logical_len })
    }

    /// Copy `src` into `scratch`, append the padding, and wrap the result.
    ///
    /// The general-purpose constructor, and the one that costs a `memcpy` —
    /// which is exactly why the pool and packet paths, which allocate padded in
    /// the first place, exist. `scratch` is reused across calls: cleared, not
    /// freed, so a decoder calling this per NAL is allocation-free in steady
    /// state.
    #[must_use]
    pub fn from_slice_copying(src: &[u8], scratch: &'a mut Vec<u8>) -> Self {
        let logical_len = src.len();
        scratch.clear();
        scratch.extend_from_slice(src);
        scratch.resize(logical_len + Self::PAD, 0);
        Self {
            bytes: scratch,
            logical_len,
        }
    }

    /// Bytes before the padding.
    #[must_use]
    pub const fn logical_len(&self) -> usize {
        self.logical_len
    }

    /// The whole slice, padding included.
    #[must_use]
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Just the logical bytes.
    #[must_use]
    pub fn logical_bytes(&self) -> &'a [u8] {
        self.bytes.get(..self.logical_len).unwrap_or(self.bytes)
    }
}
