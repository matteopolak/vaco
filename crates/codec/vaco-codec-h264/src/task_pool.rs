//! Picture-shaped scratch reused across frame tasks (`planning/PERF-PROGRAMME.md`
//! item A0).
//!
//! Every picture used to allocate and free, afresh: its
//! [`crate::mb::MbSummary`] array (1,888 bytes each, 32,400 of them at 4K --
//! 59 MiB), its working reconstruction buffer
//! ([`crate::reconstruct::PictureReconstructor`], another ~12 MiB), and its
//! cropped output [`Frame`]'s own sample storage (~12 MiB more). None of
//! that is retained state -- a decoded picture's *pixels* end up in the
//! output `Frame` or a DPB entry, never in these -- so freeing it is
//! correct, but freeing and immediately reallocating the same size on the
//! very next picture is pure churn. Measured on a 4K clip: the live set is
//! ~150 MiB but peak RSS reached 3.87 GiB at 75 frames, because the
//! allocator caches freed buffers of these sizes rather than returning them
//! to the OS (`vmmap`'s `MALLOC_LARGE (empty)`), and a chunk of that cache
//! ends up swapped once it outgrows physical memory.
//!
//! [`TaskBufferPools`] is the fix: three small free lists, one per shape,
//! shared (via `Arc`) between the decoder's serial half and every
//! [`crate::frame_task::H264FrameTask`] it dispatches. A resolution switch
//! clears a free list rather than trying to reuse mis-sized buffers -- the
//! same rule [`vaco_frame::FramePool`] and `vaco_pool::BufferPool` already
//! use, which is why the output frame goes through that type directly
//! rather than a fourth bespoke mechanism (D19).
//!
//! # Budgeting
//!
//! The decoder's own aggregate budget (`H264Decoder::budget`) charges and
//! releases one lump sum per in-flight task -- `task_charge` at
//! `decoder.rs`'s dispatch site -- computed from *lengths*, not from
//! whether the bytes behind them came from the allocator or a free list.
//! That accounting is already correct for pooled buffers with no change: it
//! charges "this many bytes are part of a picture in flight" and releases
//! them when that picture is collected, which is true whether the storage
//! was just `malloc`'d or just came off a free list.
//!
//! What pooling changes is the *task-local* `Budget` each
//! [`crate::frame_task::H264FrameTask::run`] creates for its own
//! `max_alloc_single`/`max_frame_bytes` checks. A fresh allocation goes
//! through `Budget::alloc` as before; a pooled reuse calls
//! [`vaco_limits::Budget::charge`] for the same byte count instead --
//! checked and committed exactly like a real allocation, just without
//! asking the system allocator for memory it already holds. Never
//! `Vec::with_capacity`/`reserve` (denied workspace-wide): every `Vec` here
//! either comes straight off a free list or grows the ordinary way, by
//! `push`.

use std::sync::{Arc, Mutex, PoisonError};

use vaco_core::Result;
use vaco_frame::{Frame, FramePool};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;
use vaco_pool::PoolConfig;

use crate::mb::MbSummary;
use crate::reconstruct::PictureReconstructor;

#[derive(Debug, Default)]
struct ReconState {
    /// `(mbs_wide, mbs_high)` every entry in `free` is sized for. `None`
    /// until the first picture, and reset (with `free` cleared) whenever a
    /// later picture's geometry differs -- a reconstructor sized for the
    /// old resolution cannot serve the new one.
    key: Option<(u32, u32)>,
    free: Vec<PictureReconstructor>,
}

#[derive(Debug, Default)]
struct MbState {
    free: Vec<Vec<MbSummary>>,
}

/// The three per-picture buffer shapes this decoder reuses across its frame
/// tasks. See the module doc for what each one replaces and why reuse is
/// budgeted the way it is.
///
/// Cloning is cheap and shares the underlying pools -- the decoder keeps one
/// and clones it into every [`crate::frame_task::H264FrameTask`] it
/// dispatches, so a task on any worker thread can hand its buffers back when
/// it finishes.
#[derive(Debug, Clone)]
pub(crate) struct TaskBufferPools {
    recon: Arc<Mutex<ReconState>>,
    mb: Arc<Mutex<MbState>>,
    frames: FramePool,
    /// How many of each shape a free list keeps. More than this many can
    /// never be outstanding at once (bounded by the decoder's own
    /// `max_in_flight`), so anything beyond it is dropped for real rather
    /// than retained on the chance of a burst that cannot happen.
    cap: usize,
}

impl TaskBufferPools {
    /// `cap` should be the decoder's own `max_in_flight()` plus a little
    /// slack -- see [`Self::cap`]'s own doc.
    pub(crate) fn new(cap: usize) -> Self {
        let config = PoolConfig {
            max_retained_buffers: cap,
            ..PoolConfig::default()
        };
        Self {
            recon: Arc::new(Mutex::new(ReconState::default())),
            mb: Arc::new(Mutex::new(MbState::default())),
            frames: FramePool::new(config),
            cap,
        }
    }

    /// A working reconstruction buffer for `(mbs_wide, mbs_high)`, recycled
    /// from a previously finished task at the same geometry when the free
    /// list has one, freshly allocated (and charged the normal way) when it
    /// does not.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] when `budget` refuses either the
    /// charge for a reused buffer or the allocation for a new one.
    pub(crate) fn acquire_reconstructor(
        &self,
        mbs_wide: u32,
        mbs_high: u32,
        budget: &mut Budget,
    ) -> Result<PictureReconstructor> {
        let popped = {
            let mut st = self.recon.lock().unwrap_or_else(PoisonError::into_inner);
            if st.key != Some((mbs_wide, mbs_high)) {
                st.free.clear();
                st.key = Some((mbs_wide, mbs_high));
            }
            st.free.pop()
        };
        match popped {
            Some(mut r) => {
                budget.charge(r.charged_bytes())?;
                r.reset();
                Ok(r)
            }
            None => PictureReconstructor::new(mbs_wide, mbs_high, budget),
        }
    }

    /// Return a finished reconstructor to the free list for the next picture
    /// at the same geometry -- dropped for real (freeing its buffers) if the
    /// geometry has since moved on or the list is already at `cap`.
    pub(crate) fn release_reconstructor(&self, r: PictureReconstructor) {
        let mut st = self.recon.lock().unwrap_or_else(PoisonError::into_inner);
        if st.key == Some(r.geometry()) && st.free.len() < self.cap {
            st.free.push(r);
        }
    }

    /// A cleared `Vec<MbSummary>`, its capacity kept from a previous picture
    /// at the same macroblock count when the free list has one that fits, or
    /// a fresh empty `Vec` (grown the ordinary way, by `push`, inside
    /// `crate::mb::decode_slice_cabac_into`/`decode_slice_cavlc_into`) when
    /// it does not.
    pub(crate) fn acquire_macroblocks(&self, total_mbs: usize) -> Vec<MbSummary> {
        let mut st = self.mb.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some(mut v) = st.free.pop() {
            if v.capacity() >= total_mbs {
                v.clear();
                return v;
            }
            // Wrong-sized (a resolution switch since this entry was
            // retained): let it drop for real instead of growing it, since
            // growing it here would be exactly the `Vec::reserve` this
            // project bans.
        }
        Vec::new()
    }

    /// Return a picture's macroblock array once nothing borrows it any
    /// more (after `crate::deblock::DeblockCtx`'s own borrow of it is
    /// dropped).
    pub(crate) fn release_macroblocks(&self, mut v: Vec<MbSummary>) {
        v.clear();
        let mut st = self.mb.lock().unwrap_or_else(PoisonError::into_inner);
        if st.free.len() < self.cap {
            st.free.push(v);
        }
    }

    /// The output frame's own storage, pooled through
    /// [`vaco_frame::FramePool`] -- the one mechanism this tree already has
    /// for frame-shaped pooling (D19), used here for the first time outside
    /// its own crate's tests.
    ///
    /// # Errors
    ///
    /// As [`vaco_frame::FramePool::acquire_video`].
    pub(crate) fn acquire_frame(&self, format: PixFmt, width: u32, height: u32) -> Result<Frame> {
        self.frames.acquire_video(format, width, height)
    }
}
