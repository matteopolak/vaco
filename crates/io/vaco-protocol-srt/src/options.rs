//! The option surface — issue #557, scoped deliberately narrow.
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
//! [`crate::arq::ReceiveConfig::latency_ms`], and
//! [`crate::arq::SendConfig::rto_ms`]. Nothing else — a `streamid` or
//! `passphrase` field would be pure surface with nothing behind it, since
//! `SRT_CMD_SID`'s raw bytes are already parseable via `handshake::
//! Extension`/`parse_extensions` (#555) but nothing in this crate
//! interprets its contents as an application-facing option yet, and
//! `passphrase`/key-derivation waits on the same crypto-ownership question
//! `km.rs`'s docs name.

use crate::arq::{ReceiveConfig, SendConfig};
use crate::message::TransmissionMode;

/// Everything a caller configures before starting a handshake with this
/// crate.
#[derive(Debug, Clone, Copy)]
pub struct SrtOptions {
    pub transmission_mode: TransmissionMode,
    pub latency_ms: u64,
    pub rto_ms: u64,
}

impl SrtOptions {
    /// [`TransmissionMode::Message`] (matching this module's own reading of
    /// `STREAM`'s absence as the default, see `message.rs`) and this
    /// crate's `IMPLEMENTATION-DEFINED` latency/RTO defaults (`arq.rs`).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            transmission_mode: TransmissionMode::Message,
            latency_ms: crate::arq::DEFAULT_LATENCY_MS,
            rto_ms: crate::arq::DEFAULT_RTO_MS,
        }
    }

    #[must_use]
    pub const fn receive_config(&self) -> ReceiveConfig {
        ReceiveConfig { latency_ms: self.latency_ms }
    }

    #[must_use]
    pub const fn send_config(&self) -> SendConfig {
        SendConfig { rto_ms: self.rto_ms }
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
        let opts = SrtOptions { transmission_mode: TransmissionMode::Stream, latency_ms: 250, rto_ms: 30 };
        assert_eq!(opts.receive_config().latency_ms, 250);
        assert_eq!(opts.send_config().rto_ms, 30);
    }
}
