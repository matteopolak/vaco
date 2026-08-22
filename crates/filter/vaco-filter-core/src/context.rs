//! What a filter can see and do during one `activate` call.
//!
//! [`FilterContext`](crate::FilterContext) is the *only* thing a filter is
//! handed. It reaches link state and nothing else — never another filter's
//! private state — which is the invariant the reference maintains by convention
//! and that the arena split makes structural here.
//!
//! # The frame-flow contract
//!
//! Nine rules. Six of them are checked by the scheduler and reported as a
//! [`Violation`](crate::Violation); the rest are checked here.
//!
//! | Rule | Statement |
//! |---|---|
//! | **F1** | [`take_input`](crate::FilterContext::take_input) hands over queued frames in order, and `None` when none is queued. It never skips a frame to report end of stream. |
//! | **F2** | [`input_at_eof`](crate::FilterContext::input_at_eof) is **sticky** and **ordered behind the queue**: false while frames remain, true once the producer closed *and* the queue drained, and true forever after. |
//! | **F3** | Pushing to a closed output pad is a defect. The frame is refused, not dropped. |
//! | **F4** | [`close_output`](crate::FilterContext::close_output) is idempotent. |
//! | **F5** | [`Activity::Eof`](crate::Activity::Eof) may be returned only when every output pad is closed, and the filter is not run again afterwards. |
//! | **F6** | [`Activity::Progressed`](crate::Activity::Progressed) requires that something observable changed: a frame taken, a frame pushed, or a pad closed. |
//! | **F7** | [`Activity::NeedInput`](crate::Activity::NeedInput) requires that at least one input is not yet at end of stream. |
//! | **F8** | [`Activity::Blocked`](crate::Activity::Blocked) means an output is full or unwanted. The filter keeps whatever it was holding. |
//! | **F9** | A pushed frame's timestamps are interpreted in **the output link's** time base, and the framework rescales exactly if the frame says otherwise. The frame's format must match the link's negotiated format. |
//!
//! F2 is the one that costs the most when it is missing. `vaco-format-core`
//! found the same rule the hard way on the demuxer side and its docs asked for
//! it to be stated next time; this is that.

use vaco_core::{Result, Timestamp};
use vaco_frame::{Frame, FramePool};

use crate::link::{LinkArena, LinkId, Status};
use crate::{FilterContext, LinkFormat};

/// Which link each of a node's pads is attached to.
///
/// Cloned into the driver for the duration of an `activate` call, so that the
/// node arena is free while the filter holds `&mut` on the link arena.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeLinks {
    inputs: Vec<Option<LinkId>>,
    outputs: Vec<Option<LinkId>>,
}

impl NodeLinks {
    /// Room for `inputs` input pads and `outputs` output pads, none connected.
    #[must_use]
    pub fn new(inputs: usize, outputs: usize) -> Self {
        Self {
            inputs: vec![None; inputs],
            outputs: vec![None; outputs],
        }
    }

    /// The link on input pad `pad`.
    #[must_use]
    pub fn input(&self, pad: u32) -> Option<LinkId> {
        self.inputs.get(pad as usize).copied().flatten()
    }

    /// The link on output pad `pad`.
    #[must_use]
    pub fn output(&self, pad: u32) -> Option<LinkId> {
        self.outputs.get(pad as usize).copied().flatten()
    }

    /// Attach input pad `pad`.
    pub fn set_input(&mut self, pad: u32, link: LinkId) {
        if let Some(slot) = self.inputs.get_mut(pad as usize) {
            *slot = Some(link);
        }
    }

    /// Attach output pad `pad`.
    pub fn set_output(&mut self, pad: u32, link: LinkId) {
        if let Some(slot) = self.outputs.get_mut(pad as usize) {
            *slot = Some(link);
        }
    }

    /// Every input pad's link, by pad index.
    #[must_use]
    pub fn inputs(&self) -> &[Option<LinkId>] {
        &self.inputs
    }

    /// Every output pad's link, by pad index.
    #[must_use]
    pub fn outputs(&self) -> &[Option<LinkId>] {
        &self.outputs
    }
}

impl<'a> FilterContext<'a> {
    /// Build a context for one `activate` call.
    pub(crate) fn new(links: &'a mut LinkArena, node: &'a NodeLinks, pool: &'a FramePool) -> Self {
        Self {
            links,
            node,
            pool,
            format_mismatch: false,
            push_after_close: false,
            dropped_by_backpressure: false,
        }
    }

    pub(crate) const fn saw_format_mismatch(&self) -> bool {
        self.format_mismatch
    }

    pub(crate) const fn saw_push_after_close(&self) -> bool {
        self.push_after_close
    }

    pub(crate) const fn saw_dropped_by_backpressure(&self) -> bool {
        self.dropped_by_backpressure
    }

    /// How many input pads this filter has.
    #[must_use]
    pub fn input_count(&self) -> usize {
        self.node.inputs().len()
    }

    /// How many output pads this filter has.
    #[must_use]
    pub fn output_count(&self) -> usize {
        self.node.outputs().len()
    }

    /// The pool to allocate output frames from.
    ///
    /// Use it rather than [`Frame::alloc_video`](vaco_frame::Frame::alloc_video):
    /// a pooled plane comes off a free list rather than the allocator, and goes
    /// back on the last `Arc` drop, which is what makes steady-state filtering
    /// allocation-free.
    ///
    /// A recycled plane holds the previous frame's bytes. Overwrite what you
    /// care about; the pool exists precisely to avoid paying for a memset that
    /// almost every filter would immediately overwrite.
    #[must_use]
    pub const fn pool(&self) -> &FramePool {
        self.pool
    }

    /// The negotiated format of input pad `pad`.
    #[must_use]
    pub fn input_link(&self, pad: usize) -> Option<&LinkFormat> {
        let id = self.node.input(u32::try_from(pad).ok()?)?;
        self.links.get(id).map(crate::link::Link::format)
    }

    /// The negotiated format of output pad `pad`.
    #[must_use]
    pub fn output_link(&self, pad: usize) -> Option<&LinkFormat> {
        let id = self.node.output(u32::try_from(pad).ok()?)?;
        self.links.get(id).map(crate::link::Link::format)
    }

    /// Replace the format of output pad `pad`.
    ///
    /// This is how a filter that changes geometry or timing declares it, from
    /// [`Filter::configure`](crate::Filter::configure): `scale` sets the
    /// dimensions, `fps` sets the time base and frame rate. Calling it outside
    /// `configure` is not checked but is a mistake — downstream has already been
    /// configured against the old value.
    pub fn set_output_link(&mut self, pad: usize, format: LinkFormat) {
        let Some(id) = u32::try_from(pad).ok().and_then(|p| self.node.output(p)) else {
            return;
        };
        if let Some(link) = self.links.get_mut(id) {
            link.set_format(format);
        }
    }

    /// Look at the next frame on input pad `pad` without taking it.
    ///
    /// What a filter that has to decide *whether* to consume — every framesync
    /// filter — needs, and what a `take` + `put back` API could not give without
    /// making the queue order a filter's responsibility.
    #[must_use]
    pub fn peek_input(&self, pad: usize) -> Option<&Frame> {
        let id = self.node.input(u32::try_from(pad).ok()?)?;
        self.links.get(id).and_then(crate::link::Link::peek)
    }

    /// How many frames are queued on input pad `pad`.
    #[must_use]
    pub fn input_depth(&self, pad: usize) -> usize {
        u32::try_from(pad)
            .ok()
            .and_then(|p| self.node.input(p))
            .and_then(|id| self.links.get(id))
            .map_or(0, crate::link::Link::depth)
    }

    /// Take the terminal status of input pad `pad`, once its queue has drained.
    ///
    /// Consumes, so a filter that must act on end of stream exactly once — to
    /// flush an internal buffer, say — can use it as the trigger. Ordered behind
    /// the frames, so this is `None` while any remain (rule F2).
    pub fn take_input_status(&mut self, pad: usize) -> Option<Status> {
        let id = u32::try_from(pad).ok().and_then(|p| self.node.input(p))?;
        self.links
            .get_mut(id)
            .and_then(crate::link::Link::pop_status)
    }

    /// The timestamp the producer said input pad `pad` ended at, in that link's
    /// time base.
    ///
    /// `tpad`, `xfade` and `concat` need it, and it is why end of stream is a
    /// value rather than a flag.
    #[must_use]
    pub fn input_end_pts(&self, pad: usize) -> Timestamp {
        u32::try_from(pad)
            .ok()
            .and_then(|p| self.node.input(p))
            .and_then(|id| self.links.get(id))
            .map_or(Timestamp::NONE, crate::link::Link::end_pts)
    }

    /// Ask the producer on input pad `pad` for a frame.
    ///
    /// The pull half of backpressure. A filter that returns
    /// [`Activity::NeedInput`](crate::Activity::NeedInput) gets this for free on
    /// every input; call it directly only to request one input in particular.
    pub fn request_input(&mut self, pad: usize) {
        let Some(id) = u32::try_from(pad).ok().and_then(|p| self.node.input(p)) else {
            return;
        };
        if let Some(link) = self.links.get_mut(id) {
            link.request();
        }
    }

    /// Whether the consumer on output pad `pad` has asked for a frame.
    #[must_use]
    pub fn output_wanted(&self, pad: usize) -> bool {
        u32::try_from(pad)
            .ok()
            .and_then(|p| self.node.output(p))
            .and_then(|id| self.links.get(id))
            .is_some_and(crate::link::Link::is_wanted)
    }

    /// Whether output pad `pad` can take a frame right now.
    #[must_use]
    pub fn output_has_room(&self, pad: usize) -> bool {
        u32::try_from(pad)
            .ok()
            .and_then(|p| self.node.output(p))
            .and_then(|id| self.links.get(id))
            .is_some_and(|l| !l.is_full() && !l.is_closed())
    }

    /// Whether output pad `pad` has been closed.
    #[must_use]
    pub fn output_closed(&self, pad: usize) -> bool {
        u32::try_from(pad)
            .ok()
            .and_then(|p| self.node.output(p))
            .and_then(|id| self.links.get(id))
            .is_some_and(crate::link::Link::is_closed)
    }

    /// Close output pad `pad`, recording the timestamp the stream ended at.
    ///
    /// `pts` is in that output link's time base. The frozen
    /// [`close_output`](crate::FilterContext::close_output) is this with
    /// [`Timestamp::NONE`].
    pub fn close_output_at(&mut self, pad: usize, pts: Timestamp) {
        let Some(id) = u32::try_from(pad).ok().and_then(|p| self.node.output(p)) else {
            return;
        };
        if let Some(link) = self.links.get_mut(id) {
            link.close(Status::Eof, pts);
        }
    }

    /// Close every output pad, carrying the end timestamp across from the first
    /// closed input and rescaling it into each output's time base.
    ///
    /// The `forward_status_all` of plan 16 §1.8.1, and what almost every filter
    /// wants to do at end of stream.
    pub fn close_all_outputs(&mut self) {
        let (pts, base) = (0..self.input_count())
            .find_map(|p| {
                let id = u32::try_from(p).ok().and_then(|p| self.node.input(p))?;
                let link = self.links.get(id)?;
                link.is_closed().then(|| (link.end_pts(), link.time_base()))
            })
            .unwrap_or((Timestamp::NONE, vaco_core::Rational::UNDEFINED));
        for pad in 0..self.output_count() {
            let Some(id) = u32::try_from(pad).ok().and_then(|p| self.node.output(p)) else {
                continue;
            };
            let Some(link) = self.links.get_mut(id) else {
                continue;
            };
            let target = crate::link::rescale_pts(pts, base, link.time_base());
            link.close(Status::Eof, target);
        }
    }

    /// Ask every input for a frame if any output is wanted.
    ///
    /// The `forward_wanted_all` of plan 16 §1.8.1.
    pub fn forward_wanted(&mut self) {
        let wanted = (0..self.output_count()).any(|p| self.output_wanted(p));
        if !wanted {
            return;
        }
        for pad in 0..self.input_count() {
            self.request_input(pad);
        }
    }

    /// Whether every input pad has reached end of stream and drained.
    ///
    /// `true` for a filter with no inputs, which is what makes a source's
    /// "am I done?" check read the same as everyone else's.
    #[must_use]
    pub fn all_inputs_at_eof(&self) -> bool {
        (0..self.input_count()).all(|p| self.input_at_eof(p))
    }

    pub(crate) fn push_checked(&mut self, pad: usize, frame: Frame) -> Result<()> {
        let Some(id) = u32::try_from(pad).ok().and_then(|p| self.node.output(p)) else {
            return Err(vaco_core::Error::InvalidData(
                "filter pushed to an output pad it does not have",
            ));
        };
        let Some(link) = self.links.get_mut(id) else {
            return Err(vaco_core::Error::InvalidData(
                "filter pushed to an unconnected output pad",
            ));
        };
        if !link.format().accepts(&frame) {
            self.format_mismatch = true;
        }
        // The frame is consumed either way — `push` takes it by value and the
        // error path drops it. Both losses are recorded so neither is silent;
        // see `Violation::FrameDroppedByBackpressure` for why the backpressure
        // one is reachable at all.
        match link.push(frame) {
            Ok(()) => Ok(()),
            Err(r) => {
                match r.error {
                    vaco_core::Error::Eof => self.push_after_close = true,
                    vaco_core::Error::OutputPending => self.dropped_by_backpressure = true,
                    _ => {}
                }
                Err(r.error)
            }
        }
    }
}
