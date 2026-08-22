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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// A cooperative cancellation flag shared by every task of one decode.
///
/// Cheap to clone and `Sync`; a task polls it at picture, slice or row
/// granularity. Cancelling also unblocks readers, because the cancelled task
/// drops its [`PictureWriter`], which marks its picture failed.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel every task holding this token.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
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
