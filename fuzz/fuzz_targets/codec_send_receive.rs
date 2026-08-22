//! The send/receive protocol under an arbitrary caller.
//!
//! Not a parser fuzzer: the untrusted input here is the *call sequence*. Any
//! legal sequence must never panic, never report an illegal error, never lose
//! an output and never duplicate one, for any codec behaviour and any queue
//! depth.
//! fuzz-crate: vaco-codec-core

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::mock::{MockCodec, MockPacket, MockProgram, MockUnit, Step};
use vaco_codec_core::{OnViolation, SendReceive, Validated};
use vaco_core::Error;

#[derive(Debug, arbitrary::Arbitrary)]
enum Action {
    Send(u8),
    Receive,
    Flush,
    Eof,
}

#[derive(Debug, arbitrary::Arbitrary)]
enum WireStep {
    Emit(u8),
    Reorder,
    Skip,
    Corrupt,
}

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    steps: Vec<WireStep>,
    reorder_delay: u8,
    capacity: u8,
    actions: Vec<Action>,
}

fuzz_target!(|input: Input| {
    if input.steps.is_empty() || input.actions.len() > 512 {
        return;
    }
    let steps: Vec<Step> = input
        .steps
        .iter()
        .map(|s| match s {
            WireStep::Emit(n) => Step::Emit(u32::from(*n % 4) + 1),
            WireStep::Reorder => Step::Reorder,
            WireStep::Skip => Step::Skip,
            WireStep::Corrupt => Step::Corrupt,
        })
        .collect();
    let program = MockProgram::new(steps).with_reorder_delay(usize::from(input.reorder_delay % 8));
    let capacity = usize::from(input.capacity % 8) + 1;
    let mut codec = Validated::with_mode(
        MockCodec::with_capacity(program.clone(), capacity),
        OnViolation::Record,
    );

    let mut fed: Vec<u64> = Vec::new();
    let mut seen: Vec<MockUnit> = Vec::new();
    let mut eof = false;

    for action in &input.actions {
        match action {
            Action::Send(id) => {
                let pkt = MockPacket::new(u64::from(*id));
                match codec.send(Some(&pkt)) {
                    Ok(()) | Err(Error::InvalidData(_)) => {
                        if !eof {
                            fed.push(u64::from(*id));
                        }
                    }
                    Err(Error::OutputPending) => {
                        assert!(!eof, "backpressure after end of stream");
                        // Backpressure must be backed by output.
                        match codec.receive() {
                            Ok(u) => seen.push(u),
                            Err(e) => panic!("backpressure with nothing pending: {e}"),
                        }
                    }
                    Err(Error::Eof) => assert!(eof, "Eof from send before end of stream"),
                    Err(e) => panic!("illegal send error: {e}"),
                }
            }
            Action::Receive => match codec.receive() {
                Ok(u) => seen.push(u),
                Err(Error::NeedMoreInput) => assert!(!eof, "NeedMoreInput while draining"),
                Err(Error::Eof) => assert!(eof, "Eof before draining began"),
                Err(Error::InvalidData(_)) => {}
                Err(e) => panic!("illegal receive error: {e}"),
            },
            Action::Flush => {
                codec.flush();
                fed.clear();
                seen.clear();
                eof = false;
            }
            Action::Eof => {
                if eof {
                    assert!(matches!(codec.send(None), Err(Error::Eof)));
                } else {
                    codec.send(None).expect("send(None) must be accepted once");
                    eof = true;
                }
            }
        }
        // What has been observed is always a prefix of the model.
        let (want_fed, want_drained) = program.expected(&fed);
        let expected: Vec<MockUnit> = want_fed.iter().chain(want_drained.iter()).copied().collect();
        assert!(
            seen.len() <= expected.len() && seen[..] == expected[..seen.len()],
            "output diverged from the model"
        );
    }

    if !eof {
        codec.send(None).expect("send(None) must be accepted once");
    }
    loop {
        match codec.receive() {
            Ok(u) => seen.push(u),
            Err(Error::Eof) => break,
            Err(e) => panic!("illegal drain error: {e}"),
        }
    }
    let (want_fed, want_drained) = program.expected(&fed);
    let expected: Vec<MockUnit> = want_fed.into_iter().chain(want_drained).collect();
    assert_eq!(seen, expected, "an output was lost or duplicated");
    assert!(
        codec.violations().is_empty(),
        "the reference codec violated the protocol: {:?}",
        codec.violations()
    );
});
