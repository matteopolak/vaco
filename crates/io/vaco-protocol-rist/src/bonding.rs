//! Bonding and multi-link support — `VSF TR-06-1:2020` §5.4 (RIST Simple
//! Profile: bonding across raw network connections) and `TR-06-2:2022`
//! §5.5 (Main Profile: tunnel-level multi-path over GRE paths). Both are
//! the same mechanism at this crate's level of abstraction: multiple
//! paths carry copies (replication, for redundancy) or a split (combining,
//! for capacity) of one sequenced stream, and a receiver reassembles by
//! sequence number regardless of which path a packet arrived on — exactly
//! what [`crate::buffer::ReceiveBuffer`] already does, since it only ever
//! keyed on sequence number, never on a link identity. One implementation
//! here, not two, and that generalization is stated rather than left
//! implicit.
//!
//! §5.4 (draft-derived): "If a RIST sender is replicating packets over
//! multiple network connections, all copies of a given packet shall have
//! the same RTP sequence number and timestamp" — which is exactly what
//! [`ReceiveBuffer`]'s own duplicate
//! handling (`seq < next_deliver` or an already-`pending` `seq` is a
//! no-op) already needs to be true for. No new dedup logic is needed;
//! [`BondedReceiver`] is a thin wrapper that adds only what
//! [`crate::buffer::ReceiveBuffer`] does not track on its own: which link
//! each arrival came in on.

use crate::buffer::{BufferConfig, BufferEvent, ReceiveBuffer};
use std::collections::BTreeMap;

/// One bonded link's own receive counters — the numerator half of the
/// statistics surface (§5.4/§5.5 name bonding's existence but leave
/// statistics reporting to implementer discretion, same as Simple
/// Profile's §5.3.4/§5.3.5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Every packet arrival on this link, duplicate or not — this is the
    /// raw count of what the link actually carried.
    pub packets_received: u64,
}

/// A receiver bonding two or more links into one [`ReceiveBuffer`].
/// Packets are fed in tagged with a caller-chosen `link_id` (`u32` — a
/// network-connection index for §5.4, a tunnel-path index for §5.5,
/// whichever this crate's caller is bonding); which link a packet arrives
/// on affects only [`LinkStats`], never delivery — that is the point of
/// bonding.
#[derive(Debug)]
pub struct BondedReceiver {
    buffer: ReceiveBuffer,
    link_stats: BTreeMap<u32, LinkStats>,
}

impl BondedReceiver {
    #[must_use]
    pub fn new(config: BufferConfig, start_seq: u32) -> Self {
        Self {
            buffer: ReceiveBuffer::new(config, start_seq),
            link_stats: BTreeMap::new(),
        }
    }

    /// Feed one packet that arrived on `link_id`. Delivery/loss behaviour
    /// is exactly [`ReceiveBuffer::on_packet`]'s own — bonding changes
    /// nothing about *when* a packet is delivered, only that it may have
    /// arrived by more than one path.
    pub fn on_packet(&mut self, link_id: u32, seq: u32, payload: Vec<u8>, now_ms: u64) -> Vec<BufferEvent> {
        self.link_stats.entry(link_id).or_default().packets_received += 1;
        self.buffer.on_packet(seq, payload, now_ms)
    }

    pub fn on_tick(&mut self, now_ms: u64) -> Vec<BufferEvent> {
        self.buffer.on_tick(now_ms)
    }

    #[must_use]
    pub fn link_stats(&self, link_id: u32) -> LinkStats {
        self.link_stats.get(&link_id).copied().unwrap_or_default()
    }

    pub fn links(&self) -> impl Iterator<Item = (u32, LinkStats)> + '_ {
        self.link_stats.iter().map(|(&id, &stats)| (id, stats))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    // self-consistency: §5.4/§5.5's own replication rule ("all copies...
    // same sequence number and timestamp") applied directly -- there is no
    // spec-given numeric example to check this against, only the
    // mechanism's stated property.

    #[test]
    fn a_packet_replicated_on_both_links_is_delivered_exactly_once() {
        let mut rx = BondedReceiver::new(BufferConfig::new(), 0);
        let events_a = rx.on_packet(1, 0, vec![0], 0);
        let events_b = rx.on_packet(2, 0, vec![0], 1); // same seq, same payload, other link
        assert_eq!(events_a, vec![BufferEvent::Delivered { seq: 0, payload: vec![0] }]);
        assert!(events_b.is_empty(), "the second copy of seq 0 must not be delivered again");
        assert_eq!(rx.link_stats(1).packets_received, 1);
        assert_eq!(rx.link_stats(2).packets_received, 1);
    }

    /// This is #560's own Acc, replayed directly: "a bonded two-link
    /// session survives the loss of either link with no delivered-packet
    /// loss." A sender replicates 100 packets across links 1 and 2; link 1
    /// goes completely silent from packet 50 onward; every packet must
    /// still arrive, via link 2 alone.
    #[test]
    fn losing_one_of_two_replicated_links_loses_no_packets() {
        let mut rx = BondedReceiver::new(BufferConfig { total_ms: 1000 }, 0);
        let mut delivered = Vec::new();
        for seq in 0u32..100 {
            let now = u64::from(seq) * 10;
            if seq < 50 {
                // Both links still up: replicate.
                for event in rx.on_packet(1, seq, seq.to_be_bytes().to_vec(), now) {
                    record(&event, &mut delivered);
                }
            }
            // Link 2 never goes down, for the whole run.
            for event in rx.on_packet(2, seq, seq.to_be_bytes().to_vec(), now) {
                record(&event, &mut delivered);
            }
        }
        assert_eq!(delivered.len(), 100, "every packet must still be delivered via the surviving link");
        assert_eq!(delivered, (0u32..100).collect::<Vec<_>>(), "delivery order must still be in-order");
        assert_eq!(rx.link_stats(1).packets_received, 50);
        assert_eq!(rx.link_stats(2).packets_received, 100);
    }

    #[test]
    fn losing_the_other_link_instead_also_loses_no_packets() {
        // The symmetric case -- bonding's guarantee should not depend on
        // which of the two links happens to be the one that survives.
        let mut rx = BondedReceiver::new(BufferConfig { total_ms: 1000 }, 0);
        let mut delivered = Vec::new();
        for seq in 0u32..100 {
            let now = u64::from(seq) * 10;
            for event in rx.on_packet(1, seq, seq.to_be_bytes().to_vec(), now) {
                record(&event, &mut delivered);
            }
            if seq < 50 {
                for event in rx.on_packet(2, seq, seq.to_be_bytes().to_vec(), now) {
                    record(&event, &mut delivered);
                }
            }
        }
        assert_eq!(delivered.len(), 100);
    }

    #[test]
    fn combining_mode_splits_traffic_across_links_without_loss() {
        // §5.4's other bonding scenario: capacity combining, not
        // redundancy -- each packet arrives on exactly one link (its
        // "assigned" path), never replicated. Delivery must still be
        // complete and in order, the same as a single-link stream.
        let mut rx = BondedReceiver::new(BufferConfig::new(), 0);
        let mut delivered = Vec::new();
        for seq in 0u32..20 {
            let link = if seq % 2 == 0 { 1 } else { 2 };
            for event in rx.on_packet(link, seq, seq.to_be_bytes().to_vec(), u64::from(seq)) {
                record(&event, &mut delivered);
            }
        }
        assert_eq!(delivered, (0u32..20).collect::<Vec<_>>());
        assert_eq!(rx.link_stats(1).packets_received, 10);
        assert_eq!(rx.link_stats(2).packets_received, 10);
    }

    fn record(event: &BufferEvent, delivered: &mut Vec<u32>) {
        if let BufferEvent::Delivered { seq, .. } = event {
            delivered.push(*seq);
        }
    }
}
