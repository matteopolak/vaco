//! A lossy-link simulation between this crate's own [`SendWindow`]/
//! [`ReceiveWindow`] pair — the replacement bar for issue #556's own Acc
//! ("recovers to the same delivered-packet set as the reference peer"),
//! substituted per the same #609/#610 pattern already used for #555: no
//! reference SRT peer is reachable here, so this tests the pair's own
//! functional correctness under loss, not conformance to a real
//! implementation's behaviour.
//!
//! **Self-consistency, with a bounded weakness, stated rather than left
//! implicit**: both sides of this simulation are this crate's own code, so
//! a shared misreading of the draft — if `arq.rs` misunderstood something
//! `draft-sharabayko-srt-01` states, both the sending and receiving logic
//! would misunderstand it identically and this test would not catch it.
//! That weakness is bounded here in a way it is not for the handshake
//! loopback test: *functional* delivery under loss (does everything
//! arrive, in order, exactly once) is a property of this pair's own
//! internal consistency almost by definition — an ARQ engine that
//! correctly retransmits what it itself declared lost and delivers what
//! it itself buffered in order is correct in the sense this test checks,
//! independent of whether the wire format or timing constants happen to
//! match a real peer's. What it does *not* prove: interop with a real SRT
//! sender/receiver, or that the IMPLEMENTATION-DEFINED constants
//! (`arq::DEFAULT_RTO_MS`, `arq::DEFAULT_LATENCY_MS`) are anywhere close
//! to what a real deployment tunes them to.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use vaco_protocol_srt::arq::{ReceiveConfig, ReceiveStats, ReceiveWindow, SendConfig, SendStats, SendWindow};

/// A tiny deterministic PRNG (xorshift32) — not `rand`, so this test adds
/// no new dependency (D10) for what is just "a reproducible sequence of
/// coin flips".
struct Xorshift32(u32);

impl Xorshift32 {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// `true` with probability `percent / 100`.
    fn hits(&mut self, percent: u32) -> bool {
        (self.next_u32() % 100) < percent
    }
}

/// Run `packet_count` packets from a sender to a receiver over a
/// simulated link that drops `loss_percent`% of every transmission
/// attempt (initial sends *and* retransmissions, independently each try —
/// the harsher, more realistic assumption). One simulated "round" is one
/// tick of `arq::ack::FULL_ACK_INTERVAL_MS`-scale time; the sender emits
/// one new packet per round for `packet_count` rounds, then both sides
/// keep ticking for `drain_rounds` more rounds to let retransmissions and
/// the latency-window drop policy resolve everything still outstanding.
///
/// Returns `(delivered_in_order, dropped)`.
fn simulate(packet_count: u32, loss_percent: u32, seed: u32) -> (Vec<(u32, Vec<u8>)>, Vec<u32>, SendStats, ReceiveStats) {
    const ROUND_MS: u64 = 10; // matches ack::FULL_ACK_INTERVAL_MS's own granularity
    const DRAIN_ROUNDS: u64 = 300; // 3s of extra time to resolve retries within the 1s latency window

    let mut rng = Xorshift32(seed | 1); // xorshift32 requires a nonzero seed
    let mut sender = SendWindow::new(SendConfig::default());
    let mut receiver = ReceiveWindow::new(ReceiveConfig::default(), 1);

    let mut delivered = Vec::new();
    let mut dropped = Vec::new();
    let mut now_ms: u64;

    let send_one = |seq_no: u32, sender: &mut SendWindow, receiver: &mut ReceiveWindow, now_ms: u64, rng: &mut Xorshift32| {
        let payload = seq_no.to_be_bytes().to_vec();
        sender.on_send(seq_no, 0, payload.clone(), now_ms);
        if !rng.hits(loss_percent) {
            let new_losses = receiver.on_data(seq_no, payload, now_ms);
            let _ = new_losses; // immediate-NAK path exercised via on_tick's renak below too
        }
    };

    for round in 0..u64::from(packet_count) {
        now_ms = round * ROUND_MS;
        let seq_no = u32::try_from(round).unwrap() + 1;
        send_one(seq_no, &mut sender, &mut receiver, now_ms, &mut rng);

        // Deliver/drop/NAK, then let the sender act on the NAK (also
        // subject to the same lossy link on the way back and on the
        // retransmission itself).
        let tick = receiver.on_tick(now_ms);
        delivered.extend(tick.delivered);
        dropped.extend(tick.dropped);

        if !tick.renak.is_empty() && !rng.hits(loss_percent) {
            // The NAK itself reached the sender.
            let resend = sender.on_nak(&tick.renak, now_ms);
            for (seq, _ts, payload) in resend {
                if !rng.hits(loss_percent) {
                    receiver.on_data(seq, payload, now_ms);
                }
            }
        }

        let rto_resends = sender.on_tick(now_ms);
        for (seq, _ts, payload) in rto_resends {
            if !rng.hits(loss_percent) {
                receiver.on_data(seq, payload, now_ms);
            }
        }
    }

    // Drain: keep ticking so RTO/NAK cycles and the latency-window drop
    // policy can resolve everything still outstanding.
    for extra in 1..=DRAIN_ROUNDS {
        now_ms = (u64::from(packet_count) + extra) * ROUND_MS;
        let tick = receiver.on_tick(now_ms);
        delivered.extend(tick.delivered);
        dropped.extend(tick.dropped);

        if !tick.renak.is_empty() {
            let resend = sender.on_nak(&tick.renak, now_ms);
            for (seq, _ts, payload) in resend {
                if !rng.hits(loss_percent) {
                    receiver.on_data(seq, payload, now_ms);
                }
            }
        }
        let rto_resends = sender.on_tick(now_ms);
        for (seq, _ts, payload) in rto_resends {
            if !rng.hits(loss_percent) {
                receiver.on_data(seq, payload, now_ms);
            }
        }
        if delivered.len() + dropped.len() == packet_count as usize {
            break;
        }
    }

    (delivered, dropped, sender.stats(), receiver.stats())
}

/// Checks the stats counters two different ways, labeled by which kind of
/// evidence each is (per the coordinator's own request): `packets_sent`
/// against `packet_count` and `delivered+dropped` against `packet_count`
/// are **independently-computed expectations** — `packet_count` comes from
/// the test's own loop bound, not from anything the counters or the
/// returned event vectors reported. `packets_delivered`/`packets_dropped`
/// against `delivered.len()`/`dropped.len()` are **merely reported**: both
/// numbers come from the same `ReceiveWindow`, just via two different
/// paths (a running counter vs. the events it returned), so agreement
/// there checks the counter's own internal consistency, not an outside
/// truth.
fn assert_stats_are_sound(
    packet_count: u32,
    delivered: &[(u32, Vec<u8>)],
    dropped: &[u32],
    send_stats: SendStats,
    receive_stats: ReceiveStats,
) {
    // Independently-computed: the test loop sent exactly `packet_count`
    // original packets (retransmissions do not call `on_send` again).
    assert_eq!(send_stats.packets_sent, u64::from(packet_count));
    // Independently-computed: a conservation invariant against the same
    // `packet_count` the test loop used to drive the simulation, not
    // against `delivered`/`dropped` themselves.
    assert_eq!(receive_stats.packets_delivered + receive_stats.packets_dropped, u64::from(packet_count));

    // Merely reported: the counter agrees with the events it itself
    // returned. A real bug in `ReceiveWindow` shared between the counter
    // and the event push would pass this check identically on both sides.
    assert_eq!(receive_stats.packets_delivered, delivered.len() as u64);
    assert_eq!(receive_stats.packets_dropped, dropped.len() as u64);
}

fn assert_sound_delivery(packet_count: u32, delivered: &[(u32, Vec<u8>)], dropped: &[u32]) {
    // Every packet is accounted for exactly once: delivered, or explicitly
    // dropped as too-late. Nothing simply vanishes.
    assert_eq!(
        delivered.len() + dropped.len(),
        packet_count as usize,
        "every packet must be either delivered or explicitly dropped, delivered={delivered:?} dropped={dropped:?}"
    );

    // Delivered packets arrive in strictly increasing sequence order.
    let mut last = 0u32;
    for (seq, payload) in delivered {
        assert!(*seq > last, "delivery must be strictly in order");
        assert_eq!(*payload, seq.to_be_bytes().to_vec(), "payload must be the one actually sent for this sequence number");
        last = *seq;
    }
}

#[test]
fn five_percent_loss_recovers_almost_everything() {
    const PACKETS: u32 = 500;
    let (delivered, dropped, send_stats, receive_stats) = simulate(PACKETS, 5, 0xC0FF_EE01);
    assert_sound_delivery(PACKETS, &delivered, &dropped);
    assert_stats_are_sound(PACKETS, &delivered, &dropped, send_stats, receive_stats);
    let delivered_ratio = delivered.len() as f64 / f64::from(PACKETS);
    assert!(
        delivered_ratio >= 0.99,
        "5% loss with a generous latency budget should recover almost everything via ARQ, got {delivered_ratio}"
    );
}

#[test]
fn twenty_percent_loss_still_recovers_the_large_majority() {
    const PACKETS: u32 = 500;
    let (delivered, dropped, send_stats, receive_stats) = simulate(PACKETS, 20, 0xC0FF_EE02);
    assert_sound_delivery(PACKETS, &delivered, &dropped);
    assert_stats_are_sound(PACKETS, &delivered, &dropped, send_stats, receive_stats);
    let delivered_ratio = delivered.len() as f64 / f64::from(PACKETS);
    assert!(
        delivered_ratio >= 0.95,
        "20% loss should still recover the large majority via retransmission, got {delivered_ratio}"
    );
}

#[test]
fn zero_loss_delivers_everything_with_nothing_dropped() {
    const PACKETS: u32 = 200;
    let (delivered, dropped, send_stats, receive_stats) = simulate(PACKETS, 0, 1);
    assert_sound_delivery(PACKETS, &delivered, &dropped);
    assert_stats_are_sound(PACKETS, &delivered, &dropped, send_stats, receive_stats);
    assert_eq!(delivered.len(), PACKETS as usize);
    assert!(dropped.is_empty());
}

/// A packet that is lost on every single attempt (initial send and every
/// retransmission) for the whole drain window must eventually be given up
/// on — exercising the too-late-drop path directly, not just as an
/// occasional side effect of randomness.
#[test]
fn a_packet_lost_on_every_attempt_is_eventually_dropped_not_stalled_on_forever() {
    let mut sender = SendWindow::new(SendConfig { rto_ms: 20 });
    let mut receiver = ReceiveWindow::new(ReceiveConfig { latency_ms: 200 }, 1);

    sender.on_send(1, 0, vec![1], 0);
    receiver.on_data(1, vec![1], 0); // packet 1 "arrives"
    // Packet 2 is never delivered to the receiver at all, on any attempt.
    sender.on_send(2, 0, vec![2], 0);
    receiver.on_data(3, vec![3], 0); // packet 3 arrives, revealing the gap at 2
    sender.on_send(3, 0, vec![3], 0);

    let mut delivered = Vec::new();
    let mut dropped = Vec::new();
    for ms in (0..=300u64).step_by(10) {
        let tick = receiver.on_tick(ms);
        delivered.extend(tick.delivered);
        dropped.extend(tick.dropped);
        let _ = sender.on_tick(ms); // RTO fires repeatedly; every resend is dropped by this test on purpose
    }

    assert_eq!(dropped, vec![2], "packet 2 must be given up on, not waited for forever");
    assert_eq!(delivered, vec![(1, vec![1]), (3, vec![3])]);
}
