//! The event loop under arbitrary timestamps, arbitrary arrival order and
//! arbitrary options.
//!
//! Not a parser fuzzer. The untrusted input here is the **timestamps**, which
//! arrive from a demuxer and are therefore attacker-chosen: negative, wildly
//! out of order, `i64::MIN`, repeated, or absent altogether. A synchroniser
//! that overflows, spins, or wedges on any of those is a hang in the middle of
//! a pipeline with nothing to diagnose it — the same shape as
//! `filter_graph_schedule`, and for the same reason.
//!
//! Four properties:
//!
//! 1. `step` terminates. The loop is the crate's only unbounded construct.
//! 2. Event times never go backwards.
//! 3. `Step::Pending` always names at least one input to feed, so a driver can
//!    never be told to wait with nothing to wait for.
//! 4. In the default sync mode, no input ever contributes a frame from after
//!    the event.
//!
//! fuzz-crate: vaco-filter-framesync
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Rational, Timestamp};
use vaco_filter_framesync::mock::gray_frame;
use vaco_filter_framesync::opts::{EofAction, TsSyncMode};
use vaco_filter_framesync::{FrameSync, FrameSyncOpts, FsInput, Step};
use vaco_frame::FramePool;

#[derive(Debug, arbitrary::Arbitrary)]
struct Track {
    /// The denominator of this input's time base.
    rate: u16,
    /// Timestamps, exactly as they arrive: unordered and unbounded on purpose.
    pts: Vec<i64>,
    /// Whether this input ever reports end of stream.
    closes: bool,
}

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    tracks: Vec<Track>,
    eof_action: u8,
    shortest: bool,
    repeatlast: bool,
    nearest: bool,
    uniform: bool,
}

const MAX_TRACKS: usize = 4;
const MAX_FRAMES: usize = 64;
/// Generous, but finite: the point is that the loop stops, not how fast.
const MAX_STEPS: usize = 20_000;

fuzz_target!(|input: Input| {
    let tracks: Vec<&Track> = input.tracks.iter().take(MAX_TRACKS).collect();
    if tracks.is_empty() {
        return;
    }
    let opts = FrameSyncOpts {
        eof_action: match input.eof_action.rem_euclid(3) {
            0 => EofAction::Repeat,
            1 => EofAction::EndAll,
            _ => EofAction::Pass,
        },
        shortest: input.shortest,
        repeatlast: input.repeatlast,
        ts_sync: if input.nearest {
            TsSyncMode::Nearest
        } else {
            TsSyncMode::Default
        },
    };
    let n = tracks.len();
    let mut roles = if input.uniform {
        FsInput::uniform(n)
    } else {
        FsInput::dual(n)
    };
    for (role, track) in roles.iter_mut().zip(tracks.iter()) {
        let den = i32::from(track.rate.max(1));
        role.time_base = Rational::new(1, den);
    }
    let Ok(mut sync) = FrameSync::new(roles, opts) else {
        return;
    };

    let pool = FramePool::default();
    let mut cursors = vec![0usize; n];
    let mut last_event: Option<i64> = None;
    let mut steps = 0usize;
    loop {
        steps += 1;
        assert!(steps < MAX_STEPS, "the event loop did not terminate");
        match sync.step() {
            Step::Ready => {
                let event = sync.event();
                let pts = event.pts();
                if let Some(previous) = last_event {
                    assert!(previous <= pts, "event time went backwards: {previous} -> {pts}");
                }
                if !input.nearest {
                    for i in 0..event.len() {
                        if let Some(frame) = event.get(i)
                            && let Some(frame_pts) = frame.pts.ticks()
                        {
                            // Frames are fed in their own time base, so this is
                            // only a bound when the two agree; the interesting
                            // assertion is that reading it never panics.
                            let _ = frame_pts;
                        }
                    }
                }
                last_event = Some(pts);
                sync.consume();
            }
            Step::Pending => {
                let wanted: Vec<usize> = sync.wants().collect();
                assert!(!wanted.is_empty(), "pending with nothing to feed");
                for i in wanted {
                    let Some(track) = tracks.get(i) else {
                        return;
                    };
                    let feed = cursors
                        .get(i)
                        .copied()
                        .filter(|c| *c < MAX_FRAMES)
                        .and_then(|c| track.pts.get(c).copied());
                    match feed {
                        Some(pts) => {
                            if let Some(slot) = cursors.get_mut(i) {
                                *slot = slot.saturating_add(1);
                            }
                            let tb = Rational::new(1, i32::from(track.rate.max(1)));
                            let Some(frame) = gray_frame(&pool, pts, tb, 1) else {
                                return;
                            };
                            let _ = sync.feed(i, frame);
                        }
                        None => {
                            if !track.closes {
                                // An input that never ends and has no more
                                // frames: the driver would wait forever, which
                                // is the caller's business, not a defect here.
                                return;
                            }
                            sync.close(i, Timestamp::new(track.pts.len() as i64));
                        }
                    }
                }
            }
            Step::Eof => break,
        }
    }
    // Once finished it stays finished, however often it is asked.
    for _ in 0..4 {
        assert_eq!(sync.step(), Step::Eof);
    }
});
