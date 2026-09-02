//! The option surface, scoped deliberately narrow.
//!
//! **This is not an attempt to reconstruct `ffmpeg -h protocol=srt`'s own
//! option table.** There is no `libsrt`-carrying `ffmpeg` build on this
//! machine to measure that table against (`lib.rs`'s own docs), and real
//! SRT deployments have option names (`latency`, `rcvbuf`, `sndbuf`,
//! `passphrase`, `streamid`, `transtype`, ...) this crate has not verified
//! come from `draft-sharabayko-srt-01` itself rather than from a specific
//! implementation's own conventions — reconstructing that table from
//! general knowledge would risk smuggling implementation-specific detail
//! into this crate under a draft-derived label, the same clean-room
//! concern that keeps GXF's own scope narrow.
//!
//! [`SrtOptions`] instead names exactly the knobs something in this crate
//! actually reads today: [`crate::message::TransmissionMode`] (derived
//! from the peer's own `HSREQ`/`HSRSP`, but a caller building the local
//! side's own handshake needs to state which mode it wants),
//! [`crate::arq::ReceiveConfig::latency_ms`], [`crate::arq::SendConfig::rto_ms`],
//! and an optional [`crate::pacing::Pacer`] ceiling
//! (`rate_limit_bytes_per_sec`). Nothing else — a `streamid` or
//! `passphrase` field would be pure surface with nothing behind it, since
//! `SRT_CMD_SID`'s raw bytes are already parseable via
//! `handshake::Extension`/`parse_extensions` but nothing in this crate
//! interprets its contents as an application-facing
//! option yet, and `passphrase`/key-derivation waits on the same
//! crypto-ownership question `km.rs`'s docs name. `rate_limit_bytes_per_sec`
//! is `None` by default — matching real SRT's own unbounded-unless-asked
//! `maxbw`, though this crate has no `libsrt` build to confirm that name or
//! default against (`lib.rs`'s own docs on why this module does not
//! attempt real SRT's option table) — not a value this crate invented a
//! number for.

use crate::arq::{ReceiveConfig, SendConfig};
use crate::message::TransmissionMode;

/// Everything a caller configures before starting a handshake with this
/// crate.
#[derive(Debug, Clone, Copy)]
pub struct SrtOptions {
    pub transmission_mode: TransmissionMode,
    pub latency_ms: u64,
    pub rto_ms: u64,
    /// A plain token-bucket byte-rate ceiling for [`crate::arq::SendWindow`]
    /// — `None` means unthrottled, `arq::SendWindow::new`'s own default.
    /// Not `LiveCC`/`FileCC`; see `pacing.rs`'s module docs for what this
    /// is and is not.
    pub rate_limit_bytes_per_sec: Option<u64>,
}

impl SrtOptions {
    /// [`TransmissionMode::Message`] (matching this module's own reading of
    /// `STREAM`'s absence as the default, see `message.rs`), this crate's
    /// `IMPLEMENTATION-DEFINED` latency/RTO defaults (`arq.rs`), and no
    /// rate limit (unthrottled, the behaviour this crate always had).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            transmission_mode: TransmissionMode::Message,
            latency_ms: crate::arq::DEFAULT_LATENCY_MS,
            rto_ms: crate::arq::DEFAULT_RTO_MS,
            rate_limit_bytes_per_sec: None,
        }
    }

    #[must_use]
    pub const fn receive_config(&self) -> ReceiveConfig {
        ReceiveConfig {
            latency_ms: self.latency_ms,
        }
    }

    #[must_use]
    pub const fn send_config(&self) -> SendConfig {
        SendConfig {
            rto_ms: self.rto_ms,
        }
    }

    /// A [`crate::arq::SendWindow`] built from [`Self::send_config`] and,
    /// if [`Self::rate_limit_bytes_per_sec`](SrtOptions::rate_limit_bytes_per_sec)
    /// is set, gated by a fresh [`crate::pacing::Pacer`] starting its
    /// one-second burst budget full at `now_ms`.
    #[must_use]
    pub fn send_window(&self, now_ms: u64) -> crate::arq::SendWindow {
        let window = crate::arq::SendWindow::new(self.send_config());
        match self.rate_limit_bytes_per_sec {
            Some(bytes_per_sec) => window.with_rate_limit(bytes_per_sec, now_ms),
            None => window,
        }
    }
}

impl Default for SrtOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_this_crates_own_documented_implementation_defined_values() {
        let opts = SrtOptions::new();
        assert_eq!(opts.transmission_mode, TransmissionMode::Message);
        assert_eq!(opts.latency_ms, crate::arq::DEFAULT_LATENCY_MS);
        assert_eq!(opts.rto_ms, crate::arq::DEFAULT_RTO_MS);
    }

    #[test]
    fn feeds_the_arq_configs_it_names() {
        let opts = SrtOptions {
            transmission_mode: TransmissionMode::Stream,
            latency_ms: 250,
            rto_ms: 30,
            rate_limit_bytes_per_sec: None,
        };
        assert_eq!(opts.receive_config().latency_ms, 250);
        assert_eq!(opts.send_config().rto_ms, 30);
    }

    #[test]
    fn default_options_build_an_unthrottled_send_window() {
        let opts = SrtOptions::new();
        assert_eq!(opts.rate_limit_bytes_per_sec, None);
        let mut window = opts.send_window(0);
        assert_eq!(window.rate_limit_bytes_per_sec(), None);
        // Unthrottled: an enormous single send is still permitted.
        assert!(window.send_permitted(0, 10_000_000));
    }

    #[test]
    fn a_configured_rate_limit_reaches_the_built_send_window() {
        let opts = SrtOptions {
            rate_limit_bytes_per_sec: Some(1000),
            ..SrtOptions::new()
        };
        let mut window = opts.send_window(0);
        assert_eq!(window.rate_limit_bytes_per_sec(), Some(1000));
        assert!(window.send_permitted(0, 1000));
        assert!(
            !window.send_permitted(0, 1),
            "the one-second burst budget is now spent"
        );
    }
}
