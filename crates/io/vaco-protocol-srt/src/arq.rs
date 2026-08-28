//! The ARQ (retransmission) engine: a send-side retransmission buffer and
//! a receive-side loss-detector/reorder-buffer/TSBPD-ish delivery gate.
//!
//! **Sans-io, via an explicit `on_tick`** — `planning/INTERFACE-GAPS.md`
//! gap 28's addendum: `on_packet`-shaped methods handle network input,
//! `on_tick(now_ms)` is called by a driver on its own cadence for
//! timer-driven work (RTO retransmission, delivery-deadline drops, NAK
//! re-announcement). Neither type here owns a socket or reads a clock.
//!
//! # What is draft-derived and what is not
//!
//! `draft-sharabayko-srt-01` §3.2.4 states one interval this module uses
//! verbatim (`ack::FULL_ACK_INTERVAL_MS`, checked in `ack.rs`'s own
//! tests). Everything else this module needs a number for —
//! [`SendConfig::rto_ms`], [`ReceiveConfig::latency_ms`], and the NAK
//! re-announcement policy (every `on_tick` call, unconditionally) — has
//! **no formula or default in the fetched draft text**, checked directly
//! against §4.8 (retransmission), §4.5-4.6 (TSBPD/too-late-drop) and
//! §5.1-5.2 (LiveCC/FileCC) across two independently-worded fetches that
//! agreed: NAK timing/backoff, an RTO computation from RTT/RTTVar, the
//! exact too-late-drop threshold, and any congestion/rate-control
//! algorithm are left to the implementation, not specified by this
//! document. **This is not "unreachable but checkable" the way the
//! missing reference peer is — there is no instrument for these at all,
//! not even in principle, from this draft alone.**
//!
//! Every constant below is therefore marked `IMPLEMENTATION-DEFINED` at
//! its declaration, with its own reasoning, rather than left to look like
//! a spec value — this project has previously carried a plausible-looking
//! constant for nine rounds because nobody recorded whether it was
//! measured or guessed (`planning/TECH-DEBT.md`, the MPEG-1 escape-level
//! sentinel search).
//!
//! **No congestion control / rate limiting is implemented at all.**
//! `draft` §5.1/§5.2 name LiveCC/FileCC but do not give their algorithms
//! in the fetched text, and a made-up AIMD-shaped rate controller would
//! be exactly the kind of unverifiable-looking-verified constant this
//! module's own docs just warned about — omitted and named, not
//! attempted, the same call this dispatch made for WHIP's DTLS gap.

use std::collections::BTreeMap;

/// IMPLEMENTATION-DEFINED: no RTO formula is given in the fetched draft
/// text (see module docs). 100ms is a fixed, conservative placeholder —
/// large enough that a working link's own ACK/NAK round trip resolves
/// loss before RTO fires in the common case, not a value derived from
/// any measured RTT.
pub const DEFAULT_RTO_MS: u64 = 100;

/// IMPLEMENTATION-DEFINED: no TSBPD/too-late-drop threshold formula is
/// given in the fetched draft text (see module docs). 1000ms is a round,
/// generous placeholder — real deployments commonly configure SRT's own
/// `latency` option in roughly this range for live contribution, which is
/// corroborating precedent for the *order of magnitude*, not a value this
/// draft states.
pub const DEFAULT_LATENCY_MS: u64 = 1000;

#[derive(Debug, Clone, Copy)]
pub struct SendConfig {
    pub rto_ms: u64,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self { rto_ms: DEFAULT_RTO_MS }
    }
}

#[derive(Debug)]
struct InFlight {
    timestamp: u32,
    payload: Vec<u8>,
    last_sent_at_ms: u64,
}

/// Counters [`SendWindow`] reports, for the statistics surface (#557).
/// Every field here is this crate's own bookkeeping, not a value read from
/// or compared against a real peer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendStats {
    pub packets_sent: u64,
    pub packets_retransmitted: u64,
}

/// The sender's retransmission buffer.
#[derive(Debug)]
pub struct SendWindow {
    config: SendConfig,
    buffer: BTreeMap<u32, InFlight>,
    stats: SendStats,
}

impl SendWindow {
    #[must_use]
    pub fn new(config: SendConfig) -> Self {
        Self {
            config,
            buffer: BTreeMap::new(),
            stats: SendStats::default(),
        }
    }

    #[must_use]
    pub const fn stats(&self) -> SendStats {
        self.stats
    }

    /// Record a freshly-sent data packet, so it can be retransmitted later.
    pub fn on_send(&mut self, seq_no: u32, timestamp: u32, payload: Vec<u8>, now_ms: u64) {
        self.stats.packets_sent += 1;
        self.buffer.insert(
            seq_no,
            InFlight {
                timestamp,
                payload,
                last_sent_at_ms: now_ms,
            },
        );
    }

    /// The peer has acknowledged everything up to (not including)
    /// `last_ack_seq_no` — those packets need no further retransmission.
    pub fn on_ack(&mut self, last_ack_seq_no: u32) {
        self.buffer = self.buffer.split_off(&last_ack_seq_no);
    }

    /// The peer named these sequence numbers lost — resend every one this
    /// window still holds (a NAK for a packet already `ACKed` or already
    /// dropped from the buffer is simply a no-op, not an error).
    pub fn on_nak(&mut self, lost: &[u32], now_ms: u64) -> Vec<(u32, u32, Vec<u8>)> {
        let mut out = Vec::new();
        for &seq in lost {
            if let Some(entry) = self.buffer.get_mut(&seq) {
                entry.last_sent_at_ms = now_ms;
                out.push((seq, entry.timestamp, entry.payload.clone()));
                self.stats.packets_retransmitted += 1;
            }
        }
        out
    }

    /// RTO-triggered retransmission: anything not resent (by either path)
    /// within `rto_ms` goes out again.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<(u32, u32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut resent = 0u64;
        for (&seq, entry) in &mut self.buffer {
            if now_ms.saturating_sub(entry.last_sent_at_ms) >= self.config.rto_ms {
                entry.last_sent_at_ms = now_ms;
                out.push((seq, entry.timestamp, entry.payload.clone()));
                resent += 1;
            }
        }
        self.stats.packets_retransmitted += resent;
        out
    }

    #[must_use]
    pub fn in_flight_count(&self) -> usize {
        self.buffer.len()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReceiveConfig {
    pub latency_ms: u64,
}

impl Default for ReceiveConfig {
    fn default() -> Self {
        Self { latency_ms: DEFAULT_LATENCY_MS }
    }
}

/// One `on_tick` call's worth of receive-side results.
#[derive(Debug, Default)]
pub struct ReceiveTick {
    /// In-order payloads now ready for the application, oldest first.
    pub delivered: Vec<(u32, Vec<u8>)>,
    /// Sequence numbers given up on — missing for at least `latency_ms`
    /// with no arrival — and skipped past so delivery can continue.
    pub dropped: Vec<u32>,
    /// Sequence numbers still outstanding, to (re-)NAK. IMPLEMENTATION-
    /// DEFINED policy: every still-missing sequence number is re-announced
    /// on every `on_tick` call, since no backoff schedule is specified
    /// (see module docs) — cheap and correct for this crate's own
    /// self-consistency tests; a real deployment would want a coarser
    /// repeat interval to bound NAK traffic, which is exactly the kind of
    /// tuning this module declines to invent.
    pub renak: Vec<u32>,
}

/// Counters [`ReceiveWindow`] reports, for the statistics surface (#557).
/// Every field here is this crate's own bookkeeping, not a value read from
/// or compared against a real peer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReceiveStats {
    pub packets_delivered: u64,
    pub packets_dropped: u64,
    pub bytes_delivered: u64,
}

/// The receiver's loss-detector, reorder buffer, and TSBPD-ish delivery
/// gate.
#[derive(Debug)]
pub struct ReceiveWindow {
    config: ReceiveConfig,
    next_expected: u32,
    highest_seen: Option<u32>,
    /// Missing sequence number -> when it was first noticed missing.
    loss_list: BTreeMap<u32, u64>,
    buffered: BTreeMap<u32, Vec<u8>>,
    stats: ReceiveStats,
}

impl ReceiveWindow {
    #[must_use]
    pub fn new(config: ReceiveConfig, first_expected_seq_no: u32) -> Self {
        Self {
            config,
            next_expected: first_expected_seq_no,
            highest_seen: None,
            loss_list: BTreeMap::new(),
            buffered: BTreeMap::new(),
            stats: ReceiveStats::default(),
        }
    }

    #[must_use]
    pub const fn stats(&self) -> ReceiveStats {
        self.stats
    }

    /// Feed one arrived data packet. Returns newly-detected losses (a gap
    /// between the previous highest sequence number seen and this one) —
    /// the caller sends an immediate NAK for these, rather than waiting
    /// for the next `on_tick`, matching real ARQ's usual "announce loss as
    /// soon as it is noticed" behaviour (a reasonable inference, not a
    /// number the draft states, since it needs no timer at all — a gap
    /// is either present or not the moment a packet arrives).
    pub fn on_data(&mut self, seq_no: u32, payload: Vec<u8>, now_ms: u64) -> Vec<u32> {
        if seq_no < self.next_expected {
            return Vec::new(); // stale duplicate/retransmit of an already-delivered packet
        }
        self.buffered.insert(seq_no, payload);
        self.loss_list.remove(&seq_no);

        let mut new_losses = Vec::new();
        let previous_highest = self.highest_seen;
        if let Some(highest) = previous_highest
            && seq_no > highest + 1
        {
            for missing in (highest + 1)..seq_no {
                if !self.buffered.contains_key(&missing) && !self.loss_list.contains_key(&missing) {
                    self.loss_list.insert(missing, now_ms);
                    new_losses.push(missing);
                }
            }
        }
        self.highest_seen = Some(previous_highest.map_or(seq_no, |h| h.max(seq_no)));
        new_losses
    }

    /// Deliver everything ready, drop everything too late, and report
    /// what still needs a NAK.
    pub fn on_tick(&mut self, now_ms: u64) -> ReceiveTick {
        let mut tick = ReceiveTick::default();
        loop {
            if let Some(payload) = self.buffered.remove(&self.next_expected) {
                self.stats.packets_delivered += 1;
                self.stats.bytes_delivered += payload.len() as u64;
                tick.delivered.push((self.next_expected, payload));
                self.loss_list.remove(&self.next_expected);
                self.next_expected = self.next_expected.saturating_add(1);
            } else if let Some(&detected_at) = self.loss_list.get(&self.next_expected) {
                if now_ms.saturating_sub(detected_at) >= self.config.latency_ms {
                    self.loss_list.remove(&self.next_expected);
                    self.stats.packets_dropped += 1;
                    tick.dropped.push(self.next_expected);
                    self.next_expected = self.next_expected.saturating_add(1);
                } else {
                    break; // still within the latency window; keep waiting
                }
            } else {
                break; // not yet arrived, and not (yet) known lost
            }
        }
        tick.renak = self.loss_list.keys().copied().collect();
        tick
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn send_window_resends_on_nak_and_forgets_on_ack() {
        let mut w = SendWindow::new(SendConfig::default());
        w.on_send(1, 0, vec![1], 0);
        w.on_send(2, 0, vec![2], 0);
        w.on_send(3, 0, vec![3], 0);
        assert_eq!(w.in_flight_count(), 3);

        let resent = w.on_nak(&[2], 10);
        assert_eq!(resent, vec![(2, 0, vec![2])]);

        w.on_ack(2); // everything before 2 is acknowledged
        assert_eq!(w.in_flight_count(), 2); // 2 and 3 remain
    }

    #[test]
    fn send_window_retransmits_on_rto_with_no_nak() {
        let mut w = SendWindow::new(SendConfig { rto_ms: 50 });
        w.on_send(1, 0, vec![9], 0);
        assert!(w.on_tick(10).is_empty(), "too soon for RTO");
        let resent = w.on_tick(50);
        assert_eq!(resent, vec![(1, 0, vec![9])]);
    }

    #[test]
    fn receive_window_delivers_in_order_and_detects_gaps() {
        let mut r = ReceiveWindow::new(ReceiveConfig::default(), 1);
        assert!(r.on_data(1, vec![1], 0).is_empty());
        let losses = r.on_data(3, vec![3], 0);
        assert_eq!(losses, vec![2], "gap at 2 must be noticed immediately");

        let tick = r.on_tick(1);
        assert_eq!(tick.delivered, vec![(1, vec![1])]);
        assert_eq!(tick.renak, vec![2], "2 is still missing");

        r.on_data(2, vec![2], 1);
        let tick2 = r.on_tick(2);
        assert_eq!(tick2.delivered, vec![(2, vec![2]), (3, vec![3])]);
        assert!(tick2.renak.is_empty());
    }

    #[test]
    fn receive_window_drops_and_continues_past_a_packet_that_never_arrives() {
        let mut r = ReceiveWindow::new(ReceiveConfig { latency_ms: 100 }, 1);
        r.on_data(1, vec![1], 0);
        let losses = r.on_data(3, vec![3], 0); // 2 goes missing
        assert_eq!(losses, vec![2]);

        let tick_early = r.on_tick(50);
        assert_eq!(tick_early.delivered, vec![(1, vec![1])]);
        assert!(tick_early.dropped.is_empty(), "still within the latency window");

        let tick_late = r.on_tick(100);
        assert_eq!(tick_late.dropped, vec![2], "gave up waiting at the latency deadline");
        assert_eq!(tick_late.delivered, vec![(3, vec![3])], "delivery continues past the drop");
    }

    #[test]
    fn duplicate_and_stale_packets_are_ignored() {
        let mut r = ReceiveWindow::new(ReceiveConfig::default(), 1);
        r.on_data(1, vec![1], 0);
        let _ = r.on_tick(0);
        assert!(r.on_data(1, vec![1], 5).is_empty(), "already delivered, must not re-buffer");
    }
}
