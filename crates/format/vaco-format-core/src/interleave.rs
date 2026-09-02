//! Muxer-side packet ordering and the timestamp chain that precedes it.
//!
//! A muxer sees packets in one order and one order only, and this module is
//! what decides it. The chain is fixed:
//!
//! ```text
//!  packet, in the input stream's time base
//!   M1  rescale to the output stream's time base            (round to nearest)
//!   M2  + output_ts_offset
//!   M3  + the avoid_negative_ts offset, computed once
//!   M4  monotonicity check — an error, never a repair
//!   N   interleave queue
//!       Muxer::write_packet
//! ```
//!
//! M1 to M4 are [`MuxTimestamps`]; N is [`InterleaveQueue`].
//!
//! # Determinism
//!
//! Muxing is the one part of this subsystem where byte-identical output is
//! fully achievable, because nothing is estimated — so every tie-break here is
//! total and explicit. Two packets sharing a DTS are ordered by stream index
//! and then by arrival sequence, and never by anything a `HashMap` iteration or
//! a thread schedule could perturb.

use std::collections::VecDeque;

use vaco_core::{Duration, Result, Rounding, TimeBase, Timestamp};
use vaco_packet::Packet;

use crate::flags::FormatFlags;
use crate::options::{AvoidNegativeTs, FormatOptions};
use crate::time::{TIME_BASE_Q, check_monotonic};

/// How a container that stores per-track runs wants its packets grouped (N5).
///
/// MOV chunks, AVI `movi` chunks and MXF content packages all store consecutive
/// packets from one track together rather than strictly alternating. The policy
/// says how big a run may get before the queue switches streams.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkPolicy {
    /// Media time in one run, microseconds. Zero disables the limit.
    pub max_duration_us: i64,
    /// Payload bytes in one run. Zero disables the limit.
    pub max_size_bytes: u64,
    /// Bias audio packets earlier by this many microseconds, **for
    /// interleaving purposes only**. It never modifies a timestamp.
    pub audio_preload_us: i64,
}

impl ChunkPolicy {
    /// Read the policy off the option set.
    #[must_use]
    pub fn from_options(opts: &FormatOptions) -> Self {
        Self {
            max_duration_us: i64::from(opts.chunk_duration),
            max_size_bytes: u64::try_from(opts.chunk_size).unwrap_or(0),
            audio_preload_us: i64::from(opts.audio_preload),
        }
    }

    /// Whether any grouping is in force.
    #[must_use]
    pub const fn is_chunked(&self) -> bool {
        self.max_duration_us > 0 || self.max_size_bytes > 0
    }
}

#[derive(Debug)]
struct Queued {
    pkt: Packet,
    /// DTS in microseconds, already biased by `audio_preload`. The sort key,
    /// never written back onto the packet.
    key_us: i64,
    seq: u64,
}

/// Per-stream queues plus the readiness rules over them.
#[derive(Debug)]
pub struct InterleaveQueue {
    per_stream: Vec<VecDeque<Queued>>,
    live: Vec<bool>,
    time_bases: Vec<TimeBase>,
    preload: Vec<bool>,
    max_delta_us: i64,
    chunk: ChunkPolicy,
    seq: u64,
    newest_us: Option<i64>,
    /// The stream currently mid-run, under a chunk policy.
    run_stream: Option<usize>,
    run_bytes: u64,
    run_start_us: Option<i64>,
    /// Accept packets with no DTS. See [`InterleaveQueue::without_timestamps`].
    notimestamps: bool,
}

impl InterleaveQueue {
    /// A queue for `stream_count` streams, configured from `opts`.
    ///
    /// Every stream starts live and every time base starts at
    /// [`TIME_BASE_Q`]; the caller sets the real ones with
    /// [`InterleaveQueue::set_time_base`] as it adds streams to the muxer.
    #[must_use]
    pub fn new(stream_count: usize, opts: &FormatOptions) -> Self {
        Self {
            per_stream: (0..stream_count).map(|_| VecDeque::new()).collect(),
            live: vec![true; stream_count],
            time_bases: vec![TIME_BASE_Q; stream_count],
            preload: vec![false; stream_count],
            max_delta_us: opts.max_interleave_delta,
            chunk: ChunkPolicy::from_options(opts),
            seq: 0,
            newest_us: None,
            run_stream: None,
            run_bytes: 0,
            run_start_us: None,
            notimestamps: false,
        }
    }

    /// Declare a stream's output time base, so DTS can be compared across
    /// streams exactly.
    pub fn set_time_base(&mut self, stream_index: u32, tb: TimeBase) {
        if let Ok(i) = usize::try_from(stream_index)
            && let Some(slot) = self.time_bases.get_mut(i)
        {
            *slot = tb;
        }
    }

    /// Mark a stream as one `audio_preload` applies to.
    pub fn set_preloaded(&mut self, stream_index: u32, preloaded: bool) {
        if let Ok(i) = usize::try_from(stream_index)
            && let Some(slot) = self.preload.get_mut(i)
        {
            *slot = preloaded;
        }
    }

    /// Total packets waiting.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.per_stream.iter().map(VecDeque::len).sum()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.per_stream.iter().all(VecDeque::is_empty)
    }

    /// Streams still expecting packets.
    #[must_use]
    pub fn live_streams(&self) -> usize {
        self.live.iter().filter(|&&l| l).count()
    }

    /// Accept packets with no DTS, ordering them by arrival (`notimestamps`).
    ///
    /// # The contradiction this resolves
    ///
    /// [`MuxTimestamps::apply`] under [`FormatFlags::NOTIMESTAMPS`] *clears*
    /// `pts` and `dts` — that is the flag's entire meaning, and the reference's
    /// `null` and raw muxers carry it. [`InterleaveQueue::push`] then rejected
    /// exactly the packet the other function had just produced. Two parts of
    /// one module disagreeing about what a valid packet is, and between them a
    /// faithful null sink was unbuildable.
    ///
    /// A container that stores no timestamps has nothing to interleave *by*, so
    /// ordering falls back to arrival — which the queue already tracks in `seq`
    /// as its tie-break. Every packet simply ties.
    ///
    /// Opt-in rather than inferred from the flags passed to `new`, because a
    /// missing DTS in a *timestamped* container is a real bug and must keep
    /// erroring. Set this only when the muxer's own `FormatFlags` carry
    /// `NOTIMESTAMPS`.
    #[must_use]
    pub const fn without_timestamps(mut self) -> Self {
        self.notimestamps = true;
        self
    }

    /// Declare a stream finished (N4).
    ///
    /// The queue then interleaves whatever remains among the survivors, so a
    /// short audio track does not stall a long video one at the end of a file.
    pub fn end_stream(&mut self, stream_index: u32) {
        if let Ok(i) = usize::try_from(stream_index)
            && let Some(slot) = self.live.get_mut(i)
        {
            *slot = false;
        }
    }

    /// Queue one packet.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] when the packet names a stream the
    /// queue does not have, or carries no DTS. A muxer needs DTS to order
    /// anything; a packet without one has to be given a value upstream, by
    /// [`MuxTimestamps`], where the decision is visible.
    pub fn push(&mut self, pkt: Packet) -> Result<()> {
        let i = usize::try_from(pkt.stream_index)
            .ok()
            .filter(|&i| i < self.per_stream.len())
            .ok_or(vaco_core::Error::InvalidData(
                "packet names a stream the muxer does not have",
            ))?;
        let tb = self.time_bases.get(i).copied().unwrap_or(TIME_BASE_Q);
        let dts_us = match pkt
            .dts
            .rescale(tb, TIME_BASE_Q, Rounding::default())
            .ticks()
        {
            Some(t) => t,
            // A `notimestamps` container has nothing to order by, so everything
            // ties at zero and `seq` — the existing tie-break — puts them in
            // arrival order. See `without_timestamps`.
            None if self.notimestamps => 0,
            None => {
                return Err(vaco_core::Error::InvalidData(
                    "packet has no dts; interleaving cannot order it",
                ));
            }
        };
        let key_us = if self.preload.get(i).copied().unwrap_or(false) {
            dts_us.saturating_sub(self.chunk.audio_preload_us)
        } else {
            dts_us
        };
        self.newest_us = Some(self.newest_us.map_or(dts_us, |n| n.max(dts_us)));
        self.seq = self.seq.saturating_add(1);
        let seq = self.seq;
        if let Some(q) = self.per_stream.get_mut(i) {
            q.push_back(Queued { pkt, key_us, seq });
        }
        Ok(())
    }

    /// The oldest queued key across all streams, and its stream.
    fn oldest(&self) -> Option<(usize, i64, u64)> {
        self.per_stream
            .iter()
            .enumerate()
            .filter_map(|(i, q)| q.front().map(|h| (i, h.key_us, h.seq)))
            .min_by(|a, b| {
                a.1.cmp(&b.1)
                    .then_with(|| a.0.cmp(&b.0))
                    .then_with(|| a.2.cmp(&b.2))
            })
    }

    /// Whether every live stream has something queued (N1).
    fn all_ready(&self) -> bool {
        self.live
            .iter()
            .enumerate()
            .filter(|&(_, &l)| l)
            .all(|(i, _)| self.per_stream.get(i).is_some_and(|q| !q.is_empty()))
    }

    /// Whether the sparse-stream escape fires (N3).
    ///
    /// A subtitle or timed-metadata track produces packets rarely and would
    /// otherwise stall everything behind it. The default threshold is ten
    /// seconds precisely because an eight-second subtitle gap is normal.
    fn sparse_escape(&self) -> bool {
        let (Some(newest), Some((_, oldest, _))) = (self.newest_us, self.oldest()) else {
            return false;
        };
        newest.saturating_sub(oldest) > self.max_delta_us
    }

    /// Take the next packet in output order, or `None` if the queue is not
    /// ready to commit to one yet.
    ///
    /// `flush` forces a decision: it is what the caller passes at end of
    /// stream, and what `fflags +flush_packets` style low-latency muxing passes
    /// on every packet.
    pub fn next(&mut self, flush: bool) -> Option<Packet> {
        if self.is_empty() {
            self.run_stream = None;
            return None;
        }
        if !flush && !self.all_ready() && !self.sparse_escape() {
            return None;
        }
        let pick = self.pick_stream()?;
        let out = self.per_stream.get_mut(pick)?.pop_front()?;
        self.advance_run(pick, &out);
        Some(out.pkt)
    }

    /// Which stream the next packet comes from: the current run if one is open
    /// and still viable, otherwise the smallest head packet (N2, N5).
    fn pick_stream(&mut self) -> Option<usize> {
        if self.chunk.is_chunked()
            && let Some(run) = self.run_stream
        {
            let viable = self.per_stream.get(run).is_some_and(|q| !q.is_empty())
                && !self.run_exhausted(run)
                && !self.sparse_escape();
            if viable {
                return Some(run);
            }
            // The run is over. Clearing it here is what makes `advance_run`
            // start a fresh one even when the next pick is the same stream.
            self.run_stream = None;
        }
        let pick = self.oldest().map(|(i, _, _)| i)?;
        self.run_stream = None;
        Some(pick)
    }

    /// Whether the open run has hit either chunk limit.
    fn run_exhausted(&self, run: usize) -> bool {
        if self.chunk.max_size_bytes > 0 && self.run_bytes >= self.chunk.max_size_bytes {
            return true;
        }
        if self.chunk.max_duration_us > 0
            && let (Some(start), Some(head)) = (
                self.run_start_us,
                self.per_stream.get(run).and_then(VecDeque::front),
            )
            && head.key_us.saturating_sub(start) >= self.chunk.max_duration_us
        {
            return true;
        }
        false
    }

    fn advance_run(&mut self, pick: usize, taken: &Queued) {
        if self.run_stream == Some(pick) {
            self.run_bytes = self.run_bytes.saturating_add(taken.pkt.len as u64);
        } else {
            self.run_stream = Some(pick);
            self.run_bytes = taken.pkt.len as u64;
            self.run_start_us = Some(taken.key_us);
        }
    }

    /// Drain everything in `(dts, stream, seq)` order. The final flush.
    pub fn drain(&mut self) -> Vec<Packet> {
        let mut out = Vec::new();
        while let Some(p) = self.next(true) {
            out.push(p);
        }
        out
    }
}

/// The plan-shaped one-call form: push `pkt` if there is one, then take a
/// packet if the queue is ready to give one up.
///
/// # Errors
///
/// As [`InterleaveQueue::push`].
pub fn interleave_per_dts(
    queue: &mut InterleaveQueue,
    pkt: Option<Packet>,
    flush: bool,
) -> Result<Option<Packet>> {
    if let Some(p) = pkt {
        queue.push(p)?;
    }
    Ok(queue.next(flush))
}

/// The N7 pass-through policy: never buffer, never reorder.
///
/// MPEG-TS does not interleave in the queue sense — it multiplexes at the
/// 188-byte packet level against a PCR clock, so the real scheduling lives in
/// the muxer and anything this queue did first would be undone. The same
/// applies to any muxer whose output order is fixed by the caller: N6's
/// `write_frame` path, and the segmenting muxers driving a child muxer.
///
/// R26 still applies — [`MuxTimestamps::apply`] has already run, so a
/// non-monotonic DTS was refused before it reached here.
///
/// # Errors
///
/// Never. The signature matches [`interleave_per_dts`] so the two are
/// interchangeable as a [`crate::Muxer::interleave`] body.
pub fn interleave_none(
    queue: &mut InterleaveQueue,
    pkt: Option<Packet>,
    flush: bool,
) -> Result<Option<Packet>> {
    // A policy may be swapped at runtime, so honour anything an earlier
    // buffering policy left behind before passing new packets straight through.
    if let Some(held) = queue.next(true) {
        if let Some(p) = pkt {
            queue.push(p)?;
        }
        return Ok(Some(held));
    }
    let _ = flush;
    Ok(pkt)
}

/// The M1 to M4 chain, applied before a packet reaches the interleave queue.
///
/// One instance per muxer. It holds the single output offset and the per-stream
/// last-DTS, which are the only two pieces of state the chain needs — and both
/// of them are things a muxer would otherwise get subtly wrong.
#[derive(Debug, Clone)]
pub struct MuxTimestamps {
    policy: AvoidNegativeTs,
    /// The M3 offset, in microseconds, computed from the first packet written
    /// across *all* streams and then frozen.
    offset_us: Option<i64>,
    output_ts_offset: Duration,
    flags: FormatFlags,
    last_dts: Vec<Timestamp>,
    notimestamps: bool,
    /// R19c's inputs: each stream's reorder depth (`has_b_frames`, `0` for a
    /// stream that does not reorder) and how many of its packets this chain
    /// has already processed, real DTS or not. See [`Self::apply`]'s R19c
    /// comment for what the pair is for.
    reorder_delay: Vec<u8>,
    decode_index: Vec<u64>,
}

impl MuxTimestamps {
    /// A chain for `stream_count` streams.
    ///
    /// `flags` are the *muxer's*, and they decide two things: whether
    /// `avoid_negative_ts auto` resolves to shifting or to leaving alone, and
    /// whether DTS must be strictly increasing.
    #[must_use]
    pub fn new(stream_count: usize, flags: FormatFlags, opts: &FormatOptions) -> Self {
        Self {
            policy: AvoidNegativeTs::resolve(
                opts.avoid_negative_ts,
                flags.contains(FormatFlags::TS_NEGATIVE),
            ),
            offset_us: None,
            output_ts_offset: opts.output_ts_offset,
            flags,
            last_dts: vec![Timestamp::NONE; stream_count],
            notimestamps: flags.contains(FormatFlags::NOTIMESTAMPS),
            reorder_delay: vec![0; stream_count],
            decode_index: vec![0; stream_count],
        }
    }

    /// Declare a stream's reorder depth (the codec's own `has_b_frames`),
    /// `0` for a stream that does not reorder at all.
    ///
    /// The mux-side mirror of [`crate::time::TimestampFixer::set_stream_delay`]
    /// — same source (`CodecParameters::video::has_b_frames`), same reason:
    /// R19c needs it to tell "no DTS yet because the reorder window is still
    /// filling" from "no DTS at all", which it cannot do from `flags` alone.
    pub fn set_reorder_delay(&mut self, stream_index: u32, delay: u8) {
        if let Some(slot) = usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.reorder_delay.get_mut(i))
        {
            *slot = delay;
        }
    }

    /// The resolved shifting policy.
    #[must_use]
    pub const fn policy(&self) -> AvoidNegativeTs {
        self.policy
    }

    /// The offset M3 settled on, in microseconds, once the first packet has
    /// been seen. Reported by `-copyts` diagnostics.
    #[must_use]
    pub const fn offset_us(&self) -> Option<i64> {
        self.offset_us
    }

    /// Run the chain over one packet, in place.
    ///
    /// `from` is the packet's current time base and `to` is the output stream's.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] when DTS does not advance as the
    /// container requires (M4/R26), or when a container that needs a DTS is
    /// handed a packet with neither DTS nor PTS (R27). Both are errors rather
    /// than repairs: silently repairing here is how files with subtly wrong
    /// durations get made.
    pub fn apply(&mut self, pkt: &mut Packet, from: TimeBase, to: TimeBase) -> Result<()> {
        // M1 — into the output base. pts and dts move together or the stream
        // drifts, which is why `rescale_ts` is one call and not two.
        pkt.rescale_ts(from, to, Rounding::default());

        // R27 — a container that needs DTS gets one, or the packet is refused.
        if self.notimestamps {
            pkt.pts = Timestamp::NONE;
            pkt.dts = Timestamp::NONE;
            return Ok(());
        }
        let stream_idx = usize::try_from(pkt.stream_index).ok();
        let decode_index = stream_idx.and_then(|i| self.decode_index.get(i)).copied();
        if let Some(slot) = stream_idx.and_then(|i| self.decode_index.get_mut(i)) {
            *slot = slot.saturating_add(1);
        }

        if pkt.dts.is_none() {
            if pkt.pts.is_none() {
                return Err(vaco_core::Error::InvalidData(
                    "this container needs timestamps and the packet has none",
                ));
            }
            // R19c — a reordering stream's leading `delay` packets, backfilled
            // from decode order rather than from PTS.
            //
            // `TimestampFixer`'s own R19b (crate::time) already derives a real
            // DTS for every packet once its reorder window has seen `delay + 1`
            // arrivals, and correctly leaves the first `delay` packets at
            // `None` (matches the reference's own `ffprobe` output: `dts =
            // N/A, N/A, 0, 40, 80, …` for `pts = 0, 160, 80, 40, 120, …`).
            // Those leading `None`s cannot simply become `dts = pts`: PTS is a
            // *display* time, already shifted forward by the reorder delay
            // relative to decode order, so the second packet's `pts = 160`
            // would exceed the third packet's real, smaller DTS (`0`) —
            // "non-monotonic dts" on a strict container (`vaco-mux-mp4`) even
            // though the source timestamps were correct throughout.
            //
            // No lookahead is needed: `delay` is a static per-stream fact
            // (`has_b_frames`, threaded in by `set_reorder_delay`) and this
            // packet already carries its own `duration`, so decode order can
            // be reconstructed directly — `dts = (decode_index - delay) *
            // duration`, which is negative for exactly the leading `delay`
            // packets and lands on the real, demuxer-supplied DTS the moment
            // `decode_index == delay`. Measured against the reference's own
            // remux of the same file, `ffmpeg`'s `mp4` output assigns this
            // exact shape (`dts = -1280, -640, 0, 640, …` for a two-frame
            // delay at a 640-tick step) — matching that shape, not those exact
            // values (D6 requires only the latter), is the bar here.
            let delay = stream_idx
                .and_then(|i| self.reorder_delay.get(i))
                .copied()
                .unwrap_or(0);
            let index = decode_index.unwrap_or(0);
            let backfilled = if delay > 0 && index < u64::from(delay) {
                pkt.duration
                    .to_ticks(to)
                    .filter(|&step| step > 0)
                    .map(|step| {
                        let behind = i64::try_from(u64::from(delay) - index).unwrap_or(i64::MAX);
                        Timestamp::new(behind.saturating_neg().saturating_mul(step))
                    })
            } else {
                None
            };
            pkt.dts = backfilled.unwrap_or(pkt.pts);
        }

        // M2 — the user's output offset, in the output base.
        if self.output_ts_offset != Duration::ZERO
            && let Some(ticks) = self.output_ts_offset.to_ticks(to)
        {
            pkt.pts = pkt.pts.offset(ticks);
            pkt.dts = pkt.dts.offset(ticks);
        }

        // M3 — the shift, computed once and applied to every stream. A
        // per-stream offset would desynchronise them.
        self.establish_offset(pkt, to);
        if let Some(off_us) = self.offset_us
            && off_us != 0
            && let Some(ticks) = Duration::from_micros(off_us).to_ticks(to)
        {
            pkt.pts = pkt.pts.offset(ticks);
            pkt.dts = pkt.dts.offset(ticks);
        }

        // M4 — monotonicity, per stream.
        if let Some(prev) = usize::try_from(pkt.stream_index)
            .ok()
            .and_then(|i| self.last_dts.get(i))
            .copied()
        {
            check_monotonic(prev, pkt.dts, self.flags)?;
        }
        if let Some(slot) = usize::try_from(pkt.stream_index)
            .ok()
            .and_then(|i| self.last_dts.get_mut(i))
        {
            *slot = pkt.dts;
        }
        Ok(())
    }

    /// M3's offset, from the first packet across all streams.
    ///
    /// # The reference derives it from `min(pts, dts)`, not from `dts`
    ///
    /// Measured, ffmpeg 8.1, an mpeg4 stream encoded `-bf 2` whose first packet
    /// in decode order reports `dts=0.000000 pts=-0.040000`:
    ///
    /// ```sh
    /// ffmpeg -y -i bf.mp4 -c copy -copyts -fflags +bitexact \
    ///        -avoid_negative_ts make_non_negative -f FMT out
    /// ```
    ///
    /// DTS is already non-negative, so a dts-only rule shifts by nothing. Every
    /// one of mp4, matroska, mpegts, nut and avi shifted by **+0.040000** —
    /// exactly `-pts` — putting PTS at zero and DTS at 0.040000. So the trigger
    /// and the magnitude both come from whichever of the two is smaller.
    ///
    /// Only the pts-negative branch was observed directly; the toolchain to
    /// hand could not construct `dts < 0 <= pts` from a real file. `min` covers
    /// both and degrades to the old behaviour whenever `pts == dts`, which is
    /// every packet of every stream that does not reorder.
    fn establish_offset(&mut self, pkt: &Packet, to: TimeBase) {
        if self.offset_us.is_some() || self.policy == AvoidNegativeTs::Disabled {
            self.offset_us.get_or_insert(0);
            return;
        }
        let dts_us = pkt.dts.rescale(to, TIME_BASE_Q, Rounding::Down).ticks();
        let pts_us = pkt.pts.rescale(to, TIME_BASE_Q, Rounding::Down).ticks();
        let earliest = match (pts_us, dts_us) {
            (Some(p), Some(d)) => Some(p.min(d)),
            (p, d) => p.or(d),
        };
        let Some(first_us) = earliest else {
            return;
        };
        self.offset_us = Some(match self.policy {
            AvoidNegativeTs::MakeZero => first_us.saturating_neg(),
            AvoidNegativeTs::MakeNonNegative if first_us < 0 => first_us.saturating_neg(),
            _ => 0,
        });
    }
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
    use vaco_core::Rational;
    use vaco_limits::{Budget, Limits};

    fn pkt(stream: u32, dts: i64, len: usize) -> Packet {
        let mut budget = Budget::new(Limits::strict());
        let mut p = Packet::from_slice(&mut budget, &vec![0u8; len]).unwrap();
        p.stream_index = stream;
        p.dts = Timestamp::new(dts);
        p.pts = Timestamp::new(dts);
        p
    }

    fn ms(opts: &FormatOptions) -> InterleaveQueue {
        InterleaveQueue::new(2, opts)
    }

    #[test]
    fn nothing_is_emitted_until_every_live_stream_has_a_packet() {
        let opts = FormatOptions::default();
        let mut q = ms(&opts);
        q.push(pkt(0, 0, 1)).unwrap();
        assert!(q.next(false).is_none());
        q.push(pkt(1, 5, 1)).unwrap();
        let out = q.next(false).unwrap();
        assert_eq!(out.stream_index, 0);
    }

    #[test]
    fn selection_is_by_dts_then_index_then_arrival() {
        let opts = FormatOptions::default();
        let mut q = ms(&opts);
        // Identical DTS on both streams: the lower index wins.
        q.push(pkt(1, 10, 1)).unwrap();
        q.push(pkt(0, 10, 1)).unwrap();
        assert_eq!(q.next(true).unwrap().stream_index, 0);
        assert_eq!(q.next(true).unwrap().stream_index, 1);
    }

    #[test]
    fn ordering_is_independent_of_arrival_order() {
        let opts = FormatOptions::default();
        let mut a = ms(&opts);
        let mut b = ms(&opts);
        // Interleaved arrival.
        for (s, d) in [(0u32, 0i64), (1, 1), (0, 2), (1, 3)] {
            a.push(pkt(s, d, 1)).unwrap();
        }
        // The same packets, one whole stream at a time. Per-stream order is
        // preserved in both, which is the only thing a muxer may assume: the
        // queue orders *between* streams and never reorders within one.
        for (s, d) in [(1u32, 1i64), (1, 3), (0, 0), (0, 2)] {
            b.push(pkt(s, d, 1)).unwrap();
        }
        let ka: Vec<_> = a.drain().iter().map(|p| p.dts.ticks()).collect();
        let kb: Vec<_> = b.drain().iter().map(|p| p.dts.ticks()).collect();
        assert_eq!(ka, kb);
        assert_eq!(ka, vec![Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn a_sparse_stream_does_not_stall_the_queue() {
        let opts = FormatOptions::default();
        let mut q = ms(&opts);
        // Stream 1 is a subtitle track and never speaks.
        for i in 0..40i64 {
            q.push(pkt(0, i * 1_000_000, 1)).unwrap();
        }
        // Once the spread passes max_interleave_delta (10 s), packets flow.
        let mut emitted = 0;
        while q.next(false).is_some() {
            emitted += 1;
        }
        assert!(emitted > 0, "the sparse escape never fired");
        assert!(
            emitted < 40,
            "the escape emitted everything; it should stop at the threshold"
        );
    }

    #[test]
    fn ending_a_stream_lets_the_rest_drain() {
        let opts = FormatOptions::default();
        let mut q = ms(&opts);
        q.push(pkt(0, 0, 1)).unwrap();
        assert!(q.next(false).is_none());
        q.end_stream(1);
        assert!(q.next(false).is_some());
        assert_eq!(q.live_streams(), 1);
    }

    #[test]
    fn cross_base_ordering_is_exact() {
        let opts = FormatOptions::default();
        let mut q = ms(&opts);
        // 1/90000 video against 1/48000 audio: the same instant in both.
        q.set_time_base(0, Rational::new(1, 90_000));
        q.set_time_base(1, Rational::new(1, 48_000));
        q.push(pkt(0, 90_000, 1)).unwrap(); // 1.000000 s
        q.push(pkt(1, 47_999, 1)).unwrap(); // 0.999979 s
        assert_eq!(q.next(true).unwrap().stream_index, 1);
        assert_eq!(q.next(true).unwrap().stream_index, 0);
    }

    #[test]
    fn packets_without_dts_are_refused() {
        let opts = FormatOptions::default();
        let mut q = ms(&opts);
        let mut p = pkt(0, 0, 1);
        p.dts = Timestamp::NONE;
        assert!(q.push(p).is_err());
        let mut p = pkt(9, 0, 1);
        p.stream_index = 9;
        assert!(q.push(p).is_err());
    }

    #[test]
    fn chunking_groups_consecutive_packets_from_one_stream() {
        let mut opts = FormatOptions::default();
        opts.chunk_size = 3;
        let mut q = InterleaveQueue::new(2, &opts);
        for i in 0..6i64 {
            q.push(pkt(0, i * 1000, 1)).unwrap();
            q.push(pkt(1, i * 1000, 1)).unwrap();
        }
        let order: Vec<u32> = q.drain().iter().map(|p| p.stream_index).collect();
        // Runs of three from one stream before switching, rather than strict
        // alternation.
        assert_eq!(order.len(), 12);
        let runs = order.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(runs < 11, "chunking did not group anything: {order:?}");
    }

    #[test]
    fn audio_preload_biases_selection_without_touching_timestamps() {
        let mut opts = FormatOptions::default();
        opts.audio_preload = 500_000;
        let mut q = InterleaveQueue::new(2, &opts);
        q.set_preloaded(1, true);
        // Audio is genuinely later, but the preload pulls it forward.
        q.push(pkt(0, 0, 1)).unwrap();
        q.push(pkt(1, 400_000, 1)).unwrap();
        let first = q.next(true).unwrap();
        assert_eq!(first.stream_index, 1);
        assert_eq!(first.dts.ticks(), Some(400_000), "timestamp was modified");
    }

    #[test]
    fn avoid_negative_ts_shifts_once_across_every_stream() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(2, FormatFlags::empty(), &opts);
        assert_eq!(m.policy(), AvoidNegativeTs::MakeNonNegative);

        let mut a = pkt(0, -250, 1);
        m.apply(&mut a, tb, tb).unwrap();
        assert_eq!(a.dts.ticks(), Some(0));
        assert_eq!(m.offset_us(), Some(250_000));

        // The second stream gets the *same* offset, not its own.
        let mut b = pkt(1, 0, 1);
        m.apply(&mut b, tb, tb).unwrap();
        assert_eq!(b.dts.ticks(), Some(250));
    }

    #[test]
    fn make_zero_shifts_in_both_directions() {
        let mut opts = FormatOptions::default();
        opts.avoid_negative_ts = 2;
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::empty(), &opts);
        let mut p = pkt(0, 5000, 1);
        m.apply(&mut p, tb, tb).unwrap();
        assert_eq!(p.dts.ticks(), Some(0));
    }

    #[test]
    fn auto_leaves_a_negative_capable_container_alone() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::TS_NEGATIVE, &opts);
        assert_eq!(m.policy(), AvoidNegativeTs::Disabled);
        let mut p = pkt(0, -250, 1);
        m.apply(&mut p, tb, tb).unwrap();
        assert_eq!(p.dts.ticks(), Some(-250));
    }

    #[test]
    fn output_ts_offset_is_applied_in_the_output_base() {
        let mut opts = FormatOptions::default();
        opts.output_ts_offset = Duration::from_micros(1_000_000);
        opts.avoid_negative_ts = 0;
        let mut m = MuxTimestamps::new(1, FormatFlags::empty(), &opts);
        let mut p = pkt(0, 0, 1);
        m.apply(&mut p, Rational::new(1, 1000), Rational::new(1, 90_000))
            .unwrap();
        assert_eq!(p.dts.ticks(), Some(90_000));
    }

    #[test]
    fn non_monotonic_dts_is_an_error_not_a_repair() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::empty(), &opts);
        m.apply(&mut pkt(0, 10, 1), tb, tb).unwrap();
        let mut p = pkt(0, 10, 1);
        assert!(m.apply(&mut p, tb, tb).is_err());

        // TS_NONSTRICT tolerates equality but not a decrease.
        let mut m = MuxTimestamps::new(1, FormatFlags::TS_NONSTRICT, &opts);
        m.apply(&mut pkt(0, 10, 1), tb, tb).unwrap();
        m.apply(&mut pkt(0, 10, 1), tb, tb).unwrap();
        let mut p = pkt(0, 9, 1);
        assert!(m.apply(&mut p, tb, tb).is_err());
    }

    /// R19c: a reordering stream's leading `delay` packets
    /// reach `apply` with `dts = None` by design (`TimestampFixer`'s R19b
    /// leaves them that way rather than guess) — this is the fix that
    /// `pts` is not a valid stand-in for that window, because it produces a
    /// non-monotonic sequence the moment the third packet's real, smaller
    /// DTS arrives. Values are the measured shape of a real 2-B-frame
    /// x264/Matroska file (`pts = 0, 160, 80, 40, …`, `duration = 40`
    /// throughout); the exact backfilled numbers (`-80, -40`) do not have
    /// to match the reference's own choice (D6), only land before `0` and
    /// keep advancing, which the sequence-strict `check_monotonic` call at
    /// the end of this test enforces directly.
    #[test]
    fn a_reordering_streams_leading_window_is_backfilled_not_copied_from_pts() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        // TS_NEGATIVE (mp4's own flag, matching the real repro this pins) so
        // M3's `avoid_negative_ts auto` leaves the backfilled negative values
        // alone rather than shifting the whole stream — a real, separate
        // stage this test is not about, and the reason a first attempt at
        // this test measured `[0, 40, 80]` instead: `FormatFlags::empty()`
        // resolves `auto` to "shift to non-negative", which is correct for a
        // container that cannot express negative DTS, just not what this
        // assertion is checking.
        let mut m = MuxTimestamps::new(1, FormatFlags::TS_NEGATIVE, &opts);
        m.set_reorder_delay(0, 2);

        let reorder_pkt = |pts: i64| {
            let mut p = pkt(0, pts, 1);
            p.dts = Timestamp::NONE;
            p.duration = Duration::from_micros(40_000); // 40 ticks @ 1/1000
            p
        };

        let mut p0 = reorder_pkt(0);
        m.apply(&mut p0, tb, tb).unwrap();
        let mut p1 = reorder_pkt(160);
        m.apply(&mut p1, tb, tb).unwrap();
        // The real DTS a conforming demuxer supplies once its own reorder
        // window has filled — this packet's `dts` was never `None`.
        let mut p2 = pkt(0, 0, 1);
        p2.pts = Timestamp::new(80);
        p2.duration = Duration::from_micros(40_000);
        m.apply(&mut p2, tb, tb).unwrap();

        let dts: Vec<_> = [&p0, &p1, &p2].iter().map(|p| p.dts.ticks()).collect();
        assert_eq!(dts, vec![Some(-80), Some(-40), Some(0)]);
        assert!(
            dts.is_sorted_by(|a, b| a < b),
            "must strictly increase: {dts:?}"
        );
    }

    /// A stream with no declared reorder delay is unaffected: `dts = pts`
    /// exactly as before R19c, since `set_reorder_delay` is never called
    /// (the default is `0`, matching every existing caller before this test
    /// was written).
    #[test]
    fn a_missing_dts_with_no_declared_reorder_delay_still_copies_pts() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::empty(), &opts);
        let mut p = pkt(0, 42, 1);
        p.dts = Timestamp::NONE;
        m.apply(&mut p, tb, tb).unwrap();
        assert_eq!(p.dts.ticks(), Some(42));
    }

    /// The two halves of `notimestamps` now agree. `MuxTimestamps` clears the
    /// fields and `InterleaveQueue` accepts what it produced — which is the
    /// whole point of the flag, and was impossible before: one function made a
    /// packet the other rejected.
    #[test]
    fn notimestamps_survives_the_round_trip_through_the_queue() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::NOTIMESTAMPS, &opts);
        let mut q = InterleaveQueue::new(1, &opts).without_timestamps();

        for pts in [0_i64, 100, 200] {
            let mut p = pkt(0, pts, 1);
            m.apply(&mut p, tb, tb).unwrap();
            assert!(p.dts.is_none(), "notimestamps must clear dts");
            // A notimestamps queue accepts a packet with no dts; that is the
            // whole assertion.
            q.push(p).unwrap();
        }
        // Arrival order, since everything ties at zero and `seq` breaks it.
        let mut seen = 0;
        while q.next(true).is_some() {
            seen += 1;
        }
        assert_eq!(seen, 3, "every packet must come back out");
    }

    #[test]
    fn a_notimestamps_container_drops_both_fields() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::NOTIMESTAMPS, &opts);
        let mut p = pkt(0, 10, 1);
        m.apply(&mut p, tb, tb).unwrap();
        assert!(p.dts.is_none() && p.pts.is_none());
    }

    #[test]
    fn a_packet_with_no_timestamps_at_all_is_refused() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::empty(), &opts);
        let mut p = pkt(0, 0, 1);
        p.dts = Timestamp::NONE;
        p.pts = Timestamp::NONE;
        assert!(m.apply(&mut p, tb, tb).is_err());
    }

    #[test]
    fn dts_is_filled_from_pts_when_only_pts_exists() {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1000);
        let mut m = MuxTimestamps::new(1, FormatFlags::empty(), &opts);
        let mut p = pkt(0, 7, 1);
        p.dts = Timestamp::NONE;
        m.apply(&mut p, tb, tb).unwrap();
        assert_eq!(p.dts.ticks(), Some(7));
    }
}
