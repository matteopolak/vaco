//! Self-consistency: a full sender/receiver simulation over a deterministic
//! lossy link, closing the loop `crate::buffer` (loss detection, delivery,
//! give-up) actually opens for — this crate's own two sides (a fake
//! sender that resends on request, a real `ReceiveBuffer` that requests)
//! agreeing that a lossy link recovers to the intended delivered set.
//!
//! This is **not** checked against a reference RIST peer (none is
//! reachable on this machine — see the crate's own module docs) and is
//! not evidence of spec conformance beyond the field-layout tests in
//! `src/rtcp.rs`; it is evidence that this crate's own buffer, NACK
//! encoding, and retransmission tagging compose into a working recovery
//! loop, which is the substitute this issue's replacement bar names for
//! "a simple-profile session against a reference peer recovers a lossy
//! link to the same delivered set".

#![allow(clippy::unwrap_used, reason = "test code")]

use vaco_protocol_rist::buffer::{BufferConfig, BufferEvent, ReceiveBuffer};
use vaco_protocol_rist::retransmit::Origin;
use vaco_protocol_rist::rtcp::{GenericNack, NackEntry};

/// A tiny, deterministic PRNG — same shape as `vaco-protocol-srt`'s own
/// `lossy_link.rs`, so a fixed seed reproduces the exact same run.
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

    /// Roughly `percent`% chance of `true`.
    fn chance(&mut self, percent: u32) -> bool {
        self.next_u32() % 100 < percent
    }
}

/// One packet as it exists on the wire: sequence number, origin, payload.
/// `origin` documents that a retransmission is a real §5.3.3 SSRC-LSB-
/// tagged packet in this scenario, not read by `ReceiveBuffer` itself
/// (which keys purely on sequence number, matching the spec: the tag is
/// for a receiver's own bookkeeping/debugging, not for reorder logic).
#[derive(Debug, Clone)]
struct WirePacket {
    seq: u32,
    #[allow(dead_code, reason = "documents provenance; ReceiveBuffer does not branch on it")]
    origin: Origin,
    payload: Vec<u8>,
}

/// Runs `packet_count` packets over a link that drops `loss_percent`% of
/// both original and retransmitted packets, retrying lost packets via
/// NACK up to `max_rounds` times. Returns every payload the receiver
/// delivered, in delivery order, and every sequence number it gave up on.
#[allow(clippy::too_many_lines, reason = "one self-contained simulation")]
fn simulate(
    packet_count: u32,
    loss_percent: u32,
    max_rounds: u32,
    seed: u32,
    always_lose: &[u32],
) -> (Vec<(u32, Vec<u8>)>, Vec<u32>) {
    const ROUND_SPACING_MS: u64 = 50;

    let mut rng = Xorshift32(seed | 1); // never zero, or xorshift sticks at 0
    let ssrc_media_source = 0xAABB_CC00u32;

    // What the sender would transmit, if nothing were ever lost.
    let originals: Vec<WirePacket> = (0..packet_count)
        .map(|seq| WirePacket {
            seq,
            origin: Origin::Original,
            payload: seq.to_be_bytes().to_vec(),
        })
        .collect();

    // in_flight[round]: what actually reaches the receiver on that round,
    // after the link drops some fraction.
    let mut receiver = ReceiveBuffer::new(BufferConfig { total_ms: 10_000 }, 0);
    let mut delivered = Vec::new();
    let mut dropped = Vec::new();
    let mut now_ms = 0u64;
    let mut to_send = originals;
    for _round in 0..max_rounds {
        if to_send.is_empty() {
            break;
        }
        let mut still_missing = Vec::new();
        for pkt in &to_send {
            now_ms += 1;
            if always_lose.contains(&pkt.seq) || rng.chance(loss_percent) {
                still_missing.push(pkt.seq);
                continue; // lost on the wire, never reaches on_packet
            }
            for event in receiver.on_packet(pkt.seq, pkt.payload.clone(), now_ms) {
                record(event, &mut delivered, &mut dropped);
            }
        }
        now_ms += ROUND_SPACING_MS;
        for event in receiver.on_tick(now_ms) {
            record(event, &mut delivered, &mut dropped);
        }

        // Build the NACK a real receiver would send, then act as the
        // sender receiving it: resend exactly what it asks for, tagged as
        // retransmissions (§5.3.3), same seq/payload as the original.
        let missing = receiver.missing();
        assert_eq!(
            missing.iter().copied().collect::<std::collections::BTreeSet<_>>(),
            still_missing.iter().copied().collect::<std::collections::BTreeSet<_>>(),
            "the buffer's own missing() must agree with what the link actually dropped this round"
        );
        if missing.is_empty() {
            break;
        }
        let nack = GenericNack {
            ssrc_packet_sender: 0,
            ssrc_media_source,
            entries: bitmask_entries(&missing),
        };
        // Round-trip the NACK through its own wire encoding, proving the
        // simulation is driven by the real encode/decode path rather than
        // the in-memory `missing` list directly.
        let (count_or_fmt, data) = nack.serialize();
        let nack = GenericNack::parse(count_or_fmt, &data).unwrap();
        assert_eq!(nack.ssrc_media_source, ssrc_media_source);

        to_send = requested_seqs(&nack)
            .into_iter()
            .map(|seq| WirePacket {
                seq,
                origin: Origin::Retransmission,
                payload: seq.to_be_bytes().to_vec(),
            })
            .collect();
    }

    // Whatever is still outstanding after `max_rounds` genuinely could not
    // be recovered within this simulation's own retry budget -- run the
    // buffer's deadline out so it gives up cleanly rather than hanging
    // forever on packets this test intentionally stopped retrying.
    for event in receiver.on_tick(now_ms + 20_000) {
        record(event, &mut delivered, &mut dropped);
    }

    (delivered, dropped)
}

fn record(event: BufferEvent, delivered: &mut Vec<(u32, Vec<u8>)>, dropped: &mut Vec<u32>) {
    match event {
        BufferEvent::Delivered { seq, payload } => delivered.push((seq, payload)),
        BufferEvent::Dropped { seq } => dropped.push(seq),
    }
}

/// Group consecutive-ish missing sequence numbers into Generic NACK
/// entries (PID + up to 16 following bits), the same encoding
/// `rtcp::tests::generic_nack_matches_appendix_a_scenario_rule` checks
/// against the spec's own worked example.
fn bitmask_entries(missing: &[u32]) -> Vec<NackEntry> {
    let missing: std::collections::BTreeSet<u32> = missing.iter().copied().collect();
    let mut entries = Vec::new();
    let mut iter = missing.iter().copied().peekable();
    while let Some(pid) = iter.next() {
        let mut blp = 0u16;
        for i in 1..=16u32 {
            if missing.contains(&(pid + i)) {
                blp |= 1 << (i - 1);
                // Consume it so the outer loop does not also start a new
                // entry at this already-covered sequence number.
                if iter.peek() == Some(&(pid + i)) {
                    iter.next();
                }
            }
        }
        entries.push(NackEntry {
            pid: u16::try_from(pid).unwrap_or(u16::MAX),
            blp,
        });
    }
    entries
}

fn requested_seqs(nack: &GenericNack) -> Vec<u32> {
    let mut out = Vec::new();
    for entry in &nack.entries {
        out.push(u32::from(entry.pid));
        for i in 1..=16u32 {
            if entry.blp & (1 << (i - 1)) != 0 {
                out.push(u32::from(entry.pid) + i);
            }
        }
    }
    out
}

fn assert_sound_delivery(packet_count: u32, delivered: &[(u32, Vec<u8>)], dropped: &[u32]) {
    // Independently-computed: every sequence number is accounted for
    // exactly once, either delivered or dropped -- checked against the
    // test's own known packet_count, not against the delivered/dropped
    // lists' own lengths.
    let mut seen: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for &(seq, ref payload) in delivered {
        assert!(seen.insert(seq), "sequence {seq} delivered more than once");
        assert_eq!(*payload, seq.to_be_bytes().to_vec(), "delivered payload for {seq} does not match what was sent");
    }
    for &seq in dropped {
        assert!(seen.insert(seq), "sequence {seq} both delivered and dropped, or dropped twice");
    }
    assert_eq!(
        seen,
        (0..packet_count).collect(),
        "every sequence number 0..packet_count must be either delivered or dropped, exactly once"
    );
}

#[test]
fn zero_loss_recovers_everything_on_the_first_round() {
    let (delivered, dropped) = simulate(200, 0, 5, 0xC0FF_EE01, &[]);
    assert!(dropped.is_empty());
    assert_eq!(delivered.len(), 200);
    assert_sound_delivery(200, &delivered, &dropped);
}

#[test]
fn five_percent_loss_recovers_the_whole_set_within_a_few_rounds() {
    let (delivered, dropped) = simulate(500, 5, 6, 0xC0FF_EE02, &[]);
    assert!(dropped.is_empty(), "5% loss with 6 retry rounds should recover everything");
    assert_eq!(delivered.len(), 500);
    assert_sound_delivery(500, &delivered, &dropped);
}

#[test]
fn twenty_percent_loss_recovers_the_large_majority() {
    let (delivered, dropped) = simulate(500, 20, 6, 0xC0FF_EE03, &[]);
    assert_sound_delivery(500, &delivered, &dropped);
    // 20% independent loss per round for 6 rounds leaves a vanishingly
    // small tail truly unrecovered; assert the large-majority bar rather
    // than a specific count, since the exact number is a function of this
    // PRNG's own sequence, not a spec-given target.
    assert!(
        delivered.len() >= 490,
        "expected the large majority of 500 packets delivered, got {}",
        delivered.len()
    );
}

#[test]
fn a_permanently_lost_packet_is_eventually_given_up_on_while_the_rest_recover() {
    // Sequence 0 never arrives, on the original send or on any retry --
    // but sequences 1..5 arrive normally, which is what reveals the gap
    // in the first place (`missing()` only reports a hole once something
    // *after* it has arrived -- §5.3.1's own "discontinuities in the RTP
    // sequence number" detection method: a loss with nothing ever
    // arriving after it is genuinely indistinguishable from "not sent
    // yet", the same limitation a real deployment has). Zero random loss
    // otherwise, so this is a clean, deterministic check of the give-up
    // path alone, not conflated with the random-loss recovery path the
    // other tests already cover.
    let (delivered, dropped) = simulate(5, 0, 3, 0xC0FF_EE04, &[0]);
    assert_eq!(dropped, vec![0]);
    assert_eq!(delivered.len(), 4);
    assert!(delivered.iter().all(|&(seq, _)| seq != 0));
    assert_sound_delivery(5, &delivered, &dropped);
}
