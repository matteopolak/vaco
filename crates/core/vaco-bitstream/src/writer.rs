//! The MSB-first bit writer, and the RBSP escaping wrapper.

use vaco_limits::{Budget, LimitError};

/// MSB-first bit writer with a 64-bit cache.
///
/// The writer never fails: it appends to a `Vec` and grows. Bounding that growth
/// is the caller's job at the site that knows the limit — see
/// [`BitWriter::with_capacity`], which takes a [`Budget`] precisely because
/// `Vec::with_capacity` is denied project-wide.
///
/// # Example
///
/// ```
/// use vaco_bitstream::{BitReader, BitWriter};
///
/// let mut w = BitWriter::new();
/// w.put(3, 0b101);
/// w.ue(42);
/// w.se(-7);
/// w.rbsp_trailing();
/// let bytes = w.finish();
///
/// let mut r = BitReader::new(&bytes);
/// assert_eq!(r.get(3), 0b101);
/// # use vaco_bitstream::GolombRead;
/// assert_eq!(r.ue(), 42);
/// assert_eq!(r.se(), -7);
/// ```
#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    out: Vec<u8>,
    /// MSB-aligned; `cache_bits` is always `<= 7` between calls.
    cache: u64,
    cache_bits: u32,
}

impl BitWriter {
    /// An empty writer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            out: Vec::new(),
            cache: 0,
            cache_bits: 0,
        }
    }

    /// An empty writer with room for `n` bytes, charged to `budget`.
    ///
    /// # Note
    ///
    /// `planning/11-foundations.md` §8.5 sketches this as `with_capacity(n:
    /// usize)`. It cannot be: `clippy.toml` denies `Vec::with_capacity` so that
    /// every input-derived allocation goes through a budget, and an encoder
    /// sizing its output from a decoded frame header is exactly that case.
    ///
    /// # Errors
    ///
    /// Whatever [`Budget::alloc`] returns when the capacity is over budget.
    pub fn with_capacity(budget: &mut Budget, n: usize) -> Result<Self, LimitError> {
        let mut out = budget.alloc::<u8>(n)?;
        out.clear();
        Ok(Self {
            out,
            cache: 0,
            cache_bits: 0,
        })
    }

    /// Write the low `n` bits of `value`, MSB first. `n <= 32`.
    ///
    /// Bits above `n` are ignored. A larger `n` debug-asserts and clamps; it
    /// never panics.
    #[inline]
    pub fn put(&mut self, n: u32, value: u32) {
        debug_assert!(n <= 32, "BitWriter::put: n must be <= 32, got {n}");
        let n = n.min(32);
        if n == 0 {
            return;
        }
        let masked = if n == 32 {
            value
        } else {
            value & ((1u32 << n) - 1)
        };
        // `cache_bits <= 7` on entry and `n <= 32`, so the shift is `>= 25`.
        let shift = 64 - self.cache_bits - n;
        self.cache |= u64::from(masked) << shift;
        self.cache_bits += n;
        self.flush_bytes();
    }

    /// Write the low `n` bits of a `u64`. `n <= 64`.
    #[inline]
    pub fn put_long(&mut self, n: u32, value: u64) {
        debug_assert!(n <= 64, "BitWriter::put_long: n must be <= 64, got {n}");
        let n = n.min(64);
        if n <= 32 {
            self.put(n, value as u32);
            return;
        }
        self.put(n - 32, (value >> 32) as u32);
        self.put(32, value as u32);
    }

    /// Write `value` as an `n`-bit two's-complement field. `n <= 32`.
    #[inline]
    pub fn put_signed(&mut self, n: u32, value: i32) {
        self.put(n, value as u32);
    }

    /// Write `n` zero bits, for any `n`, without looping per bit.
    #[inline]
    pub fn put_zeros(&mut self, n: u32) {
        let mut left = n;
        while left > 32 {
            self.put(32, 0);
            left -= 32;
        }
        self.put(left, 0);
    }

    /// Write `v` as `ue(v)`.
    ///
    /// # Domain
    ///
    /// `0 ..= u32::MAX - 1`, which is what H.264 §9.1 permits: `u32::MAX` needs
    /// a 32-zero prefix, which the reader rejects as malformed by design (that
    /// cap is what stops a fuzz hang). `u32::MAX` saturates and debug-asserts.
    pub fn ue(&mut self, v: u32) {
        debug_assert!(
            v != u32::MAX,
            "BitWriter::ue: u32::MAX is not representable"
        );
        let code = u64::from(v.min(u32::MAX - 1)) + 1;
        let bits = 64 - code.leading_zeros();
        self.put_zeros(bits - 1);
        self.put_long(bits, code);
    }

    /// Write `v` as `se(v)`.
    ///
    /// # Domain
    ///
    /// `-(2^31 - 1) ..= 2^31 - 1`, which is what H.264 §9.1.1 permits.
    /// [`i32::MIN`] saturates to `-(2^31 - 1)` and debug-asserts, because the
    /// alternative — a 65-bit code number no `ue` reader can return — is worse
    /// than a documented clamp.
    pub fn se(&mut self, v: i32) {
        debug_assert!(
            v != i32::MIN,
            "BitWriter::se: i32::MIN is not representable"
        );
        let v = v.max(-i32::MAX);
        let code = if v > 0 {
            (v as u32) * 2 - 1
        } else {
            v.unsigned_abs() * 2
        };
        self.ue(code);
    }

    /// Pad to the next byte boundary with zeros.
    pub fn align_zero(&mut self) {
        let pad = (8 - (self.cache_bits & 7)) & 7;
        self.put(pad, 0);
    }

    /// Pad to the next byte boundary with ones.
    pub fn align_one(&mut self) {
        let pad = (8 - (self.cache_bits & 7)) & 7;
        if pad != 0 {
            self.put(pad, u32::MAX);
        }
    }

    /// `rbsp_trailing_bits()`: a one bit, then zeros to the byte boundary.
    pub fn rbsp_trailing(&mut self) {
        self.put(1, 1);
        self.align_zero();
    }

    /// Bits written so far, including the unflushed partial byte.
    #[must_use]
    pub fn bit_len(&self) -> u64 {
        (self.out.len() as u64)
            .saturating_mul(8)
            .saturating_add(u64::from(self.cache_bits))
    }

    /// The complete bytes written so far. The partial byte, if any, is not
    /// included — call [`align_zero`](BitWriter::align_zero) first.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.out
    }

    /// Pad to a byte boundary and take the output.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        self.align_zero();
        self.out
    }

    /// Pad to a byte boundary and take the output, leaving the writer empty and
    /// reusable.
    ///
    /// Takes the allocation with it. For a per-NAL encoder loop that must not
    /// allocate at all, use [`bytes`](BitWriter::bytes) plus
    /// [`clear`](BitWriter::clear) instead, which keeps the buffer.
    pub fn reset(&mut self) -> Vec<u8> {
        self.align_zero();
        self.cache = 0;
        self.cache_bits = 0;
        std::mem::take(&mut self.out)
    }

    /// Discard everything, keeping the allocation.
    ///
    /// The steady-state encoder path: write a NAL, hand `bytes()` to the muxer,
    /// `clear()`, repeat — no allocation after the first unit.
    pub fn clear(&mut self) {
        self.out.clear();
        self.cache = 0;
        self.cache_bits = 0;
    }

    #[inline]
    fn flush_bytes(&mut self) {
        while self.cache_bits >= 8 {
            self.out.push((self.cache >> 56) as u8);
            self.cache <<= 8;
            self.cache_bits -= 8;
        }
    }
}

/// A [`BitWriter`] whose output carries emulation-prevention bytes.
///
/// H.264/HEVC forbid `00 00 00`, `00 00 01`, `00 00 02` and `00 00 03` inside a
/// NAL payload, because the first two are start codes. The encoder inserts a
/// `03` after any two zero bytes that would otherwise be followed by one of
/// them; [`crate::annexb::to_rbsp`] removes it again.
///
/// Escaping happens once, in [`finish`](RbspWriter::finish), over the finished
/// byte string. Doing it per flushed byte would interleave two state machines
/// for no measurable gain — a NAL is written once and escaped once.
#[derive(Debug, Default)]
pub struct RbspWriter {
    inner: BitWriter,
    escaped: Vec<u8>,
}

impl RbspWriter {
    /// An empty writer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: BitWriter::new(),
            escaped: Vec::new(),
        }
    }

    /// The underlying bit writer, for the full write vocabulary.
    pub fn bits(&mut self) -> &mut BitWriter {
        &mut self.inner
    }

    /// Terminate the RBSP and produce the escaped (EBSP) byte string.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        self.inner.rbsp_trailing();
        let rbsp = self.inner.finish();
        crate::annexb::to_ebsp(&rbsp, &mut self.escaped);
        self.escaped
    }

    /// Terminate the RBSP and produce an Annex-B unit: a four-byte start code
    /// followed by the escaped payload.
    #[must_use]
    pub fn finish_annexb(self) -> Vec<u8> {
        let mut out = vec![0, 0, 0, 1];
        out.extend_from_slice(&self.finish());
        out
    }
}
