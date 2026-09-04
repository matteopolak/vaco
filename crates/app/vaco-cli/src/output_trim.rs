//! `-t`/`-to`, **output**-positioned: stop *writing* a packet once its own
//! presentation time reaches the requested bound. See `crate::seek_trim` for
//! the input-side form (`-ss`/`-t`/`-to` before `-i`, applied by seeking the
//! demuxer) — this is the other half the reference exposes for the same two
//! options, and, until now, the one `crate::cli` parsed but never consumed
//! (`crate::cli::refuse_unimplemented_options`'s doc has the measurement).
//!
//! # Scope, deliberately
//!
//! - **No output-side `-ss`.** The reference's output `-ss` decodes and
//!   discards up to the timestamp rather than seeking — a materially
//!   different, unimplemented feature, refused explicitly by
//!   `crate::cli::refuse_unimplemented_options` rather than silently doing
//!   nothing. [`OutputTrim`] therefore only ever measures from the output's
//!   own start — see "Anchored to the first packet" below for what that
//!   means when an upstream `-ss` is also present.
//! - **Per stream, independently.** Unlike [`crate::seek_trim::SeekTrim`],
//!   which seeks one physical demuxer position and needs a single reference
//!   stream to pick it, trimming on write is just "drop this packet" —
//!   every stream's own packets are checked against their own presentation
//!   time, with no shared anchor. OBSERVED (`ffmpeg -i <10s av file> -t 3
//!   -c copy out.mp4`): video and audio both end within their own 3 s
//!   window (`77` and `130` packets respectively, on a 25 fps / 44100 Hz
//!   fixture) — neither stream's cutoff depends on the other's packet
//!   timing.
//! - **The bound is checked against the packet's own `pts`, in whatever base
//!   `vaco_format_core::mux::MuxBuilder` actually rescaled it into.** By the
//!   time [`Muxer::write_packet`]/[`Muxer::interleave`] are called, M1–M4
//!   has already rescaled every packet from its input time base into that
//!   stream's `output_time_base` — but that is **not** always
//!   [`Muxer::stream_time_base`]'s answer: `MuxBuilder::add_stream_with_
//!   matrix` resolves it as `stream_time_base().or(input_time_base).
//!   unwrap_or(TIME_BASE_Q)`, so a muxer with no opinion (`-f null`'s
//!   `NullMuxer`, which answers `None` deliberately — see its own doc) is
//!   still rescaled, just into the *input* stream's own base instead of one
//!   the muxer chose. OBSERVED (`ffmpeg -i <10s av file> -t 3 -c copy -f
//!   null -`): `frame=77`, matching the real-muxer measurement above bit for
//!   bit — the reference's own `-t` is no less exact against `null` than
//!   against a real container. Reading only [`Muxer::stream_time_base`]
//!   here would silently stop trimming the moment a wrapped muxer answers
//!   `None`, which is exactly the `-f null` case every test in this crate's
//!   own suite defaults to — so [`OutputTrim`] also captures the
//!   `input_time_base` [`vaco_format_core::mux::MuxBuilder::add_stream_
//!   with_matrix`] hands down through [`StreamSpec::time_base`] at
//!   [`Muxer::add_stream_with`] time, and falls back to it the same way
//!   `MuxBuilder` itself does, rather than reimplementing the wrapped
//!   muxer's own [`Muxer::stream_time_base`] choice.
//! - **Multiple outputs from the same input are already independent of one
//!   another** in this crate ([`crate::exec::run_pipeline`] opens one
//!   `OutputTrim` per output), so `ffmpeg -i in.mp4 -t 2 out1.mp4 -t 4
//!   out2.mp4`'s per-output `-t` values need no special plumbing here:
//!   OBSERVED, the reference gives `out1.mp4` `duration=2.08` and
//!   `out2.mp4` `duration=4.08` from the same input and the same run.
//!
//! # Anchored to the first packet, not to absolute zero
//!
//! `-ss` (input-side) does not rewrite surviving packets' timestamps down to
//! zero — it only stops [`crate::seek_trim::SeekTrim`] from forwarding the
//! ones before the seek target. [`vaco_format_core::interleave::
//! MuxTimestamps::apply`]'s own offset (M3) only ever fires for a *negative*
//! first timestamp (`avoid_negative_ts`); a forward seek to a positive time
//! never produces one, so it leaves the surviving packets' `pts`/`dts` at
//! their original absolute values. DEBUGGED: `ffmpeg -ss 2 -i <10s av file>
//! -c copy out.mp4`'s first video frame reports `pts_time=0.000000` — but
//! that reading comes from `vaco-mux-mp4`'s own `elst`/`media_time` (an edit
//! list adjusts *presentation*, read by `ffprobe`'s frame decoder), not from
//! the packet timestamps [`Muxer::write_packet`] actually receives, which
//! this layer reads directly and which stay near `2_000_000`.
//!
//! Checking that against an `end_us` of `3_000_000` (`-t 3`) cut the output
//! off after only one second of content, not three: MEASURED, `-ss 2 -t 3`
//! before this fix produced `duration=1.000000`; the reference produces
//! `duration=3.08` for the same invocation, matching plain `-t 3` with no
//! `-ss` at all (this layer never sees `-ss`'s value — see "No output-side
//! `-ss`" above — so it has no way to know `2_000_000` needs subtracting
//! except by watching for it). The fix: each stream's bound is measured
//! from *that stream's own first packet*, captured the first time
//! [`Muxer::write_packet`] sees it, not from a literal `0`.
//!
//! # Sticky per-stream drop, not per-packet
//!
//! Once a stream has dropped one packet for being at or past the bound,
//! [`OutputTrim`] drops every later packet on that stream too, rather than
//! resuming forwarding if a still-later packet's own `pts` happens to land
//! back under the bound.
//!
//! A B-frame-reordered video stream makes that resumption a real case, not
//! a hypothetical one: decode order interleaves a high-`pts` packet ahead of
//! one or two lower-`pts` packets that still decode after it (the reference
//! stream's own reorder window). Checking each packet purely on its own
//! `pts` therefore drops an *interior* packet in [`Muxer::write_packet`]'s
//! call sequence while still forwarding ones that arrive right after it —
//! DEBUGGED on a 25 fps fixture: dts-order arrivals `…70, 71, 72, 73, 74…`
//! carried `pts` `…2.84, 3.04, 2.96, 2.92, 3.00…`; a naive per-packet check
//! forwards `70`, drops `71` (`pts` `3.04`), forwards `72` and `73`, drops
//! `74`. `vaco-mux-mp4`'s sample table is built from exactly the sequence of
//! `dts`s it is handed, in call order, and assumes each entry is the next
//! one after the last — so the resulting hole (`70` then `72`, never `71`)
//! corrupted the written file's own duration bookkeeping badly enough that
//! `ffprobe -count_frames` reported *two fewer* frames than were actually
//! forwarded (`73` instead of `75`), silently losing the two genuinely
//! valid trailing packets (`72`, `73`) along with the one that should have
//! been dropped. [`crate::seek_trim::SeekTrim`] never creates this shape of
//! hole: an input-side `Eof` stops the whole demuxer, so whatever prefix
//! reached the muxer before it is always contiguous.
//!
//! The fix is the sticky flag: the first packet at or past the bound ends
//! that stream for good, so [`OutputTrim`] only ever forwards a *prefix* of
//! each stream's arrival order — exactly the shape [`Muxer::write_packet`]'s
//! callers already assume. This trades a couple of trailing B-frames worth
//! of leniency (the reference's own `dts`-based cutoff, already documented
//! as an accepted divergence in `crate::seek_trim`) for never handing a
//! muxer a sequence it cannot represent — a strictly better trade than a
//! silently corrupted output file.
//!
//! # A trimming muxer cannot advertise `NOTIMESTAMPS`
//!
//! [`vaco_format_core::interleave::MuxTimestamps::apply`] (M18) *clears*
//! both `pts` and `dts` outright for a muxer whose [`Muxer::flags`]
//! declares [`FormatFlags::NOTIMESTAMPS`] — and `-f null`'s own
//! `vaco-mux-utility::NullSinkMuxer` declares exactly that (a discard-only
//! sink has no need of timing). `MuxWriter::write_packet` applies M18
//! *before* calling [`Muxer::interleave`]/[`Muxer::write_packet`] at all,
//! so by the time this layer's own `write_packet` runs, the packet it
//! receives already has no timestamp left to read — DEBUGGED: `pts_ticks`
//! and `dts_ticks` both `None` on every packet, against a fixture whose
//! demuxer unconditionally sets `pts` (confirmed by reading `vaco-demux-
//! matroska`'s own packet-construction code). This is not a corner case:
//! `-f null` is this crate's own test suite's default output, named as
//! exactly this risk in "Per stream, independently" above, and it is what
//! `crate::tests::output_side_t_stops_writing_early` exists to catch.
//!
//! `MuxWriter::new` reads `flags()` on the *outermost* `Muxer` exactly
//! once, before construction — the same object [`crate::exec::run_pipeline`]
//! builds by wrapping [`OutputTrim`] around the real muxer. So
//! [`Muxer::flags`] here masks `NOTIMESTAMPS` off the wrapped muxer's own
//! answer, which is the only point in this pipeline that can change what
//! M18 sees. This is scoped as narrowly as `OutputTrim` itself already is:
//! [`OutputTrim::wrap`] returns the muxer unwrapped whenever there is no
//! `-t`/`-to` on this output at all (the overwhelmingly common case), so no
//! output's `NOTIMESTAMPS` behavior changes unless that output specifically
//! asked to be trimmed. The real underlying muxer (`NullSinkMuxer` or any
//! other `NOTIMESTAMPS` container) never reads `pts`/`dts` for anything of
//! its own, so leaving them populated costs it nothing; what changes is
//! [`vaco_format_core::interleave::InterleaveQueue`]'s own ordering mode
//! (M18's other half, `without_timestamps`), which reverts to ordering by
//! real `dts` instead of arrival order — the correct behavior for a source
//! that does carry real timestamps, which every demuxer-sourced stream in
//! this crate does. A source that genuinely has none and relied on
//! `NOTIMESTAMPS`'s leniency while *also* asking for `-t` on that same
//! output is not a combination this fix has a way to serve correctly
//! either way — untested, and left as a named gap rather than a silent one.
//!
//! # `-t`/`-to` priority and `-to <= 0`
//!
//! Resolved once, before either output or input side ever sees a bound —
//! see `crate::cli::end_bound_of`/`crate::cli::validate_bounds`, both shared
//! with the input-side form. [`OutputTrim::wrap`] only ever receives an
//! already-validated, already-prioritised [`EndBound`].

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Rational, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::interleave::InterleaveQueue;
use vaco_format_core::metadata::MuxMetadata;
use vaco_format_core::mux::{BitstreamAction, CodecSupport};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Muxer, StreamSpec};
use vaco_packet::Packet;

use crate::cli::EndBound;

/// Wraps a [`Muxer`], dropping every packet whose own presentation time is
/// at or past the requested bound rather than forwarding it.
///
/// No `Debug` derive, same reason as [`crate::seek_trim::SeekTrim`]: `Box<dyn
/// Muxer>` cannot provide one.
pub struct OutputTrim {
    inner: Box<dyn Muxer>,
    /// The bound in microseconds, measured from this output's own start
    /// (packet time `0` in each stream's own time base) — see the module
    /// doc for why there is no separate "relative to `-ss`" case the way
    /// [`crate::seek_trim::SeekTrim::end_us`] has one.
    end_us: i64,
    /// Per-stream `input_time_base`, captured from [`StreamSpec::time_base`]
    /// at [`Muxer::add_stream_with`] time — see the module doc's
    /// `MuxBuilder` fallback explanation. Indexed by stream index; `None` at
    /// an index means that stream was declared through plain
    /// [`Muxer::add_stream`] (no `spec`, hence no better answer than
    /// [`TIME_BASE_Q`] either) rather than genuinely unknown.
    input_time_base: Vec<Option<Rational>>,
    /// Sticky per-stream "this stream is done" flag — see the module doc's
    /// "why sticky, not per-packet" section. Indexed by stream index,
    /// parallel to `input_time_base`.
    ended: Vec<bool>,
    /// Each stream's own first-seen packet time, in absolute microseconds —
    /// the anchor "output start" is measured from. See the module doc's
    /// "Anchored to the first packet, not to absolute zero" section: an
    /// upstream `-ss` does not rewrite packet timestamps down to zero, it
    /// only stops feeding the pre-seek ones in, so this layer's own bound
    /// must subtract out whatever absolute time the surviving packets
    /// happen to start at. `None` until that stream's first packet arrives.
    first_us: Vec<Option<i64>>,
}

impl core::fmt::Debug for OutputTrim {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OutputTrim")
            .field("end_us", &self.end_us)
            .finish_non_exhaustive()
    }
}

impl OutputTrim {
    /// Wrap `inner` in a trimming layer if `end` names one, otherwise return
    /// `inner` unwrapped — the overwhelmingly common case (no `-t`/`-to` on
    /// this output) pays nothing beyond the `match`.
    #[must_use]
    pub fn wrap(inner: Box<dyn Muxer>, end: Option<EndBound>) -> Box<dyn Muxer> {
        let end_us = match end {
            Some(EndBound::AfterSeek(d) | EndBound::Absolute(d)) => d.as_micros(),
            None => return inner,
        };
        Box::new(Self {
            inner,
            end_us,
            input_time_base: Vec::new(),
            ended: Vec::new(),
            first_us: Vec::new(),
        })
    }

    /// Record stream `index`'s `input_time_base`, if `index` is the next one
    /// in sequence -- streams are always declared in order (`MuxBuilder`'s
    /// own invariant, checked against `self.streams.len()`), so a mismatch
    /// here would mean the wrapped muxer renumbered, which `MuxBuilder`
    /// itself already refuses.
    fn record_stream(&mut self, index: u32, time_base: Option<Rational>) {
        if index as usize == self.input_time_base.len() {
            self.input_time_base.push(time_base);
            self.ended.push(false);
            self.first_us.push(None);
        }
    }

    /// Grow the per-stream bookkeeping to cover `index`, if it doesn't
    /// already. [`Self::record_stream`] (called from `add_stream`/
    /// `add_stream_with`) is what normally does this, in the real pipeline
    /// where `MuxBuilder` always declares a stream before the first packet
    /// on it — but nothing here should silently disable trimming just
    /// because a caller (a test, or some future wrapped muxer with a looser
    /// contract) writes to a stream index it never declared. Filling with
    /// defaults up to `index` handles an out-of-order declaration too, not
    /// only a skipped one.
    fn ensure_stream(&mut self, index: usize) {
        while self.input_time_base.len() <= index {
            self.input_time_base.push(None);
            self.ended.push(false);
            self.first_us.push(None);
        }
    }

    /// `pkt`'s own presentation time, in absolute microseconds, in whatever
    /// base [`vaco_format_core::mux::MuxBuilder`] actually rescaled it into
    /// — see the module doc for why that is not simply [`Muxer::stream_time_
    /// base`]'s own answer.
    fn packet_us(&self, pkt: &Packet) -> Option<i64> {
        let time_base = self
            .inner
            .stream_time_base(pkt.stream_index)
            .or_else(|| {
                self.input_time_base
                    .get(pkt.stream_index as usize)
                    .copied()
                    .flatten()
            })
            .unwrap_or(TIME_BASE_Q);
        pkt.pts.to_duration(time_base).map(Duration::as_micros)
    }
}

impl Muxer for OutputTrim {
    // Masks `NOTIMESTAMPS` off the wrapped muxer's own answer -- see the
    // module doc's "A trimming muxer cannot advertise NOTIMESTAMPS"
    // section. `MuxWriter::new` reads `flags()` exactly once, on this
    // outermost `Muxer`, before `write_packet` is ever called, so this is
    // the only place that can change what M18's wipe sees.
    fn flags(&self) -> FormatFlags {
        self.inner.flags() - FormatFlags::NOTIMESTAMPS
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let index = self.inner.add_stream(params)?;
        self.record_stream(index, None);
        Ok(index)
    }

    // Forwarded explicitly, not inherited from the default -- the same trap
    // `vaco_format_core::Muxer::add_stream_with`'s own doc names, and the
    // same reason `crate::nullmux::TallyingMuxer` (which wraps `OutputTrim`,
    // not the other way around — see `crate::exec::run_pipeline`) forwards
    // it too. Also where this module's own `input_time_base` fallback
    // (module doc) is captured: `spec.time_base` is exactly the
    // `input_time_base` `MuxBuilder::add_stream_with_matrix` is about to
    // resolve `output_time_base` from.
    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        let index = self.inner.add_stream_with(params, spec)?;
        self.record_stream(index, spec.time_base);
        Ok(index)
    }

    fn init(&mut self) -> Result<()> {
        self.inner.init()
    }

    fn write_header(&mut self) -> Result<()> {
        self.inner.write_header()
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let idx = packet.stream_index as usize;
        self.ensure_stream(idx);
        if self.ended.get(idx).copied().unwrap_or(false) {
            return Ok(());
        }
        let us = self.packet_us(packet);
        // Anchor to this stream's own first packet -- see the module doc
        // and the `first_us` field's own doc for why an upstream `-ss`
        // means this cannot simply be `0`. `ensure_stream` above guarantees
        // `idx` is in range, so `first_us[idx]` is always `Some(_)` here;
        // its *contents* start `None` until the first packet is seen.
        let first = if let Some(slot) = self.first_us.get_mut(idx) {
            if slot.is_none() {
                *slot = us;
            }
            *slot
        } else {
            None
        };
        let local_us = match (us, first) {
            (Some(us), Some(first)) => Some(us.saturating_sub(first)),
            _ => None,
        };
        if local_us.is_some_and(|us| us >= self.end_us) {
            if let Some(slot) = self.ended.get_mut(idx) {
                *slot = true;
            }
            return Ok(());
        }
        self.inner.write_packet(packet)
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.inner.write_trailer()
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        self.inner.stream_time_base(stream_index)
    }

    fn interleave(
        &mut self,
        queue: &mut InterleaveQueue,
        packet: Option<Packet>,
        flush: bool,
    ) -> Result<Option<Packet>> {
        self.inner.interleave(queue, packet, flush)
    }

    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        packet: &Packet,
    ) -> Result<BitstreamAction> {
        self.inner.check_bitstream(params, packet)
    }

    fn query_codec(&self, codec: CodecId, strict: i32) -> CodecSupport {
        self.inner.query_codec(codec, strict)
    }

    fn write_flush(&mut self) -> Result<()> {
        self.inner.write_flush()
    }

    fn set_metadata(&mut self, metadata: &MuxMetadata) -> Result<()> {
        self.inner.set_metadata(metadata)
    }

    fn set_bitexact(&mut self, bitexact: bool) {
        self.inner.set_bitexact(bitexact);
    }

    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        self.inner.set_option(name, value)
    }

    fn bind_url(&mut self, url: &str) -> Result<()> {
        self.inner.bind_url(url)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use vaco_core::{Rational, Timestamp};
    use vaco_limits::Budget;

    /// A fake muxer over a shared, in-memory sink, recording every packet
    /// that actually reached [`Muxer::write_packet`] -- so `OutputTrim`'s
    /// dropping logic is tested without any real container. Shared (rather
    /// than owned) because `OutputTrim::wrap` takes the `Fake` by value into
    /// a `Box<dyn Muxer>` that never gives it back, the same reason
    /// `crate::nullmux::Sink` is `Arc<Mutex<_>>`.
    struct Fake {
        time_base: Rational,
        written: Arc<Mutex<Vec<i64>>>,
    }

    impl Muxer for Fake {
        fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
            Ok(0)
        }

        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }

        fn write_packet(&mut self, packet: &Packet) -> Result<()> {
            self.written
                .lock()
                .unwrap()
                .push(packet.pts.ticks().unwrap_or(0));
            Ok(())
        }

        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }

        fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
            (stream_index == 0).then_some(self.time_base)
        }
    }

    fn packet(pts_ticks: i64) -> Packet {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, &[0]).unwrap();
        pkt.pts = Timestamp::new(pts_ticks);
        pkt
    }

    /// µs-resolution time base, so ticks in these tests double as
    /// microseconds directly -- the same convenience `seek_trim`'s own tests
    /// use.
    fn fake() -> (Box<dyn Muxer>, Arc<Mutex<Vec<i64>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let muxer: Box<dyn Muxer> = Box::new(Fake {
            time_base: Rational::new(1, 1_000_000),
            written: written.clone(),
        });
        (muxer, written)
    }

    #[test]
    fn no_bound_forwards_every_packet() {
        let (inner, written) = fake();
        let mut wrapped = OutputTrim::wrap(inner, None);
        for us in [0, 1_000_000, 5_000_000] {
            wrapped.write_packet(&packet(us)).unwrap();
        }
        assert_eq!(*written.lock().unwrap(), vec![0, 1_000_000, 5_000_000]);
    }

    #[test]
    fn packets_at_or_past_the_bound_are_dropped() {
        let (inner, written) = fake();
        let mut wrapped = OutputTrim::wrap(
            inner,
            Some(EndBound::AfterSeek(Duration::from_micros(3_000_000))),
        );
        for us in [0, 1_000_000, 2_000_000, 3_000_000, 4_000_000] {
            wrapped.write_packet(&packet(us)).unwrap();
        }
        // `3_000_000` is the bound itself: dropped, matching `SeekTrim`'s own
        // `>=` cutoff on the input side (`crate::seek_trim`'s doc tests).
        assert_eq!(*written.lock().unwrap(), vec![0, 1_000_000, 2_000_000]);
    }

    /// Regression for the module doc's "Anchored to the first packet"
    /// section: an upstream `-ss` leaves surviving packets at their
    /// original absolute timestamps (measured: no zero-rewrite happens
    /// upstream of this layer), so a stream whose first packet already
    /// starts well after zero must still get the *full* `-t` window from
    /// that point, not have it measured from literal zero.
    #[test]
    fn the_bound_is_measured_from_this_streams_own_first_packet_not_zero() {
        let (inner, written) = fake();
        let mut wrapped = OutputTrim::wrap(
            inner,
            Some(EndBound::AfterSeek(Duration::from_micros(3_000_000))),
        );
        // As if an upstream `-ss 2` left these packets at their original,
        // un-rewritten absolute timestamps.
        for us in [2_000_000, 3_000_000, 4_000_000, 5_000_000, 6_000_000] {
            wrapped.write_packet(&packet(us)).unwrap();
        }
        // Bound is `first (2_000_000) + 3_000_000 = 5_000_000`, not
        // `3_000_000` measured from literal zero -- the pre-fix bug kept
        // only `[2_000_000]` here (one packet, "1 second" of content).
        assert_eq!(
            *written.lock().unwrap(),
            vec![2_000_000, 3_000_000, 4_000_000]
        );
    }

    /// Regression for the module doc's "Sticky per-stream drop" section: a
    /// B-frame-reordered arrival order where a high-`pts` packet (dropped)
    /// is immediately followed by lower-`pts` packets that are themselves
    /// under the bound. A naive per-packet check forwards the interior
    /// packets that follow the drop, handing the muxer a `dts`-order
    /// sequence with a hole in it — measured (module doc) to corrupt
    /// `vaco-mux-mp4`'s sample table badly enough that two genuinely valid
    /// trailing packets vanished from the file entirely, on top of the one
    /// packet that should have been dropped. The fix must instead treat the
    /// first drop as final: nothing after it is ever forwarded again, even
    /// though its own `pts` would otherwise pass.
    #[test]
    fn a_dropped_packet_ends_the_stream_even_if_a_later_one_is_under_the_bound() {
        let (inner, written) = fake();
        let mut wrapped = OutputTrim::wrap(
            inner,
            Some(EndBound::AfterSeek(Duration::from_micros(3_000_000))),
        );
        // A leading packet establishes the anchor at `0` (this is the start
        // of the stream, not an excerpt from its middle -- see "Anchored to
        // the first packet" above for why that matters to the bound math).
        // Arrival (dts) order after it; `pts` jumps ahead at the third
        // packet the way a B-frame reorder window does, then drops back
        // under the bound for the two packets right after it.
        for us in [0, 2_800_000, 3_200_000, 2_960_000, 2_920_000] {
            wrapped.write_packet(&packet(us)).unwrap();
        }
        // Only the contiguous prefix before the first drop survives -- not
        // the two later, individually-under-bound packets.
        assert_eq!(*written.lock().unwrap(), vec![0, 2_800_000]);
    }

    #[test]
    fn to_and_t_both_measure_from_output_zero() {
        // No output-side `-ss` exists (see the module doc), so `Absolute`
        // (`-to`) and `AfterSeek` (`-t`) must behave identically here.
        let (inner, written) = fake();
        let mut wrapped = OutputTrim::wrap(
            inner,
            Some(EndBound::Absolute(Duration::from_micros(2_000_000))),
        );
        for us in [0, 1_000_000, 2_000_000] {
            wrapped.write_packet(&packet(us)).unwrap();
        }
        assert_eq!(*written.lock().unwrap(), vec![0, 1_000_000]);
    }

    /// A muxer with no opinion at all -- `Muxer::stream_time_base`'s own
    /// default, and exactly what both `crate::nullmux`'s `NullMuxer` and the
    /// real `-f null` (`vaco-mux-utility::NullSinkMuxer`) answer. The module
    /// doc's whole point: `OutputTrim` must still trim against such a muxer,
    /// falling back to the `input_time_base` it captures at `add_stream_
    /// with` time.
    struct NoOpinion {
        written: Arc<Mutex<Vec<i64>>>,
    }

    impl Muxer for NoOpinion {
        fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
            Ok(0)
        }

        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }

        fn write_packet(&mut self, packet: &Packet) -> Result<()> {
            self.written
                .lock()
                .unwrap()
                .push(packet.pts.ticks().unwrap_or(0));
            Ok(())
        }

        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_muxer_with_no_opinion_still_trims_via_the_input_time_base() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let inner: Box<dyn Muxer> = Box::new(NoOpinion {
            written: written.clone(),
        });
        let mut wrapped = OutputTrim::wrap(
            inner,
            Some(EndBound::AfterSeek(Duration::from_micros(3_000_000))),
        );
        // `add_stream_with` (not `add_stream`), with the input's own µs time
        // base -- exactly what `vaco_format_core::mux::MuxBuilder::
        // add_stream_with_matrix` hands every wrapped muxer, real or not.
        let index = wrapped
            .add_stream_with(
                &CodecParameters::new(vaco_core::MediaType::Video),
                &StreamSpec {
                    time_base: Some(Rational::new(1, 1_000_000)),
                    display_matrix: None,
                },
            )
            .unwrap();
        assert_eq!(index, 0);
        for us in [0, 2_000_000, 3_000_000, 4_000_000] {
            let mut pkt = packet(us);
            pkt.stream_index = index;
            wrapped.write_packet(&pkt).unwrap();
        }
        assert_eq!(*written.lock().unwrap(), vec![0, 2_000_000]);
    }

    /// Regression for the module doc's "A trimming muxer cannot advertise
    /// `NOTIMESTAMPS`" section: `-f null`'s own muxer declares that flag,
    /// which makes `MuxTimestamps::apply` (M18) clear `pts`/`dts` on every
    /// packet before this layer ever sees it -- so `OutputTrim` must mask
    /// the flag off its own `flags()` answer, since that is the only
    /// `Muxer` `MuxWriter::new` actually reads.
    struct NotimestampsMuxer;

    impl Muxer for NotimestampsMuxer {
        fn flags(&self) -> FormatFlags {
            FormatFlags::NOTIMESTAMPS | FormatFlags::NOFILE
        }

        fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
            Ok(0)
        }

        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }

        fn write_packet(&mut self, _packet: &Packet) -> Result<()> {
            Ok(())
        }

        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn notimestamps_is_masked_off_only_while_trimming_is_active() {
        let inner: Box<dyn Muxer> = Box::new(NotimestampsMuxer);
        let wrapped = OutputTrim::wrap(inner, Some(EndBound::AfterSeek(Duration::from_micros(1))));
        assert!(!wrapped.flags().contains(FormatFlags::NOTIMESTAMPS));
        // Every other flag the real muxer declared must survive the mask.
        assert!(wrapped.flags().contains(FormatFlags::NOFILE));

        // With no `-t`/`-to` at all, `wrap` returns `inner` unwrapped (the
        // module doc's own point: this must not change any output that did
        // not ask to be trimmed), so the real muxer's flags pass straight
        // through, `NOTIMESTAMPS` included.
        let unwrapped = OutputTrim::wrap(Box::new(NotimestampsMuxer), None);
        assert!(unwrapped.flags().contains(FormatFlags::NOTIMESTAMPS));
    }
}
