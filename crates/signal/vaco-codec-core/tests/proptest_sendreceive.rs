//! Property tests over send/receive orderings.
//!
//! The invariant the whole framework rests on: **any legal sequence of calls
//! must never panic, and must never lose or duplicate an output.** A caller is
//! free to receive eagerly, receive lazily, retry on backpressure, flush at any
//! point and drain at the end, and the set of outputs it observes is determined
//! entirely by what it fed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use proptest::prelude::*;
use vaco_codec_core::mock::{MockCodec, MockPacket, MockProgram, MockUnit, Step};
use vaco_codec_core::{SendReceive, Stage, Validated};
use vaco_core::Error;

/// What the caller does at each point in the sequence.
#[derive(Debug, Clone, Copy)]
enum Action {
    Send(u64),
    Receive,
    Flush,
    Eof,
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (1u32..4).prop_map(Step::Emit),
        Just(Step::Reorder),
        Just(Step::Skip),
        Just(Step::Corrupt),
    ]
}

fn program_strategy() -> impl Strategy<Value = MockProgram> {
    (
        proptest::collection::vec(step_strategy(), 1..6),
        0usize..4,
        1usize..6,
    )
        .prop_map(|(steps, delay, _cap)| MockProgram::new(steps).with_reorder_delay(delay))
}

fn action_strategy() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => (0u64..64).prop_map(Action::Send),
        4 => Just(Action::Receive),
        1 => Just(Action::Flush),
        1 => Just(Action::Eof),
    ]
}

/// Drive `codec` through `actions`, tracking what a correct implementation
/// would have produced. Returns nothing; it asserts as it goes.
fn drive(program: &MockProgram, capacity: usize, actions: &[Action]) {
    let mut codec = Validated::new(MockCodec::with_capacity(program.clone(), capacity));
    // Ids fed since the last flush, and outputs observed since the last flush.
    let mut fed_ids: Vec<u64> = Vec::new();
    let mut seen: Vec<MockUnit> = Vec::new();
    let mut eof = false;

    for &action in actions {
        match action {
            Action::Send(id) => {
                if eof {
                    assert!(matches!(
                        codec.send(Some(&MockPacket::new(id))),
                        Err(Error::Eof)
                    ));
                    continue;
                }
                match codec.send(Some(&MockPacket::new(id))) {
                    // A corrupt input is consumed and recoverable, so both of
                    // these count as fed.
                    Ok(()) | Err(Error::InvalidData(_)) => fed_ids.push(id),
                    // Backpressure: the input is still ours, nothing was
                    // consumed, and something must be available to take.
                    Err(Error::OutputPending) => {
                        assert_eq!(codec.stage(), Stage::Feeding);
                        match codec.receive() {
                            Ok(u) => seen.push(u),
                            Err(e) => panic!("backpressure with nothing pending: {e}"),
                        }
                    }
                    Err(e) => panic!("illegal send error: {e}"),
                }
            }
            Action::Receive => match codec.receive() {
                Ok(u) => seen.push(u),
                Err(Error::NeedMoreInput) => assert!(!eof, "NeedMoreInput while draining"),
                Err(Error::Eof) => assert!(eof, "Eof before draining began"),
                Err(e) => panic!("illegal receive error: {e}"),
            },
            Action::Flush => {
                codec.flush();
                // A flush discards everything: the accounting restarts.
                fed_ids.clear();
                seen.clear();
                eof = false;
            }
            Action::Eof => {
                if eof {
                    assert!(matches!(codec.send(None), Err(Error::Eof)));
                } else {
                    codec.send(None).unwrap();
                    eof = true;
                }
            }
        }
        // Invariant, checked after every single call: what we have observed is
        // always a prefix of what the reference model says we should observe.
        let (want_fed, want_drained) = program.expected(&fed_ids);
        let expected: Vec<MockUnit> = want_fed
            .iter()
            .chain(want_drained.iter())
            .copied()
            .collect();
        assert!(
            seen.len() <= expected.len() && seen[..] == expected[..seen.len()],
            "output diverged from the model:\n  seen: {seen:?}\n  want: {expected:?}"
        );
    }

    // Finish the stream properly and check nothing was lost.
    if !eof {
        codec.send(None).unwrap();
    }
    loop {
        match codec.receive() {
            Ok(u) => seen.push(u),
            Err(Error::Eof) => break,
            Err(e) => panic!("illegal drain error: {e}"),
        }
    }
    let (want_fed, want_drained) = program.expected(&fed_ids);
    let expected: Vec<MockUnit> = want_fed.into_iter().chain(want_drained).collect();
    assert_eq!(seen, expected, "an output was lost or duplicated");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Any legal call sequence: no panic, no lost output, no duplicate.
    #[test]
    fn arbitrary_orderings_never_lose_or_duplicate(
        program in program_strategy(),
        capacity in 1usize..8,
        actions in proptest::collection::vec(action_strategy(), 0..64),
    ) {
        drive(&program, capacity, &actions);
    }

    /// The queue depth is a performance knob, never a correctness one.
    #[test]
    fn capacity_does_not_change_the_output(
        program in program_strategy(),
        ids in proptest::collection::vec(0u64..32, 0..24),
    ) {
        let mut outs = Vec::new();
        for capacity in [1usize, 2, 5, 16] {
            let mut codec = Validated::new(MockCodec::with_capacity(program.clone(), capacity));
            let mut seen = Vec::new();
            for &id in &ids {
                let pkt = MockPacket::new(id);
                loop {
                    match codec.send(Some(&pkt)) {
                        Ok(()) | Err(Error::InvalidData(_)) => break,
                        Err(Error::OutputPending) => seen.push(codec.receive().unwrap()),
                        Err(e) => panic!("illegal: {e}"),
                    }
                }
            }
            codec.send(None).unwrap();
            while let Ok(u) = codec.receive() {
                seen.push(u);
            }
            outs.push(seen);
        }
        for w in outs.windows(2) {
            prop_assert_eq!(&w[0], &w[1]);
        }
    }

    /// Flushing at any point leaves a codec indistinguishable from a fresh one.
    #[test]
    fn flush_is_equivalent_to_a_fresh_codec(
        program in program_strategy(),
        before in proptest::collection::vec(0u64..32, 0..12),
        after in proptest::collection::vec(0u64..32, 0..12),
    ) {
        let mut flushed = Validated::new(MockCodec::new(program.clone()));
        for &id in &before {
            let _ = flushed.send(Some(&MockPacket::new(id)));
            while flushed.receive().is_ok() {}
        }
        flushed.flush();

        let mut fresh = Validated::new(MockCodec::new(program.clone()));

        let mut a = Vec::new();
        let mut b = Vec::new();
        for &id in &after {
            let pkt = MockPacket::new(id);
            let _ = flushed.send(Some(&pkt));
            let _ = fresh.send(Some(&pkt));
            while let Ok(u) = flushed.receive() { a.push(u); }
            while let Ok(u) = fresh.receive() { b.push(u); }
        }
        flushed.send(None).unwrap();
        fresh.send(None).unwrap();
        while let Ok(u) = flushed.receive() { a.push(u); }
        while let Ok(u) = fresh.receive() { b.push(u); }
        prop_assert_eq!(a, b);
    }
}
