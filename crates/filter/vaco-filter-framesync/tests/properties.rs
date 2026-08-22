//! Properties. Plan 13 §3.2: where there is an invariant, state it and let
//! proptest look for the counterexample.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_core::{Rational, TimeBase, Timestamp};
use vaco_filter_framesync::mock::gray_frame;
use vaco_filter_framesync::opts::{EofAction, TsSyncMode};
use vaco_filter_framesync::{FrameSync, FrameSyncOpts, FsInput, Step, apply_opts, gcd_q};
use vaco_frame::FramePool;

#[derive(Debug, Clone)]
struct Script {
    time_base: TimeBase,
    /// Strictly increasing timestamps.
    pts: Vec<i64>,
}

/// What one run observed: the event times, and each input's contributed pts.
#[derive(Debug, Default)]
struct Observed {
    times: Vec<i64>,
    contributed: Vec<Vec<Option<i64>>>,
}

fn run(scripts: &[Script], roles: Vec<FsInput>, opts: FrameSyncOpts) -> Observed {
    let pool = FramePool::default();
    let mut roles = roles;
    for (role, script) in roles.iter_mut().zip(scripts.iter()) {
        role.time_base = script.time_base;
    }
    let Ok(mut sync) = FrameSync::new(roles, opts) else {
        return Observed::default();
    };
    let mut cursors = vec![0usize; scripts.len()];
    let mut observed = Observed::default();
    for _ in 0..20_000 {
        match sync.step() {
            Step::Ready => {
                let event = sync.event();
                observed.times.push(event.pts());
                observed.contributed.push(
                    (0..event.len())
                        .map(|i| event.get(i).and_then(|f| f.pts.ticks()))
                        .collect(),
                );
                sync.consume();
            }
            Step::Pending => {
                for i in sync.wants().collect::<Vec<_>>() {
                    let script = &scripts[i];
                    match script.pts.get(cursors[i]) {
                        Some(&pts) => {
                            cursors[i] += 1;
                            let Some(frame) = gray_frame(&pool, pts, script.time_base, 1) else {
                                return observed;
                            };
                            let _ = sync.feed(i, frame);
                        }
                        None => sync.close(
                            i,
                            Timestamp::new(script.pts.last().copied().unwrap_or(0) + 1),
                        ),
                    }
                }
            }
            Step::Eof => return observed,
        }
    }
    panic!("framesync did not terminate");
}

fn script() -> impl Strategy<Value = Script> {
    (
        prop::sample::select(vec![1i32, 4, 10, 24, 25, 30, 1000]),
        prop::collection::vec(0i64..6, 0..8),
    )
        .prop_map(|(rate, gaps)| {
            let mut pts = Vec::new();
            let mut at = 0i64;
            for g in gaps {
                pts.push(at);
                at = at.saturating_add(g).saturating_add(1);
            }
            Script {
                time_base: Rational::new(1, rate),
                pts,
            }
        })
}

fn options() -> impl Strategy<Value = FrameSyncOpts> {
    (
        prop::sample::select(vec![EofAction::Repeat, EofAction::EndAll, EofAction::Pass]),
        any::<bool>(),
        any::<bool>(),
        prop::sample::select(vec![TsSyncMode::Default, TsSyncMode::Nearest]),
    )
        .prop_map(
            |(eof_action, shortest, repeatlast, ts_sync)| FrameSyncOpts {
                eof_action,
                shortest,
                repeatlast,
                ts_sync,
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// The property that matters most: whatever it is fed, and whatever the
    /// options say, the loop terminates. A synchroniser that does not is a hung
    /// pipeline with no diagnosis.
    #[test]
    fn the_event_loop_always_terminates(
        scripts in prop::collection::vec(script(), 1..4),
        opts in options(),
    ) {
        let n = scripts.len();
        let _ = run(&scripts, FsInput::dual(n), opts);
        let _ = run(&scripts, FsInput::uniform(n), opts);
    }

    /// Events are emitted in order. Downstream is a muxer eventually, and a
    /// synchroniser that went backwards would be impossible to debug there.
    #[test]
    fn event_timestamps_never_go_backwards(
        scripts in prop::collection::vec(script(), 1..4),
        opts in options(),
    ) {
        let n = scripts.len();
        for roles in [FsInput::dual(n), FsInput::uniform(n)] {
            let observed = run(&scripts, roles, opts);
            for pair in observed.times.windows(2) {
                prop_assert!(pair[0] <= pair[1], "{:?}", observed.times);
            }
        }
    }

    /// In the default sync mode an input never contributes a frame from the
    /// future: that is the whole difference between `default` and `nearest`.
    #[test]
    fn default_mode_never_looks_ahead(
        scripts in prop::collection::vec(script(), 1..4),
    ) {
        let n = scripts.len();
        let observed = run(&scripts, FsInput::dual(n), FrameSyncOpts::default());
        for (event, contributions) in observed.times.iter().zip(&observed.contributed) {
            for pts in contributions.iter().flatten() {
                prop_assert!(
                    *pts <= *event,
                    "contributed {pts} at event {event}"
                );
            }
        }
    }

    /// A single input is a passthrough: one event per frame, at its own times.
    #[test]
    fn one_input_conserves_frames(script in script(), opts in options()) {
        let observed = run(core::slice::from_ref(&script), FsInput::uniform(1), opts);
        prop_assert_eq!(observed.times, script.pts);
    }

    /// The option-to-mode mapping is a function of the options alone, and
    /// applying it twice changes nothing.
    #[test]
    fn applying_the_options_is_idempotent(opts in options(), n in 1usize..5) {
        let mut once = FsInput::dual(n);
        apply_opts(&mut once, opts);
        let mut twice = once.clone();
        apply_opts(&mut twice, opts);
        prop_assert_eq!(once, twice);
    }

    /// The common time base divides both inputs' time bases, or is the
    /// documented fallback.
    #[test]
    fn the_common_time_base_divides_its_inputs(
        a in prop::sample::select(vec![1i32, 4, 10, 24, 25, 30, 48, 1000, 30000]),
        b in prop::sample::select(vec![1i32, 4, 10, 24, 25, 30, 48, 1000, 30000]),
    ) {
        let (x, y) = (Rational::new(1, a), Rational::new(1, b));
        let g = gcd_q(x, y).reduced();
        prop_assert!(g.num > 0 && g.den > 0, "{g:?}");
        if g != vaco_filter_framesync::FALLBACK_TIME_BASE {
            // x / g and y / g must both be whole numbers.
            for t in [x, y] {
                let ratio = t.checked_div(g).map(Rational::reduced);
                prop_assert!(
                    ratio.is_some_and(|r| r.den == 1),
                    "{t:?} / {g:?} = {ratio:?}"
                );
            }
        }
    }
}
