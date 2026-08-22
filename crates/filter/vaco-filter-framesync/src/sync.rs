//! The event loop: a pure state machine, fed frames and told about end of
//! stream, producing one event at a time.
//!
//! Deliberately free of `FilterContext`. The loop is subtle enough that it
//! wants to be testable without a graph, and keeping it pure also means a
//! filter that needs an unusual pulling strategy can drive it directly. The
//! [`Synced`](crate::Synced) adapter does the I/O.

use vaco_core::{Error, Result, Rounding, TimeBase, Timestamp};
use vaco_frame::Frame;

use crate::opts::{ExtendMode, FrameSyncOpts, FsInput, TsSyncMode, apply_opts};

/// The common time base is never finer than this.
///
/// Measured: two inputs at 1/1000 and 1/1001 select `1/1000000` rather than
/// `1/1001000`, so the reduction is capped rather than exact.
pub const MAX_DENOMINATOR: i64 = 1_000_000;

/// The time base used when the reduction would exceed [`MAX_DENOMINATOR`].
pub const FALLBACK_TIME_BASE: TimeBase = TimeBase {
    num: 1,
    den: 1_000_000,
};

/// What one [`FrameSync::step`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// An event is ready. Read it with [`FrameSync::event`].
    Ready,
    /// More input is needed. [`FrameSync::wants`] says which.
    Pending,
    /// No further event will ever occur.
    Eof,
}

/// Where an input is relative to its own frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before its first frame.
    BeforeFirst,
    /// Producing frames.
    Running,
    /// Past its end of stream.
    AfterEof,
}

#[derive(Debug)]
struct Input {
    role: FsInput,
    state: State,
    /// The live sync level. Dropped to zero when the input ends, which is what
    /// hands the clock to whoever is still running.
    sync: u32,
    /// The frame this input currently contributes.
    frame: Option<Frame>,
    /// Its timestamp in the common time base.
    pts: Option<i64>,
    /// The lookahead: one frame past `frame`.
    next: Option<Frame>,
    next_pts: Option<i64>,
    /// End of stream has been seen, but the transition has not been applied.
    eof_seen: bool,
    /// The timestamp end of stream was reported at, in the common time base.
    eof_pts: Option<i64>,
    /// The last timestamp assigned, so a frame with no timestamp still lands
    /// somewhere monotonic.
    last_assigned: i64,
}

impl Input {
    fn new(role: FsInput) -> Self {
        Self {
            sync: role.sync,
            role,
            state: State::BeforeFirst,
            frame: None,
            pts: None,
            next: None,
            next_pts: None,
            eof_seen: false,
            eof_pts: None,
            last_assigned: 0,
        }
    }

    const fn has_next(&self) -> bool {
        self.next_pts.is_some()
    }

    /// Whether this input can say nothing more about the future.
    const fn exhausted(&self) -> bool {
        self.eof_seen && self.next_pts.is_none()
    }
}

/// Aligns several inputs on one timeline.
///
/// # The loop, and how it was recovered
///
/// The shape below is not the one plan 16 §3.2 describes, and the differences
/// were all found by measuring `overlay`, `blend` and `hstack` against ffmpeg
/// 8.1 with sources of mismatched rates and lengths.
///
/// ```text
/// step():
///   1. Apply any end of stream seen since the last event: state -> AfterEof,
///      sync -> 0, recompute the sync level. Deferred by exactly one event,
///      which is why a secondary's last frame is delivered once even with
///      repeatlast=0.
///   2. If no input still has the sync level, there is nothing left to drive
///      the clock: end.
///   3. pts = the earliest lookahead among inputs at the sync level. Any input
///      that has no lookahead and has not ended must be fed first.
///   4. Every input advances while its lookahead is at or before pts.
///   5. Deliver, unless an input whose `before` is Stop has not started.
/// ```
///
/// Three things about that are worth stating because each contradicts the plan:
///
/// * **The sync level is dynamic.** An input that ends has its sync set to
///   zero, so the clock passes to whoever is left. That is what makes
///   `overlay` with a 10 fps main and a 25 fps overlay emit *twelve* frames for
///   one second: ten at the main's timestamps, then two more at the overlay's
///   remaining ones. The plan models `after = Infinity` as holding the last
///   frame forever, which on its own would never terminate.
/// * **Non-driving inputs advance in bulk, not one event at a time.** Step 4
///   consumes every frame at or before the event, so "the most recent frame
///   with `pts <= event`" falls out rather than being a separate rule.
/// * **End of stream takes effect one event late.** Applying it immediately
///   makes a secondary's final frame vanish before it is ever composited;
///   measured, that frame is delivered exactly once.
#[derive(Debug)]
pub struct FrameSync {
    inputs: Vec<Input>,
    opts: FrameSyncOpts,
    time_base: TimeBase,
    sync_level: u32,
    /// Filled at each event, so a callback can take frames without disturbing
    /// what the synchroniser is holding.
    event_frames: Vec<Option<Frame>>,
    event_pts: i64,
    /// The event time chosen but not yet delivered, when a `step` had to stop
    /// half way to ask for more input. Resuming with it rather than recomputing
    /// is what makes end of stream take effect exactly one event late — see
    /// [`FrameSync::step`].
    pending_pts: Option<i64>,
    ready: bool,
    eof: bool,
}

impl FrameSync {
    /// Build a synchroniser for `roles`, with the user's options already
    /// applied to them.
    ///
    /// The common time base is the greatest common divisor of the inputs' time
    /// bases, capped at [`MAX_DENOMINATOR`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when there are no inputs, or when an input has no
    /// usable time base.
    pub fn new(mut roles: Vec<FsInput>, opts: FrameSyncOpts) -> Result<Self> {
        if roles.is_empty() {
            return Err(Error::InvalidData("a frame synchroniser needs an input"));
        }
        apply_opts(&mut roles, opts);
        let mut time_base: Option<TimeBase> = None;
        for role in &roles {
            if !role.time_base.is_defined() || role.time_base.is_zero() {
                return Err(Error::InvalidData(
                    "every framesync input needs a time base before configuration",
                ));
            }
            time_base = Some(match time_base {
                None => role.time_base,
                Some(acc) => gcd_q(acc, role.time_base),
            });
        }
        let time_base = time_base.unwrap_or(FALLBACK_TIME_BASE);
        let sync_level = roles.iter().map(|r| r.sync).max().unwrap_or(0);
        let n = roles.len();
        Ok(Self {
            inputs: roles.into_iter().map(Input::new).collect(),
            opts,
            time_base,
            sync_level,
            event_frames: (0..n).map(|_| None).collect(),
            event_pts: 0,
            pending_pts: None,
            ready: false,
            eof: false,
        })
    }

    /// The timeline every event is expressed in.
    #[must_use]
    pub const fn time_base(&self) -> TimeBase {
        self.time_base
    }

    /// How many frames of lookahead the configuration costs.
    ///
    /// One for `ts_sync_mode=nearest`, which cannot choose between the frame
    /// before an event and the one after without seeing both.
    #[must_use]
    pub const fn latency(&self) -> u32 {
        match self.opts.ts_sync {
            TsSyncMode::Default => 0,
            TsSyncMode::Nearest => 1,
        }
    }

    /// How many inputs there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Whether there are no inputs. Never true for a built synchroniser.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    /// The inputs that must be fed before the next event can be determined.
    pub fn wants(&self) -> impl Iterator<Item = usize> + '_ {
        self.inputs
            .iter()
            .enumerate()
            .filter(|(_, i)| !i.has_next() && !i.eof_seen)
            .map(|(n, _)| n)
    }

    /// Whether input `i` still has room for a frame.
    #[must_use]
    pub fn wants_input(&self, i: usize) -> bool {
        self.inputs
            .get(i)
            .is_some_and(|input| !input.has_next() && !input.eof_seen)
    }

    /// Hand a frame to input `i`.
    ///
    /// A frame with no timestamp is placed one tick after the last one this
    /// input contributed, so an untimed stream still advances monotonically
    /// rather than collapsing onto zero.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for an unknown input.
    /// [`Error::OutputPending`] when the input already holds a lookahead — the
    /// caller should have consulted [`FrameSync::wants_input`]. The frame is
    /// not consumed.
    pub fn feed(&mut self, i: usize, frame: Frame) -> Result<()> {
        let common = self.time_base;
        let Some(input) = self.inputs.get_mut(i) else {
            return Err(Error::InvalidData("no such framesync input"));
        };
        if input.has_next() {
            return Err(Error::OutputPending);
        }
        let pts = frame
            .pts
            .rescale(input.role.time_base, common, Rounding::NearestAwayFromZero)
            .ticks()
            .unwrap_or_else(|| input.last_assigned.saturating_add(1));
        input.last_assigned = pts;
        input.next_pts = Some(pts);
        input.next = Some(frame);
        Ok(())
    }

    /// Tell input `i` that it has ended, at `pts` in **its own** time base.
    ///
    /// Idempotent: a second call is ignored, which matches the sticky end of
    /// stream `vaco-filter-core` rule F2 guarantees on the link.
    pub fn close(&mut self, i: usize, pts: Timestamp) {
        let common = self.time_base;
        let Some(input) = self.inputs.get_mut(i) else {
            return;
        };
        if input.eof_seen {
            return;
        }
        input.eof_seen = true;
        input.eof_pts = pts
            .rescale(input.role.time_base, common, Rounding::NearestAwayFromZero)
            .ticks();
    }

    /// The timestamp the whole synchroniser ended at, in the common time base.
    ///
    /// The latest end of stream any input reported, which is what `concat`,
    /// `tpad` and `xfade` need to place what they emit after it.
    #[must_use]
    pub fn end_pts(&self) -> Timestamp {
        self.inputs
            .iter()
            .filter_map(|i| i.eof_pts)
            .max()
            .map_or(Timestamp::NONE, Timestamp::new)
    }

    /// Try to produce the next event.
    ///
    /// Returns [`Step::Pending`] as often as it needs to; the caller feeds the
    /// inputs [`FrameSync::wants`] names and calls again.
    ///
    /// # Why the resumption point matters
    ///
    /// A `step` that has already chosen an event time and begun advancing
    /// inputs remembers that time in `pending_pts` rather than recomputing it
    /// on the next call. Without that, an input whose lookahead was consumed
    /// during the interrupted call and which then turned out to be at end of
    /// stream would make the event time un-recomputable, and the event it had
    /// already been advanced for would be lost. It is also exactly what defers
    /// the end-of-stream transition by one event, which is the behaviour
    /// `repeatlast=0` is measured to have.
    pub fn step(&mut self) -> Step {
        if self.eof {
            return Step::Eof;
        }
        if self.ready {
            return Step::Ready;
        }
        loop {
            if self.pending_pts.is_none() {
                // 1. Apply any end of stream whose final frame has been
                //    delivered. Only ever between events, never in the middle
                //    of determining one.
                self.apply_pending_eof();
                if self.eof {
                    return Step::Eof;
                }
                // 2. Nobody left to drive the clock.
                if self.sync_level == 0 {
                    self.eof = true;
                    return Step::Eof;
                }
                // 3. The next event time, from the driving inputs.
                let mut pts: Option<i64> = None;
                for input in &self.inputs {
                    if input.sync != self.sync_level {
                        continue;
                    }
                    match input.next_pts {
                        Some(p) => pts = Some(pts.map_or(p, |q: i64| q.min(p))),
                        None if !input.eof_seen => return Step::Pending,
                        None => {}
                    }
                }
                let Some(pts) = pts else {
                    self.eof = true;
                    return Step::Eof;
                };
                self.pending_pts = Some(pts);
            }
            let Some(pts) = self.pending_pts else {
                self.eof = true;
                return Step::Eof;
            };
            // 4. Everyone advances up to that time. A passive input that has
            //    neither a lookahead nor an end of stream cannot say whether it
            //    has something at or before `pts`, so it has to be fed first.
            for i in 0..self.inputs.len() {
                while let Some(input) = self.inputs.get(i) {
                    match input.next_pts {
                        Some(p) if p <= pts => {}
                        Some(_) => break,
                        None if input.eof_seen => break,
                        None => return Step::Pending,
                    }
                    self.promote(i);
                }
            }
            self.pending_pts = None;
            // 5. An input that has not started and must not be skipped over
            //    suppresses the event entirely.
            let blocked = self.inputs.iter().any(|input| {
                input.state == State::BeforeFirst && input.role.before == ExtendMode::Stop
            });
            if blocked {
                // Nothing to deliver, but time moved; go round again.
                continue;
            }
            self.event_pts = pts;
            self.fill_event(pts);
            self.ready = true;
            return Step::Ready;
        }
    }

    /// The current event. Only meaningful directly after [`Step::Ready`].
    pub fn event(&mut self) -> FrameSyncEvent<'_> {
        FrameSyncEvent {
            pts: self.event_pts,
            time_base: self.time_base,
            frames: &mut self.event_frames,
        }
    }

    /// Finish with the current event, so the next [`FrameSync::step`] looks for
    /// another.
    pub fn consume(&mut self) {
        self.ready = false;
        for slot in &mut self.event_frames {
            *slot = None;
        }
    }

    /// Discard everything in flight, keeping the configuration. What a seek
    /// does.
    pub fn flush(&mut self) {
        for input in &mut self.inputs {
            input.state = State::BeforeFirst;
            input.sync = input.role.sync;
            input.frame = None;
            input.pts = None;
            input.next = None;
            input.next_pts = None;
            input.eof_seen = false;
            input.eof_pts = None;
            input.last_assigned = 0;
        }
        self.sync_level = self.inputs.iter().map(|i| i.sync).max().unwrap_or(0);
        self.consume();
        self.pending_pts = None;
        self.eof = false;
    }

    /// Move an input's lookahead into its current slot.
    fn promote(&mut self, i: usize) {
        let Some(input) = self.inputs.get_mut(i) else {
            return;
        };
        input.frame = input.next.take();
        input.pts = input.next_pts.take();
        input.state = State::Running;
    }

    /// End of stream takes effect one event after it is seen.
    ///
    /// Deferring it is what makes a secondary's final frame appear exactly
    /// once under `repeatlast=0`, measured at three different main frame rates.
    fn apply_pending_eof(&mut self) {
        let mut changed = false;
        for input in &mut self.inputs {
            if !input.exhausted() || input.state == State::AfterEof {
                continue;
            }
            // An input that ends without ever producing a frame cannot be
            // waited for, and if the filter was going to wait for it (`before
            // = Stop`) there is nothing to synchronise against. Plan 16 §3.3's
            // last row.
            let never_started = input.state == State::BeforeFirst;
            input.state = State::AfterEof;
            input.sync = 0;
            changed = true;
            if never_started && input.role.before == ExtendMode::Stop {
                self.eof = true;
            }
            match input.role.after {
                ExtendMode::Stop => self.eof = true,
                ExtendMode::Null => {
                    input.frame = None;
                    input.pts = None;
                }
                ExtendMode::Infinity => {}
            }
        }
        if changed {
            self.sync_level = self.inputs.iter().map(|i| i.sync).max().unwrap_or(0);
        }
    }

    /// Choose what each input contributes to the event at `pts`.
    fn fill_event(&mut self, pts: i64) {
        for (i, input) in self.inputs.iter().enumerate() {
            let chosen = match input.state {
                State::BeforeFirst => match input.role.before {
                    // `Stop` never reaches here: it suppresses the event.
                    ExtendMode::Stop | ExtendMode::Null => None,
                    ExtendMode::Infinity => input.next.clone(),
                },
                State::Running => pick(input, pts, self.opts.ts_sync),
                State::AfterEof => input.frame.clone(),
            };
            if let Some(slot) = self.event_frames.get_mut(i) {
                *slot = chosen;
            }
        }
    }
}

/// Which of an input's two held frames the event should carry.
///
/// `Default` takes the current one, which step 4 has already advanced to the
/// newest at or before the event. `Nearest` compares it against the lookahead
/// and takes whichever is closer, **keeping the current one on a tie** —
/// measured with an 8 fps main against a 4 fps secondary, where every other
/// event lands exactly halfway between two secondary frames and the earlier one
/// wins each time.
fn pick(input: &Input, pts: i64, mode: TsSyncMode) -> Option<Frame> {
    let current = input.frame.clone();
    if mode == TsSyncMode::Default {
        return current;
    }
    let (Some(cur_pts), Some(next_pts)) = (input.pts, input.next_pts) else {
        return current;
    };
    if next_pts.abs_diff(pts) < cur_pts.abs_diff(pts) {
        return input.next.clone().or(current);
    }
    current
}

/// One aligned set of frames.
#[derive(Debug)]
pub struct FrameSyncEvent<'a> {
    pts: i64,
    time_base: TimeBase,
    frames: &'a mut [Option<Frame>],
}

impl FrameSyncEvent<'_> {
    /// The event time, in the common time base.
    #[must_use]
    pub const fn pts(&self) -> i64 {
        self.pts
    }

    /// The event time as a timestamp.
    #[must_use]
    pub const fn timestamp(&self) -> Timestamp {
        Timestamp::new(self.pts)
    }

    /// The time base [`FrameSyncEvent::pts`] is expressed in.
    #[must_use]
    pub const fn time_base(&self) -> TimeBase {
        self.time_base
    }

    /// How many inputs the event covers.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the event covers no inputs. Never true in practice.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The frame input `i` contributes, if it has one.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<&Frame> {
        self.frames.get(i).and_then(Option::as_ref)
    }

    /// Take ownership of input `i`'s frame.
    ///
    /// Taking input 0 is the usual "modify the main frame in place" path: the
    /// frame is copy-on-write, so writing through it copies only if the
    /// synchroniser is still holding the same buffer.
    pub fn take(&mut self, i: usize) -> Option<Frame> {
        self.frames.get_mut(i).and_then(Option::take)
    }
}

/// The greatest common divisor of two time bases, capped at
/// [`MAX_DENOMINATOR`].
///
/// Measured against the reference's own choice, read out of `-v verbose`:
///
/// | inputs | selected |
/// |---|---|
/// | 1/10, 1/25 | 1/50 |
/// | 1001/30000, 1/25 | 1/30000 |
/// | 1/1000, 1/1001 | 1/1000000 (the cap) |
/// | 1001/30000, 1001/24000 | 1001/120000 |
#[must_use]
pub fn gcd_q(a: TimeBase, b: TimeBase) -> TimeBase {
    if !a.is_defined() || a.is_zero() {
        return b;
    }
    if !b.is_defined() || b.is_zero() {
        return a;
    }
    // gcd(a/b, c/d) = gcd(a*d, c*b) / (b*d)
    let (an, ad) = (i64::from(a.num), i64::from(a.den));
    let (bn, bd) = (i64::from(b.num), i64::from(b.den));
    let num = gcd(
        an.saturating_mul(bd).unsigned_abs(),
        bn.saturating_mul(ad).unsigned_abs(),
    );
    let den = ad.saturating_mul(bd).unsigned_abs();
    if num == 0 || den == 0 {
        return FALLBACK_TIME_BASE;
    }
    let g = gcd(num, den);
    let (num, den) = (num.div_euclid(g), den.div_euclid(g));
    if den > MAX_DENOMINATOR.unsigned_abs() || num > i64::from(i32::MAX).unsigned_abs() {
        return FALLBACK_TIME_BASE;
    }
    let (Ok(num), Ok(den)) = (i32::try_from(num), i32::try_from(den)) else {
        return FALLBACK_TIME_BASE;
    };
    TimeBase::new(num, den)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a.rem_euclid(b);
        a = t;
    }
    a
}
