//! Timestamp compensation: soft, hard, the `async` convenience, `first_pts`
//! and the manual API.
//!
//! # What the reference's four knobs measure
//!
//! Probed against `FFmpeg` 9.0.1 through the `aresample` filter, feeding a
//! filter-graph-injected pts anomaly (`asetpts`) into a raw source and
//! reading the actual sample count back out (never the reference's source —
//! only its binary output, per D6/D7). The full commands and byte counts are
//! recorded in `docs/signal/vaco-resample.md` §"Timestamp compensation,
//! measured". The short version:
//!
//! * **`min_comp`** (seconds) is the master switch. Its default is `FLT_MAX`,
//!   and with it untouched **no compensation of either kind ever fires**,
//!   confirmed by feeding a full one-second pts jump through plain
//!   `aresample=48000` and reading back exactly the original sample count.
//!   Below `min_comp`, drift is ignored outright.
//! * **`min_hard_comp`** (seconds) is the hard threshold, and the boundary is
//!   exact and exclusive: a 4800-sample (0.100000 s) jump at the default
//!   `min_hard_comp=0.1` produces zero extra samples; 4801 samples
//!   (0.100021 s) produces *exactly* 4801 extra samples, inserted as a single
//!   step. A full one-second jump inserts exactly 48000 samples. Hard
//!   compensation is a one-shot, exact fill or trim, not a gradual process.
//! * **Soft compensation reacts to continuous drift, not a one-off jump.** A
//!   pts track scaled by `1.0004` (a simulated ~0.04% clock skew) against
//!   `min_comp=0:max_soft_comp=1000:comp_duration=1:min_hard_comp=999`
//!   produced 59 extra samples over 5 seconds — real, gradually-inserted
//!   correction — while the same one-shot step-jump scenario that drives hard
//!   compensation produced no measurable change under a soft-only
//!   configuration (`min_hard_comp=999`, i.e. hard disabled): soft
//!   compensation is the reference's answer to a source clock that runs at
//!   very slightly the wrong rate, not to a discontinuity.
//! * **`async`** resolves to the other three. `async=1` reproduces hard-only
//!   behaviour with the default `min_hard_comp` (measured identical sample
//!   counts to the explicit-hard cases above). `async=1000` reproduces the
//!   continuous-drift soft-correction behaviour. The resolution rule that
//!   fits every measurement: `async != 0` sets `min_comp = 0` (arm the master
//!   switch) and sets `max_soft_comp = async` only when `|async| > 1`
//!   (`async == 1` stays hard-only, matching the legacy `-async 1`
//!   "fill/trim only" description); `min_hard_comp` is left at whatever it
//!   was, since `async` never touched it in any measurement.
//! * **`first_pts`** sets the assumed pts of the first input sample. Left
//!   unset, a stream whose first real pts is nonzero (e.g. a container start
//!   offset) is *not* treated as drift — probed by holding a constant
//!   `+48000`-sample pts offset from the very first frame and confirming zero
//!   inserted samples under `async=1`. Explicitly setting `first_pts` to a
//!   value that *disagrees* with the real first pts (`first_pts=0` against a
//!   real start of 48000) reliably reproduces the drift and triggers the
//!   expected hard correction (48000 samples inserted) — confirming
//!   `first_pts` is the assumed baseline the first observed pts is compared
//!   against, not merely a label applied to the output.
//!
//! # What is ours, not measured
//!
//! The reference does not expose *how* a soft correction reshapes the
//! sample stream — only its aggregate effect on the sample count, which is
//! all `swr_set_compensation`'s public contract promises. The actual
//! stretch here ([`linear_resample`]) is a plain linear-interpolation resize
//! of the affected span: original DSP, not a transcription of anything. Per
//! the owner's byte-exactness ruling, that is a legitimate divergence as
//! long as it stays small and unstructured, which it does by construction —
//! it only ever touches spans bounded by [`MAX_COMPENSATION_SAMPLES`], a tiny
//! fraction of any real stream.

#![allow(
    clippy::integer_division,
    reason = "denominators here (rates, distances) are checked non-zero immediately before use"
)]

use vaco_core::Error;

use crate::convert::Internal;

/// Bound on any single compensation request — automatic (from a drift
/// measurement) or manual ([`crate::Resampler::set_compensation`]) — in
/// samples.
///
/// Nothing structural stops a caller from passing an adversarial pts miles
/// away from reality, which would otherwise ask for an unbounded silence
/// insertion or an unbounded stretch window. Ten seconds at the highest
/// common broadcast rate (192 kHz) is far more slack than any real A/V-sync
/// correction needs, and still small enough that even the worst case is a
/// bounded, momentary allocation rather than a denial-of-service surface —
/// the same reasoning [`crate::rate::MAX_RATE_RATIO`] and
/// [`crate::design::MAX_TAPS`] already use for their own bounds.
pub const MAX_COMPENSATION_SAMPLES: i64 = 10 * 192_000;

/// The resolved policy: `async` already folded into `min_comp` /
/// `max_soft_comp`, so downstream code only ever sees the three-threshold
/// model.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Policy {
    pub min_comp_s: f64,
    pub min_hard_comp_s: f64,
    pub comp_duration_s: f64,
    pub max_soft_comp: f64,
}

/// What [`Tracker::observe`] decided to do about one pts observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Decision {
    /// Drift is within `min_comp`, or nothing to do.
    None,
    /// Insert (positive) or drop (negative) exactly this many *input-rate*
    /// samples, immediately.
    Hard(i64),
    /// Spread a correction of this many *input-rate* samples over this many
    /// seconds.
    Soft(i64, f64),
}

/// Tracks the expected input pts against actual samples consumed, so a
/// slowly drifting source clock (not just a step discontinuity) is visible.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Tracker {
    /// `first_pts`, re-applied by [`Tracker::reset`] so a reset resampler
    /// behaves like a freshly constructed one.
    seed: Option<i64>,
    /// `(baseline pts, samples consumed since)`, once a baseline exists.
    baseline: Option<(i64, u64)>,
}

impl Tracker {
    pub(crate) fn new(first_pts: Option<i64>) -> Self {
        Self {
            seed: first_pts,
            baseline: first_pts.map(|p| (p, 0)),
        }
    }

    /// Record that `n` samples — real, inserted silence, or dropped real
    /// samples — have now passed the point the baseline is measured from.
    /// Every one of those three actually changes the stream's true position,
    /// which is what a later drift measurement compares against.
    pub(crate) fn account(&mut self, n: u64) {
        if let Some((_, consumed)) = &mut self.baseline {
            *consumed = consumed.saturating_add(n);
        }
    }

    /// Restore the state a freshly constructed tracker would have.
    pub(crate) fn reset(&mut self) {
        self.baseline = self.seed.map(|p| (p, 0));
    }

    /// `input_pts` is the pts (input-rate samples) the *next* input chunk is
    /// expected to carry. The first call ever made establishes the baseline
    /// (unless `first_pts` already supplied one at construction) and never
    /// signals compensation — a stream's own starting position is not drift.
    pub(crate) fn observe(&mut self, policy: &Policy, in_rate: u32, input_pts: i64) -> Decision {
        let Some((base_pts, consumed)) = self.baseline else {
            self.baseline = Some((input_pts, 0));
            return Decision::None;
        };
        let consumed_i = i64::try_from(consumed).unwrap_or(i64::MAX);
        let predicted = base_pts.saturating_add(consumed_i);
        let drift_samples = input_pts.saturating_sub(predicted);
        if drift_samples == 0 {
            return Decision::None;
        }
        let rate = f64::from(in_rate.max(1));
        let drift_s = (drift_samples as f64) / rate;
        if drift_s.abs() <= policy.min_comp_s {
            Decision::None
        } else if drift_s.abs() > policy.min_hard_comp_s {
            Decision::Hard(drift_samples)
        } else if policy.max_soft_comp > 0.0 {
            Decision::Soft(drift_samples, policy.comp_duration_s)
        } else {
            Decision::None
        }
    }
}

/// An in-progress soft correction, in output-rate samples: `remaining_delta`
/// still to add (positive) or remove (negative), spread over the next
/// `remaining_distance` output samples.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SoftWindow {
    pub remaining_delta: i64,
    pub remaining_distance: u64,
}

impl SoftWindow {
    /// # Errors
    /// [`Error::LimitExceeded`] if `delta` exceeds [`MAX_COMPENSATION_SAMPLES`].
    pub(crate) fn new(delta: i64, distance: u64) -> Result<Self, Error> {
        if delta.unsigned_abs() > MAX_COMPENSATION_SAMPLES.unsigned_abs() {
            return Err(Error::LimitExceeded {
                limit: "resample timestamp compensation",
                requested: delta.unsigned_abs(),
                cap: MAX_COMPENSATION_SAMPLES.unsigned_abs(),
            });
        }
        Ok(Self {
            remaining_delta: delta,
            remaining_distance: distance.max(1),
        })
    }

    /// Claim up to `available` output samples of the window, returning
    /// `(share_distance, share_delta)`: how many of those samples this call
    /// covers, and the (rounded) portion of the outstanding delta that
    /// applies to them. Exhausting the window is the caller's cue to drop
    /// this state.
    pub(crate) fn share(&mut self, available: usize) -> (usize, i64) {
        let share_distance = available.min(self.remaining_distance as usize);
        if share_distance == 0 {
            return (0, 0);
        }
        // i128: `delta * distance` can exceed i64 only for values far beyond
        // MAX_COMPENSATION_SAMPLES, but the multiplication itself is cheap
        // and this keeps the bound a property of the inputs, not of this sum.
        let share_delta = i64::try_from(
            i128::from(self.remaining_delta) * i128::from(share_distance as u64)
                / i128::from(self.remaining_distance),
        )
        .unwrap_or(self.remaining_delta);
        self.remaining_delta -= share_delta;
        self.remaining_distance -= share_distance as u64;
        (share_distance, share_delta)
    }

    pub(crate) const fn is_exhausted(&self) -> bool {
        self.remaining_distance == 0
    }
}

/// Resize `src` to `target_len` samples by linear interpolation.
///
/// This is the mechanism behind soft compensation's stretch/squeeze: see the
/// module docs for why it is original DSP rather than a measured property of
/// the reference.
pub(crate) fn linear_resample<T: Internal>(src: &[T], target_len: usize) -> Vec<T> {
    let n = src.len();
    if target_len == 0 || n == 0 {
        return Vec::new();
    }
    if n == 1 || target_len == 1 {
        let v = src.first().copied().unwrap_or(T::ZERO);
        let mut out = Vec::new();
        for _ in 0..target_len {
            out.push(v);
        }
        return out;
    }
    let mut out = Vec::new();
    let scale = (n - 1) as f64 / (target_len - 1) as f64;
    for j in 0..target_len {
        let pos = j as f64 * scale;
        let i0 = pos.floor().max(0.0) as usize;
        let frac = pos - (i0 as f64);
        let a = src.get(i0).copied().unwrap_or(T::ZERO);
        let b = src.get(i0 + 1).copied().unwrap_or(a);
        out.push(T::from_f64(a.to_f64() + (b.to_f64() - a.to_f64()) * frac));
    }
    out
}
