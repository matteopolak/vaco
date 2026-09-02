//! A token-bucket pacer capping outgoing bytes/sec to a caller-configured
//! ceiling.
//!
//! **Not `LiveCC`/`FileCC`, and not claimed to be.** `draft-sharabayko-srt-01`
//! §5.1/§5.2 name SRT's own congestion-control algorithms but, checked
//! directly across two independently-worded fetches that agreed, do not
//! give either one's formula in the fetched text — see `arq.rs`'s module
//! docs, which made the identical call for the ARQ engine's own
//! IMPLEMENTATION-DEFINED constants. Inventing a plausible-looking
//! AIMD/rate-control formula here would be exactly the kind of
//! unverifiable-looking-verified constant this crate has already declined
//! twice.
//!
//! What this *is*: the property every real deployment over a real,
//! capacity-limited link needs regardless of which named algorithm (if
//! any) sits on top of it — something that stops a sender from injecting
//! data faster than a configured ceiling and self-inducing loss purely
//! from link saturation. This crate is sans-io, so nothing here owns a
//! socket or a clock; a caller supplies `now_ms` the same way it drives
//! [`crate::arq::SendWindow::on_tick`].
//!
//! [`Pacer`] is deliberately not wired into [`crate::arq::SendWindow`]
//! automatically — a caller that never asks for a limit
//! (`SendWindow::new`) gets the exact unthrottled behaviour it always
//! had; `SendWindow::with_rate_limit` is additive.

/// A token bucket: `bytes_per_sec` tokens accrue per second of wall time,
/// up to a one-second burst capacity, and [`Pacer::permit`] both checks
/// and (on success) spends the requested amount in one call, so a caller
/// cannot check the budget and then forget to charge it.
#[derive(Debug, Clone)]
pub struct Pacer {
    bytes_per_sec: u64,
    /// Bucket capacity, in bytes — fixed at one second's worth of the
    /// configured rate. IMPLEMENTATION-DEFINED (see module docs): the
    /// draft states no burst allowance for either named congestion
    /// controller, so this is the simplest round number that lets a
    /// single at-or-under-the-ceiling-sized packet always fit, not a
    /// value derived from anything the draft specifies.
    capacity: f64,
    tokens: f64,
    last_refill_ms: u64,
}

impl Pacer {
    /// A pacer capping outgoing data to `bytes_per_sec`, starting with a
    /// full bucket at `now_ms` (so the very first send is never held back
    /// by a cold start).
    #[must_use]
    pub fn new(bytes_per_sec: u64, now_ms: u64) -> Self {
        let capacity = f64_from_u64(bytes_per_sec);
        Self {
            bytes_per_sec,
            capacity,
            tokens: capacity,
            last_refill_ms: now_ms,
        }
    }

    /// The configured ceiling, in bytes/sec.
    #[must_use]
    pub const fn bytes_per_sec(&self) -> u64 {
        self.bytes_per_sec
    }

    fn refill(&mut self, now_ms: u64) {
        let elapsed_ms = now_ms.saturating_sub(self.last_refill_ms);
        if elapsed_ms == 0 {
            return;
        }
        self.last_refill_ms = now_ms;
        if self.bytes_per_sec == 0 {
            return; // a zero-rate pacer never refills, i.e. never permits anything
        }
        let added = f64_from_u64(self.bytes_per_sec) * f64_from_u64(elapsed_ms) / 1000.0;
        self.tokens = (self.tokens + added).min(self.capacity);
    }

    /// Refill for elapsed time since the last call, then report whether
    /// `bytes` may be sent right now. If it may, the tokens are spent as
    /// part of this same call — there is no separate "commit" step.
    pub fn permit(&mut self, now_ms: u64, bytes: usize) -> bool {
        self.refill(now_ms);
        let bytes = f64_from_u64(bytes as u64);
        if self.tokens >= bytes {
            self.tokens -= bytes;
            true
        } else {
            false
        }
    }

    /// How many more milliseconds, from `now_ms`, before `bytes` worth of
    /// budget will be available — `0` if it already is. For a caller that
    /// wants to schedule a retry rather than poll `permit` in a tight
    /// loop. Does not mutate the bucket: calling this never spends tokens.
    #[must_use]
    pub fn until_permitted_ms(&self, now_ms: u64, bytes: usize) -> u64 {
        let elapsed_ms = now_ms.saturating_sub(self.last_refill_ms);
        let projected = if self.bytes_per_sec == 0 {
            self.tokens
        } else {
            let added = f64_from_u64(self.bytes_per_sec) * f64_from_u64(elapsed_ms) / 1000.0;
            (self.tokens + added).min(self.capacity)
        };
        let bytes = f64_from_u64(bytes as u64);
        if projected >= bytes {
            return 0;
        }
        if self.bytes_per_sec == 0 {
            return u64::MAX; // never — a zero-rate pacer permits nothing, ever
        }
        let deficit = bytes - projected;
        let ms = (deficit * 1000.0) / f64_from_u64(self.bytes_per_sec);
        u64_from_f64_ceil(ms)
    }
}

/// `u64 as f64` with the precision-loss lint already allowed workspace-wide
/// for exactly this shape (bit-manipulation and, here, byte/rate
/// arithmetic where the input is bounded well under 2^53) — named so the
/// cast reads as a deliberate conversion rather than a stray `as`.
fn f64_from_u64(v: u64) -> f64 {
    v as f64
}

/// The ceiling of a non-negative `f64` duration, saturating into `u64`
/// rather than panicking on a value that does not fit (`cast_precision_loss`/
/// `cast_possible_truncation` are workspace-allowed, but a saturating helper
/// keeps the intent visible at the call site instead of a bare `as`).
fn u64_from_f64_ceil(v: f64) -> u64 {
    if v <= 0.0 {
        0
    } else if v >= f64_from_u64(u64::MAX) {
        u64::MAX
    } else {
        v.ceil() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_bucket_permits_up_to_capacity_immediately() {
        let mut p = Pacer::new(1000, 0); // 1000 B/s, capacity 1000 B
        assert!(
            p.permit(0, 1000),
            "a cold start must not hold back the first send"
        );
        assert!(!p.permit(0, 1), "the bucket is now empty");
    }

    #[test]
    fn tokens_refill_linearly_with_elapsed_time() {
        let mut p = Pacer::new(1000, 0);
        assert!(p.permit(0, 1000)); // drain it
        assert!(
            !p.permit(500, 600),
            "only 500ms elapsed -> 500 bytes accrued, not 600"
        );
        assert!(
            p.permit(500, 500),
            "exactly the accrued amount must be permitted"
        );
        assert!(!p.permit(500, 1));
    }

    #[test]
    fn refill_never_exceeds_the_one_second_burst_capacity() {
        let mut p = Pacer::new(1000, 0);
        assert!(p.permit(0, 1000));
        // 10 whole seconds pass with no sends -- the bucket must cap at
        // 1000 bytes (one second's worth), not 10_000.
        assert!(p.permit(10_000, 1000));
        assert!(!p.permit(10_000, 1));
    }

    #[test]
    fn a_packet_larger_than_the_available_budget_is_refused_and_uncharged() {
        let mut p = Pacer::new(1000, 0);
        assert!(p.permit(0, 600));
        assert!(!p.permit(0, 500), "only 400 bytes left in the bucket");
        // Refusal must not have spent anything: the 400 remaining are
        // still there for a smaller request.
        assert!(p.permit(0, 400));
    }

    #[test]
    fn until_permitted_reports_zero_when_already_affordable_and_does_not_charge() {
        let p = Pacer::new(1000, 0);
        assert_eq!(p.until_permitted_ms(0, 1000), 0);
        // A read-only query: calling it twice must agree.
        assert_eq!(p.until_permitted_ms(0, 1000), 0);
    }

    #[test]
    fn until_permitted_matches_the_refill_rate() {
        let mut p = Pacer::new(1000, 0);
        assert!(p.permit(0, 1000)); // drain it
        // 400 bytes still needed at 1000 B/s -> 400ms.
        assert_eq!(p.until_permitted_ms(0, 400), 400);
        // Half the wait has already elapsed in wall time.
        assert_eq!(p.until_permitted_ms(200, 400), 200);
    }

    #[test]
    fn a_zero_rate_pacer_permits_nothing_ever() {
        let mut p = Pacer::new(0, 0);
        assert!(!p.permit(0, 1));
        assert!(!p.permit(1_000_000, 1));
        assert_eq!(p.until_permitted_ms(0, 1), u64::MAX);
    }

    #[test]
    fn a_zero_size_send_is_always_permitted_even_from_an_empty_bucket() {
        let mut p = Pacer::new(1000, 0);
        assert!(p.permit(0, 1000));
        assert!(p.permit(0, 0), "sending nothing costs nothing");
    }
}
