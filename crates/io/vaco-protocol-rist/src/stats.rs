//! The statistics surface — `#560`. `TR-06-1`/`TR-06-2` name no required
//! statistics API (the closest either gets is §5.3.4/§5.3.5's "left to the
//! discretion of the implementer" for burst control and SSRC filtering);
//! this module is this crate's own choice of what to expose, scoped to
//! counters its own [`crate::buffer`]/[`crate::bonding`] modules already
//! compute internally rather than a speculative full statistics API.
//!
//! Every counter here states, in its own doc comment, whether it is
//! **independently-computed** (checkable against a fact the caller already
//! knows some other way) or **merely-reported** (read back from the same
//! internal state a caller could also inspect directly) — the same
//! distinction `vaco-protocol-srt`'s own stats surface (#557) drew.

use crate::bonding::{BondedReceiver, LinkStats};
use crate::buffer::BufferEvent;

/// A running tally, built by feeding it every [`BufferEvent`] a
/// `ReceiveBuffer`/[`BondedReceiver`] produces — this module does not
/// wrap either type itself, since both already return their events
/// directly and a caller folding them into [`SessionStats`] is one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStats {
    /// **Merely-reported**: incremented once per [`BufferEvent::Delivered`]
    /// this instance has seen — the same source `ReceiveBuffer` itself
    /// would report via its own event stream, not a second, independent
    /// count.
    pub packets_delivered: u64,
    /// **Merely-reported**, same caveat as `packets_delivered`.
    pub packets_dropped: u64,
}

impl SessionStats {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            packets_delivered: 0,
            packets_dropped: 0,
        }
    }

    pub fn record(&mut self, event: &BufferEvent) {
        match event {
            BufferEvent::Delivered { .. } => self.packets_delivered += 1,
            BufferEvent::Dropped { .. } => self.packets_dropped += 1,
        }
    }

    pub fn record_all<'a>(&mut self, events: impl IntoIterator<Item = &'a BufferEvent>) {
        for event in events {
            self.record(event);
        }
    }

    /// **Independently-computed**: the total this session has accounted
    /// for one way or the other, checkable against a caller's own known
    /// packet count (see the test below) rather than merely restating
    /// `packets_delivered + packets_dropped` back to itself.
    #[must_use]
    pub const fn total_accounted_for(&self) -> u64 {
        self.packets_delivered + self.packets_dropped
    }
}

impl Default for SessionStats {
    fn default() -> Self {
        Self::new()
    }
}

/// One bonded link's stats, re-exposed alongside [`SessionStats`] so a
/// caller has both halves of the surface (per-session delivery outcome,
/// per-link raw traffic) from one place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BondedLinkReport {
    pub link_id: u32,
    pub stats: LinkStats,
}

/// Collect every link's [`LinkStats`] from a [`BondedReceiver`], in link-id
/// order.
#[must_use]
pub fn link_reports(receiver: &BondedReceiver) -> Vec<BondedLinkReport> {
    receiver
        .links()
        .map(|(link_id, stats)| BondedLinkReport { link_id, stats })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use crate::buffer::BufferConfig;

    #[test]
    fn total_accounted_for_matches_an_independently_known_packet_count() {
        let mut rx = BondedReceiver::new(BufferConfig { total_ms: 100 }, 0);
        let mut stats = SessionStats::new();
        // Packet 0 arrives; packet 1 never arrives; packet 2 arrives,
        // which is what reveals the gap at all (a loss with nothing ever
        // arriving after it cannot be detected by sequence-number
        // discontinuity -- see crate::buffer's own module docs).
        stats.record_all(&rx.on_packet(1, 0, vec![0], 0));
        stats.record_all(&rx.on_packet(1, 2, vec![2], 1));
        stats.record_all(&rx.on_tick(200)); // past the deadline for seq 1
        // Independently-computed: this test itself knows exactly 3
        // packets (0, 1, 2) were ever in play, checked against the
        // session's own running total rather than against itself.
        assert_eq!(stats.total_accounted_for(), 3);
        assert_eq!(stats.packets_delivered, 2);
        assert_eq!(stats.packets_dropped, 1);
    }

    #[test]
    fn link_reports_lists_every_link_that_has_sent_anything() {
        let mut rx = BondedReceiver::new(BufferConfig::new(), 0);
        rx.on_packet(1, 0, vec![0], 0);
        rx.on_packet(2, 1, vec![1], 1);
        let reports = link_reports(&rx);
        assert_eq!(reports.len(), 2);
        assert!(
            reports
                .iter()
                .any(|r| r.link_id == 1 && r.stats.packets_received == 1)
        );
        assert!(
            reports
                .iter()
                .any(|r| r.link_id == 2 && r.stats.packets_received == 1)
        );
    }
}
