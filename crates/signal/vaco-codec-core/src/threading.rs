//! The threading contract: three axes, and the split-decoder shape that makes
//! frame threading expressible in safe Rust.
//!
//! Pipeline parallelism is the scheduler's business and needs nothing from a
//! codec. The other two axes do, and they are declared here.
//!
//! # Why a decoder is split in two
//!
//! The conventional approach propagates decoder state between per-thread
//! contexts. That is a mechanism for sharing mutable state safely, and it is
//! exactly what we do not want. Instead a frame-threaded decoder is two pieces:
//!
//! * a **sequential header stage** — [`FrameThreadedDecoder::split`] — that owns
//!   *all* mutable decoder state: parameter sets, the DPB, reference lists, and
//!   the allocation of the output picture. It runs on the caller's thread, in
//!   decode order, and emits a self-contained task;
//! * a **stateless frame task** — [`FrameTask`] — that owns its bitstream bytes,
//!   holds `Arc` snapshots of every parameter set it needs, holds
//!   [`PictureRef`]s to its references and the sole [`PictureWriter`] for its
//!   own output, and touches nothing else.
//!
//! Because the task holds only owned data and `Arc`s, it is `Send` by
//! construction and the compiler proves the absence of data races. There is no
//! state-propagation step because there is no shared state to propagate. The
//! cost is that the header stage is serial — which is fine: it is a low
//! single-digit percentage of decode time, and it is where the reference
//! semantics live, which is the part you most want single-threaded.

pub use vaco_core::CancelToken;

use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_packet::Packet;

use crate::picture::{PictureRef, PictureWriter, PlaneView};
use crate::{Caps, CodecParameters, Decoder};

/// What kind of intra-component parallelism an implementation offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Threading {
    /// Single-threaded.
    #[default]
    None,
    /// One picture's slices, tiles or wavefronts decode concurrently.
    Slice {
        /// Jobs the implementation can use at once.
        max_jobs: usize,
    },
    /// Several pictures decode concurrently.
    Frame {
        /// Pictures in flight at once.
        max_frames: usize,
        /// Extra output latency, in frames.
        delay: usize,
    },
    /// Both axes at once.
    Both {
        /// Pictures in flight at once.
        max_frames: usize,
        /// Jobs per picture.
        max_jobs: usize,
        /// Extra output latency, in frames.
        delay: usize,
    },
}

impl Threading {
    /// Pictures that may be in flight at once. One when frame threading is off.
    #[must_use]
    pub const fn max_frames(self) -> usize {
        match self {
            Self::None | Self::Slice { .. } => 1,
            Self::Frame { max_frames, .. } | Self::Both { max_frames, .. } => max_frames,
        }
    }

    /// Jobs one picture may be split into. One when slice threading is off.
    #[must_use]
    pub const fn max_jobs(self) -> usize {
        match self {
            Self::None | Self::Frame { .. } => 1,
            Self::Slice { max_jobs } | Self::Both { max_jobs, .. } => max_jobs,
        }
    }

    /// Extra output latency in frames, which the caller must add to the codec's
    /// own reorder delay when it sizes its buffers.
    #[must_use]
    pub const fn delay(self) -> usize {
        match self {
            Self::None | Self::Slice { .. } => 0,
            Self::Frame { delay, .. } | Self::Both { delay, .. } => delay,
        }
    }

    /// The capability bits a component must declare to make this claim.
    #[must_use]
    pub const fn required_caps(self) -> Caps {
        match self {
            Self::None => Caps::empty(),
            Self::Slice { .. } => Caps::SLICE_THREADS,
            Self::Frame { .. } => Caps::FRAME_THREADS,
            Self::Both { .. } => Caps::FRAME_THREADS.union(Caps::SLICE_THREADS),
        }
    }

    /// Whether a declaration is consistent with the descriptor's capabilities.
    #[must_use]
    pub const fn is_consistent_with(self, caps: Caps) -> bool {
        caps.contains(self.required_caps())
    }

    /// What this becomes when the user asks for `threads` threads.
    ///
    /// Determinism is a contract: output must be bit-identical for any legal
    /// thread count, so this only ever *narrows* what the implementation
    /// offers. `threads == 1` always yields [`Threading::None`].
    #[must_use]
    pub const fn clamped_to(self, threads: usize) -> Self {
        if threads <= 1 {
            return Self::None;
        }
        match self {
            Self::None => Self::None,
            Self::Slice { max_jobs } => Self::Slice {
                max_jobs: if max_jobs < threads {
                    max_jobs
                } else {
                    threads
                },
            },
            Self::Frame { max_frames, delay } => Self::Frame {
                max_frames: if max_frames < threads {
                    max_frames
                } else {
                    threads
                },
                delay,
            },
            Self::Both {
                max_frames,
                max_jobs,
                delay,
            } => Self::Both {
                max_frames: if max_frames < threads {
                    max_frames
                } else {
                    threads
                },
                max_jobs: if max_jobs < threads {
                    max_jobs
                } else {
                    threads
                },
                delay,
            },
        }
    }
}

/// Everything a frame task is allowed to reach that it does not own.
///
/// It carries no per-picture mutable state, which is what makes it `Sync` and
/// shareable by every task at once. `decode_index` is per-task but immutable,
/// and exists so [`TaskCtx::wait_rows`] can assert the wait graph is acyclic.
#[derive(Debug, Clone, Copy)]
pub struct TaskCtx<'a> {
    decode_index: u64,
    cancel: &'a CancelToken,
}

impl<'a> TaskCtx<'a> {
    /// A context for the task decoding picture `decode_index`.
    #[must_use]
    pub const fn new(decode_index: u64, cancel: &'a CancelToken) -> Self {
        Self {
            decode_index,
            cancel,
        }
    }

    /// This task's position in decode order.
    #[must_use]
    pub const fn decode_index(&self) -> u64 {
        self.decode_index
    }

    /// The shared cancellation flag.
    #[must_use]
    pub const fn cancel_token(&self) -> &'a CancelToken {
        self.cancel
    }

    /// Give up early if the decode has been cancelled.
    ///
    /// # Errors
    ///
    /// [`Error::Eof`], which the runner treats as "this task produced nothing"
    /// rather than as a decode failure.
    pub fn check_cancelled(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Error::Eof);
        }
        Ok(())
    }

    /// Wait for rows of a reference picture, checking the acyclicity invariant.
    ///
    /// # Errors
    ///
    /// As [`PictureRef::wait_rows`].
    pub fn wait_rows<'r>(
        &self,
        reference: &'r PictureRef,
        plane: usize,
        y: u32,
    ) -> Result<PlaneView<'r>> {
        reference.wait_rows_for(self.decode_index, plane, y)
    }
}

/// A self-contained unit of frame-level decode work.
///
/// `Send + 'static` is the whole design in a bound: a task that can be moved to
/// another thread and outlive the call that made it cannot be holding a
/// reference into decoder state.
pub trait FrameTask: Send + 'static {
    /// Decode this picture.
    ///
    /// # Errors
    ///
    /// Whatever the codec reports. Returning `Err` drops the task's
    /// [`PictureWriter`], which wakes every reader waiting on this picture with
    /// an error rather than leaving them blocked.
    fn run(self: Box<Self>, ctx: &TaskCtx<'_>) -> Result<Frame>;
}

/// What the header stage decided to do with a packet.
#[derive(Debug)]
pub enum SplitOutcome<T> {
    /// A picture to decode.
    Task(T),
    /// A header-only packet — parameter sets, metadata — with nothing to
    /// schedule.
    NoOutput,
    /// A resolution or format change. The runner drains every outstanding task
    /// before continuing, because the new configuration invalidates the old
    /// pictures.
    Reconfigure(Box<CodecParameters>),
}

/// A decoder that can be split into a serial header stage and parallel frame
/// tasks.
pub trait FrameThreadedDecoder: Decoder {
    /// The self-contained work unit this decoder emits.
    type Task: FrameTask;

    /// Parse headers, update the reference state, allocate the output picture
    /// and emit a task.
    ///
    /// Runs on the caller's thread, strictly in decode order. This is the only
    /// place mutable decoder state exists.
    ///
    /// # Errors
    ///
    /// Whatever the codec reports for a malformed or unsupported header.
    fn split(&mut self, packet: &Packet) -> Result<SplitOutcome<Self::Task>>;

    /// What this decoder offers, after any user thread-count clamp.
    fn threading(&self) -> Threading {
        Threading::None
    }
}

/// A decoder whose current picture can be split into independently decodable
/// jobs.
///
/// Driven by a scoped thread pool over [`PictureWriter::split_bands_mut`]:
/// each job holds a disjoint band range, so the disjointness is
/// `split_at_mut`-style and nothing exotic.
pub trait SliceThreadedDecoder: Decoder {
    /// One independently decodable partition of the current picture.
    type Job: Send;

    /// Partition the current picture, handing each job a disjoint region of the
    /// output.
    ///
    /// # Errors
    ///
    /// Whatever the codec reports for a partitioning it cannot express.
    fn slice_jobs<'a>(&'a mut self, writer: &'a mut PictureWriter) -> Result<Vec<Self::Job>>;

    /// Run one job. `&self`: every piece of per-job mutable state lives in the
    /// job itself, which is what lets these run concurrently.
    ///
    /// # Errors
    ///
    /// Whatever the codec reports.
    fn run_slice(&self, job: Self::Job) -> Result<()>;
}

// ---------------------------------------------------------------- the runner

/// A pool that runs [`FrameTask`]s concurrently and hands their frames back in
/// **dispatch order**, never completion order.
///
/// # Why dispatch-order collection is the whole determinism argument
///
/// A frame-threaded decoder still has to make every *ordering* decision — the
/// reorder buffer, the output bumping, an IDR's flush — in decode order, or the
/// emitted sequence becomes a function of how the threads happened to be
/// scheduled. This runner therefore refuses to hand a caller frame `k + 1`
/// before frame `k`: the caller sees exactly the sequence the serial decoder
/// saw, and `threads` changes only *when* the pixels were computed. Combined
/// with [`crate::PictureRef`]'s publish-once bands (a task cannot read a
/// reference sample that has not been written) that is the entire basis for
/// bit-identical output, and none of it needs `unsafe`.
///
/// # Why it cannot deadlock
///
/// Tasks are taken off one shared queue in dispatch order, and a task only ever
/// waits on pictures *earlier* in decode order (asserted by
/// [`TaskCtx::wait_rows`]). So the lowest-indexed task currently in flight has
/// every task before it already finished, and therefore never blocks — some
/// worker is always making progress. Dropping the runner closes the queue, and
/// dropping an undispatched task drops its [`crate::PictureWriter`], which wakes
/// every waiter with an error rather than leaving it parked.
///
/// # D18
///
/// `threads` is clamped to 1 where the target has no threads, exactly as
/// `vaco_sched::Driver` clamps, and a one-thread runner spawns nothing at all:
/// [`FrameRunner::dispatch`] runs the task inline on the caller's thread. The
/// single-threaded path therefore costs nothing for this machinery beyond one
/// `Vec` push, and is the path every other thread count must match.
#[derive(Debug)]
pub struct FrameRunner<T: FrameTask> {
    threads: usize,
    cancel: CancelToken,
    /// Next index to hand to [`FrameRunner::dispatch`].
    next_dispatch: u64,
    /// Index [`FrameRunner::collect`] will return next.
    next_collect: u64,
    /// One slot per dispatched-but-not-yet-collected task, `next_collect` first.
    slots: std::collections::VecDeque<Option<Result<Frame>>>,
    #[cfg(not(target_family = "wasm"))]
    pool: Option<Pool<T>>,
    #[cfg(target_family = "wasm")]
    _marker: core::marker::PhantomData<fn() -> T>,
}

impl<T: FrameTask> FrameRunner<T> {
    /// Whether this target can run tasks concurrently at all.
    #[must_use]
    pub const fn threads_available() -> bool {
        cfg!(not(target_family = "wasm"))
    }

    /// A runner with `threads` workers, clamped to what the target supports.
    ///
    /// `threads <= 1` spawns nothing and runs every task inline.
    #[must_use]
    pub fn new(threads: usize) -> Self {
        let threads = if Self::threads_available() {
            threads.max(1)
        } else {
            1
        };
        Self {
            threads,
            cancel: CancelToken::new(),
            next_dispatch: 0,
            next_collect: 0,
            slots: std::collections::VecDeque::new(),
            #[cfg(not(target_family = "wasm"))]
            pool: None,
            #[cfg(target_family = "wasm")]
            _marker: core::marker::PhantomData,
        }
    }

    /// Workers this runner will actually use.
    #[must_use]
    pub const fn threads(&self) -> usize {
        self.threads
    }

    /// Tasks dispatched but not yet collected.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.slots.len()
    }

    /// The decode index the next [`FrameRunner::dispatch`] will use.
    #[must_use]
    pub const fn next_decode_index(&self) -> u64 {
        self.next_dispatch
    }

    /// The shared cancellation flag every task's [`TaskCtx`] carries.
    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Queue `task`. Its decode index is [`FrameRunner::next_decode_index`].
    ///
    /// With one thread this runs the task immediately, so the caller's own
    /// `send_packet` does the same work at the same point it always did.
    pub fn dispatch(&mut self, task: T) {
        let index = self.next_dispatch;
        self.next_dispatch = self.next_dispatch.saturating_add(1);
        #[cfg(not(target_family = "wasm"))]
        if self.threads > 1 {
            self.slots.push_back(None);
            self.pool_mut().submit(index, task);
            return;
        }
        let ctx = TaskCtx::new(index, &self.cancel);
        self.slots.push_back(Some(Box::new(task).run(&ctx)));
    }

    /// Take the next task's result in dispatch order, blocking until it is
    /// ready. `None` when nothing is in flight.
    pub fn collect(&mut self) -> Option<Result<Frame>> {
        if self.slots.is_empty() {
            return None;
        }
        loop {
            if self.slots.front().is_some_and(Option::is_some) {
                let done = self.slots.pop_front().flatten();
                self.next_collect = self.next_collect.saturating_add(1);
                return done;
            }
            #[cfg(not(target_family = "wasm"))]
            {
                let base = self.next_collect;
                let Some((index, result)) = self.pool_mut().recv() else {
                    // Every worker is gone and nothing more can arrive; report
                    // the outstanding slot rather than blocking forever.
                    self.slots.pop_front();
                    self.next_collect = self.next_collect.saturating_add(1);
                    return Some(Err(Error::InvalidData(
                        "vaco-codec-core: a frame worker pool lost a task",
                    )));
                };
                if let Some(slot) = index
                    .checked_sub(base)
                    .and_then(|d| usize::try_from(d).ok())
                    .and_then(|d| self.slots.get_mut(d))
                {
                    *slot = Some(result);
                }
            }
            #[cfg(target_family = "wasm")]
            {
                // One thread: every slot is filled at dispatch, so the branch
                // above always took it.
                return None;
            }
        }
    }

    /// Take the next result only if it is already available.
    #[must_use]
    pub fn try_collect(&mut self) -> Option<Result<Frame>> {
        #[cfg(not(target_family = "wasm"))]
        if self.threads > 1 {
            let base = self.next_collect;
            while let Some((index, result)) = self.pool_mut().try_recv() {
                if let Some(slot) = index
                    .checked_sub(base)
                    .and_then(|d| usize::try_from(d).ok())
                    .and_then(|d| self.slots.get_mut(d))
                {
                    *slot = Some(result);
                }
            }
        }
        if self.slots.front().is_some_and(Option::is_some) {
            let done = self.slots.pop_front().flatten();
            self.next_collect = self.next_collect.saturating_add(1);
            return done;
        }
        None
    }

    /// Drain and discard everything in flight, then reset the index counters.
    ///
    /// What a decoder's `flush` needs after a seek: the pictures in flight
    /// reference a DPB that is about to be emptied.
    pub fn reset(&mut self) {
        while self.collect().is_some() {}
        self.next_dispatch = 0;
        self.next_collect = 0;
    }

    #[cfg(not(target_family = "wasm"))]
    fn pool_mut(&mut self) -> &mut Pool<T> {
        let threads = self.threads;
        self.pool.get_or_insert_with(|| Pool::spawn(threads))
    }
}

/// The worker threads and the two channels around them.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
struct Pool<T: FrameTask> {
    queue: std::sync::Arc<Queue<T>>,
    done: std::sync::mpsc::Receiver<(u64, Result<Frame>)>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

/// The shared task queue. A `Condvar` rather than an `mpsc::Receiver` because
/// several workers must wait on the *same* queue: whichever worker is free
/// takes the next task, which is what keeps the lowest in-flight index running.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
struct Queue<T> {
    state: std::sync::Mutex<QueueState<T>>,
    wake: std::sync::Condvar,
}

#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
struct QueueState<T> {
    items: std::collections::VecDeque<(u64, T)>,
    closed: bool,
}

#[cfg(not(target_family = "wasm"))]
fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sends a reply if it is still armed when it drops — the case where the task
/// unwound. Mirrors `vaco_sched::driver`'s own guard, for the same reason: the
/// collector must receive exactly one message per dispatched task or it waits
/// for one nobody will send.
#[cfg(not(target_family = "wasm"))]
struct ReplyGuard {
    tx: std::sync::mpsc::Sender<(u64, Result<Frame>)>,
    index: u64,
    armed: bool,
}

#[cfg(not(target_family = "wasm"))]
impl Drop for ReplyGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.tx.send((
                self.index,
                Err(Error::InvalidData("vaco-codec-core: a frame task panicked")),
            ));
        }
    }
}

#[cfg(not(target_family = "wasm"))]
impl<T: FrameTask> Pool<T> {
    fn spawn(threads: usize) -> Self {
        let queue: std::sync::Arc<Queue<T>> = std::sync::Arc::new(Queue {
            state: std::sync::Mutex::new(QueueState {
                items: std::collections::VecDeque::new(),
                closed: false,
            }),
            wake: std::sync::Condvar::new(),
        });
        let (done_tx, done) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..threads {
            let queue = std::sync::Arc::clone(&queue);
            let done_tx = done_tx.clone();
            let cancel = CancelToken::new();
            workers.push(std::thread::spawn(move || {
                loop {
                    let next = {
                        let mut st = lock(&queue.state);
                        loop {
                            if let Some(item) = st.items.pop_front() {
                                break Some(item);
                            }
                            if st.closed {
                                break None;
                            }
                            st = queue
                                .wake
                                .wait(st)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    };
                    let Some((index, task)) = next else { return };
                    let mut guard = ReplyGuard {
                        tx: done_tx.clone(),
                        index,
                        armed: true,
                    };
                    let ctx = TaskCtx::new(index, &cancel);
                    let result = Box::new(task).run(&ctx);
                    guard.armed = false;
                    let _ = done_tx.send((index, result));
                }
            }));
        }
        Self {
            queue,
            done,
            workers,
        }
    }

    fn submit(&self, index: u64, task: T) {
        let mut st = lock(&self.queue.state);
        st.items.push_back((index, task));
        self.queue.wake.notify_one();
    }

    fn recv(&self) -> Option<(u64, Result<Frame>)> {
        self.done.recv().ok()
    }

    fn try_recv(&self) -> Option<(u64, Result<Frame>)> {
        self.done.try_recv().ok()
    }
}

#[cfg(not(target_family = "wasm"))]
impl<T: FrameTask> Drop for Pool<T> {
    fn drop(&mut self) {
        {
            let mut st = lock(&self.queue.state);
            st.closed = true;
            st.items.clear();
        }
        self.queue.wake.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}
