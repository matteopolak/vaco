//! Bounded queues, and the two things that make them a scheduler rather than a
//! `while` loop: what travels on them, and what happens when one is full.
//!
//! # Why the bound is two numbers and not one
//!
//! Plan 12 §7.1 sizes a link as `depth = clamp(target_bytes / frame_bytes, 2,
//! 64)`. That formula needs `frame_bytes` at build time, which nobody has: the
//! first frame's size is not known until a decoder has produced one, and an
//! `fps` filter can change it mid-stream. [`Capacity`] therefore carries *both*
//! bounds and lets whichever binds first do the binding. For 4K frames the byte
//! cap bites at a depth of three or four; for 40-byte audio packets the item cap
//! bites at 64. That is the same answer the formula gives, computed from the
//! data that actually flowed rather than from an estimate made before it did.
//!
//! # The one exception, and why it is load-bearing
//!
//! An *empty* wire always has room, even for an item larger than its byte cap.
//! Without that rule a single frame bigger than `max_bytes` is unschedulable and
//! the pipeline stops with every queue empty — a deadlock produced by the
//! anti-unbounded-memory mechanism itself. See [`Wire::has_room`].

use std::collections::VecDeque;

use vaco_core::{Result, TimeBase, Timestamp};
use vaco_frame::Frame;
use vaco_limits::Budget;
use vaco_packet::Packet;

/// What a wire carries. Fixed at build time and checked when the wire is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Compressed packets: demuxer to decoder, encoder to muxer, or a
    /// stream-copy edge straight from a demuxer to a muxer.
    Packets,
    /// Decoded frames: decoder to filter graph to encoder.
    Frames,
}

/// One item in transit.
///
/// A single enum rather than two queue types, because every wire needs the same
/// end-of-stream bookkeeping and a scheduler that carried two kinds of queue
/// would have to write it twice.
#[derive(Debug, Clone)]
pub enum Payload {
    /// A compressed packet.
    Packet(Packet),
    /// A decoded frame.
    Frame(Frame),
}

impl Payload {
    /// Which flow this item belongs on.
    #[must_use]
    pub const fn flow(&self) -> Flow {
        match self {
            Self::Packet(_) => Flow::Packets,
            Self::Frame(_) => Flow::Frames,
        }
    }

    /// Payload bytes, for the byte half of a [`Capacity`].
    ///
    /// A frame's planes are counted by their buffer length, which is what a
    /// pool actually holds, not by `width * height * bpp`, which is what the
    /// picture would occupy if it were tightly packed.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        match self {
            Self::Packet(p) => p.len as u64,
            Self::Frame(f) => match &f.data {
                vaco_frame::FrameData::Video { planes, .. } => {
                    planes.iter().map(|p| p.data.len() as u64).sum()
                }
                vaco_frame::FrameData::Audio { planes, .. } => {
                    planes.iter().map(|p| p.data.len() as u64).sum()
                }
            },
        }
    }

    /// The presentation timestamp, in the wire's time base.
    #[must_use]
    pub const fn pts(&self) -> Timestamp {
        match self {
            Self::Packet(p) => p.pts,
            Self::Frame(f) => f.pts,
        }
    }

    /// Take the packet, or `None` if this is a frame.
    #[must_use]
    pub fn into_packet(self) -> Option<Packet> {
        match self {
            Self::Packet(p) => Some(p),
            Self::Frame(_) => None,
        }
    }

    /// Take the frame, or `None` if this is a packet.
    #[must_use]
    pub fn into_frame(self) -> Option<Frame> {
        match self {
            Self::Frame(f) => Some(f),
            Self::Packet(_) => None,
        }
    }
}

/// The bound on one wire: items and bytes, whichever binds first.
///
/// Both are *admission* limits, checked before a producer is allowed to run.
/// A producer that is admitted may overshoot by its own expansion factor — a
/// decoder draining a five-frame reorder delay emits five frames from one
/// packet — so a wire's peak occupancy is `cap + codec delay`, not `cap`. The
/// overshoot is bounded by the codec's declared delay, and the pipeline's
/// [`Budget`] is the hard ceiling underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Items admitted before the wire is considered full.
    pub max_items: usize,
    /// Payload bytes admitted before the wire is considered full.
    pub max_bytes: u64,
}

impl Capacity {
    /// The default: 64 items or 16 MiB, whichever binds first.
    ///
    /// 64 is plan 12 §7.1's upper clamp; 16 MiB is a quarter of its 64 MiB
    /// per-link target, chosen down because this crate applies the byte cap to
    /// *every* wire in a fan-out rather than to one link per stage.
    pub const DEFAULT: Self = Self {
        max_items: 64,
        max_bytes: 16 << 20,
    };

    /// The shallowest legal bound: one item. Used by the deadlock tests, where
    /// the point is to prove the pipeline still completes with no slack at all.
    pub const MINIMAL: Self = Self {
        max_items: 1,
        max_bytes: 1,
    };

    /// A capacity with an explicit item bound and the default byte bound.
    #[must_use]
    pub const fn items(max_items: usize) -> Self {
        Self {
            max_items: if max_items == 0 { 1 } else { max_items },
            ..Self::DEFAULT
        }
    }

    /// A capacity with an explicit byte bound and the default item bound.
    #[must_use]
    pub const fn bytes(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            ..Self::DEFAULT
        }
    }
}

impl Default for Capacity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Per-wire counters, the raw material for plan 12 §7.1's tuning table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WireStats {
    /// Items that have entered the wire.
    pub pushed: u64,
    /// Items that have left it.
    pub popped: u64,
    /// The deepest the wire ever got, in items.
    pub high_water: usize,
    /// Times a producer was held back because this wire was full. The
    /// occupancy-pinned-at-max signal in plan 12 §7.1's diagnosis table.
    pub stalls: u64,
}

/// A bounded, single-producer single-consumer queue between two nodes.
///
/// There is no blocking primitive here and none anywhere else in this crate.
/// A full wire does not park a thread; it makes its producer *unrunnable*, and
/// the scheduler picks a different node. That is the whole of the backpressure
/// mechanism, and it is why a deadlock in this crate would have to be a
/// scheduling bug rather than a lock-ordering bug.
#[derive(Debug)]
pub struct Wire {
    queue: VecDeque<Payload>,
    flow: Flow,
    cap: Capacity,
    bytes: u64,
    /// The producer has finished. Sticky: never cleared except by `reset`.
    closed: bool,
    /// The timestamp the producer's stream ended at, in `time_base`.
    end_pts: Timestamp,
    /// The unit every timestamp on this wire is counted in.
    time_base: TimeBase,
    stats: WireStats,
}

impl Wire {
    /// A wire carrying `flow`, bounded by `cap`, whose timestamps are counted
    /// in `time_base`.
    #[must_use]
    pub fn new(flow: Flow, cap: Capacity, time_base: TimeBase) -> Self {
        Self {
            queue: VecDeque::new(),
            flow,
            cap,
            bytes: 0,
            closed: false,
            end_pts: Timestamp::NONE,
            time_base,
            stats: WireStats::default(),
        }
    }

    /// What this wire carries.
    #[must_use]
    pub const fn flow(&self) -> Flow {
        self.flow
    }

    /// The unit timestamps on this wire are counted in.
    #[must_use]
    pub const fn time_base(&self) -> TimeBase {
        self.time_base
    }

    /// Items waiting.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.queue.len()
    }

    /// Payload bytes waiting.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Nothing waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The producer has declared it will send nothing further.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Closed and drained: the consumer will never see another item.
    #[must_use]
    pub fn at_eof(&self) -> bool {
        self.closed && self.queue.is_empty()
    }

    /// The timestamp the producer's stream ended at.
    #[must_use]
    pub const fn end_pts(&self) -> Timestamp {
        self.end_pts
    }

    /// Counters.
    #[must_use]
    pub const fn stats(&self) -> WireStats {
        self.stats
    }

    /// The bound.
    #[must_use]
    pub const fn capacity(&self) -> Capacity {
        self.cap
    }

    /// Whether a producer may be admitted.
    ///
    /// An empty wire always has room. Without that an item larger than
    /// `max_bytes` could never be scheduled, and the pipeline would stop with
    /// every queue empty and every node unrunnable — a deadlock caused by the
    /// mechanism that exists to prevent unbounded memory. Since a wire is
    /// drained by exactly one consumer and a consumer always empties what it
    /// takes, "empty" is always reachable, so this rule guarantees forward
    /// progress rather than merely making it likely.
    #[must_use]
    pub fn has_room(&self) -> bool {
        if self.closed {
            return false;
        }
        self.queue.is_empty()
            || (self.queue.len() < self.cap.max_items && self.bytes < self.cap.max_bytes)
    }

    /// Record that a producer was held back by this wire.
    pub const fn note_stall(&mut self) {
        self.stats.stalls = self.stats.stalls.saturating_add(1);
    }

    /// Append an item, charging its bytes to `budget`.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] when the pipeline's total memory
    /// budget is exhausted. That is the hard ceiling under the soft per-wire
    /// caps: reaching it means the caps were configured larger than the budget,
    /// which is a configuration error rather than a stall to be waited out.
    /// [`vaco_core::Error::InvalidData`] if the item is the wrong flow for this
    /// wire, which the builder's type-level separation of packet and frame taps
    /// already makes unreachable from safe caller code.
    pub fn push(&mut self, item: Payload, budget: &mut Budget) -> Result<()> {
        if item.flow() != self.flow {
            return Err(vaco_core::Error::InvalidData(
                "an item of the wrong kind was pushed onto a wire",
            ));
        }
        if self.closed {
            return Err(vaco_core::Error::InvalidData(
                "an item was pushed onto a wire whose producer had already closed it",
            ));
        }
        let n = item.bytes();
        budget.charge(n)?;
        self.bytes = self.bytes.saturating_add(n);
        self.queue.push_back(item);
        self.stats.pushed = self.stats.pushed.saturating_add(1);
        self.stats.high_water = self.stats.high_water.max(self.queue.len());
        Ok(())
    }

    /// Take the oldest item, releasing its bytes back to `budget`.
    pub fn pop(&mut self, budget: &mut Budget) -> Option<Payload> {
        let item = self.queue.pop_front()?;
        let n = item.bytes();
        self.bytes = self.bytes.saturating_sub(n);
        budget.release(n);
        self.stats.popped = self.stats.popped.saturating_add(1);
        Some(item)
    }

    /// Declare that no further item will be pushed. Idempotent, and sticky:
    /// the first `end_pts` wins, because a second close is either a duplicate
    /// or a bug and neither should move the timestamp.
    pub fn close(&mut self, end_pts: Timestamp) {
        if !self.closed {
            self.closed = true;
            self.end_pts = end_pts;
        }
    }

    /// Discard everything queued and reopen the wire. What a seek does.
    pub fn reset(&mut self, budget: &mut Budget) {
        budget.release(self.bytes);
        self.bytes = 0;
        self.queue.clear();
        self.closed = false;
        self.end_pts = Timestamp::NONE;
    }
}
