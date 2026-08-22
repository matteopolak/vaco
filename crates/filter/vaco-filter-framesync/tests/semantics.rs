//! The synchronisation semantics, pinned against ffmpeg 8.1.
//!
//! Every expected value here was measured first. The probe overlays a solid
//! colour whose luma identifies which secondary frame was chosen, reads the
//! output back byte by byte, and reports the event timestamps with `showinfo`:
//!
//! ```sh
//! SEC="color=c=black:s=2x2:r=4:d=1,geq=lum='(N+1)*40':cb=128:cr=128,format=yuv420p"
//! ffmpeg -v info -fps_mode passthrough -filter_complex \
//!   "color=c=white:s=2x2:r=10:d=1[m];${SEC}[s];[m][s]overlay,format=gray,showinfo" -f null -
//! ```
//!
//! `-fps_mode passthrough` matters: without it the encoder duplicates frames to
//! reach a constant rate and the output count is the frame rate, not the event
//! count. That cost one wrong reading before it was noticed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    reason = "test code"
)]

use vaco_core::{Rational, TimeBase, Timestamp};
use vaco_filter_framesync::mock::{first_byte, gray_frame};
use vaco_filter_framesync::opts::{EofAction, TsSyncMode};
use vaco_filter_framesync::{FrameSync, FrameSyncOpts, FsInput, Step};
use vaco_frame::FramePool;

/// One input's script: its time base, its frames as `(pts, value)`, and the
/// timestamp it reports at end of stream.
struct Track {
    time_base: TimeBase,
    frames: Vec<(i64, u8)>,
    end: i64,
}

impl Track {
    /// `count` frames at `rate` frames per second, numbered from 1 so that a
    /// zero in the output means "no frame", exactly as the probe reads it.
    fn at(rate: i32, count: i64) -> Self {
        Self {
            time_base: Rational::new(1, rate),
            frames: (0..count)
                .map(|i| (i, u8::try_from(i + 1).unwrap_or(255)))
                .collect(),
            end: count,
        }
    }

    /// Shift every timestamp, and the end, by `by` ticks.
    fn delayed(mut self, by: i64) -> Self {
        for f in &mut self.frames {
            f.0 = f.0.saturating_add(by);
        }
        self.end = self.end.saturating_add(by);
        self
    }
}

/// Run a scenario and return `(event pts in the common time base, the value
/// each input contributed)`.
fn drive(tracks: Vec<Track>, roles: Vec<FsInput>, opts: FrameSyncOpts) -> Vec<(i64, Vec<u8>)> {
    let pool = FramePool::default();
    let mut roles = roles;
    for (role, track) in roles.iter_mut().zip(tracks.iter()) {
        role.time_base = track.time_base;
    }
    let mut sync = FrameSync::new(roles, opts).expect("configure");
    let mut cursors = vec![0usize; tracks.len()];
    let mut out = Vec::new();
    for _ in 0..10_000 {
        match sync.step() {
            Step::Ready => {
                let event = sync.event();
                let values = (0..event.len())
                    .map(|i| event.get(i).and_then(first_byte).unwrap_or(0))
                    .collect();
                out.push((event.pts(), values));
                sync.consume();
            }
            Step::Pending => {
                let wanted: Vec<usize> = sync.wants().collect();
                assert!(!wanted.is_empty(), "pending with nothing wanted");
                for i in wanted {
                    let track = &tracks[i];
                    match track.frames.get(cursors[i]) {
                        Some(&(pts, value)) => {
                            cursors[i] += 1;
                            let frame =
                                gray_frame(&pool, pts, track.time_base, value).expect("frame");
                            sync.feed(i, frame).expect("feed");
                        }
                        None => sync.close(i, Timestamp::new(track.end)),
                    }
                }
            }
            Step::Eof => return out,
        }
    }
    panic!("framesync did not finish");
}

fn dual(n: usize) -> Vec<FsInput> {
    FsInput::dual(n)
}

fn seconds(events: &[(i64, Vec<u8>)], tb: TimeBase) -> Vec<f64> {
    events
        .iter()
        .map(|(p, _)| f64::from(tb.num) * (*p as f64) / f64::from(tb.den))
        .collect()
}

fn secondary(events: &[(i64, Vec<u8>)]) -> Vec<u8> {
    events.iter().map(|(_, v)| v[1]).collect()
}

// ------------------------------------------------------- ts_sync_mode

#[test]
fn default_mode_takes_the_newest_frame_at_or_before_the_event() {
    // main 10 fps, secondary 4 fps, both one second.
    //   measured: 1c 1c 1c 4b 4b 79 79 79 a8 a8
    let events = drive(
        vec![Track::at(10, 10), Track::at(4, 4)],
        dual(2),
        FrameSyncOpts::default(),
    );
    assert_eq!(events.len(), 10);
    assert_eq!(secondary(&events), [1, 1, 1, 2, 2, 3, 3, 3, 4, 4]);
}

#[test]
fn nearest_mode_may_take_a_frame_from_the_future() {
    //   measured: 1c 1c 4b 4b 79 79 79 a8 a8 a8
    let events = drive(
        vec![Track::at(10, 10), Track::at(4, 4)],
        dual(2),
        FrameSyncOpts {
            ts_sync: TsSyncMode::Nearest,
            ..FrameSyncOpts::default()
        },
    );
    assert_eq!(secondary(&events), [1, 1, 2, 2, 3, 3, 3, 4, 4, 4]);
}

#[test]
fn nearest_breaks_an_exact_tie_towards_the_earlier_frame() {
    // main 8 fps against a 4 fps secondary puts every other event exactly half
    // way between two secondary frames.
    //   measured: 1c 1c 4b 4b 79 79 a8 a8
    let events = drive(
        vec![Track::at(8, 8), Track::at(4, 4)],
        dual(2),
        FrameSyncOpts {
            ts_sync: TsSyncMode::Nearest,
            ..FrameSyncOpts::default()
        },
    );
    assert_eq!(secondary(&events), [1, 1, 2, 2, 3, 3, 4, 4]);
}

// ------------------------------------------------------------ eof_action

#[test]
fn repeat_holds_the_last_secondary_frame_and_hands_the_clock_over() {
    // Secondary ends first: its last frame is held.
    let events = drive(
        vec![Track::at(10, 10), Track::at(10, 5)],
        dual(2),
        FrameSyncOpts::default(),
    );
    assert_eq!(events.len(), 10);
    assert_eq!(secondary(&events), [1, 2, 3, 4, 5, 5, 5, 5, 5, 5]);

    // Main ends first: the *secondary* takes over the clock, which is what
    // makes the output ten frames rather than five. Plan 16 §3.2 models
    // `after = Infinity` as holding a frame forever, which alone would never
    // terminate; the mechanism is that an ended input's sync level drops to
    // zero. Confirmed in the reference's own log:
    //   [framesync] Sync level 2 / Sync level 1 / Sync level 0
    let events = drive(
        vec![Track::at(10, 5), Track::at(10, 10)],
        dual(2),
        FrameSyncOpts::default(),
    );
    assert_eq!(events.len(), 10);
}

#[test]
fn endall_stops_at_the_first_end_of_stream() {
    for (main, second) in [(10i64, 5i64), (5, 10)] {
        let events = drive(
            vec![Track::at(10, main), Track::at(10, second)],
            dual(2),
            FrameSyncOpts {
                eof_action: EofAction::EndAll,
                ..FrameSyncOpts::default()
            },
        );
        assert_eq!(events.len(), 5, "main={main} second={second}");
    }
}

#[test]
fn pass_lets_the_main_finish_and_drops_a_spent_secondary() {
    // Secondary short: the main runs to its own end and the overlay vanishes.
    let events = drive(
        vec![Track::at(10, 10), Track::at(10, 5)],
        dual(2),
        FrameSyncOpts {
            eof_action: EofAction::Pass,
            ..FrameSyncOpts::default()
        },
    );
    assert_eq!(events.len(), 10);
    assert_eq!(secondary(&events), [1, 2, 3, 4, 5, 0, 0, 0, 0, 0]);

    // Main short: everything stops with it.
    let events = drive(
        vec![Track::at(10, 5), Track::at(10, 10)],
        dual(2),
        FrameSyncOpts {
            eof_action: EofAction::Pass,
            ..FrameSyncOpts::default()
        },
    );
    assert_eq!(events.len(), 5);
}

#[test]
fn repeatlast_zero_is_exactly_eof_action_pass() {
    // Plan 16 §3.3 says these are "nearly but not exactly" the same and that
    // `repeatlast=0` touches only the non-driving inputs. Measured, they are
    // identical, and both stop when input 0 ends.
    for (main, second) in [(10i64, 5i64), (5, 10)] {
        let pass = drive(
            vec![Track::at(10, main), Track::at(10, second)],
            dual(2),
            FrameSyncOpts {
                eof_action: EofAction::Pass,
                ..FrameSyncOpts::default()
            },
        );
        let repeatlast = drive(
            vec![Track::at(10, main), Track::at(10, second)],
            dual(2),
            FrameSyncOpts {
                repeatlast: false,
                ..FrameSyncOpts::default()
            },
        );
        assert_eq!(pass, repeatlast, "main={main} second={second}");
    }
}

#[test]
fn a_spent_secondary_is_delivered_exactly_once_before_it_disappears() {
    // The subtlest rule in the crate, and the one that fixed the model.
    // Measured at three different main frame rates against a secondary whose
    // last frame is at 0.75 s:
    //
    //   main 20 fps, sec 4 fps d=1, repeatlast=0
    //     … 0.7=04BA 0.75=0690 0.8=09F6 …
    //
    // The last secondary frame appears at exactly one event — the first at or
    // after its own timestamp — and is gone from the next. Applying end of
    // stream as soon as it is seen loses that frame entirely.
    let events = drive(
        vec![Track::at(20, 20), Track::at(4, 4)],
        dual(2),
        FrameSyncOpts {
            repeatlast: false,
            ..FrameSyncOpts::default()
        },
    );
    let values = secondary(&events);
    assert_eq!(values.len(), 20);
    assert_eq!(
        values,
        [1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 4, 0, 0, 0, 0]
    );
}

#[test]
fn shortest_overrides_every_other_setting() {
    for eof_action in [EofAction::Repeat, EofAction::EndAll, EofAction::Pass] {
        for repeatlast in [true, false] {
            let events = drive(
                vec![Track::at(10, 10), Track::at(10, 5)],
                dual(2),
                FrameSyncOpts {
                    eof_action,
                    repeatlast,
                    shortest: true,
                    ts_sync: TsSyncMode::Default,
                },
            );
            assert_eq!(events.len(), 5, "{eof_action:?} repeatlast={repeatlast}");
        }
    }
}

// --------------------------------------------------------- sync levels

#[test]
fn a_dual_input_filter_is_driven_by_its_main_then_by_whoever_is_left() {
    // main 10 fps, overlay 25 fps, both one second.
    //   measured: 0 0.1 … 0.9 0.92 0.96   — twelve events, not ten and not
    //   thirty-five, because the clock passes to the overlay when the main ends.
    let events = drive(
        vec![Track::at(10, 10), Track::at(25, 25)],
        dual(2),
        FrameSyncOpts::default(),
    );
    let tb = Rational::new(1, 50);
    let times = seconds(&events, tb);
    assert_eq!(events.len(), 12);
    assert!((times[9] - 0.9).abs() < 1e-9, "{times:?}");
    assert!((times[10] - 0.92).abs() < 1e-9, "{times:?}");
    assert!((times[11] - 0.96).abs() < 1e-9, "{times:?}");
}

#[test]
fn a_uniform_filter_fires_at_the_union_of_every_input() {
    // hstack, same sources, 0.4 s:
    //   measured: 0 0.04 0.08 0.1 0.12 0.16 0.2 0.24 0.28 0.3 0.32 0.36
    let events = drive(
        vec![Track::at(10, 4), Track::at(25, 10)],
        FsInput::uniform(2),
        FrameSyncOpts::default(),
    );
    let times = seconds(&events, Rational::new(1, 50));
    let want = [
        0.0, 0.04, 0.08, 0.1, 0.12, 0.16, 0.2, 0.24, 0.28, 0.3, 0.32, 0.36,
    ];
    assert_eq!(times.len(), want.len(), "{times:?}");
    for (got, expect) in times.iter().zip(want) {
        assert!((got - expect).abs() < 1e-9, "{times:?}");
    }
}

// ------------------------------------------------------------- before

#[test]
fn a_secondary_that_starts_late_is_simply_absent_until_it_does() {
    // ffmpeg … "[m][s]overlay" with the secondary delayed half a second
    //   -> ten events, the first five with nothing composited.
    let events = drive(
        vec![Track::at(10, 10), Track::at(10, 5).delayed(5)],
        dual(2),
        FrameSyncOpts::default(),
    );
    assert_eq!(events.len(), 10);
    assert_eq!(secondary(&events), [0, 0, 0, 0, 0, 1, 2, 3, 4, 5]);
}

#[test]
fn a_main_that_starts_late_delays_every_event() {
    // ffmpeg … main delayed half a second -> events at 0.5 … 0.9 only.
    let events = drive(
        vec![Track::at(10, 5).delayed(5), Track::at(10, 10)],
        dual(2),
        FrameSyncOpts::default(),
    );
    let times = seconds(&events, Rational::new(1, 10));
    assert_eq!(events.len(), 5);
    assert!((times[0] - 0.5).abs() < 1e-9, "{times:?}");
}

#[test]
fn a_uniform_filter_waits_for_every_input_to_start() {
    // hstack with either input delayed -> events start at 0.5 in both cases.
    for delay_first in [true, false] {
        let tracks = if delay_first {
            vec![Track::at(10, 5).delayed(5), Track::at(10, 10)]
        } else {
            vec![Track::at(10, 10), Track::at(10, 5).delayed(5)]
        };
        let events = drive(tracks, FsInput::uniform(2), FrameSyncOpts::default());
        let times = seconds(&events, Rational::new(1, 10));
        assert!((times[0] - 0.5).abs() < 1e-9, "{times:?}");
    }
}

// ----------------------------------------------------------- time base

#[test]
fn the_common_time_base_is_the_capped_gcd() {
    // Read out of the reference's own log: "[framesync] Selected 1/50 time base"
    let cases: [(TimeBase, TimeBase, TimeBase); 6] = [
        (
            Rational::new(1, 10),
            Rational::new(1, 25),
            Rational::new(1, 50),
        ),
        (
            Rational::new(1001, 30000),
            Rational::new(1, 25),
            Rational::new(1, 30000),
        ),
        (
            Rational::new(1, 1000),
            Rational::new(1, 1001),
            Rational::new(1, 1_000_000),
        ),
        (
            Rational::new(1, 24),
            Rational::new(1, 24),
            Rational::new(1, 24),
        ),
        (
            Rational::new(1001, 30000),
            Rational::new(1001, 24000),
            Rational::new(1001, 120_000),
        ),
        (
            Rational::new(1, 1),
            Rational::new(1, 65537),
            Rational::new(1, 65537),
        ),
    ];
    for (a, b, want) in cases {
        let mut roles = FsInput::dual(2);
        roles[0].time_base = a;
        roles[1].time_base = b;
        let sync = FrameSync::new(roles, FrameSyncOpts::default()).unwrap();
        assert_eq!(sync.time_base().reduced(), want.reduced(), "{a:?} {b:?}");
    }
}

#[test]
fn nearest_costs_exactly_one_frame_of_latency() {
    let mut roles = FsInput::dual(2);
    for r in &mut roles {
        r.time_base = Rational::new(1, 25);
    }
    let sync = FrameSync::new(roles.clone(), FrameSyncOpts::default()).unwrap();
    assert_eq!(sync.latency(), 0);
    let sync = FrameSync::new(
        roles,
        FrameSyncOpts {
            ts_sync: TsSyncMode::Nearest,
            ..FrameSyncOpts::default()
        },
    )
    .unwrap();
    assert_eq!(sync.latency(), 1);
}

// ----------------------------------------------------------- degenerate

#[test]
fn an_input_that_never_produces_a_frame_ends_the_synchroniser() {
    let events = drive(
        vec![Track::at(10, 0), Track::at(10, 5)],
        dual(2),
        FrameSyncOpts::default(),
    );
    assert!(events.is_empty());
}

#[test]
fn a_single_input_is_a_passthrough_on_its_own_timeline() {
    let events = drive(
        vec![Track::at(10, 4)],
        FsInput::uniform(1),
        FrameSyncOpts::default(),
    );
    assert_eq!(events.len(), 4);
    assert_eq!(
        events.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
}

#[test]
fn configuration_refuses_an_input_with_no_time_base() {
    assert!(FrameSync::new(Vec::new(), FrameSyncOpts::default()).is_err());
    assert!(FrameSync::new(FsInput::dual(2), FrameSyncOpts::default()).is_err());
}

#[test]
fn a_flush_returns_it_to_the_state_it_configured_in() {
    let pool = FramePool::default();
    let mut roles = FsInput::dual(2);
    for r in &mut roles {
        r.time_base = Rational::new(1, 10);
    }
    let mut sync = FrameSync::new(roles, FrameSyncOpts::default()).unwrap();
    for i in 0..2 {
        sync.feed(i, gray_frame(&pool, 0, Rational::new(1, 10), 7).unwrap())
            .unwrap();
    }
    // One frame each is not enough: the loop needs a lookahead past the event
    // before it can say that nothing else belongs to it. That one frame of
    // lookahead is inherent — the reference holds `frame_next` for the same
    // reason — and end of stream is what releases it.
    assert_eq!(sync.step(), Step::Pending);
    for i in 0..2 {
        sync.close(i, Timestamp::new(1));
    }
    assert_eq!(sync.step(), Step::Ready);
    assert_eq!(sync.event().pts(), 0);
    sync.consume();

    sync.flush();
    assert_eq!(sync.step(), Step::Pending);
    assert_eq!(sync.wants().count(), 2);
}
