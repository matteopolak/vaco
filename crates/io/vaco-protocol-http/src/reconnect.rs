//! Reconnect policy: whether to retry, and how long to wait first.
//!
//! Portable: takes and returns plain values (`vaco_time::Duration` included —
//! it is a `core::time::Duration` re-export, present on every target), never
//! touches a socket or sleeps. [`crate::source::HttpSource`] is the only
//! caller, and it is the one that actually calls `std::thread::sleep` and
//! reopens a connection.
//!
//! # What the reference documents versus what it does
//!
//! `ffprobe -h protocol=http` documents `-reconnect_delay_max` and
//! `-reconnect_max_retries` as caps, but not the backoff schedule between
//! attempts — that is an implementation detail the reference does not commit
//! to as observable behaviour, so D17 does not bind us to guessing at it byte
//! for byte. We use a plain doubling schedule (1s, 2s, 4s, … capped at
//! `reconnect_delay_max`), which is the standard shape for this kind of
//! policy and is documented here as *our* choice, not a measured one.

use vaco_time::{Duration, Instant};

use crate::options::HttpOptions;

/// Why a read or connect attempt failed, for [`decide`] to classify against
/// the option set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The connect attempt itself failed (TCP refused, TLS handshake error).
    NetworkError,
    /// An established stream broke before the expected end.
    StreamDropped,
    /// The stream ended (`read` returned 0) before the known total size, or
    /// with no known total at all (a live/forward-only source).
    UnexpectedEof { total_known: bool },
    /// The server answered with a status code, which
    /// `-reconnect_on_http_error` may name as reconnect-worthy.
    HttpStatus(u16),
}

/// What [`decide`] recommends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Wait `after`, then retry.
    Retry { after: Duration },
    /// Stop and surface the failure.
    GiveUp,
}

/// Running state across the reconnect attempts of a single open stream.
///
/// Reset (via [`State::new`]) whenever a read actually makes forward
/// progress, mirroring `vaco_limits::ProgressGuard`'s "a run of stalls, not
/// isolated ones, is the problem" shape — a stream that reconnects once every
/// ten minutes for a week should not be treated as having burned through
/// `reconnect_max_retries` on attempt one.
#[derive(Debug, Clone, Copy)]
pub struct State {
    attempts: u32,
    first_failure_at: Option<Instant>,
}

impl State {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            attempts: 0,
            first_failure_at: None,
        }
    }

    /// Forward progress happened: the run of failures is over.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// Decide whether to reconnect after `failure`, and for how long to wait
/// first.
///
/// `retry_after_secs` carries a server-supplied `Retry-After` value (already
/// parsed by [`crate::parse::parse_retry_after_secs`]); it is honoured only
/// when `-respect_retry_after` is set, and only ever *extends* the wait
/// relative to our own backoff floor, never shortens it below zero — a
/// server cannot use this to make us hammer it with `Retry-After: 0`.
#[must_use]
pub fn decide(
    opts: &HttpOptions,
    state: &mut State,
    failure: Failure,
    now: Instant,
    retry_after_secs: Option<u64>,
) -> Decision {
    let eligible = match failure {
        Failure::NetworkError => opts.reconnect_on_network_error,
        Failure::StreamDropped => opts.reconnect,
        Failure::UnexpectedEof { total_known } => {
            if total_known {
                opts.reconnect_at_eof
            } else {
                opts.reconnect_streamed
            }
        }
        Failure::HttpStatus(code) => {
            crate::parse::parse_reconnect_codes(&opts.reconnect_on_http_error).contains(&code)
        }
    };
    if !eligible {
        return Decision::GiveUp;
    }

    if opts.reconnect_max_retries >= 0
        && state.attempts >= u32_from_nonneg_i32(opts.reconnect_max_retries)
    {
        return Decision::GiveUp;
    }

    let started = *state.first_failure_at.get_or_insert(now);
    let elapsed = now.duration_since(started);

    let cap = Duration::from_secs(u64_from_nonneg_i32(opts.reconnect_delay_max));
    // `attempts` is capped at 20 before the shift, so this never approaches
    // `u64`'s width — a plain `<<` is exact and cannot overflow.
    let backoff = Duration::from_secs(1u64 << state.attempts.min(20)).min(cap);
    let wait = if opts.respect_retry_after {
        match retry_after_secs {
            Some(s) => backoff
                .max(Duration::from_secs(s))
                .min(cap.max(Duration::from_secs(s))),
            None => backoff,
        }
    } else {
        backoff
    };

    let total_max = Duration::from_secs(u64_from_nonneg_i32(opts.reconnect_delay_total_max));
    if elapsed.saturating_add(wait) > total_max {
        return Decision::GiveUp;
    }

    state.attempts = state.attempts.saturating_add(1);
    Decision::Retry { after: wait }
}

const fn u32_from_nonneg_i32(v: i32) -> u32 {
    if v < 0 { 0 } else { v as u32 }
}

const fn u64_from_nonneg_i32(v: i32) -> u64 {
    if v < 0 { 0 } else { v as u64 }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    fn opts_with(f: impl FnOnce(&mut HttpOptions)) -> HttpOptions {
        let mut o = HttpOptions::default();
        f(&mut o);
        o
    }

    #[test]
    fn disabled_by_default_for_every_failure_kind() {
        let opts = HttpOptions::default();
        let mut state = State::new();
        let now = Instant::now();
        for f in [
            Failure::NetworkError,
            Failure::StreamDropped,
            Failure::UnexpectedEof { total_known: true },
            Failure::UnexpectedEof { total_known: false },
            Failure::HttpStatus(503),
        ] {
            assert_eq!(decide(&opts, &mut state, f, now, None), Decision::GiveUp);
        }
    }

    #[test]
    fn reconnect_flag_gates_stream_dropped_only() {
        let opts = opts_with(|o| o.reconnect = true);
        let mut state = State::new();
        let now = Instant::now();
        assert!(matches!(
            decide(&opts, &mut state, Failure::StreamDropped, now, None),
            Decision::Retry { .. }
        ));
        state = State::new();
        assert_eq!(
            decide(&opts, &mut state, Failure::NetworkError, now, None),
            Decision::GiveUp
        );
    }

    #[test]
    fn http_status_reconnects_only_when_listed() {
        let opts = opts_with(|o| o.reconnect_on_http_error = "503,504".to_owned());
        let mut state = State::new();
        let now = Instant::now();
        assert!(matches!(
            decide(&opts, &mut state, Failure::HttpStatus(503), now, None),
            Decision::Retry { .. }
        ));
        state = State::new();
        assert_eq!(
            decide(&opts, &mut state, Failure::HttpStatus(500), now, None),
            Decision::GiveUp
        );
    }

    #[test]
    fn max_retries_is_enforced() {
        let opts = opts_with(|o| {
            o.reconnect = true;
            o.reconnect_max_retries = 2;
            o.reconnect_delay_max = 0;
            o.reconnect_delay_total_max = 4294;
        });
        let mut state = State::new();
        let now = Instant::now();
        assert!(matches!(
            decide(&opts, &mut state, Failure::StreamDropped, now, None),
            Decision::Retry { .. }
        ));
        assert!(matches!(
            decide(&opts, &mut state, Failure::StreamDropped, now, None),
            Decision::Retry { .. }
        ));
        assert_eq!(
            decide(&opts, &mut state, Failure::StreamDropped, now, None),
            Decision::GiveUp
        );
    }

    #[test]
    fn total_delay_cap_is_enforced() {
        let opts = opts_with(|o| {
            o.reconnect = true;
            o.reconnect_max_retries = -1;
            o.reconnect_delay_max = 100;
            o.reconnect_delay_total_max = 1;
        });
        let mut state = State::new();
        let now = Instant::now();
        // First attempt waits 1s (2^0), which exactly meets the 1s total cap.
        assert!(matches!(
            decide(&opts, &mut state, Failure::StreamDropped, now, None),
            Decision::Retry { .. }
        ));
        // Simulate having actually waited the first 1s backoff: `elapsed`
        // since the first failure is now 1s, and a second 2s backoff would
        // push it past the 1s total cap.
        let later = now.saturating_add(Duration::from_secs(1));
        assert_eq!(
            decide(&opts, &mut state, Failure::StreamDropped, later, None),
            Decision::GiveUp
        );
    }

    #[test]
    fn retry_after_extends_but_never_shortens_the_wait() {
        let opts = opts_with(|o| {
            o.reconnect = true;
            o.reconnect_delay_max = 5;
            o.reconnect_delay_total_max = 4294;
        });
        let mut state = State::new();
        let now = Instant::now();
        // Backoff floor is 1s; a server-supplied 0 must not undercut it.
        let Decision::Retry { after } =
            decide(&opts, &mut state, Failure::StreamDropped, now, Some(0))
        else {
            panic!("expected a retry");
        };
        assert!(after >= Duration::from_secs(1));
    }

    #[test]
    fn respect_retry_after_false_ignores_the_header() {
        let opts = opts_with(|o| {
            o.reconnect = true;
            o.respect_retry_after = false;
            o.reconnect_delay_max = 100;
            o.reconnect_delay_total_max = 4294;
        });
        let mut state = State::new();
        let now = Instant::now();
        let Decision::Retry { after } =
            decide(&opts, &mut state, Failure::StreamDropped, now, Some(90))
        else {
            panic!("expected a retry");
        };
        assert_eq!(after, Duration::from_secs(1));
    }
}
