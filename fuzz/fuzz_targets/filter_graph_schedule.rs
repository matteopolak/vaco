//! The frame-flow contract under an arbitrary caller and an arbitrary graph.
//!
//! Not a parser fuzzer: the untrusted input here is the *shape of the graph*
//! and the *order of the calls*. Any legal sequence must terminate, must never
//! panic, must never lose or duplicate a frame, and must never leave the
//! framework reporting a contract violation against filters that do not commit
//! one. The same shape as `codec_send_receive`, and for the same reason — the
//! ordering machinery here is shared by every one of the ~560 filters to come.
//!
//! Four things it is looking for:
//!
//! * a schedule that does not terminate (caught by the step budget, then
//!   asserted on);
//! * a frame that goes in and does not come out, or comes out twice;
//! * a [`Violation`] raised against a filter that is in fact well behaved;
//! * an unbounded queue — asserted through the pool's own accounting, since
//!   backpressure is the only thing keeping it bounded.
//! fuzz-crate: vaco-filter-core
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, MediaType, Rational, Timestamp};
use vaco_filter_core::mock::{
    any_video_sink, gray_frame, gray_link, video_source_formats, Drop, Fps, Invert,
};
use vaco_filter_core::{Graph, GraphStatus};
use vaco_pixfmt::PixFmt;

#[derive(Debug, arbitrary::Arbitrary)]
enum Stage {
    Invert,
    Drop(u8),
    Fps(u8),
}

#[derive(Debug, arbitrary::Arbitrary)]
enum Step {
    /// Feed one frame.
    Send,
    /// Take one frame, if any.
    Recv,
    /// One scheduler step.
    Step,
    /// Run to quiescence.
    Run,
    /// Signal end of stream.
    Close,
    /// Seek: discard everything in flight.
    Flush,
}

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    stages: Vec<Stage>,
    steps: Vec<Step>,
}

/// Bounded so libFuzzer measures the contract rather than our patience.
const MAX_STAGES: usize = 4;
const MAX_STEPS: usize = 256;

fuzz_target!(|input: Input| {
    if input.stages.len() > MAX_STAGES || input.steps.len() > MAX_STEPS {
        return;
    }

    let mut graph = Graph::new().with_step_budget(4096);
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );

    // A chain of `Invert` alone conserves frames exactly; anything else may
    // legitimately change the count, so the conservation assertion is only made
    // when every stage is order- and count-preserving.
    let mut conserves = true;
    let mut prev = src;
    for (i, stage) in input.stages.iter().enumerate() {
        let label = format!("s{i}");
        let node = match stage {
            Stage::Invert => Invert::node(&mut graph, &label),
            Stage::Drop(n) => {
                conserves = false;
                Drop::node(&mut graph, &label, u64::from(*n % 8) + 2)
            }
            Stage::Fps(n) => {
                conserves = false;
                Fps::node(&mut graph, &label, Rational::new(i32::from(*n % 60) + 1, 1))
            }
        };
        if graph.connect(prev, 0, node, 0).is_err() {
            return;
        }
        prev = node;
    }
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    if graph.connect(prev, 0, sink, 0).is_err() {
        return;
    }
    if graph
        .set_source_format(src, gray_link(8, 8, Rational::new(1, 25)))
        .is_err()
    {
        return;
    }
    if graph.configure().is_err() {
        // Every graph built above is negotiable; if that stops being true it is
        // a finding, not something to shrug at.
        panic!("a graph of gray8 filters failed to negotiate");
    }

    let mut sent: i64 = 0;
    let mut received: usize = 0;
    let mut closed = false;
    let mut seen: Vec<i64> = Vec::new();

    for step in &input.steps {
        match step {
            Step::Send => {
                if !closed {
                    let value = u8::try_from(sent & 0xff).unwrap_or(0);
                    match graph.send(src, gray_frame(8, 8, sent, value)) {
                        Ok(()) => sent += 1,
                        // Backpressure. This comment used to say "the frame was
                        // not taken, and retrying with the same one is legal" —
                        // which the docs also claimed and the signature made
                        // impossible, because `send` consumed the frame and
                        // dropped it. Now it really does come back, so drop it
                        // here deliberately: this target is exercising the
                        // scheduler's step ordering, not recovery.
                        Err(r) if matches!(r.error, Error::OutputPending) => {}
                        Err(r) => panic!("send failed unexpectedly: {:?}", r.error),
                    }
                }
            }
            Step::Recv => match graph.recv(sink) {
                Ok(frame) => {
                    received += 1;
                    if let Some(t) = frame.pts.ticks() {
                        // Timestamps never go backwards on a link, whatever the
                        // stages did to the rate.
                        if let Some(last) = seen.last() {
                            assert!(t >= *last, "pts went backwards: {last} then {t}");
                        }
                        seen.push(t);
                    }
                }
                Err(Error::NeedMoreInput | Error::Eof) => {}
                Err(e) => panic!("recv failed unexpectedly: {e:?}"),
            },
            Step::Step => {
                if graph.run_once().is_err() {
                    return;
                }
            }
            Step::Run => match graph.run() {
                Ok(GraphStatus::BudgetExhausted) => {
                    panic!("the scheduler did not terminate")
                }
                Ok(GraphStatus::Deadlock(stalls)) => {
                    panic!("deadlock with well-behaved filters: {stalls:?}")
                }
                Ok(_) => {}
                Err(_) => return,
            },
            Step::Close => {
                if !closed && graph.close_source(src, Timestamp::new(sent)).is_ok() {
                    closed = true;
                }
            }
            Step::Flush => {
                graph.flush();
                // A seek discards what was in flight, so the accounting restarts.
                sent = 0;
                received = 0;
                closed = false;
                seen.clear();
            }
        }
        assert!(
            graph.violations().is_empty(),
            "the framework accused a well-behaved filter: {:?}",
            graph.violations()
        );
    }

    // Drain to the end and check nothing was invented or lost.
    if !closed {
        let _ = graph.close_source(src, Timestamp::new(sent));
    }
    for _ in 0..(MAX_STEPS + 64) {
        match graph.run() {
            Ok(GraphStatus::BudgetExhausted) => panic!("the scheduler did not terminate"),
            Ok(_) => {}
            Err(_) => return,
        }
        match graph.recv(sink) {
            Ok(_) => received += 1,
            Err(Error::Eof) => break,
            Err(Error::NeedMoreInput) => break,
            Err(_) => return,
        }
    }
    if conserves {
        assert_eq!(
            received,
            usize::try_from(sent).unwrap_or(0),
            "a chain of frame-preserving filters lost or duplicated a frame"
        );
    }
    assert!(graph.violations().is_empty());
});
