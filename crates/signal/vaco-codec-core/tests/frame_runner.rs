//! `FrameRunner`: dispatch-order collection, and the reference-wait shape a
//! frame-threaded decoder actually uses.
//!
//! The point of these is the *ordering* contract. A pool that hands results
//! back in completion order would make a decoder's reorder buffer a function of
//! the scheduler; this one must not, at any thread count.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use vaco_codec_core::picture::{PictureRef, PictureSpec, PictureWriter, PlaneSpec, ProgressPicture};
use vaco_codec_core::{FrameRunner, FrameTask, TaskCtx};
use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};

/// A one-plane placeholder: these tests care about `pts` ordering, not pixels.
fn smallvec_one() -> smallvec::SmallVec<[vaco_frame::Plane; 4]> {
    smallvec::SmallVec::new()
}

/// A task that waits for its reference's samples, adds one to the first byte,
/// publishes the result and reports it as the frame's `pts`.
///
/// The chain `0 -> 1 -> 2 -> ...` is a serial dependency by construction, so a
/// wrong answer means a task read a picture before it was written.
struct Chained {
    reference: Option<PictureRef>,
    writer: PictureWriter,
    sleep_us: u64,
    ran: Arc<AtomicU32>,
}

impl FrameTask for Chained {
    fn run(self: Box<Self>, ctx: &TaskCtx<'_>) -> Result<Frame> {
        let Self {
            reference,
            mut writer,
            sleep_us,
            ran,
        } = *self;
        let value = match reference.as_ref() {
            None => 0u8,
            Some(r) => {
                let view = ctx.wait_rows(r, 0, 0)?;
                view.row(0).and_then(|row| row.first().copied()).unwrap_or(0) + 1
            }
        };
        // Uneven work, so completion order differs from dispatch order.
        std::thread::sleep(std::time::Duration::from_micros(sleep_us));
        {
            let mut band = writer.band_mut(0, 0)?;
            band.data_mut().fill(value);
        }
        writer.finish()?;
        ran.fetch_add(1, Ordering::SeqCst);
        let mut frame = Frame::from_data(vaco_frame::FrameData::Video {
            format: vaco_pixfmt::PixFmt::from_name("gray")
                .map_err(|_| vaco_core::Error::InvalidData("gray is not registered"))?,
            width: 1,
            height: 1,
            planes: smallvec_one(),
        });
        frame.pts = vaco_core::Timestamp::new(i64::from(value));
        Ok(frame)
    }
}

fn run_chain(threads: usize) -> Vec<i64> {
    let mut budget = Budget::new(Limits::permissive());
    let spec = PictureSpec::new(vec![PlaneSpec::new(16, 16)]).single_band();
    let mut runner: FrameRunner<Chained> = FrameRunner::new(threads);
    let ran = Arc::new(AtomicU32::new(0));
    let mut previous: Option<PictureRef> = None;
    let mut out = Vec::new();
    for i in 0..16u64 {
        let index = runner.next_decode_index();
        let (writer, reference) = ProgressPicture::allocate(&spec, index, &mut budget).unwrap();
        runner.dispatch(Chained {
            reference: previous.replace(reference),
            writer,
            // Descending, so later tasks finish sooner if nothing orders them.
            sleep_us: (16 - i) * 200,
            ran: Arc::clone(&ran),
        });
        if runner.in_flight() > threads {
            out.push(runner.collect().unwrap().unwrap().pts.ticks().unwrap());
        }
    }
    while let Some(frame) = runner.collect() {
        out.push(frame.unwrap().pts.ticks().unwrap());
    }
    assert_eq!(ran.load(Ordering::SeqCst), 16);
    out
}

#[test]
fn results_arrive_in_dispatch_order_at_every_thread_count() {
    let serial = run_chain(1);
    assert_eq!(serial, (0..16i64).collect::<Vec<_>>());
    for threads in [2, 3, 4, 8] {
        assert_eq!(
            run_chain(threads),
            serial,
            "{threads} threads produced a different sequence"
        );
    }
}

#[test]
fn a_failing_task_wakes_its_waiters_instead_of_parking_them() {
    struct Fails {
        reference: Option<PictureRef>,
        _writer: PictureWriter,
    }
    impl FrameTask for Fails {
        fn run(self: Box<Self>, ctx: &TaskCtx<'_>) -> Result<Frame> {
            if let Some(r) = self.reference.as_ref() {
                // The producer dropped its writer without finishing, so this
                // must return an error rather than block forever.
                ctx.wait_rows(r, 0, 0)?;
            }
            Err(vaco_core::Error::InvalidData("test"))
        }
    }
    let mut budget = Budget::new(Limits::permissive());
    let spec = PictureSpec::new(vec![PlaneSpec::new(16, 16)]).single_band();
    let mut runner: FrameRunner<Fails> = FrameRunner::new(4);
    let mut previous = None;
    for _ in 0..4 {
        let index = runner.next_decode_index();
        let (writer, reference) = ProgressPicture::allocate(&spec, index, &mut budget).unwrap();
        runner.dispatch(Fails {
            reference: previous.replace(reference),
            _writer: writer,
        });
    }
    for _ in 0..4 {
        assert!(runner.collect().unwrap().is_err());
    }
    assert!(runner.collect().is_none());
}

#[test]
fn one_thread_spawns_nothing_and_runs_inline() {
    let mut budget = Budget::new(Limits::permissive());
    let spec = PictureSpec::new(vec![PlaneSpec::new(8, 8)]).single_band();
    let mut runner: FrameRunner<Chained> = FrameRunner::new(1);
    assert_eq!(runner.threads(), 1);
    let (writer, _reference) = ProgressPicture::allocate(&spec, 0, &mut budget).unwrap();
    runner.dispatch(Chained {
        reference: None,
        writer,
        sleep_us: 0,
        ran: Arc::new(AtomicU32::new(0)),
    });
    // Inline: the result is already there before `collect` is called.
    assert_eq!(runner.in_flight(), 1);
    assert_eq!(runner.try_collect().unwrap().unwrap().pts.ticks(), Some(0));
}
