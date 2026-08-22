//! The adapter, so that no filter implements the event loop.
//!
//! As with `Simple` in `vaco-filter-core`: 68 filters need this behaviour and
//! all 68 must have it identically, so it is written once. A filter supplies
//! `on_event` and its per-input roles; it gets `eof_action`, `shortest`,
//! `repeatlast` and `ts_sync_mode` for free.

use std::collections::VecDeque;

use vaco_core::Result;
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::{Activity, Filter, FilterContext, LinkFormat};

use crate::opts::{FrameSyncOpts, FsInput};
use crate::sync::{FrameSync, FrameSyncEvent, Step};

/// How many frames one `activate` may pull before giving up and reporting.
///
/// A bound rather than a tuning knob: the loop is already bounded by what the
/// input links can hold, and this is the belt-and-braces cap that turns a
/// mis-written synchroniser into a diagnosable stall instead of a spin.
const MAX_PULLS_PER_STEP: usize = 256;

/// A filter that consumes one aligned set of frames at a time.
pub trait FrameSyncFilter: Send {
    /// Handle one aligned set of frames.
    ///
    /// Returning [`FrameOut`] rather than pushing directly — which is what
    /// plan 16 §3.4 sketches — lets the adapter hold frames back when the
    /// output link is full, exactly as `Simple` does. A filter that pushed for
    /// itself would have to own that queue 68 times over.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut>;

    /// Per-input roles. The dual-input default — input 0 drives, the rest are
    /// sampled — covers `overlay`, `blend`, `lut2` and most of the family;
    /// `hstack` and friends override it with [`FsInput::uniform`].
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    /// The user's options.
    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts::default()
    }

    /// Called once after the synchroniser has been configured and the output
    /// link has taken the common time base. Override the geometry here.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are
    /// unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Discard buffered state after a seek.
    fn flush_state(&mut self) {}
}

/// Adapts a [`FrameSyncFilter`] to [`Filter`].
#[derive(Debug)]
pub struct Synced<F> {
    inner: F,
    sync: Option<FrameSync>,
    pending: VecDeque<vaco_frame::Frame>,
    done: bool,
}

impl<F> Synced<F> {
    /// Wrap a filter.
    pub const fn new(inner: F) -> Self {
        Self {
            inner,
            sync: None,
            pending: VecDeque::new(),
            done: false,
        }
    }

    /// Borrow the wrapped filter.
    pub const fn inner(&self) -> &F {
        &self.inner
    }

    /// Recover the wrapped filter.
    pub fn into_inner(self) -> F {
        self.inner
    }

    /// The synchroniser, once configured. What `latency`/`alatency` and
    /// `graphmonitor` read.
    pub const fn sync(&self) -> Option<&FrameSync> {
        self.sync.as_ref()
    }
}

impl<F: FrameSyncFilter> Filter for Synced<F> {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let n = ctx.input_count();
        let mut roles = self.inner.inputs(n);
        roles.resize(n, FsInput::default());
        for (i, role) in roles.iter_mut().enumerate() {
            role.time_base = ctx
                .input_link(i)
                .map_or(vaco_core::Rational::UNDEFINED, LinkFormat::time_base);
        }
        let sync = FrameSync::new(roles, self.inner.opts())?;
        // The output carries the common timeline by default, which is what
        // `blend` and the stack family do. `overlay` keeps its main input's
        // time base instead, and says so by overriding it below.
        if let Some(mut out) = ctx.output_link(0).cloned() {
            set_time_base(&mut out, sync.time_base());
            ctx.set_output_link(0, out);
        }
        self.sync = Some(sync);
        self.inner.configure(ctx)
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.done = false;
        if let Some(sync) = self.sync.as_mut() {
            sync.flush();
        }
        self.inner.flush_state();
    }

    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if self.done {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        if let Some(activity) = push_pending(ctx, &mut self.pending)? {
            return Ok(activity);
        }
        if !ctx.output_has_room(0) {
            return Ok(Activity::Blocked);
        }
        if self.sync.is_none() {
            return Err(vaco_core::Error::InvalidData(
                "a framesync filter was activated before it was configured",
            ));
        }
        // One activate emits at most one event, so siblings are not starved.
        // The loop only runs while the synchroniser *cannot* emit: each turn
        // either moves a frame off a link or records an end of stream, both of
        // which are monotone, so it is bounded by what the links can hold.
        let mut moved_a_frame = false;
        for _ in 0..MAX_PULLS_PER_STEP {
            let Some(sync) = self.sync.as_mut() else {
                return Ok(Activity::Blocked);
            };
            match sync.step() {
                Step::Ready => {
                    let mut event = sync.event();
                    let out = self.inner.on_event(ctx, &mut event)?;
                    if let Some(sync) = self.sync.as_mut() {
                        sync.consume();
                    }
                    out.drain_into(&mut self.pending);
                    let _ = push_pending(ctx, &mut self.pending)?;
                    return Ok(Activity::Progressed);
                }
                Step::Eof => {
                    ctx.close_all_outputs();
                    self.done = true;
                    return Ok(Activity::Eof);
                }
                Step::Pending => {
                    let wanted: Vec<usize> = sync.wants().collect();
                    if wanted.is_empty() {
                        // The synchroniser wants something it cannot name.
                        // Unreachable through this adapter; reporting rather
                        // than spinning keeps a defect diagnosable.
                        return Ok(Activity::Blocked);
                    }
                    let mut advanced = false;
                    for pad in &wanted {
                        if let Some(frame) = ctx.take_input(*pad) {
                            if let Some(sync) = self.sync.as_mut() {
                                sync.feed(*pad, frame)?;
                            }
                            moved_a_frame = true;
                            advanced = true;
                        } else if ctx.input_at_eof(*pad) {
                            // Recording end of stream changes no link, so this
                            // is *not* progress the scheduler can see (rule F6).
                            // Loop instead of reporting it, or the node parks
                            // against an epoch that will never move again.
                            let end = ctx.input_end_pts(*pad);
                            if let Some(sync) = self.sync.as_mut() {
                                sync.close(*pad, end);
                            }
                            advanced = true;
                        }
                    }
                    if advanced {
                        continue;
                    }
                    for pad in wanted {
                        ctx.request_input(pad);
                    }
                    return Ok(if moved_a_frame {
                        Activity::Progressed
                    } else {
                        Activity::NeedInput
                    });
                }
            }
        }
        Ok(if moved_a_frame {
            Activity::Progressed
        } else {
            Activity::Blocked
        })
    }

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = (name, value);
        Err(vaco_core::Error::Unsupported(
            "filter accepts no runtime commands",
        ))
    }
}

fn set_time_base(format: &mut LinkFormat, tb: vaco_core::TimeBase) {
    match format {
        LinkFormat::Video { time_base, .. } | LinkFormat::Audio { time_base, .. } => {
            *time_base = tb;
        }
    }
}

/// Push as much of `pending` as the link will take.
fn push_pending(
    ctx: &mut FilterContext<'_>,
    pending: &mut VecDeque<vaco_frame::Frame>,
) -> Result<Option<Activity>> {
    let mut pushed = false;
    while let Some(frame) = pending.pop_front() {
        if !ctx.output_has_room(0) {
            pending.push_front(frame);
            return Ok(Some(if pushed {
                Activity::Progressed
            } else {
                Activity::Blocked
            }));
        }
        ctx.push_output(0, frame)?;
        pushed = true;
    }
    Ok(None)
}
