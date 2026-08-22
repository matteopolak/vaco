//! The mutable half: the per-instance meter.

use crate::{LimitError, Limits, Result};

/// The allocation and fuel meter for one component instance.
///
/// A `Budget` is a **required constructor parameter** for anything that sizes a
/// buffer from input (plan 13 §2.2.2). Not an `Option`, not a builder field with
/// a default — a positional parameter, so there is no code path that forgets it.
/// `clippy.toml` bans `Vec::with_capacity` / `reserve` / `reserve_exact` project
/// wide to make [`Budget::alloc`] the only way in.
///
/// Every counter is behind `&mut self`. There is no interior mutability and no
/// atomics, so a single-threaded parse consumes the budget in a deterministic
/// order and a fuzz finding replays to the same byte.
///
/// # Accounting model
///
/// - `committed` — bytes the caller owns right now.
/// - `pending` — bytes held by live [`Reservation`]s, checked but not yet spent.
/// - Both count against `max_alloc_total`, so a parser cannot reserve its way
///   past the cap by never committing.
///
/// # Example
///
/// ```
/// use vaco_limits::{Budget, Limits};
///
/// let mut budget = Budget::new(Limits::strict());
///
/// // One-shot: check and commit together.
/// let buf: Vec<u8> = budget.alloc(4096)?;
/// assert_eq!(buf.len(), 4096);
///
/// // Two-phase: validate the declared size before spending it.
/// let declared = 1 << 30;
/// assert!(budget.reserve(declared).is_err());
/// # Ok::<(), vaco_limits::LimitError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Budget {
    limits: Limits,
    committed: u64,
    pending: u64,
    peak: u64,
    fuel_spent: u64,
}

impl Budget {
    /// Create a meter over `limits`.
    #[must_use]
    pub const fn new(limits: Limits) -> Self {
        Self {
            limits,
            committed: 0,
            pending: 0,
            peak: 0,
            fuel_spent: 0,
        }
    }

    /// The policy this budget enforces.
    #[must_use]
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Bytes currently committed.
    #[must_use]
    pub const fn committed(&self) -> u64 {
        self.committed
    }

    /// Bytes held by live reservations.
    #[must_use]
    pub const fn pending(&self) -> u64 {
        self.pending
    }

    /// High-water mark of `committed + pending`. Useful in tests to assert that
    /// a parser never went near its cap on well-formed input.
    #[must_use]
    pub const fn peak(&self) -> u64 {
        self.peak
    }

    /// Bytes still available before `max_alloc_total`.
    #[must_use]
    pub const fn available(&self) -> u64 {
        self.limits
            .max_alloc_total
            .saturating_sub(self.committed.saturating_add(self.pending))
    }

    // ---------------------------------------------------------------- phase 1

    /// Phase one of a two-phase reservation: check `bytes` against both caps and
    /// hold them, without allocating.
    ///
    /// The returned [`Reservation`] releases the hold when dropped, so the
    /// "validate the header, then decide" path cannot leak budget on the reject
    /// branch. Commit it with [`Reservation::commit`] or turn it straight into a
    /// buffer with [`Reservation::alloc`].
    ///
    /// This is the specific defence against declared-length amplification: a box
    /// header claiming 4 GiB is rejected here, before a byte is touched.
    ///
    /// # Errors
    ///
    /// [`LimitError::Exceeded`] if `bytes` is over `max_alloc_single`, or if the
    /// running total would pass `max_alloc_total`; [`LimitError::Overflow`] if
    /// the running total does not fit in a `u64`.
    pub fn reserve(&mut self, bytes: u64) -> Result<Reservation<'_>> {
        self.check(bytes)?;
        self.pending = self.pending.saturating_add(bytes);
        self.note_peak();
        Ok(Reservation {
            budget: self,
            bytes,
        })
    }

    /// [`Budget::reserve`] for `n` elements of `T`, with the size computed in
    /// checked arithmetic.
    ///
    /// # Errors
    ///
    /// [`LimitError::Overflow`] if `n * size_of::<T>()` does not fit in a `u64`,
    /// otherwise as [`Budget::reserve`].
    pub fn reserve_array<T>(&mut self, n: usize) -> Result<Reservation<'_>> {
        let bytes = byte_size::<T>(n)?;
        self.reserve(bytes)
    }

    /// Check `bytes` against both caps without holding them.
    ///
    /// Use when the answer is only needed to choose a branch. Prefer
    /// [`Budget::reserve`] when the bytes will actually be taken, because a bare
    /// check races with any other reservation in flight.
    ///
    /// # Errors
    ///
    /// As [`Budget::reserve`].
    pub fn check(&self, bytes: u64) -> Result<()> {
        if bytes > self.limits.max_alloc_single {
            return Err(LimitError::Exceeded {
                limit: "max_alloc_single",
                requested: bytes,
                cap: self.limits.max_alloc_single,
            });
        }
        let total = self
            .committed
            .checked_add(self.pending)
            .and_then(|t| t.checked_add(bytes))
            .ok_or(LimitError::Overflow)?;
        if total > self.limits.max_alloc_total {
            return Err(LimitError::Exceeded {
                limit: "max_alloc_total",
                requested: total,
                cap: self.limits.max_alloc_total,
            });
        }
        Ok(())
    }

    // ---------------------------------------------------------------- phase 2

    /// Check and commit `bytes` in one step, for a size that is already known to
    /// be real (bytes in hand, not bytes promised by a header).
    ///
    /// # Errors
    ///
    /// As [`Budget::reserve`].
    pub fn charge(&mut self, bytes: u64) -> Result<()> {
        self.check(bytes)?;
        self.committed = self.committed.saturating_add(bytes);
        self.note_peak();
        Ok(())
    }

    /// Give `bytes` back, when the buffer they paid for is dropped.
    ///
    /// Saturating: over-releasing is a bookkeeping bug in the caller, not a
    /// reason to panic in a parser.
    pub const fn release(&mut self, bytes: u64) {
        self.committed = self.committed.saturating_sub(bytes);
    }

    /// The sanctioned way to allocate a buffer whose length derives from input.
    ///
    /// Named in `clippy.toml`: `Vec::with_capacity` and friends are denied so
    /// that every input-sized allocation lands here. The allocation is
    /// `try_reserve_exact`, so an allocator refusal is an error rather than an
    /// abort.
    ///
    /// # Errors
    ///
    /// [`LimitError::Overflow`] if the size computation overflows,
    /// [`LimitError::Exceeded`] if a cap is hit, [`LimitError::AllocFailed`] if
    /// the allocator refuses.
    pub fn alloc<T: Copy + Default>(&mut self, n: usize) -> Result<Vec<T>> {
        self.reserve_array::<T>(n)?.alloc(n)
    }

    /// A growable buffer for the "declared size, unknown truth" case.
    ///
    /// Never allocates `declared` up front; grows geometrically as bytes
    /// actually arrive, charging each growth. A 16-byte file therefore cannot
    /// cause a gigabyte allocation however large a length field it carries.
    #[must_use]
    pub fn incremental<T: Copy>(&self, declared: usize) -> IncrementalVec<T> {
        IncrementalVec::new(declared)
    }

    // ------------------------------------------------------------------- fuel

    /// Charge `n` units of fuel to an input-derived loop.
    ///
    /// Fuel is a counter, not a clock. A loop whose trip count is a function of
    /// input data charges fuel per iteration; exhaustion is therefore
    /// reproducible, minimises cleanly, and regresses as a unit test. Wall-clock
    /// deadlines are the fallback, never the primary mechanism.
    ///
    /// # Errors
    ///
    /// [`LimitError::FuelExhausted`] once the allowance in
    /// [`Limits::fuel`] is used up. Subsequent calls keep failing.
    pub fn consume_fuel(&mut self, n: u64) -> Result<()> {
        self.fuel_spent = self.fuel_spent.saturating_add(n);
        if self.fuel_spent > self.limits.fuel {
            return Err(LimitError::FuelExhausted {
                spent: self.fuel_spent,
            });
        }
        Ok(())
    }

    /// Fuel consumed so far.
    #[must_use]
    pub const fn fuel_spent(&self) -> u64 {
        self.fuel_spent
    }

    /// Fuel still available.
    #[must_use]
    pub const fn fuel_remaining(&self) -> u64 {
        self.limits.fuel.saturating_sub(self.fuel_spent)
    }

    /// Reset the fuel counter, at a boundary where a fresh allowance is correct
    /// (one packet, one frame). Allocation accounting is untouched.
    pub const fn refuel(&mut self) {
        self.fuel_spent = 0;
    }

    // --------------------------------------------------------- derived checks

    /// Check the wall-clock deadline, if one is configured.
    ///
    /// # Errors
    ///
    /// [`LimitError::DeadlineExceeded`].
    pub fn check_deadline(&self) -> Result<()> {
        match self.limits.deadline {
            Some(at) if vaco_time::Instant::now() >= at => Err(LimitError::DeadlineExceeded),
            _ => Ok(()),
        }
    }

    /// Validate video dimensions *and* the frame size they imply, before any
    /// frame buffer is touched. All arithmetic is checked.
    ///
    /// # Errors
    ///
    /// [`LimitError::Exceeded`] if either axis is over `max_dimension` or the
    /// implied frame size is over `max_frame_bytes`; [`LimitError::Overflow`] if
    /// the product does not fit.
    pub fn check_frame(&self, width: u32, height: u32, bytes_per_pixel: u32) -> Result<u64> {
        for (name, v) in [("width", width), ("height", height)] {
            if v > self.limits.max_dimension {
                return Err(LimitError::Exceeded {
                    limit: match name {
                        "width" => "max_dimension (width)",
                        _ => "max_dimension (height)",
                    },
                    requested: u64::from(v),
                    cap: u64::from(self.limits.max_dimension),
                });
            }
        }
        let bytes = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|a| a.checked_mul(u64::from(bytes_per_pixel)))
            .ok_or(LimitError::Overflow)?;
        if bytes > self.limits.max_frame_bytes {
            return Err(LimitError::Exceeded {
                limit: "max_frame_bytes",
                requested: bytes,
                cap: self.limits.max_frame_bytes,
            });
        }
        Ok(bytes)
    }

    /// Validate a count against a named cap.
    ///
    /// The generic form behind `check_streams` / `check_channels` and friends;
    /// use it directly for a cap this crate does not name yet.
    ///
    /// # Errors
    ///
    /// [`LimitError::Exceeded`].
    pub const fn check_count(&self, limit: &'static str, n: u64, cap: u64) -> Result<()> {
        if n > cap {
            return Err(LimitError::Exceeded {
                limit,
                requested: n,
                cap,
            });
        }
        Ok(())
    }

    /// Stream count in one container.
    ///
    /// # Errors
    /// [`LimitError::Exceeded`].
    pub const fn check_streams(&self, n: u64) -> Result<()> {
        self.check_count("max_streams", n, self.limits.max_streams as u64)
    }

    /// Audio channel count.
    ///
    /// # Errors
    /// [`LimitError::Exceeded`].
    pub const fn check_channels(&self, n: u64) -> Result<()> {
        self.check_count("max_channels", n, self.limits.max_channels as u64)
    }

    /// Audio sample rate, Hz.
    ///
    /// # Errors
    /// [`LimitError::Exceeded`].
    pub const fn check_sample_rate(&self, hz: u64) -> Result<()> {
        self.check_count("max_sample_rate", hz, self.limits.max_sample_rate as u64)
    }

    /// Side-data entries on one packet or frame.
    ///
    /// # Errors
    /// [`LimitError::Exceeded`].
    pub const fn check_side_data(&self, n: u64) -> Result<()> {
        self.check_count("max_side_data", n, self.limits.max_side_data as u64)
    }

    /// Bytes consumed while probing for a format.
    ///
    /// # Errors
    /// [`LimitError::Exceeded`].
    pub const fn check_probe_bytes(&self, n: u64) -> Result<()> {
        self.check_count("max_probe_bytes", n, self.limits.max_probe_bytes)
    }

    /// Bytes of metadata retained from one container.
    ///
    /// # Errors
    /// [`LimitError::Exceeded`].
    pub const fn check_metadata_bytes(&self, n: u64) -> Result<()> {
        self.check_count("max_metadata_bytes", n, self.limits.max_metadata_bytes)
    }

    const fn note_peak(&mut self) {
        let live = self.committed.saturating_add(self.pending);
        if live > self.peak {
            self.peak = live;
        }
    }
}

/// `n * size_of::<T>()` in checked arithmetic.
///
/// # Errors
/// [`LimitError::Overflow`].
fn byte_size<T>(n: usize) -> Result<u64> {
    u64::try_from(n)
        .ok()
        .and_then(|n| n.checked_mul(size_of::<T>() as u64))
        .ok_or(LimitError::Overflow)
}

/// A checked-but-not-yet-spent claim on a [`Budget`].
///
/// Phase one of two-phase reservation. Dropping it releases the hold, so the
/// reject branch of "parse a length, validate it, then allocate" cannot leak
/// budget — there is no way to forget the release because there is no release to
/// forget.
#[derive(Debug)]
#[must_use = "a reservation holds budget; commit it or let it drop deliberately"]
pub struct Reservation<'a> {
    budget: &'a mut Budget,
    bytes: u64,
}

impl Reservation<'_> {
    /// The reserved size.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Phase two: spend the reservation. The bytes move from `pending` to
    /// `committed` and stay charged until [`Budget::release`].
    pub fn commit(self) {
        self.budget.committed = self.budget.committed.saturating_add(self.bytes);
        // `Drop` then removes the same amount from `pending`.
    }

    /// Commit and allocate in one step.
    ///
    /// Verifies that `n` elements of `T` actually fit in what was reserved — the
    /// point of two-phase reservation is lost if phase two can quietly allocate
    /// more than phase one checked.
    ///
    /// # Errors
    ///
    /// [`LimitError::Overflow`] if the size computation overflows or exceeds the
    /// reservation; [`LimitError::AllocFailed`] if the allocator refuses.
    pub fn alloc<T: Copy + Default>(self, n: usize) -> Result<Vec<T>> {
        let want = byte_size::<T>(n)?;
        if want > self.bytes {
            return Err(LimitError::Overflow);
        }
        let mut v: Vec<T> = Vec::new();
        v.try_reserve_exact(n)
            .map_err(|_| LimitError::AllocFailed { bytes: want })?;
        v.resize(n, T::default());
        self.commit();
        Ok(v)
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget.pending = self.budget.pending.saturating_sub(self.bytes);
    }
}

/// A buffer that grows towards a declared size instead of jumping to it.
///
/// The answer to "the header says 4 GiB and the file is 16 bytes". Capacity
/// doubles as data arrives and each growth is charged to the [`Budget`], so the
/// peak charge tracks what was really delivered, never what was claimed.
///
/// Dropping an `IncrementalVec` does **not** credit the budget back — call
/// [`Budget::release`] with [`IncrementalVec::charged`] if the budget outlives
/// the buffer.
#[derive(Debug)]
pub struct IncrementalVec<T> {
    data: Vec<T>,
    declared: usize,
    charged: u64,
}

/// Smallest capacity worth asking the allocator for.
const MIN_CAP: usize = 32;

impl<T: Copy> IncrementalVec<T> {
    /// An empty buffer that will refuse to exceed `declared` elements.
    #[must_use]
    pub const fn new(declared: usize) -> Self {
        Self {
            data: Vec::new(),
            declared,
            charged: 0,
        }
    }

    /// Append data that has actually been read.
    ///
    /// # Errors
    ///
    /// [`LimitError::Exceeded`] if the append would pass the declared size or a
    /// budget cap, [`LimitError::Overflow`] on a size computation overflow, and
    /// [`LimitError::AllocFailed`] if the allocator refuses.
    pub fn push_slice(&mut self, budget: &mut Budget, src: &[T]) -> Result<()> {
        let needed = self
            .data
            .len()
            .checked_add(src.len())
            .ok_or(LimitError::Overflow)?;
        if needed > self.declared {
            return Err(LimitError::Exceeded {
                limit: "declared_size",
                requested: needed as u64,
                cap: self.declared as u64,
            });
        }
        let cap = self.data.capacity();
        if needed > cap {
            // Geometric, but never past what was declared: the two together mean
            // capacity is bounded by min(2 * delivered, declared).
            let target = needed
                .max(cap.saturating_mul(2))
                .max(MIN_CAP)
                .min(self.declared.max(needed));
            let extra = byte_size::<T>(target.saturating_sub(cap))?;
            budget.charge(extra)?;
            self.charged = self.charged.saturating_add(extra);
            self.data
                .try_reserve_exact(target.saturating_sub(self.data.len()))
                .map_err(|_| LimitError::AllocFailed { bytes: extra })?;
        }
        self.data.extend_from_slice(src);
        Ok(())
    }

    /// The bytes charged to the budget so far.
    #[must_use]
    pub const fn charged(&self) -> u64 {
        self.charged
    }

    /// The size the input claimed.
    #[must_use]
    pub const fn declared(&self) -> usize {
        self.declared
    }

    /// Elements delivered so far.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether anything has been delivered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// What has been delivered.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    /// Take the buffer.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.data
    }
}
