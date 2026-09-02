//! Adapters: `activate` written once per filter *shape*.
//!
//! Writing `activate` correctly means getting demand checking, status ordering,
//! end-of-stream flushing, writability and timeline gating right — and roughly
//! four filters in five need none of it. So it is written once here and almost
//! no filter implements [`Filter`] directly. With ~560 filters to come, an
//! awkward API is paid for 560 times, so these are the most important thing in
//! the crate after the frame-flow rules themselves.
//!
//! | Adapter | Shape |
//! |---|---|
//! | [`Simple`] | 1-in 1-out, one frame in → zero or more out, plus a flush at end of stream |
//! | [`Sourced`] | 0-in 1-out, produces on demand |
//! | [`AudioFilter`] | like [`FrameFilter`] but sees exactly `frame_size` samples, with a correctly short final frame |
//! | [`Paired`] | N-in 1-out, strict lockstep: one frame from every input or the filter ends |
//! | [`Fanout`] | 1-in N-out (N fixed at construction), one frame in → exactly N out |
//! | [`Dual`] | N-in M-out (both fixed at construction, default 2/2 — `feedback`'s own arity), `Paired`'s lockstep input rule and `Fanout`'s all-outputs-have-room rule combined — gap 24's adapter half |
//!
//! Not here: `SliceFilter`, which needs a thread pool this crate does not depend
//! on, and `Synced`, which lives in `vaco-filter-framesync` and is the *other*
//! multi-input shape — see [`Paired`]'s own doc for the measured difference
//! between the two, which is not just "this crate cannot depend on that one".
//! See `docs/filter/vaco-filter-core.md`.

use smallvec::SmallVec;
use vaco_core::{Result, Timestamp};
use vaco_frame::{Frame, FrameData};

use crate::timeline::Timeline;
use crate::{Activity, Filter, FilterContext};

/// What one call to a filter produced.
///
/// Three variants rather than a `Vec` because the overwhelmingly common answers
/// are "one frame" and "none", and neither should allocate.
///
/// The size gap between `None` and `One` is a `Frame`, which is metadata plus a
/// `SmallVec` of plane handles. Boxing it to even the variants out would trade a
/// stack move — which the optimiser sees through — for a heap allocation on the
/// single hottest path in the framework, so the gap stays.
#[derive(Debug, Default)]
#[allow(clippy::large_enum_variant, reason = "see the type's documentation")]
pub enum FrameOut {
    /// Nothing this time. A filter that buffers, or one that dropped a frame.
    #[default]
    None,
    /// The usual case.
    One(Frame),
    /// Several: `fps` upsampling, a flush releasing a window.
    Many(SmallVec<[Frame; 4]>),
}

impl FrameOut {
    /// How many frames this carries.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::One(_) => 1,
            Self::Many(v) => v.len(),
        }
    }

    /// Whether this carries no frames.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Collect into a queue, in order.
    pub fn drain_into(self, out: &mut std::collections::VecDeque<Frame>) {
        match self {
            Self::None => {}
            Self::One(f) => out.push_back(f),
            Self::Many(v) => out.extend(v),
        }
    }
}

impl From<Frame> for FrameOut {
    fn from(f: Frame) -> Self {
        Self::One(f)
    }
}

impl From<Option<Frame>> for FrameOut {
    fn from(f: Option<Frame>) -> Self {
        f.map_or(Self::None, Self::One)
    }
}

impl FromIterator<Frame> for FrameOut {
    fn from_iter<I: IntoIterator<Item = Frame>>(iter: I) -> Self {
        let v: SmallVec<[Frame; 4]> = iter.into_iter().collect();
        match v.len() {
            0 => Self::None,
            1 => v.into_iter().next().map_or(Self::None, Self::One),
            _ => Self::Many(v),
        }
    }
}

/// The default filter shape: one frame in, zero or more out.
///
/// `input` arrives **by value**, so writing to it is `Arc::make_mut` and copies
/// only when the buffer is genuinely shared. That is why the reference's
/// `NEEDS_WRITABLE` pad flag has no equivalent here: ownership carries it.
pub trait FrameFilter: Send {
    /// Transform one frame.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut>;

    /// Called once after the input reaches end of stream, before the output is
    /// closed. Release anything still buffered.
    ///
    /// Called repeatedly until it returns an empty [`FrameOut`], so a filter
    /// holding many frames does not have to hand them over all at once.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let _ = ctx;
        Ok(FrameOut::None)
    }

    /// Called once when link formats have been agreed.
    ///
    /// A filter that changes geometry or timing declares it here, with
    /// [`FilterContext::set_output_link`]. Without this hook the adapter would
    /// be unusable for the whole class of rate-changing filters — which is most
    /// of the interesting ones.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Discard buffered state after a seek. Does not change configuration.
    ///
    /// A filter that holds frames — a delay line, a temporal window, a rate
    /// converter's held input — **must** implement this, or the frames it kept
    /// across the seek come out mixed with the new ones.
    fn flush_state(&mut self) {}
}

/// Adapts a [`FrameFilter`] to [`Filter`].
///
/// The step order matters and is fixed: check downstream demand → take one input
/// frame or observe end of stream → evaluate the timeline → call the filter →
/// queue → push what fits. Anything the link would not take stays in the
/// adapter's own queue, so the filter is never asked to hold a frame it already
/// produced.
#[derive(Debug)]
pub struct Simple<F> {
    inner: F,
    pending: std::collections::VecDeque<Frame>,
    timeline: Timeline,
    flushing: bool,
    done: bool,
    frame_index: u64,
}

impl<F> Simple<F> {
    /// Wrap a filter.
    pub fn new(inner: F) -> Self {
        Self {
            inner,
            pending: std::collections::VecDeque::new(),
            timeline: Timeline::always(),
            flushing: false,
            done: false,
            frame_index: 0,
        }
    }

    /// Gate the filter on an `enable=` expression.
    #[must_use]
    pub fn with_timeline(mut self, timeline: Timeline) -> Self {
        self.timeline = timeline;
        self
    }

    /// Borrow the wrapped filter.
    pub const fn inner(&self) -> &F {
        &self.inner
    }

    /// Recover the wrapped filter.
    pub fn into_inner(self) -> F {
        self.inner
    }
}

impl<F: FrameFilter> Filter for Simple<F> {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if self.done {
            // Close again rather than returning early. `close_output` is
            // idempotent, so this costs nothing in the normal case. It is also
            // belt-and-braces against a driver that re-opened a link without
            // calling `Filter::flush` — which is exactly the shape a fuzz target
            // found before that hook existed.
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        // 1. Drain anything the link refused last time, before doing new work:
        //    order is the whole contract, and a frame held back must go first.
        if let Some(activity) = push_pending(ctx, &mut self.pending)? {
            return Ok(activity);
        }
        if !ctx.output_has_room(0) {
            return Ok(Activity::Blocked);
        }
        // 2. One input frame, or end of stream.
        if let Some(frame) = ctx.take_input(0) {
            let enabled = self.timeline.evaluate(&frame, self.frame_index);
            self.frame_index = self.frame_index.saturating_add(1);
            let out = if enabled {
                self.inner.filter_frame(ctx, frame)?
            } else {
                // Timeline off: forward unchanged. Well defined only for 1-in
                // 1-out of one media type, which `FilterDesc::is_consistent`
                // checks at registration rather than leaving as a convention.
                FrameOut::One(frame)
            };
            out.drain_into(&mut self.pending);
            let _ = push_pending(ctx, &mut self.pending)?;
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            // 3. Flush, one call at a time, until nothing more comes out.
            if !self.flushing {
                self.flushing = true;
            }
            let out = self.inner.flush(ctx)?;
            let produced = !out.is_empty();
            out.drain_into(&mut self.pending);
            if produced {
                let _ = push_pending(ctx, &mut self.pending)?;
                return Ok(Activity::Progressed);
            }
            if self.pending.is_empty() {
                ctx.close_all_outputs();
                self.done = true;
                return Ok(Activity::Eof);
            }
            return Ok(Activity::Blocked);
        }
        Ok(Activity::NeedInput)
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        // The filter first, so that a rate-changing filter's new output time
        // base is in place before the timeline caches it.
        self.inner.configure(ctx)?;
        self.timeline.configure(ctx);
        Ok(())
    }

    fn flush(&mut self) {
        // Everything a seek invalidates. The compiled `enable` expression and
        // its cached geometry are configuration, not state, so they survive —
        // the same split `Decoder::flush` makes.
        self.pending.clear();
        self.flushing = false;
        self.done = false;
        self.frame_index = 0;
        self.inner.flush_state();
    }

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        if name == "enable" {
            return self.timeline.set_expression(value);
        }
        Err(vaco_core::Error::Unsupported(
            "filter accepts no runtime commands",
        ))
    }
}

/// Push as much of `pending` as the link will take.
///
/// Returns `Some(Blocked)` when frames are left over, so the caller can stop
/// without inventing a reason of its own.
fn push_pending(
    ctx: &mut FilterContext<'_>,
    pending: &mut std::collections::VecDeque<Frame>,
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

/// A filter that consumes exactly one frame from each of several inputs at a
/// time, strictly in lockstep.
///
/// # This is not `vaco-filter-framesync`, and the difference is measured, not architectural
///
/// The 68 filters `vaco-filter-framesync` names (`overlay`, `blend`, `lut2`,
/// `alphamerge` and the rest) give every input its **own timeline**: a
/// secondary that starts late is invisible until it starts, one that ends
/// early is held or dropped per `eof_action`/`shortest`/`repeatlast`, and
/// `ts_sync_mode` picks which of several buffered frames a given instant
/// samples. `framepack` and `mergeplanes` do not do any of that, and it is
/// measured rather than assumed:
///
/// ```text
/// $ ffmpeg -h filter=framepack   # no eof_action/shortest/repeatlast/ts_sync_mode section at all
/// $ ffmpeg -h filter=alphamerge  # has one, verbatim the framesync surface
/// ```
///
/// and, more sharply, `framepack` **refuses** two inputs with different time
/// bases outright (`Left and right time bases differ (1/10 vs 1/5)`) rather
/// than reconciling them — there is no timeline to reconcile onto. Feeding it
/// a 10-frame main and a 5-frame secondary at the same time base produces
/// exactly 5 output frames: not 10 with the last secondary frame repeated
/// (`eof_action=repeat`'s behaviour), not an error — the filter simply stops
/// the instant either input is exhausted, discarding whatever the longer
/// input still had queued. `mergeplanes` measures identically. That is what
/// "paired" means here: every input contributes one frame per call or the
/// whole filter ends, with no per-input timeline in between.
///
/// A filter that *does* need `eof_action`/`shortest`/`repeatlast`/
/// `ts_sync_mode` — a real per-input timeline, like `alphamerge` — wants
/// [`vaco_filter_framesync`](../../vaco_filter_framesync/index.html)'s
/// `Synced` instead, not this adapter. `vaco-filter-core` cannot depend on
/// `vaco-filter-framesync` regardless (layering: framesync depends on core,
/// not the reverse — `cargo xtask layer-check`), so this is not "framesync
/// without the crate"; it is the genuinely simpler shape those two filters
/// turn out to need, kept separate rather than duplicating framesync's event
/// loop for a filter that was never going to use most of it (D19).
///
/// # Generalised to N inputs
///
/// `framepack` is exactly two inputs, which is the shape this adapter is
/// named for. `mergeplanes` needs up to four, fixed at construction by its
/// own `mapN`s options — the same "N decided before the graph runs" shape
/// `vaco-filter-audio`'s `amix`/`amerge` hand-roll for a *different* policy
/// (theirs hold a per-input timeline too; see [`PairedFilter::input_count`]
/// vs. those crates' own `activate`). Rather than add a third adapter for
/// "N-in-1-out, lockstep" as a copy of "2-in-1-out, lockstep", this one
/// generalises: [`PairedFilter::input_count`] defaults to two and a filter
/// that needs more overrides it.
pub trait PairedFilter: Send {
    /// How many inputs this filter has. Two is the default and the common
    /// case (`framepack`); a filter with a construction-time input count
    /// (`mergeplanes`) overrides this to report it.
    fn input_count(&self) -> usize {
        2
    }

    /// Handle one aligned set: exactly [`PairedFilter::input_count`] frames,
    /// one per input pad, in pad order.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut>;

    /// Called once when link formats have been agreed.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Discard buffered state after a seek.
    ///
    /// Frames already pulled from an input but not yet paired with the
    /// others are part of that state: the adapter clears them itself, so
    /// this is for whatever the filter holds beyond that.
    fn flush_state(&mut self) {}
}

/// Adapts a [`PairedFilter`] to [`Filter`].
#[derive(Debug)]
pub struct Paired<F> {
    inner: F,
    /// One slot per input, filled as frames arrive. A call fires only once
    /// every slot is `Some`.
    held: SmallVec<[Option<Frame>; 4]>,
    pending: std::collections::VecDeque<Frame>,
    done: bool,
}

impl<F: PairedFilter> Paired<F> {
    /// Wrap a filter.
    pub fn new(inner: F) -> Self {
        let n = inner.input_count();
        Self {
            inner,
            held: std::iter::repeat_with(|| None).take(n).collect(),
            pending: std::collections::VecDeque::new(),
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
}

impl<F: PairedFilter> Filter for Paired<F> {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.inner.configure(ctx)
    }

    fn flush(&mut self) {
        for slot in &mut self.held {
            *slot = None;
        }
        self.pending.clear();
        self.done = false;
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

        let mut progressed = false;
        let mut ended = false;
        for (pad, slot) in self.held.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            if let Some(frame) = ctx.take_input(pad) {
                *slot = Some(frame);
                progressed = true;
            } else if ctx.input_at_eof(pad) {
                // No repeat, no independent timeline (see this type's doc):
                // the first input to run dry ends the whole filter, even if
                // the others still have frames queued.
                ended = true;
            } else {
                ctx.request_input(pad);
            }
        }

        if ended {
            ctx.close_all_outputs();
            self.done = true;
            return Ok(Activity::Eof);
        }

        if self.held.iter().all(Option::is_some) {
            let frames: SmallVec<[Frame; 4]> =
                self.held.iter_mut().filter_map(Option::take).collect();
            let out = self.inner.filter_frames(ctx, frames)?;
            out.drain_into(&mut self.pending);
            let _ = push_pending(ctx, &mut self.pending)?;
            return Ok(Activity::Progressed);
        }

        Ok(if progressed {
            Activity::Progressed
        } else {
            Activity::NeedInput
        })
    }

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = (name, value);
        Err(vaco_core::Error::Unsupported(
            "filter accepts no runtime commands",
        ))
    }
}

/// A filter that turns one input frame into a fixed number of output frames,
/// one per output pad — `extractplanes`' shape, with the pad count decided by
/// options (`planes=y+u+v` is three pads) before the graph runs.
///
/// # The witness this generalises
///
/// `vaco-filter-plumbing`'s `split`/`asplit` already fan one input out to N
/// *identical* outputs (`DYNAMIC_OUTPUTS`, cloning the frame — cheap, since a
/// [`Frame`] clone is `Arc` refcount bumps, never a pixel copy). This adapter
/// is the same backpressure shape — wait until every output pad has room,
/// then take one input frame — generalised from "push N clones" to "push N
/// *different* frames the filter derives from the one input", which is what
/// `extractplanes` needs and `split` does not.
///
/// # Why the room check comes before the read, for every pad
///
/// Consuming the input before every output can take a frame would mean
/// holding N frames the adapter cannot get rid of if only some pads are
/// blocked — a queue per pad, sized to the graph's discretion, for a case
/// that need not exist: the input is not read until every pad already has
/// room, so pushing the derived N frames immediately afterwards cannot fail
/// on backpressure. Same reasoning as `split.rs`; this adapter just has more
/// than one destination frame per call to account for.
pub trait FanoutFilter: Send {
    /// How many output pads this filter has, fixed at construction from its
    /// own options.
    fn output_count(&self) -> usize;

    /// Turn one input frame into exactly [`FanoutFilter::output_count`]
    /// frames, one per output pad, in pad order.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn split_frame(
        &mut self,
        ctx: &mut FilterContext<'_>,
        input: Frame,
    ) -> Result<SmallVec<[Frame; 4]>>;

    /// Release anything buffered at end of stream, one full set of
    /// [`FanoutFilter::output_count`] frames at a time. `None` when nothing
    /// more comes out — the common case, since `extractplanes` holds nothing.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<SmallVec<[Frame; 4]>>> {
        let _ = ctx;
        Ok(None)
    }

    /// Called once when link formats have been agreed.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Discard buffered state after a seek.
    fn flush_state(&mut self) {}
}

/// Adapts a [`FanoutFilter`] to [`Filter`].
#[derive(Debug)]
pub struct Fanout<F> {
    inner: F,
    outputs: usize,
    flushing: bool,
    done: bool,
}

impl<F: FanoutFilter> Fanout<F> {
    /// Wrap a filter.
    pub fn new(inner: F) -> Self {
        let outputs = inner.output_count();
        Self {
            inner,
            outputs,
            flushing: false,
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
}

impl<F: FanoutFilter> Fanout<F> {
    fn push_all(&self, ctx: &mut FilterContext<'_>, frames: SmallVec<[Frame; 4]>) -> Result<()> {
        if frames.len() != self.outputs {
            return Err(vaco_core::Error::InvalidData(
                "fanout filter produced the wrong number of frames",
            ));
        }
        for (pad, frame) in frames.into_iter().enumerate() {
            ctx.push_output(pad, frame)?;
        }
        Ok(())
    }
}

impl<F: FanoutFilter> Filter for Fanout<F> {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.inner.configure(ctx)
    }

    fn flush(&mut self) {
        self.flushing = false;
        self.done = false;
        self.inner.flush_state();
    }

    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if self.done {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        if (0..self.outputs).any(|p| !ctx.output_has_room(p)) {
            return Ok(if ctx.output_closed(0) {
                Activity::Eof
            } else {
                Activity::Blocked
            });
        }
        if let Some(frame) = ctx.take_input(0) {
            let outs = self.inner.split_frame(ctx, frame)?;
            self.push_all(ctx, outs)?;
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            if !self.flushing {
                self.flushing = true;
            }
            if let Some(outs) = self.inner.flush(ctx)? {
                self.push_all(ctx, outs)?;
                return Ok(Activity::Progressed);
            }
            ctx.close_all_outputs();
            self.done = true;
            return Ok(Activity::Eof);
        }
        ctx.forward_wanted();
        Ok(Activity::NeedInput)
    }

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = (name, value);
        Err(vaco_core::Error::Unsupported(
            "filter accepts no runtime commands",
        ))
    }
}

/// A filter with a fixed, small number of inputs *and* outputs, consuming
/// one frame from every input in lockstep and producing one frame for every
/// output each time — `feedback`'s shape (`VV->VV`).
///
/// # Why this did not exist already
///
/// Every adapter before this one is N-to-1 or 1-to-N: [`Paired`] takes
/// several inputs down to one output, [`Fanout`] takes one input out to
/// several. Enumerating them (`Simple`/`Blocked` 1-in-1-out, `Sourced`
/// 0-in-1-out, `Fanout` 1-in-*N*-out, `Paired` *N*-in-1-out) found nothing
/// *N*-in-*M*-out, which is exactly `feedback`'s arity. This adapter is
/// `Paired`'s lockstep-input rule and `Fanout`'s all-outputs-have-room rule,
/// combined rather than reimplemented — each output pad gets its own
/// pending queue where `Paired` only ever needed pad `0`'s.
///
/// # This adapter is necessary but not sufficient for `feedback`
///
/// `feedback`'s reference usage loops one output back as the filter's own
/// next-frame input (`[0][fb]feedback[out][fb]`) — a genuine cycle in the
/// filtergraph, not just an unusual pad count. `Graph::configure` requires
/// [`crate::sched::Graph::topological_order`], which rejects any cycle
/// outright (`Error::InvalidData("filtergraph contains a cycle")`) before
/// a single frame flows — checked directly against this crate's own
/// scheduler, not assumed. Wiring `feedback` with this adapter over a real
/// feedback link therefore still fails, at `configure`, independent of
/// anything this adapter does correctly. Cyclic graph negotiation is a
/// second, separate, larger capability this pass does not attempt. This
/// adapter closes the "no *N*-in-*M*-out shape exists" gap and is usable
/// today by any filter with fixed, non-cyclic multi-in/multi-out wiring;
/// it does not by itself make `feedback` runnable.
pub trait DualFilter: Send {
    /// How many inputs this filter has. `2` is `feedback`'s own arity and
    /// the default; a filter with a construction-time count may override
    /// it the way [`PairedFilter::input_count`] does.
    fn input_count(&self) -> usize {
        2
    }

    /// How many outputs this filter has. `2` is `feedback`'s own arity.
    fn output_count(&self) -> usize {
        2
    }

    /// Handle one aligned set: exactly [`DualFilter::input_count`] frames
    /// in, and exactly [`DualFilter::output_count`] frames must come back,
    /// one per output pad, in pad order.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<SmallVec<[Frame; 4]>>;

    /// Called once when link formats have been agreed.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Discard buffered state after a seek.
    fn flush_state(&mut self) {}
}

/// Adapts a [`DualFilter`] to [`Filter`].
#[derive(Debug)]
pub struct Dual<F> {
    inner: F,
    inputs: usize,
    /// One slot per input, filled as frames arrive — mirrors [`Paired`]'s
    /// `held`.
    held: SmallVec<[Option<Frame>; 4]>,
    /// One pending queue per *output* pad — the part [`Paired`] never
    /// needed, since it only ever has one output.
    pending: SmallVec<[std::collections::VecDeque<Frame>; 4]>,
    done: bool,
}

impl<F: DualFilter> Dual<F> {
    /// Wrap a filter.
    pub fn new(inner: F) -> Self {
        let inputs = inner.input_count();
        let outputs = inner.output_count();
        Self {
            held: std::iter::repeat_with(|| None).take(inputs).collect(),
            pending: std::iter::repeat_with(std::collections::VecDeque::new)
                .take(outputs)
                .collect(),
            inputs,
            inner,
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

    /// Try to drain every output pad's pending queue. Returns `Some` the
    /// moment any pad cannot take its next frame yet, the same
    /// blocked-vs-progressed distinction [`push_pending`] makes for the
    /// single-output case.
    fn drain_pending(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Activity>> {
        let mut pushed = false;
        for (pad, queue) in self.pending.iter_mut().enumerate() {
            while let Some(frame) = queue.pop_front() {
                if !ctx.output_has_room(pad) {
                    queue.push_front(frame);
                    return Ok(Some(if pushed {
                        Activity::Progressed
                    } else {
                        Activity::Blocked
                    }));
                }
                ctx.push_output(pad, frame)?;
                pushed = true;
            }
        }
        Ok(None)
    }
}

impl<F: DualFilter> Filter for Dual<F> {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.inner.configure(ctx)
    }

    fn flush(&mut self) {
        for slot in &mut self.held {
            *slot = None;
        }
        for queue in &mut self.pending {
            queue.clear();
        }
        self.done = false;
        self.inner.flush_state();
    }

    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if self.done {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        if let Some(activity) = self.drain_pending(ctx)? {
            return Ok(activity);
        }
        if (0..self.pending.len()).any(|p| !ctx.output_has_room(p)) {
            return Ok(Activity::Blocked);
        }

        let mut progressed = false;
        let mut ended = false;
        for pad in 0..self.inputs {
            let Some(slot) = self.held.get_mut(pad) else {
                continue;
            };
            if slot.is_some() {
                continue;
            }
            if let Some(frame) = ctx.take_input(pad) {
                *slot = Some(frame);
                progressed = true;
            } else if ctx.input_at_eof(pad) {
                // Same rule as `Paired`: no repeat, no independent
                // timeline — the first input to run dry ends the whole
                // filter.
                ended = true;
            } else {
                ctx.request_input(pad);
            }
        }

        if ended {
            ctx.close_all_outputs();
            self.done = true;
            return Ok(Activity::Eof);
        }

        if self.held.iter().all(Option::is_some) {
            let frames: SmallVec<[Frame; 4]> =
                self.held.iter_mut().filter_map(Option::take).collect();
            let outputs = self.inner.filter_frames(ctx, frames)?;
            if outputs.len() != self.pending.len() {
                return Err(vaco_core::Error::InvalidData(
                    "dual filter produced the wrong number of output frames",
                ));
            }
            for (pad, frame) in outputs.into_iter().enumerate() {
                if let Some(queue) = self.pending.get_mut(pad) {
                    queue.push_back(frame);
                }
            }
            let _ = self.drain_pending(ctx)?;
            return Ok(Activity::Progressed);
        }

        Ok(if progressed {
            Activity::Progressed
        } else {
            Activity::NeedInput
        })
    }

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = (name, value);
        Err(vaco_core::Error::Unsupported(
            "filter accepts no runtime commands",
        ))
    }
}

/// A filter with no inputs: it makes frames rather than transforming them.
pub trait SourceFilter: Send {
    /// Produce the next frame, or `None` when the source is exhausted.
    ///
    /// Called only when downstream has asked, so a source never runs ahead of
    /// demand and a graph never allocates without a consumer.
    ///
    /// # Errors
    /// Whatever generating the frame reports.
    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>>;

    /// Declare the output link's geometry and timing.
    ///
    /// **A source must implement this.** Every other filter inherits its output
    /// link's dimensions and time base from its input; a source has no input, so
    /// nothing else in the graph knows what it produces, and configuration fails
    /// with "a link was left without a usable format". The worked examples found
    /// this the hard way.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// The timestamp the stream ends at, once [`SourceFilter::produce`] has
    /// returned `None`. Carried on the output link's terminal status.
    fn end_pts(&self) -> Timestamp {
        Timestamp::NONE
    }

    /// Discard buffered state after a seek.
    ///
    /// A source is **not** rewound by this: nothing here carries a seek target,
    /// so a source that had finished stays finished. Rewinding is the concrete
    /// source's own business.
    fn flush_state(&mut self) {}
}

/// Adapts a [`SourceFilter`] to [`Filter`].
#[derive(Debug)]
pub struct Sourced<F> {
    inner: F,
    done: bool,
}

impl<F> Sourced<F> {
    /// Wrap a source.
    pub const fn new(inner: F) -> Self {
        Self { inner, done: false }
    }

    /// Borrow the wrapped source.
    pub const fn inner(&self) -> &F {
        &self.inner
    }
}

impl<F: SourceFilter> Filter for Sourced<F> {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.inner.configure(ctx)
    }

    fn flush(&mut self) {
        // A source is *not* rewound: it has no way to know what the caller
        // seeked to. `done` therefore stays set, and the exhausted source
        // re-closes its output rather than restarting. Rewinding is
        // `SourceFilter`'s own business and needs a seek target, which this
        // interface does not carry.
        self.inner.flush_state();
    }

    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        // Same seek recovery as `Simple`: a flush re-opens the output, and an
        // exhausted source must either restart or re-close it. It cannot
        // restart — it has no way to know what it already produced — so it
        // re-closes, idempotently.
        if self.done {
            ctx.close_output_at(0, self.inner.end_pts());
            return Ok(Activity::Eof);
        }
        if !ctx.output_has_room(0) {
            return Ok(Activity::Blocked);
        }
        if let Some(frame) = self.inner.produce(ctx)? {
            ctx.push_output(0, frame)?;
            return Ok(Activity::Progressed);
        }
        let pts = self.inner.end_pts();
        ctx.close_output_at(0, pts);
        self.done = true;
        Ok(Activity::Eof)
    }
}

/// A filter that wants a fixed number of samples per call.
///
/// FFT-domain audio filters need exactly N samples; a resampler will happily
/// hand them 1153. The adapter runs a FIFO so the filter always sees `N`, and
/// exactly one correctly-short frame at end of stream. Forgetting this is easy
/// and retrofitting it is not, which is why it is an adapter rather than advice.
pub trait AudioFilter: Send {
    /// How many samples per call. `0` means "whatever arrives", which turns the
    /// adapter into [`Simple`] with no buffering.
    fn frame_size(&self) -> u32 {
        0
    }

    /// Transform one block.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn filter_samples(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut>;

    /// Release anything buffered at end of stream.
    ///
    /// # Errors
    /// Whatever the filter's own work reports.
    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let _ = ctx;
        Ok(FrameOut::None)
    }

    /// Called once when link formats have been agreed.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Discard buffered state after a seek.
    fn flush_state(&mut self) {}
}

/// Adapts an [`AudioFilter`] to [`FrameFilter`], installing the sample FIFO.
///
/// Composed rather than a separate `Filter` impl so that the ordering,
/// timeline and end-of-stream handling in [`Simple`] are shared rather than
/// written twice.
#[derive(Debug)]
pub struct Blocked<F> {
    inner: F,
    fifo: SampleFifo,
}

impl<F> Blocked<F> {
    /// Wrap an audio filter.
    pub const fn new(inner: F) -> Self {
        Self {
            inner,
            fifo: SampleFifo::new(),
        }
    }

    /// Borrow the wrapped filter.
    pub const fn inner(&self) -> &F {
        &self.inner
    }
}

impl<F: AudioFilter> FrameFilter for Blocked<F> {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let want = self.inner.frame_size();
        if want == 0 {
            return self.inner.filter_samples(ctx, input);
        }
        self.fifo.push(input);
        let mut out: SmallVec<[Frame; 4]> = SmallVec::new();
        while let Some(block) = self.fifo.take(want, false) {
            self.inner
                .filter_samples(ctx, block)?
                .drain_into_vec(&mut out);
        }
        Ok(FrameOut::from_iter(out))
    }

    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let want = self.inner.frame_size();
        let mut out: SmallVec<[Frame; 4]> = SmallVec::new();
        if want != 0 {
            // The one short frame at the end, carrying whatever is left.
            if let Some(block) = self.fifo.take(want, true) {
                self.inner
                    .filter_samples(ctx, block)?
                    .drain_into_vec(&mut out);
            }
        }
        self.inner.flush(ctx)?.drain_into_vec(&mut out);
        Ok(FrameOut::from_iter(out))
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.inner.configure(ctx)
    }

    fn flush_state(&mut self) {
        // Half-filled blocks from before the seek would otherwise be spliced
        // onto the frames after it.
        self.fifo = SampleFifo::new();
        self.inner.flush_state();
    }
}

impl FrameOut {
    fn drain_into_vec(self, out: &mut SmallVec<[Frame; 4]>) {
        match self {
            Self::None => {}
            Self::One(f) => out.push(f),
            Self::Many(v) => out.extend(v),
        }
    }
}

/// Re-blocks audio into fixed-size frames.
///
/// Deliberately frame-granular rather than sample-granular: it hands over a
/// whole input frame when the requested size divides it, and otherwise
/// concatenates. Sample-exact splitting needs a plane-level copy, which is
/// `vaco-resample`'s business, not the framework's — so this refuses rather than
/// guesses when a block would have to be cut mid-frame, and the audio worked
/// example uses a frame size that divides its input.
#[derive(Debug, Default)]
struct SampleFifo {
    queue: std::collections::VecDeque<Frame>,
    buffered: u32,
}

impl SampleFifo {
    const fn new() -> Self {
        Self {
            queue: std::collections::VecDeque::new(),
            buffered: 0,
        }
    }

    fn push(&mut self, frame: Frame) {
        self.buffered = self.buffered.saturating_add(samples_of(&frame));
        self.queue.push_back(frame);
    }

    /// Hand over one block of `want` samples, or — when `final_block` — whatever
    /// is left.
    fn take(&mut self, want: u32, final_block: bool) -> Option<Frame> {
        if self.queue.is_empty() {
            return None;
        }
        let head = samples_of(self.queue.front()?);
        if head == want || (final_block && head <= want) {
            let frame = self.queue.pop_front()?;
            self.buffered = self.buffered.saturating_sub(head);
            return Some(frame);
        }
        if final_block {
            let frame = self.queue.pop_front()?;
            self.buffered = self.buffered.saturating_sub(samples_of(&frame));
            return Some(frame);
        }
        None
    }
}

fn samples_of(frame: &Frame) -> u32 {
    match frame.data {
        FrameData::Audio { samples, .. } => samples,
        FrameData::Video { .. } | FrameData::Subtitle { .. } => 0,
    }
}
