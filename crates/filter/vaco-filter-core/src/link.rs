//! Pads, links, per-link frame queues, and the end-of-stream convention.
//!
//! A [`Link`] joins exactly one output pad to exactly one input pad (plan 16
//! §1.4 — fan-out is `split`/`asplit`, never implicit, because implicit fan-out
//! makes frame ownership ambiguous). It carries the negotiated
//! [`LinkFormat`](crate::LinkFormat), a bounded frame queue, a terminal
//! [`Status`], and the `wanted` pull signal.
//!
//! # The end-of-stream convention
//!
//! This is the rule `vaco-format-core` had to discover the hard way and which
//! its docs asked to be written down next time, so it is written down here:
//!
//! **EOF is sticky, and it is ordered behind the frames already queued.**
//!
//! Concretely, for an input pad:
//!
//! * [`Link::pop`] hands over queued frames first. It never skips one to report
//!   end of stream.
//! * [`Link::at_eof`] is `false` while any frame is still queued, becomes `true`
//!   once the producer has closed *and* the queue has drained, and then stays
//!   `true` for the rest of the link's life.
//! * The only thing that clears it is [`Link::flush`], which is a seek, not a
//!   step.
//!
//! Without the ordering rule a filter drops the tail of every stream. Without
//! the stickiness rule a filter that checks EOF twice gets two different
//! answers and reports its own trailer as an error — exactly the bug
//! `vacoraw` shipped with.

use std::collections::VecDeque;

use vaco_core::{Error, MediaType, Rational, Result, Rounding, TimeBase, Timestamp};
use vaco_frame::{Frame, FrameData};

use crate::LinkFormat;

/// Index of a node in a [`Graph`](crate::Graph).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Index of a link in a [`Graph`](crate::Graph).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkId(pub u32);

/// A frame a link refused, handed back so the caller can actually retry.
///
/// # Why this type exists
///
/// [`Link::push`] takes the frame by value, so a plain `Result<()>` failure
/// *drops* it. The documentation used to promise the opposite — "the caller
/// keeps the frame and retries" — which was impossible against the signature,
/// and `Graph::send` repeated the same promise to external callers. A
/// backpressure signal that silently eats the frame it is refusing is worse
/// than no backpressure at all: the pipeline keeps running and the output is
/// short by exactly the frames it was busiest for.
///
/// Handing the frame back in the error makes the promise true. `Error` still
/// converts from this, so a caller that genuinely wants to discard the frame
/// can keep using `?` and pay nothing.
#[derive(Debug)]
pub struct Rejected {
    /// Why it was refused: [`Error::OutputPending`] for backpressure,
    /// [`Error::Eof`] for a push after close.
    pub error: Error,
    /// The frame, unmodified. Retry with this one.
    pub frame: Frame,
}

impl From<Rejected> for Error {
    /// Discards the frame. Deliberately explicit at the call site via `?` or
    /// `.into()`, so losing it is something the code says rather than something
    /// the signature does behind your back.
    fn from(r: Rejected) -> Self {
        r.error
    }
}

impl core::fmt::Display for Rejected {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Rejected {}

/// One end of a link: a node, a direction, and a pad index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PadRef {
    pub node: NodeId,
    pub direction: Direction,
    pub pad: u32,
}

impl PadRef {
    /// An input pad of `node`.
    #[must_use]
    pub const fn input(node: NodeId, pad: u32) -> Self {
        Self {
            node,
            direction: Direction::Input,
            pad,
        }
    }

    /// An output pad of `node`.
    #[must_use]
    pub const fn output(node: NodeId, pad: u32) -> Self {
        Self {
            node,
            direction: Direction::Output,
            pad,
        }
    }
}

/// Which side of a filter a pad is on.
///
/// `Input` sorts before `Output` so that [`PadRef`]'s derived `Ord` gives a
/// stable, documented iteration order for the negotiation fold (D6: the fold is
/// order-sensitive in *preference*, so the order has to be pinned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    Input,
    Output,
}

/// The terminal condition of a link.
///
/// A link carries at most one, and it is delivered *after* every frame already
/// queued (plan 16 §1.8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The producer will send nothing further. Normal.
    Eof,
    /// The producer failed.
    ///
    /// **The failure itself does not travel down the link.** `vaco_core::Error`
    /// is not `Clone` — it carries a `std::io::Error` — and a terminal status
    /// may be observed by more than one reader, so only the *fact* propagates.
    /// The error value is returned to whoever called
    /// [`Graph::run_once`](crate::Graph::run_once). That split is reported as a
    /// gap in `docs/filter/vaco-filter-core.md` rather than worked around by
    /// stringifying the error, which would lose the variant a caller matches on.
    Failed,
}

impl Status {
    /// Whether this is the normal end of a stream.
    #[must_use]
    pub const fn is_eof(self) -> bool {
        matches!(self, Self::Eof)
    }

    /// Whether the producer failed rather than finishing.
    #[must_use]
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::Failed)
    }
}

/// Default per-link queue depth (plan 16 §1.5).
///
/// Deep enough that a filter emitting several frames from one input never trips
/// backpressure spuriously; shallow enough that a stalled sink cannot grow
/// memory without bound.
pub const DEFAULT_QUEUE_DEPTH: usize = 8;

/// Counters kept per link, for the deadlock diagnostic and for `graphmonitor`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Frames that have passed through, counted on `pop`.
    pub frames: u64,
    /// Audio samples that have passed through, counted on `pop`.
    pub samples: u64,
    /// The deepest the queue has ever been.
    pub peak_depth: usize,
    /// How many times a push was refused for want of room.
    pub blocked: u64,
}

/// One connection between an output pad and an input pad.
///
/// Everything a filter can observe about its neighbours lives here; a filter
/// never reaches another filter's state (plan 16 §1.1).
#[derive(Debug)]
pub struct Link {
    src: PadRef,
    dst: PadRef,
    media: MediaType,
    format: LinkFormat,
    configured: bool,
    queue: VecDeque<Frame>,
    capacity: usize,
    /// Set by the producer, consumed by [`Link::pop_status`]. Ordered behind
    /// `queue`.
    status: Option<Status>,
    /// Latched once `status` is set *and* `queue` has drained. Sticky.
    eof_seen: bool,
    /// The timestamp the producer reported the stream ending at, in this link's
    /// time base. `tpad`, `xfade` and `concat` need it.
    end_pts: Timestamp,
    /// The consumer has asked for a frame.
    wanted: bool,
    stats: LinkStats,
    /// Bumped by every observable mutation. The scheduler compares it across an
    /// `activate` call to decide whether the filter really made progress, which
    /// is what makes readiness *computed* rather than *asserted* (plan 16 §1.2).
    epoch: u64,
}

impl Link {
    /// A fresh, unconfigured link between two pads.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the two pads carry different media types.
    /// Matching the reference, this is a *link-time* error, diagnosed before
    /// format negotiation ever runs.
    pub fn new(
        src: PadRef,
        dst: PadRef,
        src_media: MediaType,
        dst_media: MediaType,
    ) -> Result<Self> {
        if src_media != dst_media {
            return Err(Error::InvalidData(
                "media type mismatch between the output pad and the input pad it is linked to",
            ));
        }
        Ok(Self {
            src,
            dst,
            media: src_media,
            format: LinkFormat::unconfigured(src_media),
            configured: false,
            queue: VecDeque::new(),
            capacity: DEFAULT_QUEUE_DEPTH,
            status: None,
            eof_seen: false,
            end_pts: Timestamp::NONE,
            wanted: false,
            stats: LinkStats::default(),
            epoch: 0,
        })
    }

    /// Override the queue depth. Clamped to at least one: a link that can hold
    /// no frame could never make progress.
    #[must_use]
    pub const fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = if capacity == 0 { 1 } else { capacity };
        self
    }

    /// The producing pad.
    #[must_use]
    pub const fn src(&self) -> PadRef {
        self.src
    }

    /// The consuming pad.
    #[must_use]
    pub const fn dst(&self) -> PadRef {
        self.dst
    }

    /// What flows over this link.
    #[must_use]
    pub const fn media(&self) -> MediaType {
        self.media
    }

    /// The negotiated format. Meaningful once [`Link::is_configured`].
    #[must_use]
    pub const fn format(&self) -> &LinkFormat {
        &self.format
    }

    /// Install the negotiated format. Called by the configure pass.
    pub fn set_format(&mut self, format: LinkFormat) {
        self.format = format;
        self.configured = true;
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Whether the configure pass has run for this link.
    #[must_use]
    pub const fn is_configured(&self) -> bool {
        self.configured
    }

    /// This link's time base, or [`Rational::UNDEFINED`] before configuration.
    #[must_use]
    pub const fn time_base(&self) -> TimeBase {
        self.format.time_base()
    }

    /// Frames queued but not yet taken.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.queue.len()
    }

    /// Whether a further [`Link::push`] would be refused.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.queue.len() >= self.capacity
    }

    /// The configured queue depth.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Traffic counters.
    #[must_use]
    pub const fn stats(&self) -> LinkStats {
        self.stats
    }

    /// The observable-state counter. Two equal readings mean nothing a filter
    /// or the scheduler can see has changed.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether the consumer has asked for a frame.
    #[must_use]
    pub const fn is_wanted(&self) -> bool {
        self.wanted
    }

    /// Ask the producer for a frame. The pull half of backpressure.
    pub fn request(&mut self) {
        if !self.wanted {
            self.wanted = true;
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    /// Whether the producer has declared this link terminal.
    ///
    /// Distinct from [`Link::at_eof`]: this is true the moment the producer
    /// closes, while `at_eof` waits for the queue to drain.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.status.is_some()
    }

    /// **Sticky.** Whether the producer has closed *and* every queued frame has
    /// been taken.
    ///
    /// Once true, stays true until [`Link::flush`]. See the module docs for why
    /// that matters.
    #[must_use]
    pub const fn at_eof(&self) -> bool {
        self.eof_seen
    }

    /// The timestamp the producer reported the stream ending at, in this link's
    /// time base, or [`Timestamp::NONE`].
    #[must_use]
    pub const fn end_pts(&self) -> Timestamp {
        self.end_pts
    }

    /// The terminal status, if the producer has set one. Does not consume it.
    #[must_use]
    pub const fn status(&self) -> Option<Status> {
        self.status
    }

    /// Enqueue a frame, rescaling its timestamps into this link's time base.
    ///
    /// # Errors
    ///
    /// [`Error::OutputPending`] when the queue is at capacity — pure
    /// backpressure; the frame comes back in [`Rejected::frame`] and the caller
    /// retries with it.
    /// [`Error::Eof`] when the link has already been closed: pushing after a
    /// terminal status is a defect in the producer, and losing the frame
    /// silently would be worse than refusing it.
    ///
    /// Both failures hand the frame back **unmodified** — timestamps are
    /// rebased only on the success path, so a retry after the queue drains
    /// rescales exactly once.
    #[allow(
        clippy::result_large_err,
        reason = "the large Err *is* the feature: it carries the refused Frame back. \
                  Boxing it would trade a 392-byte return slot for a heap \
                  allocation on the backpressure path, which is the path that \
                  repeats — a saturated pipeline refuses constantly. And the \
                  size is not meaningful next to what it guards: a Frame's \
                  struct is 392 bytes, its pixel data is tens of kilobytes, and \
                  push already takes one by value on every call."
    )]
    pub fn push(&mut self, mut frame: Frame) -> core::result::Result<(), Rejected> {
        if self.status.is_some() {
            return Err(Rejected {
                error: Error::Eof,
                frame,
            });
        }
        if self.queue.len() >= self.capacity {
            self.stats.blocked = self.stats.blocked.saturating_add(1);
            return Err(Rejected {
                error: Error::OutputPending,
                frame,
            });
        }
        rebase_frame(&mut frame, self.format.time_base());
        self.queue.push_back(frame);
        self.stats.peak_depth = self.stats.peak_depth.max(self.queue.len());
        self.wanted = false;
        self.epoch = self.epoch.wrapping_add(1);
        Ok(())
    }

    /// Look at the next frame without taking it.
    #[must_use]
    pub fn peek(&self) -> Option<&Frame> {
        self.queue.front()
    }

    /// Take the next frame.
    ///
    /// Latches end of stream when this empties a queue whose producer has
    /// already closed, which is what keeps the status ordered behind the data.
    pub fn pop(&mut self) -> Option<Frame> {
        let frame = self.queue.pop_front()?;
        self.stats.frames = self.stats.frames.saturating_add(1);
        if let FrameData::Audio { samples, .. } = frame.data {
            self.stats.samples = self.stats.samples.saturating_add(u64::from(samples));
        }
        self.latch_eof();
        self.epoch = self.epoch.wrapping_add(1);
        Some(frame)
    }

    /// Take the terminal status, if the queue has drained and one is present.
    ///
    /// Returns `None` while frames remain: the status is always behind them.
    /// Unlike [`Link::at_eof`] this consumes, so a filter that needs to act on
    /// end of stream exactly once can use it as the trigger.
    pub fn pop_status(&mut self) -> Option<Status> {
        if !self.queue.is_empty() {
            return None;
        }
        let status = self.status?;
        self.latch_eof();
        self.epoch = self.epoch.wrapping_add(1);
        Some(status)
    }

    /// Declare the link terminal at `pts`, expressed in this link's time base.
    ///
    /// Idempotent: closing an already-closed link keeps the first status and
    /// the first timestamp. Forwarding helpers legitimately close the same pad
    /// more than once, so making the second call an error would push
    /// bookkeeping onto every filter for no benefit.
    pub fn close(&mut self, status: Status, pts: Timestamp) {
        if self.status.is_some() {
            return;
        }
        self.status = Some(status);
        self.end_pts = pts;
        self.latch_eof();
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Discard queued frames and the terminal status, returning to the state a
    /// fresh link is in. The negotiated format survives, because a seek does
    /// not renegotiate.
    pub fn flush(&mut self) {
        self.queue.clear();
        self.status = None;
        self.eof_seen = false;
        self.end_pts = Timestamp::NONE;
        self.wanted = false;
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn latch_eof(&mut self) {
        if self.status.is_some() && self.queue.is_empty() {
            self.eof_seen = true;
        }
    }
}

/// Rescale a frame's timestamps into `target`, exactly.
///
/// A no-op when the frame is already in the target base or when either base is
/// unusable — never a silent approximation. `Rounding::NearestAwayFromZero` matches the
/// reference's `av_rescale_q` default (plan 16 §1.8.3).
pub(crate) fn rebase_frame(frame: &mut Frame, target: TimeBase) {
    let from = frame.time_base;
    if !target.is_defined() || !from.is_defined() || from == target {
        return;
    }
    frame.pts = frame
        .pts
        .rescale(from, target, Rounding::NearestAwayFromZero);
    frame.time_base = target;
}

/// Rescale one timestamp between two link time bases, exactly.
///
/// Used when a terminal status crosses a node whose input and output bases
/// differ: the end-of-stream timestamp has to move with it or `tpad` pads to the
/// wrong length.
#[must_use]
pub fn rescale_pts(pts: Timestamp, from: TimeBase, to: TimeBase) -> Timestamp {
    if !from.is_defined() || !to.is_defined() || from == to {
        return pts;
    }
    pts.rescale(from, to, Rounding::NearestAwayFromZero)
}

/// The links of one graph, addressed by [`LinkId`].
///
/// Kept in its own arena so that the driver can hold `&mut` to one node's
/// filter and `&mut` to every link at the same time — ordinary disjoint-field
/// borrowing, no `RefCell`, no `unsafe` (plan 16 §1.1).
#[derive(Debug)]
pub struct LinkArena {
    links: Vec<Link>,
    /// Returned by [`LinkArena::format`] for a pad that has no link. A filter
    /// distinguishes it by its zero dimensions / zero channel count. Built once
    /// rather than lazily so that the accessor can take `&self`, which
    /// [`crate::FilterContext::link`]'s frozen signature requires.
    unset: LinkFormat,
}

impl Default for LinkArena {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkArena {
    /// An empty arena.
    #[must_use]
    pub fn new() -> Self {
        Self {
            links: Vec::new(),
            unset: LinkFormat::unconfigured(MediaType::Video),
        }
    }

    /// Add a link, returning its id.
    pub fn push(&mut self, link: Link) -> LinkId {
        let id = LinkId(self.links.len() as u32);
        self.links.push(link);
        id
    }

    /// How many links the arena holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Whether the arena is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Borrow one link.
    #[must_use]
    pub fn get(&self, id: LinkId) -> Option<&Link> {
        self.links.get(id.0 as usize)
    }

    /// Borrow one link mutably.
    pub fn get_mut(&mut self, id: LinkId) -> Option<&mut Link> {
        self.links.get_mut(id.0 as usize)
    }

    /// Every link, in id order.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Link> {
        self.links.iter()
    }

    /// Every link with its id, in id order.
    pub fn iter_ids(&self) -> impl Iterator<Item = (LinkId, &Link)> {
        self.links
            .iter()
            .enumerate()
            .map(|(i, l)| (LinkId(i as u32), l))
    }

    /// Every link mutably, in id order.
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = &mut Link> {
        self.links.iter_mut()
    }

    /// The format of one link, or a zero-sized sentinel when the id is absent.
    ///
    /// Total by construction because [`crate::FilterContext::link`] is frozen
    /// as returning `&LinkFormat` rather than `Option<&LinkFormat>`; see the
    /// signature gaps section of `docs/filter/vaco-filter-core.md`.
    #[must_use]
    pub fn format(&self, id: Option<LinkId>) -> &LinkFormat {
        id.and_then(|i| self.links.get(i.0 as usize))
            .map_or(&self.unset, Link::format)
    }

    /// Sum of every link's epoch. The scheduler snapshots this around an
    /// `activate` call: an unchanged sum means the step was observably a no-op.
    #[must_use]
    pub fn epoch_sum(&self) -> u64 {
        self.links
            .iter()
            .fold(0u64, |acc, l| acc.wrapping_add(l.epoch()))
    }
}

impl LinkFormat {
    /// A placeholder for a link that has not been configured yet.
    ///
    /// Recognisable by its zero dimensions (video) or zero channel count
    /// (audio), and by an undefined time base.
    #[must_use]
    pub fn unconfigured(media: MediaType) -> Self {
        if media == MediaType::Audio {
            Self::Audio {
                format: vaco_sampfmt::SampleFmt::S16,
                sample_rate: 0,
                layout: vaco_chlayout::ChannelLayout::unspecified(0),
                time_base: Rational::UNDEFINED,
            }
        } else {
            Self::Video {
                format: vaco_pixfmt::PixFmt::Yuv420p,
                width: 0,
                height: 0,
                time_base: Rational::UNDEFINED,
                frame_rate: Rational::UNDEFINED,
                sample_aspect_ratio: Rational::ONE,
                color: vaco_color::ColorInfo::default(),
            }
        }
    }

    /// The media type this format describes.
    #[must_use]
    pub const fn media_type(&self) -> MediaType {
        match self {
            Self::Video { .. } => MediaType::Video,
            Self::Audio { .. } => MediaType::Audio,
        }
    }

    /// The link's time base.
    #[must_use]
    pub const fn time_base(&self) -> TimeBase {
        match self {
            Self::Video { time_base, .. } | Self::Audio { time_base, .. } => *time_base,
        }
    }

    /// Replace the link's time base.
    pub const fn set_time_base(&mut self, tb: TimeBase) {
        match self {
            Self::Video { time_base, .. } | Self::Audio { time_base, .. } => *time_base = tb,
        }
    }

    /// Whether the format names a usable configuration: non-zero dimensions for
    /// video, a non-zero channel count and sample rate for audio, and a defined,
    /// non-zero time base for both.
    ///
    /// This is the post-condition of the configure pass (plan 16 §1.6.3 step 6).
    #[must_use]
    pub fn is_usable(&self) -> bool {
        let tb = self.time_base();
        if !tb.is_defined() || tb.is_zero() {
            return false;
        }
        match self {
            Self::Video { width, height, .. } => *width != 0 && *height != 0,
            Self::Audio {
                sample_rate,
                layout,
                ..
            } => *sample_rate != 0 && layout.channels != 0,
        }
    }

    /// Whether a frame's own description agrees with this link.
    ///
    /// The framework checks this on every push in debug builds: a filter that
    /// emits a frame whose format does not match the link it is pushed to has
    /// broken negotiation, and finding that at the push is far cheaper than
    /// finding it at the sink.
    #[must_use]
    pub fn accepts(&self, frame: &Frame) -> bool {
        match (self, &frame.data) {
            (
                Self::Video {
                    format,
                    width,
                    height,
                    ..
                },
                FrameData::Video {
                    format: ff,
                    width: fw,
                    height: fh,
                    ..
                },
            ) => format == ff && width == fw && height == fh,
            (
                Self::Audio {
                    format,
                    sample_rate,
                    layout,
                    ..
                },
                FrameData::Audio {
                    format: ff,
                    sample_rate: fr,
                    layout: fl,
                    ..
                },
            ) => format == ff && sample_rate == fr && layout == fl,
            _ => false,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_wrap,
    clippy::match_wildcard_for_single_variants,
    clippy::items_after_statements,
    clippy::single_match_else,
    clippy::option_if_let_else,
    clippy::too_many_lines,
    clippy::field_reassign_with_default,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::test_support::{audio_frame, video_frame, video_link_format};

    fn link() -> Link {
        Link::new(
            PadRef::output(NodeId(0), 0),
            PadRef::input(NodeId(1), 0),
            MediaType::Video,
            MediaType::Video,
        )
        .expect("same media type")
    }

    #[test]
    fn media_type_mismatch_is_a_link_time_error() {
        let e = Link::new(
            PadRef::output(NodeId(0), 0),
            PadRef::input(NodeId(1), 0),
            MediaType::Video,
            MediaType::Audio,
        );
        assert!(matches!(e, Err(Error::InvalidData(_))));
    }

    #[test]
    fn a_refused_push_hands_the_frame_back() {
        let mut l = link().with_capacity(1);
        l.set_format(video_link_format(16, 16));
        l.push(video_frame(16, 16, 0)).expect("room");

        // Full: the frame must come back, not vanish.
        let Err(rejected) = l.push(video_frame(16, 16, 1)) else {
            panic!("a full link must refuse");
        };
        assert!(matches!(rejected.error, Error::OutputPending));
        assert_eq!(rejected.frame.pts, Timestamp::new(1));

        // Drain, then retry with the very frame we got back. This is the whole
        // point: before `Rejected` existed the docs promised this and the
        // signature made it impossible.
        assert!(l.pop().is_some());
        l.push(rejected.frame).expect("room after draining");
        assert_eq!(l.pop().map(|f| f.pts), Some(Timestamp::new(1)));
    }

    #[test]
    fn a_push_after_close_also_hands_the_frame_back() {
        let mut l = link();
        l.set_format(video_link_format(16, 16));
        l.close(Status::Eof, Timestamp::new(0));
        let Err(rejected) = l.push(video_frame(16, 16, 7)) else {
            panic!("a closed link must refuse");
        };
        assert!(matches!(rejected.error, Error::Eof));
        // Recovering it is not useful here — the link is finished — but a
        // caller routing to several links can still send it elsewhere, and
        // dropping it silently is what we are trying to stop.
        assert_eq!(rejected.frame.pts, Timestamp::new(7));
    }

    #[test]
    fn eof_is_ordered_behind_the_queue() {
        let mut l = link();
        l.set_format(video_link_format(16, 16));
        l.push(video_frame(16, 16, 0)).expect("room");
        l.close(Status::Eof, Timestamp::new(1));
        // Closed, but a frame is still in flight.
        assert!(l.is_closed());
        assert!(!l.at_eof());
        assert!(l.pop_status().is_none());
        assert!(l.pop().is_some());
        // Now, and only now.
        assert!(l.at_eof());
        assert_eq!(l.pop_status(), Some(Status::Eof));
    }

    #[test]
    fn eof_is_sticky() {
        let mut l = link();
        l.set_format(video_link_format(16, 16));
        l.close(Status::Eof, Timestamp::ZERO);
        assert!(l.at_eof());
        assert_eq!(l.pop_status(), Some(Status::Eof));
        // Consuming the status must not un-set the latch, or a filter that asks
        // twice gets two different answers.
        assert!(l.at_eof());
        assert!(l.at_eof());
    }

    #[test]
    fn close_is_idempotent_and_keeps_the_first_status() {
        let mut l = link();
        l.set_format(video_link_format(16, 16));
        l.close(Status::Eof, Timestamp::new(7));
        l.close(Status::Failed, Timestamp::new(9));
        assert_eq!(l.status(), Some(Status::Eof));
        assert_eq!(l.end_pts(), Timestamp::new(7));
    }

    #[test]
    fn push_after_close_is_refused_not_dropped() {
        let mut l = link();
        l.set_format(video_link_format(16, 16));
        l.close(Status::Eof, Timestamp::ZERO);
        assert!(matches!(
            l.push(video_frame(16, 16, 0)),
            Err(Rejected {
                error: Error::Eof,
                ..
            })
        ));
    }

    #[test]
    fn backpressure_is_output_pending() {
        let mut l = link().with_capacity(2);
        l.set_format(video_link_format(16, 16));
        l.push(video_frame(16, 16, 0)).expect("room");
        l.push(video_frame(16, 16, 1)).expect("room");
        assert!(matches!(
            l.push(video_frame(16, 16, 2)),
            Err(Rejected {
                error: Error::OutputPending,
                ..
            })
        ));
        assert_eq!(l.stats().blocked, 1);
        assert!(l.pop().is_some());
        l.push(video_frame(16, 16, 2)).expect("room again");
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let l = link().with_capacity(0);
        assert_eq!(l.capacity(), 1);
    }

    #[test]
    fn push_rescales_into_the_link_time_base() {
        let mut l = link();
        let mut fmt = video_link_format(16, 16);
        fmt.set_time_base(Rational::new(1, 1000));
        l.set_format(fmt);
        let mut f = video_frame(16, 16, 0);
        f.time_base = Rational::new(1, 25);
        f.pts = Timestamp::new(3); // 0.12 s
        f.set_duration_ticks(1);
        l.push(f).expect("room");
        let out = l.pop().expect("frame");
        assert_eq!(out.time_base, Rational::new(1, 1000));
        assert_eq!(out.pts, Timestamp::new(120));
        assert_eq!(out.duration_ticks(), 40);
    }

    #[test]
    fn rebasing_keeps_duration_exact_when_the_target_clock_is_coarser() {
        let mut frame = video_frame(16, 16, 0);
        frame.time_base = Rational::new(1, 30_000);
        frame.set_duration_ticks(1001);
        let exact = frame.duration;
        rebase_frame(&mut frame, Rational::new(1, 1000));
        assert_eq!(frame.duration, exact);
        assert_eq!(frame.duration_ticks(), 33);
        rebase_frame(&mut frame, Rational::new(1, 30_000));
        assert_eq!(frame.duration_ticks(), 1001);
    }

    #[test]
    fn flush_restores_a_fresh_link() {
        let mut l = link();
        l.set_format(video_link_format(16, 16));
        l.push(video_frame(16, 16, 0)).expect("room");
        l.close(Status::Eof, Timestamp::ZERO);
        l.flush();
        assert_eq!(l.depth(), 0);
        assert!(!l.at_eof());
        assert!(!l.is_closed());
        assert!(l.is_configured(), "a seek does not renegotiate");
    }

    #[test]
    fn wanted_is_cleared_by_a_push() {
        let mut l = link();
        l.set_format(video_link_format(16, 16));
        l.request();
        assert!(l.is_wanted());
        l.push(video_frame(16, 16, 0)).expect("room");
        assert!(!l.is_wanted());
    }

    #[test]
    fn stats_count_audio_samples() {
        let mut l = Link::new(
            PadRef::output(NodeId(0), 0),
            PadRef::input(NodeId(1), 0),
            MediaType::Audio,
            MediaType::Audio,
        )
        .expect("audio link");
        l.set_format(LinkFormat::Audio {
            format: vaco_sampfmt::SampleFmt::S16,
            sample_rate: 48_000,
            layout: vaco_chlayout::ChannelLayout::STEREO,
            time_base: Rational::new(1, 48_000),
        });
        l.push(audio_frame(1024, 0)).expect("room");
        let _ = l.pop();
        assert_eq!(l.stats().samples, 1024);
        assert_eq!(l.stats().frames, 1);
    }

    #[test]
    fn unconfigured_formats_are_not_usable() {
        assert!(!LinkFormat::unconfigured(MediaType::Video).is_usable());
        assert!(!LinkFormat::unconfigured(MediaType::Audio).is_usable());
        assert!(video_link_format(16, 16).is_usable());
    }

    #[test]
    fn accepts_rejects_a_mismatched_frame() {
        let fmt = video_link_format(16, 16);
        assert!(fmt.accepts(&video_frame(16, 16, 0)));
        assert!(!fmt.accepts(&video_frame(32, 16, 0)));
        assert!(!fmt.accepts(&audio_frame(64, 0)));
    }

    #[test]
    fn rescale_pts_is_exact_and_a_noop_for_equal_bases() {
        let a = Rational::new(1, 25);
        let b = Rational::new(1, 90_000);
        assert_eq!(rescale_pts(Timestamp::new(5), a, a), Timestamp::new(5));
        assert_eq!(rescale_pts(Timestamp::new(1), a, b), Timestamp::new(3600));
        assert_eq!(
            rescale_pts(Timestamp::NONE, a, b),
            Timestamp::NONE,
            "absence survives rescaling"
        );
        assert_eq!(
            rescale_pts(Timestamp::new(5), Rational::UNDEFINED, b),
            Timestamp::new(5),
            "an unusable base leaves the value alone rather than inventing one"
        );
    }
}
