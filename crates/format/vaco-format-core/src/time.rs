//! The timestamp model: wraparound, generation, repair and duration.
//!
//! The most error-prone area in the subsystem, and the one where the boundary
//! against the CLI has to be stated once and then held:
//!
//! > **This crate owns** field decoding, wraparound, absent-timestamp
//! > normalisation, PTS generation from DTS, per-stream monotonic-DTS repair,
//! > packet duration fill-in, `start_time` derivation and duration estimation.
//! >
//! > **The CLI owns** `-itsoffset`, `-itsscale`, `-isync`, discontinuity
//! > *policy* against `dts_delta_threshold`, `-ss`/`-t`/`-to` trimming,
//! > output-base normalisation, `-fps_mode` and encoder time bases.
//!
//! The format layer never applies a user-specified offset. The CLI layer never
//! touches wraparound. Rules are numbered as `planning/18-formats.md` §1.7
//! numbers them so the two documents compose by citation.
//!
//! # Exactness
//!
//! Nothing here goes through `f64`. Rescaling is [`vaco_core::Timestamp::rescale`],
//! which multiplies in `i128` and divides once with a named rounding mode;
//! cross-base comparison is [`vaco_core::Timestamp::compare`], which
//! cross-multiplies rather than converting to seconds. That is not fastidiousness:
//! a 1/90000 stream and a 1/1001 stream compared through seconds order
//! *nearly*, and "nearly" is a desync.

use vaco_core::{Duration, Error, Rational, Result, Rounding, TimeBase, Timestamp};
use vaco_packet::Packet;

use vaco_codec_core::CodecProperties;

use crate::Stream;
use crate::flags::FormatFlags;
use crate::options::{FFlags, FormatOptions};

/// The base every API speaking in absolute time uses: microseconds.
pub const TIME_BASE_Q: TimeBase = Rational::MICROSECONDS;

/// A container field that decodes to exactly this is an absent timestamp, not a
/// number (R5).
///
/// Our model uses [`Timestamp::NONE`] and never a sentinel, so this constant
/// exists for exactly one purpose: recognising the alias at the *container
/// boundary*, for formats with a full 64-bit signed timestamp field. Call
/// [`decode_ts`] rather than comparing by hand.
pub const NOPTS_ALIAS: i64 = i64::MIN;

/// Normalise a raw container field into a [`Timestamp`] (R5).
///
/// One line, and it removes a whole family of "we disagree on one weird file"
/// reports.
#[must_use]
pub const fn decode_ts(raw: i64) -> Timestamp {
    if raw == NOPTS_ALIAS {
        Timestamp::NONE
    } else {
        Timestamp::new(raw)
    }
}

// ------------------------------------------------------------------ wrapping

/// What to do with a timestamp on the wrong side of the pivot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapBehavior {
    /// Leave it alone. The default, and correct whenever the recording does not
    /// cross a wrap.
    #[default]
    Ignore,
    /// Small values seen after a large one are post-wrap: add the period.
    AddOffset,
    /// Large values seen after a small one are stale pre-wrap: subtract it.
    SubOffset,
}

/// Wraparound correction for one clock.
///
/// **Per program, not per stream** (R7). A multiplex shares one clock; a state
/// that corrected video and left audio uncorrected would desynchronise them
/// permanently. Streams belonging to no program share a synthetic program-zero
/// state.
#[derive(Debug, Clone, Copy)]
pub struct WrapState {
    /// Width of the container's timestamp field. MPEG-2 PES/PCR is 33; 64 means
    /// the field cannot wrap and every method here is a no-op.
    bits: u32,
    reference: Option<i64>,
    behavior: WrapBehavior,
    /// Cumulative correction from mid-stream wraps (R9), in native units.
    offset: i64,
    last: Option<i64>,
    /// Whether R8's "did a high value follow a low one" observation has landed.
    saw_high: bool,
    correct_overflow: bool,
}

impl Default for WrapState {
    fn default() -> Self {
        Self::new(64)
    }
}

impl WrapState {
    /// A state for a `bits`-wide field. Values above 63 mean "cannot wrap".
    #[must_use]
    pub const fn new(bits: u32) -> Self {
        Self {
            bits,
            reference: None,
            behavior: WrapBehavior::Ignore,
            offset: 0,
            last: None,
            saw_high: false,
            correct_overflow: true,
        }
    }

    /// Honour `correct_ts_overflow` (option 22).
    #[must_use]
    pub const fn with_options(mut self, opts: &FormatOptions) -> Self {
        self.correct_overflow = opts.correct_ts_overflow;
        self
    }

    /// Field width in bits.
    #[must_use]
    pub const fn bits(&self) -> u32 {
        self.bits
    }

    /// Whether this clock can wrap at all.
    #[must_use]
    pub const fn wraps(&self) -> bool {
        self.bits > 0 && self.bits < 64
    }

    /// `2^bits`, or `None` when the clock cannot wrap.
    #[must_use]
    pub const fn period(&self) -> Option<i64> {
        if self.wraps() {
            Some(1i64 << self.bits)
        } else {
            None
        }
    }

    /// The correction currently being applied, in native units.
    #[must_use]
    pub const fn offset(&self) -> i64 {
        self.offset
    }

    /// The pivot, once established.
    #[must_use]
    pub const fn reference(&self) -> Option<i64> {
        self.reference
    }

    /// The behaviour R8 settled on.
    #[must_use]
    pub const fn behavior(&self) -> WrapBehavior {
        self.behavior
    }

    /// Feed a timestamp seen during stream discovery, to establish the pivot
    /// (R8).
    ///
    /// The first observation fixes the pivot at half the period and picks a
    /// direction from which end of the range it sits in. `SubOffset` is only
    /// *armed* on the first observation — it is confirmed later, by
    /// [`WrapState::observe`] seeing a high value, because "we started just
    /// after a wrap" and "this is simply a short file near zero" look identical
    /// until one does.
    pub fn observe(&mut self, ts: i64) {
        let Some(period) = self.period() else { return };
        #[allow(
            clippy::integer_division,
            reason = "period is a power of two and non-zero; these are exact"
        )]
        let (quarter, half, three_quarters) = (period / 4, period / 2, period / 4 * 3);
        match self.reference {
            None => {
                self.reference = Some(half);
                if ts > three_quarters {
                    self.behavior = WrapBehavior::AddOffset;
                    self.saw_high = true;
                } else if ts < quarter {
                    // Armed, not yet chosen: needs a subsequent high value.
                    self.behavior = WrapBehavior::Ignore;
                } else {
                    self.behavior = WrapBehavior::Ignore;
                }
            }
            Some(_) => {
                if ts > three_quarters {
                    if !self.saw_high && self.behavior == WrapBehavior::Ignore {
                        self.behavior = WrapBehavior::SubOffset;
                    }
                    self.saw_high = true;
                }
            }
        }
    }

    /// Apply the pivot rule to one raw value.
    #[must_use]
    pub const fn pivot(&self, ts: i64) -> i64 {
        let (Some(period), Some(r)) = (self.period(), self.reference) else {
            return ts;
        };
        match self.behavior {
            WrapBehavior::Ignore => ts,
            WrapBehavior::AddOffset => {
                if ts < r {
                    ts.saturating_add(period)
                } else {
                    ts
                }
            }
            WrapBehavior::SubOffset => {
                if ts >= r {
                    ts.saturating_sub(period)
                } else {
                    ts
                }
            }
        }
    }

    /// Correct one timestamp, updating the cumulative mid-stream offset (R9).
    ///
    /// Consecutive values differing by more than half the period are a wrap,
    /// not a jump: the offset moves by a whole period in the direction that
    /// minimises the delta. The offset is cumulative, so a file crossing two
    /// wraps stays monotonic rather than sawtoothing.
    ///
    /// # Why the pivot only applies to the first value
    ///
    /// R8's pivot and R9's delta tracking are two answers to the same question
    /// and applying both to every value double-counts: a raw value that the
    /// pivot lifted by a period, followed by one it did not, reads as a jump
    /// backwards. So the pivot places the *first* value — the only one with no
    /// history to take a delta against — and is folded into `offset` there.
    /// Everything after it is delta tracking in raw space, which is both
    /// simpler and the thing that actually keeps a stream monotonic.
    #[must_use]
    pub fn correct(&mut self, ts: Timestamp) -> Timestamp {
        let Some(raw) = ts.ticks() else {
            return Timestamp::NONE;
        };
        let Some(period) = self.period() else {
            return ts;
        };
        match self.last {
            None => self.offset = self.pivot(raw).saturating_sub(raw),
            Some(prev) if self.correct_overflow => {
                #[allow(clippy::integer_division, reason = "period is a non-zero power of two")]
                let half = i128::from(period / 2);
                let delta = i128::from(raw) - i128::from(prev);
                if delta > half {
                    self.offset = self.offset.saturating_sub(period);
                } else if delta < -half {
                    self.offset = self.offset.saturating_add(period);
                }
            }
            Some(_) => {}
        }
        self.last = Some(raw);
        Timestamp::new(raw.saturating_add(self.offset))
    }

    /// Recompute the cumulative offset after a seek (R10).
    ///
    /// A seek invalidates the offset, because the new position may be on the
    /// other side of a wrap. Given the corrected timestamp we asked for and the
    /// first raw value seen after landing, the offset is the whole number of
    /// periods between them. This is the rule that stops a seek into the second
    /// half of a thirty-hour recording from reporting timestamps 26.5 hours in
    /// the past.
    pub fn resync(&mut self, target: Timestamp, first_seen: Timestamp) {
        self.last = None;
        let Some(period) = self.period() else { return };
        let (Some(t), Some(seen)) = (target.ticks(), first_seen.ticks()) else {
            self.offset = 0;
            return;
        };
        let diff = i128::from(t) - i128::from(seen);
        // Round to the nearest whole period.
        let p = i128::from(period);
        #[allow(
            clippy::integer_division,
            reason = "p is a non-zero power of two; this is the round-to-nearest-period step"
        )]
        let periods = (diff + if diff >= 0 { p / 2 } else { -(p / 2) }) / p;
        self.offset = i64::try_from(periods.saturating_mul(p)).unwrap_or(0);
        self.last = Some(seen);
    }

    /// Forget everything derived from packets, keeping the field width.
    pub fn reset(&mut self) {
        let (bits, correct) = (self.bits, self.correct_overflow);
        *self = Self::new(bits);
        self.correct_overflow = correct;
    }
}

// -------------------------------------------------------------- generation

/// Per-stream state for the timestamp-generation rules.
#[derive(Debug, Clone, Copy, Default)]
struct StreamTs {
    cur_dts: Timestamp,
    last_duration: i64,
    /// Reorder window for `+genpts` (R20).
    reorder: [Timestamp; MAX_REORDER],
    reorder_len: usize,
    delay: usize,
    /// Whether the codec can reorder at all; set from
    /// [`vaco_codec_core::CodecProperties`].
    reorders: bool,
    repaired: u64,
}

/// Deepest reorder window `+genpts` will maintain.
///
/// Sixteen is the largest reference-frame count H.264 permits and comfortably
/// exceeds every other codec we ship, so a conforming stream never overflows
/// it, and a non-conforming one is bounded rather than unbounded.
const MAX_REORDER: usize = 16;

/// Why a packet's timestamps were changed. Reported at `-loglevel debug` and
/// counted in the conformance report.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one boolean per rule, which is what makes the report readable in a log"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FixReport {
    /// DTS was derived from PTS — copied outright (R19) for a codec that does
    /// not reorder, or taken from the reorder window (R19b) for one that does.
    pub dts_from_pts: bool,
    /// PTS was generated from the reorder window (R20).
    pub pts_generated: bool,
    /// Duration was filled in (R21).
    pub duration_filled: bool,
    /// DTS was pushed forward to restore monotonicity (R22).
    pub dts_repaired: bool,
    /// DTS was dropped by `+igndts` (R23).
    pub dts_ignored: bool,
    /// The repair could not advance the timestamp because `i64` saturated
    /// (R3).
    ///
    /// Found by the `format_timestamps` fuzz target, which asserted that a
    /// repaired stream is strictly increasing and was right to. Saturation is
    /// the specified behaviour for an overflowing container — wrapping would
    /// turn a corrupt file into a plausible-looking wrong one — but it means
    /// the post-condition R22 otherwise guarantees does not hold, and a caller
    /// that assumes it does will loop or mis-order. So the exception is
    /// *reported* rather than left for the caller to discover.
    pub dts_overflow: bool,
    /// R22's repair pushed DTS past this packet's own PTS.
    ///
    /// `dts > pts` is not a valid packet in any container — it tells a decoder
    /// to decode a frame after the moment it must be shown. The repair does not
    /// check for it, because the two rules answer different questions: R22
    /// restores monotonicity within the DTS sequence and knows nothing about
    /// presentation.
    ///
    /// Deliberately **reported rather than corrected**. Both corrections are
    /// worse than the disease without a measurement nobody has taken yet:
    /// clamping DTS to PTS re-breaks the monotonicity R22 exists to restore, and
    /// pushing PTS forward invents a presentation time the file never claimed.
    /// What the reference does here is unknown — D17 says a measured deviation
    /// is reproduced, so the honest move is to surface the state and decide once
    /// somebody has probed it.
    ///
    /// Reachable only through R22, and only on a stream whose PTS are out of
    /// order while its codec is marked as not reordering — a corrupt or
    /// mislabelled file, not a conforming one.
    pub dts_exceeds_pts: bool,
}

impl FixReport {
    /// Whether anything at all was changed.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        !(self.dts_from_pts
            || self.pts_generated
            || self.duration_filled
            || self.dts_repaired
            || self.dts_ignored
            || self.dts_overflow
            || self.dts_exceeds_pts)
    }
}

/// Fills in and repairs what the container omitted or got wrong.
///
/// One instance per demuxer, holding one small state record per stream. It is a pure
/// state machine over `(stream_index, pts, dts, duration)` — it never reads
/// bytes and never touches I/O — which is what makes it testable in isolation
/// and reusable by [`crate::discovery::Discovery`] and by a demuxer that wants
/// to drive it directly.
///
/// # The rule that matters most
///
/// R22's monotonic-DTS repair applies **only** when the container does not
/// declare [`FormatFlags::TS_DISCONT`]. A format that says its timestamps jump
/// gets them passed through untouched, because a legitimate discontinuity is
/// the CLI's to interpret and repairing it here would destroy the evidence.
/// This split is the single most important boundary rule in the model.
#[derive(Debug, Clone)]
pub struct TimestampFixer {
    streams: Vec<StreamTs>,
    flags: FormatFlags,
    fflags: FFlags,
    fill_in: bool,
}

impl TimestampFixer {
    /// A fixer for `stream_count` streams.
    #[must_use]
    pub fn new(stream_count: usize, flags: FormatFlags, opts: &FormatOptions) -> Self {
        Self {
            streams: vec![StreamTs::default(); stream_count],
            flags,
            fflags: opts.fflags,
            fill_in: opts.fills_in_timestamps(),
        }
    }

    /// A fixer already told about every stream's reorder depth.
    ///
    /// The setup loop this replaces lived only inside `Discovery`, so the
    /// timestamp rules ran during the **analysis** pass and nowhere else. A
    /// caller reading packets for output — `vaco-probe`'s `[PACKET]` section,
    /// `vaco-cli`'s streamcopy — got whatever the container stated and no
    /// reconstruction, which is why Matroska packets arrived with no DTS even
    /// after R19b existed to derive one.
    ///
    /// Constructing it from `&[Stream]` is the whole fix: the delay is
    /// `has_b_frames` and the reorder flag is the codec's own
    /// `CodecProperties::REORDER`, and neither is something a caller should be
    /// re-deriving by hand.
    #[must_use]
    pub fn for_streams(streams: &[Stream], flags: FormatFlags, opts: &FormatOptions) -> Self {
        let mut fixer = Self::new(streams.len(), flags, opts);
        for s in streams {
            let delay = s.params.video.as_ref().map_or(0, |v| v.has_b_frames);
            let reorders = s
                .params
                .codec_id
                .is_some_and(|c| c.properties().contains(CodecProperties::REORDER));
            fixer.set_stream_delay(s.index, delay, reorders);
        }
        fixer
    }

    /// Declare a stream's reorder depth and whether its codec reorders at all.
    ///
    /// `delay` is the codec's `has_b_frames`. Both feed R19 and R20: a stream
    /// with no delay gets `dts = pts` for free, and one with delay does not.
    pub fn set_stream_delay(&mut self, stream_index: u32, delay: u8, reorders: bool) {
        if let Some(s) = self.stream_mut(stream_index) {
            s.delay = usize::from(delay).min(MAX_REORDER - 1);
            s.reorders = reorders;
        }
    }

    /// Grow to cover `stream_count` streams. Existing state is kept.
    pub fn ensure_streams(&mut self, stream_count: usize) {
        if self.streams.len() < stream_count {
            self.streams.resize(stream_count, StreamTs::default());
        }
    }

    fn stream_mut(&mut self, index: u32) -> Option<&mut StreamTs> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.streams.get_mut(i))
    }

    /// Packets whose DTS was repaired, per stream.
    #[must_use]
    pub fn repaired(&self, stream_index: u32) -> u64 {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map_or(0, |s| s.repaired)
    }

    /// Apply R19 to R24 to one packet, in place.
    ///
    /// `time_base` is the packet's own stream's base and is used only for
    /// duration fill-in; `frame_rate` is the stream's average rate, `Rational`
    /// zero or undefined when unknown.
    ///
    /// The order is fixed and is not negotiable: `+igndts` first, then DTS from
    /// PTS, then PTS from DTS, then duration, then the monotonic repair. Every
    /// later rule depends on the earlier ones having run.
    pub fn fix(
        &mut self,
        packet: &mut Packet,
        time_base: TimeBase,
        frame_rate: Rational,
    ) -> FixReport {
        let mut report = FixReport::default();
        let genpts = self.fflags.contains(FFlags::GENPTS);
        let igndts = self.fflags.contains(FFlags::IGNDTS);
        let discont = self.flags.contains(FormatFlags::TS_DISCONT);
        let fill_in = self.fill_in;

        let Some(st) = self.stream_mut(packet.stream_index) else {
            return report;
        };

        // R23 — `+igndts` drops DTS on packets carrying both, before anything
        // else can depend on it.
        if igndts && packet.dts.is_some() && packet.pts.is_some() {
            packet.dts = Timestamp::NONE;
            report.dts_ignored = true;
        }

        // R19 — DTS from PTS, only where the codec cannot reorder.
        if fill_in && packet.dts.is_none() && packet.pts.is_some() && !st.reorders && st.delay == 0
        {
            packet.dts = packet.pts;
            report.dts_from_pts = true;
        }

        // R19b — DTS from PTS through the reorder window, where the codec
        // *does* reorder. R19's mirror, and its absence meant every packet of a
        // B-frame stream reached the muxer with no DTS at all — enough to make
        // streamcopy of ordinary H.264 impossible.
        //
        // The transform is the same one R20 uses, run over PTS instead of DTS:
        // insert, and once the window is deeper than `delay`, emit its minimum.
        // Measured against ffprobe 8.1 on x264 with `-bf 2`:
        //
        //   pts  0    200  100  300  400
        //   dts  N/A  N/A  0    100  200
        //
        // The two leading `N/A`s are the window filling, not an error, and they
        // are what a container writes as "no decode time yet".
        //
        // Sharing one window with R20 is safe because the two guards are
        // mutually exclusive — a packet cannot be missing its DTS and its PTS
        // and be usable by either rule — and it is *right*, because a stream is
        // in one mode or the other for its whole life.
        if fill_in
            && packet.dts.is_none()
            && packet.pts.is_some()
            && st.reorders
            && st.delay > 0
            && let Some(dts) = push_reorder(st, packet.pts)
        {
            packet.dts = dts;
            report.dts_from_pts = true;
        }

        // R20 — PTS from DTS, only under `+genpts`.
        if genpts
            && packet.pts.is_none()
            && packet.dts.is_some()
            && let Some(pts) = push_reorder(st, packet.dts)
        {
            packet.pts = pts;
            report.pts_generated = true;
        }

        // R21 — duration fill-in from the frame rate, when the container left
        // it at zero. The stronger source — the next packet's DTS delta — is
        // only available to a caller that reads ahead, which is why
        // `Discovery` applies it and this does not.
        if fill_in
            && packet.duration == Duration::ZERO
            && let Some(d) = duration_from_rate(frame_rate)
        {
            packet.duration = d;
            report.duration_filled = true;
        }

        // R22 — monotonic DTS repair, suppressed by TS_DISCONT.
        if fill_in
            && !discont
            && let (Some(cur), Some(dts)) = (st.cur_dts.ticks(), packet.dts.ticks())
            && dts <= cur
        {
            let step = st.last_duration.max(1);
            let repaired = cur.saturating_add(step);
            packet.dts = Timestamp::new(repaired);
            st.repaired = st.repaired.saturating_add(1);
            report.dts_repaired = true;
            // Saturated: the repair could not advance. The caller has to know,
            // because "dts is strictly increasing after a repair" is exactly
            // the invariant a scheduler leans on.
            report.dts_overflow = repaired <= cur;
            // And the repair may have pushed DTS past this packet's own PTS,
            // which no container accepts. See `FixReport::dts_exceeds_pts` for
            // why this reports rather than corrects.
            report.dts_exceeds_pts = packet.pts.ticks().is_some_and(|pts| repaired > pts);
        }

        if let Some(dts) = packet.dts.ticks() {
            st.cur_dts = Timestamp::new(dts);
        }
        if let Some(ticks) = packet.duration.to_ticks(time_base)
            && ticks > 0
        {
            st.last_duration = ticks;
        }
        report
    }

    /// Discard everything derived from packets, after a seek (S3).
    ///
    /// Forgetting one of the per-stream fields here is the classic "timestamps
    /// go strange after seeking" bug, so this resets all of them and a demuxer
    /// cannot forget.
    pub fn flush(&mut self) {
        for s in &mut self.streams {
            let (delay, reorders) = (s.delay, s.reorders);
            *s = StreamTs::default();
            s.delay = delay;
            s.reorders = reorders;
        }
    }
}

/// Push `dts` into the reorder window and return the PTS for the packet being
/// emitted, once the window is deep enough (R20).
///
/// The PTS of the packet at position `i` is the `(i - delay)`-th smallest DTS
/// observed so far. This is exact whenever the DTS sequence is a sorted
/// permutation of the PTS sequence, which is the definition of a conforming
/// reordering stream. When it is not, the answer is wrong — and it is wrong the
/// same way the reference is wrong, which is what D6 asks for.
fn push_reorder(st: &mut StreamTs, dts: Timestamp) -> Option<Timestamp> {
    let v = dts.ticks()?;
    // `delay` is clamped to MAX_REORDER - 1 when it is set, and a value is
    // popped whenever the window exceeds `delay`, so the length before an
    // insert never exceeds MAX_REORDER - 1. The guard is belt and braces: a
    // caller that reaches in and sets the field cannot make this overflow.
    if st.reorder_len >= MAX_REORDER {
        return None;
    }
    let pos = st
        .reorder
        .get(..st.reorder_len)?
        .iter()
        .position(|t| t.ticks().is_none_or(|x| x > v))
        .unwrap_or(st.reorder_len);
    st.reorder
        .copy_within(pos..st.reorder_len, pos.saturating_add(1));
    *st.reorder.get_mut(pos)? = Timestamp::new(v);
    st.reorder_len = st.reorder_len.saturating_add(1);
    if st.reorder_len <= st.delay {
        // Still filling: emitting now would hand out a PTS that a later,
        // smaller DTS should have claimed.
        return None;
    }
    let out = *st.reorder.first()?;
    st.reorder.copy_within(1..st.reorder_len, 0);
    st.reorder_len -= 1;
    Some(out)
}

/// One frame's duration at `rate`, in microseconds. `None` for an unusable rate.
#[must_use]
pub fn duration_from_rate(rate: Rational) -> Option<Duration> {
    if !rate.is_defined() || rate.is_zero() || rate.is_infinite() {
        return None;
    }
    // 1/rate seconds, expressed in microseconds: den * 1e6 / num.
    let micros = vaco_core::rescale_rnd(
        i64::from(rate.den),
        1_000_000,
        i64::from(rate.num),
        Rounding::default(),
    )?;
    if micros <= 0 {
        return None;
    }
    Some(Duration::from_micros(micros))
}

// --------------------------------------------------------------- duration

/// Where a container-level duration came from.
///
/// Not printed by `vaco-probe`, but the resulting duration is, so the choice is
/// byte-testable through its consequences. Exposed at `-loglevel verbose` for
/// triage, which is the single most useful diagnostic when a duration comes out
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DurationSource {
    /// An authoritative container field (R14).
    FromStream,
    /// Derived by scanning the tail for the last timestamps (R15).
    FromPts,
    /// `size × 8 / bit_rate` (R16).
    FromBitrate,
    /// Printed as `N/A` (R17).
    #[default]
    Unknown,
}

impl DurationSource {
    /// The name `-loglevel verbose` prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FromStream => "from_stream",
            Self::FromPts => "from_pts",
            Self::FromBitrate => "from_bitrate",
            Self::Unknown => "unknown",
        }
    }
}

/// A duration and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DurationEstimate {
    pub duration: Option<Duration>,
    pub source: DurationSource,
}

impl DurationEstimate {
    /// Nothing known.
    pub const UNKNOWN: Self = Self {
        duration: None,
        source: DurationSource::Unknown,
    };

    /// An estimate from a named source.
    #[must_use]
    pub const fn new(duration: Duration, source: DurationSource) -> Self {
        Self {
            duration: Some(duration),
            source,
        }
    }
}

/// What the demuxer and the discovery pass learned, as inputs to R14–R17.
#[derive(Debug, Clone, Copy, Default)]
pub struct DurationInputs {
    /// The container's own duration field, if it has one.
    pub container: Option<Duration>,
    /// The longest per-stream duration, rescaled.
    pub longest_stream: Option<Duration>,
    /// `max(pts + duration)` over streams, from a tail scan.
    pub from_pts: Option<Duration>,
    /// Container start time, subtracted from the `FromPts` estimate.
    pub start_time: Option<Duration>,
    /// Total byte size, when the transport knows it.
    pub size: Option<u64>,
    /// Declared bit rate, container-level or summed over streams.
    pub bit_rate: Option<u64>,
    /// The demuxer pins `FromStream` and suppresses the tail scan (R18). WAV
    /// and FLAC set this; MPEG-TS never does.
    pub authoritative: bool,
}

/// Choose a container duration (R14 to R18).
///
/// The order is fixed: an authoritative container field, then a tail scan, then
/// the bit-rate quotient, then nothing. There is exactly one tuning knob —
/// [`DurationInputs::authoritative`] — and it only ever *suppresses* a later
/// strategy, never reorders them.
///
/// R14's own subtlety: when both a container-level field and per-stream
/// durations exist and disagree, the **longest stream wins**. A container-level
/// field is often written by a first pass and never corrected.
#[must_use]
pub fn estimate_duration(inputs: &DurationInputs, opts: &FormatOptions) -> DurationEstimate {
    let from_stream = match (inputs.container, inputs.longest_stream) {
        (Some(c), Some(s)) => Some(c.max(s)),
        (c, s) => c.or(s),
    };
    if let Some(d) = from_stream
        && (inputs.authoritative || opts.skip_estimate_duration_from_pts)
    {
        return DurationEstimate::new(d, DurationSource::FromStream);
    }
    if !opts.skip_estimate_duration_from_pts
        && let Some(end) = inputs.from_pts
    {
        let start = inputs.start_time.unwrap_or(Duration::ZERO);
        let d = Duration::from_micros(end.as_micros().saturating_sub(start.as_micros()));
        if d.as_micros() > 0 {
            return DurationEstimate::new(d, DurationSource::FromPts);
        }
    }
    if let Some(d) = from_stream {
        return DurationEstimate::new(d, DurationSource::FromStream);
    }
    if let (Some(size), Some(rate)) = (inputs.size, inputs.bit_rate)
        && rate > 0
        && let Some(micros) = size
            .checked_mul(8)
            .and_then(|bits| bits.checked_mul(1_000_000))
            .map(|n| {
                #[allow(
                    clippy::integer_division,
                    reason = "rate is checked non-zero on the line above"
                )]
                {
                    n / rate
                }
            })
            .and_then(|n| i64::try_from(n).ok())
    {
        return DurationEstimate::new(Duration::from_micros(micros), DurationSource::FromBitrate);
    }
    DurationEstimate::UNKNOWN
}

/// The container-level `start_time` (R12).
///
/// The **minimum** over the streams that have one, in [`TIME_BASE_Q`], skipping
/// streams the caller marks as excluded. `planning/18-formats.md` marks the
/// min-versus-max choice VERIFY-T2; we have not measured it and this
/// implements the plan's stated rule, which is recorded in the docs file as
/// unverified rather than presented as reproduction.
///
/// The excluded set is the caller's because the reasons are container facts:
/// attached pictures have no timeline position, and a stream with no codec
/// contributes nothing meaningful.
#[must_use]
pub fn container_start_time<I>(streams: I) -> Option<Duration>
where
    I: IntoIterator<Item = (Timestamp, TimeBase)>,
{
    streams
        .into_iter()
        .filter_map(|(ts, tb)| ts.to_duration(tb))
        .min()
}

/// Rescale a seek or trim bound so the range never shrinks (R2).
///
/// A lower bound rounds down and an upper bound rounds up. Getting this
/// backwards makes a range seek miss a packet that was inside it before the
/// rescale — a bug that only shows up on files whose time base does not divide
/// the target evenly, which is to say, on real files and not on test ones.
#[must_use]
pub fn rescale_bound(ts: Timestamp, from: TimeBase, to: TimeBase, upper: bool) -> Timestamp {
    ts.rescale(from, to, if upper { Rounding::Up } else { Rounding::Down })
}

/// Check per-stream DTS monotonicity for the mux side (R26).
///
/// An error, never a repair. Silently repairing here is how files with subtly
/// wrong durations get made, and the caller is in a far better position to
/// decide what to do about it.
///
/// # Errors
///
/// [`Error::InvalidData`] when `dts` does not advance as `flags` requires.
pub fn check_monotonic(prev: Timestamp, dts: Timestamp, flags: FormatFlags) -> Result<()> {
    let (Some(p), Some(d)) = (prev.ticks(), dts.ticks()) else {
        return Ok(());
    };
    if flags.requires_strict_dts() {
        if d <= p {
            return Err(Error::InvalidData(
                "non-monotonic dts: this container requires strictly increasing timestamps",
            ));
        }
    } else if d < p {
        return Err(Error::InvalidData(
            "decreasing dts: this container requires non-decreasing timestamps",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::field_reassign_with_default,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    fn pkt(pts: Option<i64>, dts: Option<i64>) -> Packet {
        let mut budget = Budget::new(Limits::strict());
        let mut p = Packet::from_slice(&mut budget, b"x").unwrap();
        p.pts = Timestamp::from(pts);
        p.dts = Timestamp::from(dts);
        p
    }

    #[test]
    fn nopts_alias_decodes_to_absent() {
        assert!(decode_ts(i64::MIN).is_none());
        assert_eq!(decode_ts(0), Timestamp::ZERO);
        assert_eq!(decode_ts(i64::MIN + 1).ticks(), Some(i64::MIN + 1));
    }

    #[test]
    fn a_64_bit_clock_never_wraps() {
        let mut w = WrapState::new(64);
        assert!(!w.wraps());
        assert_eq!(w.period(), None);
        assert_eq!(w.correct(Timestamp::new(i64::MAX)).ticks(), Some(i64::MAX));
    }

    #[test]
    fn one_wrap_keeps_the_stream_monotonic() {
        // A 33-bit clock at 90 kHz, as MPEG-2 PES uses.
        let period = 1i64 << 33;
        let mut w = WrapState::new(33);
        let near_top = period - 1000;
        w.observe(near_top);
        assert_eq!(w.behavior(), WrapBehavior::AddOffset);
        let a = w.correct(Timestamp::new(near_top)).ticks().unwrap();
        // The raw value restarts near zero; the corrected one does not.
        let b = w.correct(Timestamp::new(500)).ticks().unwrap();
        assert!(b > a, "post-wrap value must stay ahead: {a} -> {b}");
        assert_eq!(b - a, 1500);
        assert_eq!(w.offset(), period);
    }

    #[test]
    fn repeated_wraps_accumulate() {
        // A short period, so a full sweep is a handful of steps rather than a
        // hundred thousand. The rule is width-independent.
        let bits = 8;
        let period = 1i64 << bits;
        let mut w = WrapState::new(bits);
        w.observe(250);
        let mut raw = 250i64;
        let mut prev = w.correct(Timestamp::new(raw)).ticks().unwrap();
        let first = prev;
        for _ in 0..120 {
            raw = (raw + 5) % period;
            let cur = w.correct(Timestamp::new(raw)).ticks().unwrap();
            assert!(cur > prev, "not monotonic at raw {raw}: {prev} -> {cur}");
            assert_eq!(cur - prev, 5);
            prev = cur;
        }
        assert_eq!(prev - first, 600);
        // Starting at 250 and advancing 600 crosses 256, 512 and 768.
        assert_eq!(w.offset(), 3 * period);
    }

    #[test]
    fn correct_ts_overflow_off_leaves_the_wrap_alone() {
        let mut o = FormatOptions::default();
        o.correct_ts_overflow = false;
        let period = 1i64 << 33;
        let mut w = WrapState::new(33).with_options(&o);
        w.observe(period - 1000);
        let a = w.correct(Timestamp::new(period - 1000)).ticks().unwrap();
        let b = w.correct(Timestamp::new(500)).ticks().unwrap();
        // The option's whole purpose: the wrap is reported as the container
        // stored it, jump and all.
        assert!(b < a);
        assert_eq!(w.offset(), 0);
    }

    #[test]
    fn resync_recovers_the_offset_after_a_seek() {
        let period = 1i64 << 33;
        let mut w = WrapState::new(33);
        w.observe(period - 1000);
        // We asked for a corrected timestamp two periods in.
        let target = Timestamp::new(period * 2 + 4242);
        w.resync(target, Timestamp::new(4242));
        assert_eq!(w.offset(), period * 2);
        assert_eq!(
            w.correct(Timestamp::new(4242)).ticks(),
            Some(period * 2 + 4242)
        );
    }

    /// The measured sequence, from ffprobe 8.1 on x264 with `-bf 2`:
    ///
    /// ```text
    /// pts  0    200  100  300  400
    /// dts  N/A  N/A  0    100  200
    /// ```
    ///
    /// The two leading absences are the reorder window filling. A rule that
    /// emitted a DTS for the first packet would hand out a value the third
    /// packet has the better claim to.
    #[test]
    fn dts_is_reconstructed_through_the_reorder_window() {
        let opts = FormatOptions::default();
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        f.set_stream_delay(0, 2, true);

        let pts = [0_i64, 200, 100, 300, 400];
        let want = [None, None, Some(0), Some(100), Some(200)];
        for (&pts, &want) in pts.iter().zip(want.iter()) {
            let mut p = pkt(Some(pts), None);
            f.fix(&mut p, TIME_BASE_Q, Rational::ZERO);
            assert_eq!(p.dts.ticks(), want, "pts {pts}");
        }
    }

    /// DTS never exceeds PTS on a reconstructed stream, which is the invariant
    /// a container actually requires — a frame cannot be decoded after the
    /// moment it must be shown.
    #[test]
    fn reconstructed_dts_never_exceeds_its_own_pts() {
        let opts = FormatOptions::default();
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        f.set_stream_delay(0, 2, true);
        for pts in [0_i64, 200, 100, 300, 400, 700, 500, 600] {
            let mut p = pkt(Some(pts), None);
            f.fix(&mut p, TIME_BASE_Q, Rational::ZERO);
            if let (Some(d), Some(pt)) = (p.dts.ticks(), p.pts.ticks()) {
                assert!(d <= pt, "dts {d} > pts {pt}");
            }
        }
    }

    #[test]
    fn dts_copied_from_pts_only_without_reordering() {
        let opts = FormatOptions::default();
        let mut f = TimestampFixer::new(2, FormatFlags::empty(), &opts);
        f.set_stream_delay(1, 2, true);

        let mut p = pkt(Some(100), None);
        let r = f.fix(&mut p, TIME_BASE_Q, Rational::ZERO);
        assert_eq!(p.dts.ticks(), Some(100));
        assert!(r.dts_from_pts);

        // A reordering stream gets no DTS from its *first* packet — the
        // window is still filling. That is R19b delaying, not R19 refusing;
        // `dts_is_reconstructed_through_the_reorder_window` covers the rest.
        let mut p = pkt(Some(100), None);
        p.stream_index = 1;
        let r = f.fix(&mut p, TIME_BASE_Q, Rational::ZERO);
        assert!(p.dts.is_none());
        assert!(!r.dts_from_pts);
    }

    /// R22 restores monotonicity within the DTS sequence and knows nothing
    /// about presentation, so it can push DTS past the packet's own PTS. That
    /// is an invalid packet in every container, and it used to happen silently.
    #[test]
    fn a_repair_past_the_packets_own_pts_is_reported() {
        let opts = FormatOptions::default();
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        // Not reordering, so R19b stays out of it and R22 is the only rule
        // touching DTS.
        f.set_stream_delay(0, 0, false);

        let mut seen = false;
        for pts in [0_i64, 160, 80, 40, 120] {
            let mut p = pkt(Some(pts), Some(pts));
            let r = f.fix(&mut p, TIME_BASE_Q, Rational::ZERO);
            if let (Some(d), Some(pt)) = (p.dts.ticks(), p.pts.ticks())
                && d > pt
            {
                assert!(r.dts_exceeds_pts, "dts {d} > pts {pt} went unreported");
                seen = true;
            } else {
                assert!(!r.dts_exceeds_pts);
            }
        }
        assert!(seen, "this sequence is supposed to provoke the repair");
    }

    #[test]
    fn nofillin_disables_generation_and_repair() {
        let mut opts = FormatOptions::default();
        opts.fflags.insert(FFlags::NOFILLIN);
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        let mut p = pkt(Some(100), None);
        let r = f.fix(&mut p, TIME_BASE_Q, Rational::new(25, 1));
        assert!(p.dts.is_none());
        assert_eq!(p.duration, Duration::ZERO);
        assert!(r.is_clean());
    }

    #[test]
    fn a_repair_that_saturates_says_so() {
        let opts = FormatOptions::default();
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        let mut a = pkt(Some(i64::MAX), Some(i64::MAX));
        let r = f.fix(&mut a, TIME_BASE_Q, Rational::ZERO);
        assert!(!r.dts_overflow);
        // Nothing can come after i64::MAX, so the repair cannot advance.
        let mut b = pkt(Some(0), Some(0));
        let r = f.fix(&mut b, TIME_BASE_Q, Rational::ZERO);
        assert!(r.dts_repaired);
        assert!(r.dts_overflow, "a saturated repair must be reported");
        assert_eq!(b.dts.ticks(), Some(i64::MAX));
        assert!(!r.is_clean());
    }

    #[test]
    fn monotonic_repair_advances_by_the_last_duration() {
        let opts = FormatOptions::default();
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        let mut a = pkt(Some(0), Some(0));
        a.duration = Duration::from_micros(40);
        f.fix(&mut a, TIME_BASE_Q, Rational::ZERO);
        // A backwards DTS is pushed to cur + last_duration.
        let mut b = pkt(Some(0), Some(0));
        let r = f.fix(&mut b, TIME_BASE_Q, Rational::ZERO);
        assert!(r.dts_repaired);
        assert_eq!(b.dts.ticks(), Some(40));
        assert_eq!(f.repaired(0), 1);
    }

    #[test]
    fn ts_discont_suppresses_repair() {
        let opts = FormatOptions::default();
        let mut f = TimestampFixer::new(1, FormatFlags::TS_DISCONT, &opts);
        let mut a = pkt(Some(1000), Some(1000));
        f.fix(&mut a, TIME_BASE_Q, Rational::ZERO);
        let mut b = pkt(Some(10), Some(10));
        let r = f.fix(&mut b, TIME_BASE_Q, Rational::ZERO);
        assert!(!r.dts_repaired);
        assert_eq!(b.dts.ticks(), Some(10));
    }

    #[test]
    fn igndts_drops_dts_when_both_are_present() {
        let mut opts = FormatOptions::default();
        opts.fflags.insert(FFlags::IGNDTS);
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        let mut p = pkt(Some(7), Some(3));
        let r = f.fix(&mut p, TIME_BASE_Q, Rational::ZERO);
        assert!(r.dts_ignored);
        // R19 then restores it from PTS, because this stream does not reorder.
        assert_eq!(p.dts.ticks(), Some(7));
    }

    #[test]
    fn genpts_reorders_a_conforming_sequence() {
        let mut opts = FormatOptions::default();
        opts.fflags.insert(FFlags::GENPTS);
        let mut f = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        f.set_stream_delay(0, 1, true);
        // A classic IPB decode order: dts 0,1,2,3 with one frame of delay.
        let mut got = Vec::new();
        for dts in 0..4i64 {
            let mut p = pkt(None, Some(dts));
            f.fix(&mut p, TIME_BASE_Q, Rational::ZERO);
            got.push(p.pts.ticks());
        }
        // The first packet buffers; from then on the smallest pending DTS is
        // handed out.
        assert_eq!(got, vec![None, Some(0), Some(1), Some(2)]);
    }

    #[test]
    fn duration_from_rate_is_exact_for_ntsc() {
        // 30000/1001 fps is 33366.666… µs; round to nearest.
        let d = duration_from_rate(Rational::new(30_000, 1001)).unwrap();
        assert_eq!(d.as_micros(), 33_367);
        assert_eq!(
            duration_from_rate(Rational::new(25, 1))
                .unwrap()
                .as_micros(),
            40_000
        );
        assert!(duration_from_rate(Rational::ZERO).is_none());
        assert!(duration_from_rate(Rational::UNDEFINED).is_none());
        assert!(duration_from_rate(Rational::INFINITY).is_none());
    }

    #[test]
    fn duration_strategies_are_tried_in_order() {
        let opts = FormatOptions::default();

        // Authoritative container field pins FromStream.
        let e = estimate_duration(
            &DurationInputs {
                container: Some(Duration::from_micros(10_000_000)),
                from_pts: Some(Duration::from_micros(12_000_000)),
                authoritative: true,
                ..DurationInputs::default()
            },
            &opts,
        );
        assert_eq!(e.source, DurationSource::FromStream);
        assert_eq!(e.duration.unwrap().as_micros(), 10_000_000);

        // Not authoritative: the tail scan wins.
        let e = estimate_duration(
            &DurationInputs {
                container: Some(Duration::from_micros(10_000_000)),
                from_pts: Some(Duration::from_micros(12_000_000)),
                ..DurationInputs::default()
            },
            &opts,
        );
        assert_eq!(e.source, DurationSource::FromPts);

        // R14: the longest stream beats a stale container field.
        let e = estimate_duration(
            &DurationInputs {
                container: Some(Duration::from_micros(10_000_000)),
                longest_stream: Some(Duration::from_micros(12_000_000)),
                authoritative: true,
                ..DurationInputs::default()
            },
            &opts,
        );
        assert_eq!(e.duration.unwrap().as_micros(), 12_000_000);

        // Bit rate is the last resort.
        let e = estimate_duration(
            &DurationInputs {
                size: Some(1_000_000),
                bit_rate: Some(8_000_000),
                ..DurationInputs::default()
            },
            &opts,
        );
        assert_eq!(e.source, DurationSource::FromBitrate);
        assert_eq!(e.duration.unwrap().as_micros(), 1_000_000);

        assert_eq!(
            estimate_duration(&DurationInputs::default(), &opts).source,
            DurationSource::Unknown
        );
    }

    #[test]
    fn skip_estimate_duration_from_pts_suppresses_the_scan() {
        let mut opts = FormatOptions::default();
        opts.skip_estimate_duration_from_pts = true;
        let e = estimate_duration(
            &DurationInputs {
                container: Some(Duration::from_micros(10)),
                from_pts: Some(Duration::from_micros(999)),
                ..DurationInputs::default()
            },
            &opts,
        );
        assert_eq!(e.source, DurationSource::FromStream);
    }

    #[test]
    fn start_time_is_the_minimum_across_streams() {
        let tb = Rational::new(1, 90_000);
        let got = container_start_time([
            (Timestamp::new(3753), tb),
            (Timestamp::new(0), tb),
            (Timestamp::NONE, tb),
        ]);
        assert_eq!(got, Some(Duration::ZERO));
        assert_eq!(container_start_time::<[(Timestamp, TimeBase); 0]>([]), None);
    }

    #[test]
    fn rescale_bounds_never_shrink_the_range() {
        let from = Rational::new(1, 3);
        let to = Rational::new(1, 90_000);
        let lo = rescale_bound(Timestamp::new(1), from, to, false);
        let hi = rescale_bound(Timestamp::new(1), from, to, true);
        assert_eq!(lo.ticks(), Some(30_000));
        assert_eq!(hi.ticks(), Some(30_000));
        // A base whose conversion is inexact: 1/3 of a second at 1/90000.
        let from = Rational::new(1, 7);
        let lo = rescale_bound(Timestamp::new(1), from, to, false);
        let hi = rescale_bound(Timestamp::new(1), from, to, true);
        assert!(lo.ticks().unwrap() <= hi.ticks().unwrap());
        assert_eq!(hi.ticks().unwrap() - lo.ticks().unwrap(), 1);
    }

    #[test]
    fn monotonicity_check_follows_the_flag() {
        let strict = FormatFlags::empty();
        let loose = FormatFlags::TS_NONSTRICT;
        assert!(check_monotonic(Timestamp::new(1), Timestamp::new(2), strict).is_ok());
        assert!(check_monotonic(Timestamp::new(1), Timestamp::new(1), strict).is_err());
        assert!(check_monotonic(Timestamp::new(1), Timestamp::new(1), loose).is_ok());
        assert!(check_monotonic(Timestamp::new(1), Timestamp::new(0), loose).is_err());
        // An absent timestamp is not a violation.
        assert!(check_monotonic(Timestamp::NONE, Timestamp::new(0), strict).is_ok());
    }
}
