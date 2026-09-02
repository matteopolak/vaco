//! §5.3.1's receiver buffer — `VSF TR-06-1:2020`, Figure 1: a Reorder
//! Section feeding a Retransmission Reassembly Section, packet loss
//! detected at the boundary between them (a discontinuity in the RTP
//! sequence number), no recovery possible past the far end.
//!
//! Sans-io, the same shape as `vaco_protocol_srt::arq`: nothing here owns
//! a socket or a clock. [`ReceiveBuffer::on_packet`] and
//! [`ReceiveBuffer::on_tick`] both take an explicit `now_ms`, and return
//! the events (delivered payloads, newly-missing sequence numbers, given-
//! up-on drops) a caller uses to feed a jitter-buffer output and a NACK
//! generator (`crate::rtcp::GenericNack`/`RangeNack`).
//!
//! # Sequence numbers are pre-unwrapped
//!
//! RTP's own sequence number is 16 bits and wraps every 65536 packets.
//! This module takes `u32` sequence numbers throughout and does not
//! unwrap RTP's 16-bit field itself — extending a wrapping 16-bit counter
//! into a monotonic `u32` (tracking rollovers) is a separate, well-known
//! concern the caller handles before calling in, the same separation
//! RFC 3550's own `extended highest sequence number received` RTCP field
//! assumes on the wire.
//!
//! # `IMPLEMENTATION-DEFINED`: the default buffer sizes
//!
//! §5.3.1 states plainly: "For the Simple Profile, the buffer size is
//! manually configured at both sending and receiving ends" — there is no
//! normative default. Appendix B ("Suggested Default Values") is
//! explicitly Informative and suggests 1000 ms total / 70 ms reorder
//! section; [`DEFAULT_TOTAL_MS`]/[`DEFAULT_REORDER_MS`] carry those
//! numbers forward as this crate's own defaults, not as a spec
//! requirement — a caller with better information should override them.

use std::collections::BTreeMap;

/// Appendix B (Informative): suggested receiver buffer size.
pub const DEFAULT_TOTAL_MS: u64 = 1000;
/// Appendix B (Informative): suggested reorder-section size. Named here
/// for documentation; this module does not currently give the reorder
/// section separate behaviour from the rest of the buffer (see
/// [`BufferConfig`]'s docs) — a real reorder/reassembly split by *time
/// within* the buffer, rather than by outcome, is future work once a
/// concrete deployment needs it.
pub const DEFAULT_REORDER_MS: u64 = 70;

/// How long this receiver waits, from when a gap is first observed, before
/// giving up on the packet that opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferConfig {
    pub total_ms: u64,
}

impl BufferConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            total_ms: DEFAULT_TOTAL_MS,
        }
    }
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// One outcome of feeding a packet or a tick into [`ReceiveBuffer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferEvent {
    /// `seq` is now the oldest not-yet-delivered packet's turn, and its
    /// payload was available (either it just arrived, or a retransmission
    /// filled it in earlier).
    Delivered { seq: u32, payload: Vec<u8> },
    /// `seq`'s deadline (`now_ms - gap_opened_at >= total_ms`) passed with
    /// no payload ever arriving — "no recovery after this point" (Figure
    /// 1). The buffer moves on without it.
    Dropped { seq: u32 },
}

/// The reorder/retransmission-reassembly buffer for one RIST flow.
#[derive(Debug)]
pub struct ReceiveBuffer {
    config: BufferConfig,
    /// The oldest sequence number not yet delivered or given up on.
    next_deliver: u32,
    /// Packets received ahead of `next_deliver`, waiting either for the
    /// gap to fill in or for their own turn once it does.
    pending: BTreeMap<u32, Vec<u8>>,
    /// When the current gap (if any) was first observed — `None` means
    /// `next_deliver` itself has not yet been seen missing, i.e. nothing
    /// has arrived ahead of it since the last delivery.
    gap_opened_at_ms: Option<u64>,
}

impl ReceiveBuffer {
    #[must_use]
    pub fn new(config: BufferConfig, start_seq: u32) -> Self {
        Self {
            config,
            next_deliver: start_seq,
            pending: BTreeMap::new(),
            gap_opened_at_ms: None,
        }
    }

    #[must_use]
    pub const fn next_deliver(&self) -> u32 {
        self.next_deliver
    }

    /// Sequence numbers strictly between `next_deliver` and the highest
    /// sequence number seen so far that have not yet arrived — the
    /// candidates a caller feeds into a NACK.
    #[must_use]
    pub fn missing(&self) -> Vec<u32> {
        let Some(&highest) = self.pending.keys().next_back() else {
            return Vec::new();
        };
        (self.next_deliver..highest)
            .filter(|seq| !self.pending.contains_key(seq))
            .collect()
    }

    /// Feed one arriving packet. Returns every packet this unblocks, in
    /// delivery order (zero or more — a single arrival can drain a whole
    /// contiguous run that was waiting behind it).
    pub fn on_packet(&mut self, seq: u32, payload: Vec<u8>, now_ms: u64) -> Vec<BufferEvent> {
        if seq < self.next_deliver {
            // Late arrival of something already delivered or given up on.
            return Vec::new();
        }
        self.pending.insert(seq, payload);
        if seq > self.next_deliver && self.gap_opened_at_ms.is_none() {
            self.gap_opened_at_ms = Some(now_ms);
        }
        self.drain_ready()
    }

    /// Advance time. Returns every packet this delivers or drops.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<BufferEvent> {
        let mut events = Vec::new();
        while let Some(opened_at) = self.gap_opened_at_ms {
            if now_ms.saturating_sub(opened_at) < self.config.total_ms {
                break;
            }
            // The oldest slot's deadline passed. Give up on it and see if
            // that unblocks anything already sitting in `pending`.
            events.push(BufferEvent::Dropped {
                seq: self.next_deliver,
            });
            self.next_deliver += 1;
            self.gap_opened_at_ms = if self.pending.contains_key(&self.next_deliver) {
                None
            } else if self
                .pending
                .keys()
                .next_back()
                .is_some_and(|&h| h > self.next_deliver)
            {
                Some(opened_at) // gap persists, same clock start
            } else {
                None
            };
            events.extend(self.drain_ready());
        }
        events
    }

    fn drain_ready(&mut self) -> Vec<BufferEvent> {
        let mut events = Vec::new();
        while let Some(payload) = self.pending.remove(&self.next_deliver) {
            events.push(BufferEvent::Delivered {
                seq: self.next_deliver,
                payload,
            });
            self.next_deliver += 1;
        }
        if self.pending.is_empty() {
            self.gap_opened_at_ms = None;
        }
        events
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    // self-consistency: this module's own two entry points (on_packet,
    // on_tick) agreeing with each other and with a simple hand-traced
    // scenario -- not checked against any spec-given number, since §5.3.1
    // gives a mechanism (Figure 1) but explicitly leaves sizing to
    // configuration.

    #[test]
    fn in_order_arrival_delivers_immediately() {
        let mut buf = ReceiveBuffer::new(BufferConfig::new(), 0);
        let events = buf.on_packet(0, vec![0], 0);
        assert_eq!(
            events,
            vec![BufferEvent::Delivered {
                seq: 0,
                payload: vec![0]
            }]
        );
        assert_eq!(buf.next_deliver(), 1);
    }

    #[test]
    fn out_of_order_arrival_fills_gap_and_drains_contiguous_run() {
        let mut buf = ReceiveBuffer::new(BufferConfig::new(), 0);
        assert!(buf.on_packet(1, vec![1], 0).is_empty()); // ahead of next_deliver=0
        assert!(buf.on_packet(2, vec![2], 5).is_empty());
        assert_eq!(buf.missing(), vec![0]);
        let events = buf.on_packet(0, vec![0], 10); // fills the gap
        assert_eq!(
            events,
            vec![
                BufferEvent::Delivered {
                    seq: 0,
                    payload: vec![0]
                },
                BufferEvent::Delivered {
                    seq: 1,
                    payload: vec![1]
                },
                BufferEvent::Delivered {
                    seq: 2,
                    payload: vec![2]
                },
            ]
        );
        assert!(buf.missing().is_empty());
    }

    #[test]
    fn a_packet_never_recovered_is_dropped_after_total_ms() {
        let config = BufferConfig { total_ms: 100 };
        let mut buf = ReceiveBuffer::new(config, 0);
        assert!(buf.on_packet(1, vec![1], 0).is_empty()); // opens the gap at t=0
        assert!(buf.on_tick(50).is_empty()); // deadline not reached yet
        let events = buf.on_tick(100);
        assert_eq!(
            events,
            vec![
                BufferEvent::Dropped { seq: 0 },
                BufferEvent::Delivered {
                    seq: 1,
                    payload: vec![1]
                },
            ]
        );
        assert_eq!(buf.next_deliver(), 2);
    }

    #[test]
    fn a_late_retransmission_after_the_deadline_is_ignored() {
        let config = BufferConfig { total_ms: 100 };
        let mut buf = ReceiveBuffer::new(config, 0);
        buf.on_packet(1, vec![1], 0);
        buf.on_tick(100); // gives up on 0, delivers 1, next_deliver=2
        assert_eq!(buf.next_deliver(), 2);
        let events = buf.on_packet(0, vec![0], 500); // arrives far too late
        assert!(events.is_empty());
        assert_eq!(buf.next_deliver(), 2);
    }

    #[test]
    fn a_second_gap_after_the_first_closes_gets_its_own_deadline() {
        let config = BufferConfig { total_ms: 100 };
        let mut buf = ReceiveBuffer::new(config, 0);
        buf.on_packet(0, vec![0], 0); // no gap
        let events = buf.on_packet(2, vec![2], 10); // opens a new gap at t=10
        assert!(events.is_empty());
        assert!(buf.on_tick(100).is_empty()); // 100 - 10 = 90 < 100, not yet
        let events = buf.on_tick(111); // 111 - 10 = 101 >= 100
        assert_eq!(
            events,
            vec![
                BufferEvent::Dropped { seq: 1 },
                BufferEvent::Delivered {
                    seq: 2,
                    payload: vec![2]
                },
            ]
        );
    }

    #[test]
    fn duplicate_arrival_is_ignored() {
        let mut buf = ReceiveBuffer::new(BufferConfig::new(), 0);
        buf.on_packet(0, vec![0], 0);
        let events = buf.on_packet(0, vec![0], 5); // already delivered
        assert!(events.is_empty());
    }
}
