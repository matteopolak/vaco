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
//!
//! Not here: `SliceFilter`, which needs a thread pool this crate does not depend
//! on, and `Synced`, which lives in `vaco-filter-framesync`. See
//! `docs/filter/vaco-filter-core.md`.

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
        FrameData::Video { .. } => 0,
    }
}
