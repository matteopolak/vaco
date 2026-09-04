//! Two drivers, one state machine.
//!
//! [`Pipeline`] is a step function; a driver is whatever calls it. This file
//! contains the crate's only threading, and a caller can write:
//!
//! ```no_run
//! # use vaco_sched::{Driver, Pipeline, PipelineSpec};
//! # fn go(pipeline: &mut Pipeline) -> vaco_core::Result<()> {
//! let finish = Driver::with_threads(4).run(pipeline)?;
//! # let _ = finish; Ok(()) }
//! ```
//!
//! The same code runs on `wasm32-unknown-unknown`, where
//! [`Driver::threads`] reports `1` and the loop is the serial one. There is no
//! `#[cfg]` in the caller and no second API to learn.
//!
//! # Why `std::thread` and not `rayon`
//!
//! Pipeline jobs are coarse units selected by the planner, not a divisible
//! iteration space. Keeping them out of `rayon` also preserves the invariant
//! that data-parallel workers never perform queue operations and keeps the core
//! portable without a native-only dependency. An async runtime adds no value
//! for this CPU-bound graph with little waiting.
//!
//! Spawning a thread per wave measured **45x to 60x slower than serial** on a
//! 200-packet transcode: 14–19 ms rather than 300 µs. Workers are therefore
//! spawned once per run and receive each wave through `std::sync::mpsc`.
//! The driver dispatches exactly `k` jobs and receives exactly `k` messages;
//! normal completion sends `Done`, while a panic drop guard sends `Lost`.
//! Workers never communicate with each other or touch a wire, so the driver
//! cannot wait for a result that no dispatched job will send.

#[cfg(not(target_family = "wasm"))]
use std::sync::mpsc;

use vaco_core::Result;

use crate::pipeline::{Advance, Finish, Pipeline};

/// How a pipeline is driven to completion.
///
/// Present on every target. Where threads are unavailable — `wasm32`, or a
/// build that asked for one thread — [`Driver::threads`] reports what was
/// actually granted and the pipeline runs serially through the same
/// [`Pipeline::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Driver {
    threads: usize,
}

impl Default for Driver {
    fn default() -> Self {
        Self::serial()
    }
}

impl Driver {
    /// Whether this target can run jobs concurrently at all.
    ///
    /// `false` on `wasm32-unknown-unknown`, which has no threads. Callers do
    /// not need to consult this — [`Driver::with_threads`] already clamps — but
    /// a progress display that wants to say "1 thread" can.
    #[must_use]
    pub const fn threads_available() -> bool {
        cfg!(not(target_family = "wasm"))
    }

    /// One thread. The only driver on a target without threads, and the one
    /// whose output every other driver must match.
    #[must_use]
    pub const fn serial() -> Self {
        Self { threads: 1 }
    }

    /// Up to `n` jobs in flight at once, clamped to what the target supports.
    ///
    /// Asking for more than the target can give is not an error: the point of
    /// D18 is that the same call works everywhere, so it degrades to serial and
    /// says so through [`Driver::threads`].
    #[must_use]
    pub const fn with_threads(n: usize) -> Self {
        let n = if n == 0 { 1 } else { n };
        Self {
            threads: if Self::threads_available() { n } else { 1 },
        }
    }

    /// Jobs this driver will actually run at once.
    #[must_use]
    pub const fn threads(&self) -> usize {
        self.threads
    }

    /// Drive `pipeline` until nothing is runnable, and report why it stopped.
    ///
    /// # Errors
    ///
    /// Whatever a component reported. The pipeline is cancelled before the
    /// error is returned, so no output is left half-written by a caller that
    /// retries.
    pub fn run(&self, pipeline: &mut Pipeline) -> Result<Finish> {
        #[cfg(not(target_family = "wasm"))]
        if self.threads > 1 {
            return run_threaded(self.threads, pipeline);
        }
        while pipeline.step()? == Advance::Stepped {}
        Ok(pipeline.classify())
    }
}

/// What a worker sends back for each job it was given. Exactly one per job,
/// always.
#[cfg(not(target_family = "wasm"))]
enum Reply {
    Done(Box<crate::node::Done>),
    /// The job's thread unwound. Sent by a drop guard so that the wave's
    /// message count is right even when a component panics.
    Lost,
}

/// Sends [`Reply::Lost`] if it is still armed when it drops, which is exactly
/// the case where the job panicked.
#[cfg(not(target_family = "wasm"))]
struct ReplyGuard {
    tx: mpsc::Sender<Reply>,
    armed: bool,
}

#[cfg(not(target_family = "wasm"))]
impl Drop for ReplyGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.tx.send(Reply::Lost);
        }
    }
}

/// Plan on this thread, run the wave's jobs on `width` persistent workers,
/// commit on this thread.
///
/// # Why the output is identical to the serial driver's
///
/// Jobs are committed in node order, not completion order, so the sequence of
/// pushes onto every wire is a function of the plan alone. The planner is
/// deterministic. And the one place where arrival order could still matter —
/// the muxer's interleave queue, which breaks a DTS tie by arrival sequence —
/// drains its input ports in ascending port order within a single job, so the
/// tie-break is by stream index either way. `threaded_matches_serial` asserts
/// it rather than trusting the argument.
#[cfg(not(target_family = "wasm"))]
fn run_threaded(width: usize, pipeline: &mut Pipeline) -> Result<Finish> {
    let (reply_tx, reply_rx) = mpsc::channel::<Reply>();
    let mut senders = Vec::new();
    let mut workers = Vec::new();
    for _ in 0..width {
        let (job_tx, job_rx) = mpsc::channel::<crate::node::Job>();
        let replies = reply_tx.clone();
        workers.push(std::thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let mut guard = ReplyGuard {
                    tx: replies.clone(),
                    armed: true,
                };
                let done = job.run();
                guard.armed = false;
                drop(guard);
                if replies.send(Reply::Done(Box::new(done))).is_err() {
                    break;
                }
            }
        }));
        senders.push(job_tx);
    }
    // The driver's own handle would otherwise keep the channel open forever.
    drop(reply_tx);

    let result = wave_loop(width, pipeline, &senders, &reply_rx);

    drop(senders);
    for worker in workers {
        // A worker only exits after its job channel closes, which the drop
        // above just did, so this join cannot outlive the current wave.
        let _ = worker.join();
    }
    result
}

#[cfg(not(target_family = "wasm"))]
fn wave_loop(
    width: usize,
    pipeline: &mut Pipeline,
    senders: &[mpsc::Sender<crate::node::Job>],
    replies: &mpsc::Receiver<Reply>,
) -> Result<Finish> {
    loop {
        if pipeline.cancel_token().is_cancelled() {
            break;
        }
        let mut progressed = pipeline.begin_step();
        let jobs = pipeline.check_out(width);
        if jobs.is_empty() {
            if !progressed {
                break;
            }
            pipeline.end_step(progressed)?;
            continue;
        }
        // One job per worker: `check_out` never returns more than `width`.
        let mut dispatched = 0_usize;
        let mut here = Vec::new();
        for (job, sender) in jobs.into_iter().zip(senders.iter()) {
            match sender.send(job) {
                Ok(()) => dispatched += 1,
                Err(mpsc::SendError(job)) => here.push(job.run()),
            }
        }
        let mut wave: Vec<crate::node::Done> = here;
        for _ in 0..dispatched {
            match replies.recv() {
                Ok(Reply::Done(done)) => wave.push(*done),
                // A component panicked. Its node's state went with the thread,
                // so the pipeline cannot be resumed.
                Ok(Reply::Lost) | Err(_) => {
                    pipeline.cancel();
                    return Err(vaco_core::Error::InvalidData(
                        "a pipeline job panicked; the node's component is lost",
                    ));
                }
            }
        }
        // Completion order is not commit order: sorting here is what makes the
        // output independent of how the threads happened to be scheduled.
        wave.sort_unstable_by_key(|d| d.node);
        for done in wave {
            progressed |= done.progressed;
            pipeline.check_in(done)?;
        }
        pipeline.end_step(progressed)?;
    }
    Ok(pipeline.classify())
}
