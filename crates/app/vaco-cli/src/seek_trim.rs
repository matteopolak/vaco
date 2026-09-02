//! `-ss`/`-t`/`-to`: apply the input-side seek and enforce the end bound
//! (CLI-option audit, second item). Neither reached the demux loop before
//! this module: every invocation processed the whole input regardless of
//! what any of the three said, silently.
//!
//! # Scope, deliberately
//!
//! - **Fast (keyframe-boundary) seeking only.** [`SeekTrim::wrap`] issues one
//!   [`SeekFlags::BACKWARD`] seek at construction and never decodes-and-
//!   discards to land exactly on the requested point. OBSERVED (`ffmpeg
//!   -ss 2.5 -i <10s clip, keyframes at 0 and 5>`, `-copyts`, first output
//!   frame's `pts_time`): `0.000000`, not `2.5` — the reference's own default
//!   (`-noaccurate_seek`) lands on the keyframe at-or-before the target too,
//!   confirming `BACKWARD` is the right flag and that frame-accurate
//!   `-accurate_seek`-equivalent trimming is a different, unimplemented
//!   feature, not a missing rounding step here.
//! - **Input-side placement only.** [`crate::cli`] parses `-ss`/`-t`/`-to`
//!   only from an input group ([`crate::cli::InputSpec::seek`]/`::end`); the
//!   reference's output-side form (trim after decode, not by seeking) has no
//!   parse path here at all, so there is nothing this module could receive
//!   for it.
//! - **One reference stream** picks the seek target and anchors the end
//!   bound: first video stream, else first audio, else stream `0` (whatever
//!   [`vaco_format_core::Demuxer::streams`] lists first). Every stream is
//!   still delivered — the end bound is checked against each packet's own
//!   stream's own time base, not forced through the reference's.
//!
//! # `-to` before `-ss`
//!
//! Rejected earlier, at option-binding time
//! ([`crate::cli::validate_bounds`]), matching the reference's own
//! input-opening-time failure (OBSERVED, `ffmpeg -ss 10 -to 5 -i in.wav -f
//! null -`, exit 234): `[in#0] -to value smaller than -ss; aborting.` This
//! module never sees that case.
//!
//! # `-ss` past the end of input / `-t` longer than the remaining stream
//!
//! Both need no special handling: the wrapped [`Demuxer::read_packet`]
//! simply forwards to the inner demuxer, which reaches its own
//! real `Eof` first in either case — the end bound in this module is a
//! second, possibly-later-or-never check, never an override. OBSERVED:
//! `ffmpeg -ss 50 -i <10s clip>` opens fine and encodes nothing (real `Eof`
//! immediately); `ffmpeg -ss 8 -t 100 -i <10s clip>` stops at the real end
//! of stream, `time=00:00:02.00` (`10 - 8`), never reaching the `-t` bound.
//!
//! # The end bound is checked against `pts`, not `dts`
//!
//! On a stream with no B-frames (audio; most video without reordering) the
//! two agree, and the WAV measurements above hold exactly. On a reordered
//! (B-frame) video stream the two can disagree by a handful of trailing
//! frames: OBSERVED, `ffmpeg -ss 2.5 -t 2 -copyts -i <10s clip, 10 fps,
//! B-frames, keyframes at 0 and 5> -c copy`, the reference's own output
//! carries frames up to `pts_time=4.800000` (two frames past the nominal
//! `2.5 + 2 = 4.5` bound); checking this module's bound against `pts`
//! instead stops at `4.4`. Both land on the same keyframe (`0.0`, not
//! `2.5` — confirming the `BACKWARD` finding above holds under `-t` too);
//! only the last two or three trailing frames of the cut differ, plausibly
//! because the reference's own cutoff is decode-order (`dts`) sensitive in a
//! way this module's presentation-order (`pts`) check is not. Left as
//! measured rather than chased further -- `pts` is what every other
//! duration in this crate (`Stream::duration`, `crate::cli`'s own `-t`/`-to`
//! measurements) is already keyed on, and the discrepancy is a few frames
//! at a GOP boundary, not a wrong bound.

use vaco_core::{Duration, Error, MediaType, Result, Timestamp};
use vaco_format_core::{
    Chapter, Demuxer, Program, SeekFlags, SeekTarget, Stream,
};
use vaco_limits::Limits;
use vaco_packet::Packet;

use crate::cli::EndBound;

/// Wraps a [`Demuxer`] to apply `-ss` once, at construction, and enforce
/// `-t`/`-to` on every [`Demuxer::read_packet`] after.
///
/// No `Debug` derive: `Box<dyn Demuxer>` cannot provide one, the same
/// reason [`crate::input::InputFile`] (which holds one directly) has none
/// either.
pub struct SeekTrim {
    inner: Box<dyn Demuxer>,
    /// The end bound in absolute microseconds, measured like the reference
    /// measures `-to`: from the file's own start. `None` when only `-ss` was
    /// given.
    end_us: Option<i64>,
}

impl std::fmt::Debug for SeekTrim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeekTrim")
            .field("end_us", &self.end_us)
            .finish_non_exhaustive()
    }
}

impl SeekTrim {
    /// Wrap `inner`, applying `seek`/`end`
    /// ([`crate::cli::InputSpec::seek`]/`::end`).
    ///
    /// Returns `inner` unwrapped when both are `None` (the overwhelmingly
    /// common case, no `-ss`/`-t`/`-to` given) so that case pays nothing, and
    /// also when `inner` has no streams at all — there is nothing to seek on
    /// or measure a bound against.
    ///
    /// # Errors
    /// Whatever [`Demuxer::seek`] returns for the reference stream — most
    /// commonly [`Error::NotSeekable`] for a source that cannot seek at all
    /// (a pipe). The reference itself fails opening such an input under
    /// `-ss` too, just with its own wording; this passes the inner demuxer's
    /// error through unchanged rather than inventing reference-shaped text
    /// for a case `vaco-format-core`'s own error already names.
    pub fn wrap(
        mut inner: Box<dyn Demuxer>,
        seek: Option<Duration>,
        end: Option<EndBound>,
    ) -> Result<Box<dyn Demuxer>> {
        if seek.is_none() && end.is_none() {
            return Ok(inner);
        }
        let Some(reference) = reference_stream(inner.streams()) else {
            return Ok(inner);
        };
        let stream_index = reference.index;
        let time_base = reference.time_base;
        let start_us = reference
            .start_time_absolute()
            .unwrap_or(Duration::ZERO)
            .as_micros();

        let mut base_us = start_us;
        if let Some(ss) = seek {
            let target_us = start_us.saturating_add(ss.as_micros());
            base_us = target_us;
            // A target that cannot be expressed in the reference stream's
            // time base (an undefined base) leaves the seek a no-op rather
            // than erroring the whole input open -- the same "cannot tell,
            // so do nothing" stance `crate::overwrite::exists` takes for an
            // unanswerable protocol check.
            if let Some(ticks) = Duration::from_micros(target_us).to_ticks(time_base) {
                inner.seek(
                    SeekTarget::Timestamp {
                        stream_index,
                        ts: Timestamp::new(ticks),
                    },
                    SeekFlags::BACKWARD,
                )?;
            }
        }

        let end_us = match end {
            Some(EndBound::AfterSeek(t)) => Some(base_us.saturating_add(t.as_micros())),
            Some(EndBound::Absolute(to)) => Some(start_us.saturating_add(to.as_micros())),
            None => None,
        };

        Ok(Box::new(Self { inner, end_us }))
    }

    /// `pkt`'s own presentation time, in absolute microseconds from the
    /// file's own start -- `None` when its stream is unknown to `streams()`
    /// (should not happen) or its pts is absent (a packet with no timing at
    /// all cannot be bounded, so it is let through).
    fn packet_us(&self, pkt: &Packet) -> Option<i64> {
        let stream = self
            .inner
            .streams()
            .iter()
            .find(|s| s.index == pkt.stream_index)?;
        pkt.pts.to_duration(stream.time_base).map(Duration::as_micros)
    }
}

impl Demuxer for SeekTrim {
    fn streams(&self) -> &[Stream] {
        self.inner.streams()
    }

    fn programs(&self) -> &[Program] {
        self.inner.programs()
    }

    fn chapters(&self) -> &[Chapter] {
        self.inner.chapters()
    }

    fn metadata(&self) -> &[(String, String)] {
        self.inner.metadata()
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let pkt = self.inner.read_packet()?;
        if let Some(end_us) = self.end_us
            && self.packet_us(&pkt).is_some_and(|us| us >= end_us)
        {
            return Err(Error::Eof);
        }
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        self.inner.seek(target, flags)
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn reconfigure(&mut self, limits: &Limits, opts: &vaco_format_core::FormatOptions) -> Result<()> {
        self.inner.reconfigure(limits, opts)
    }

    fn bind_url(&mut self, url: &str) -> Result<()> {
        self.inner.bind_url(url)
    }
}

/// First video stream, else first audio, else whatever [`Demuxer::streams`]
/// lists first -- the reference always has a well-defined single seek target
/// for `-ss`; this mirrors the common case without reproducing its full
/// stream-scoring heuristic (`av_find_best_stream`'s codec/bitrate/disposition
/// tie-breaks), which nothing in this crate needs for any other purpose.
fn reference_stream(streams: &[Stream]) -> Option<&Stream> {
    streams
        .iter()
        .find(|s| s.media_type() == Some(MediaType::Video))
        .or_else(|| streams.iter().find(|s| s.media_type() == Some(MediaType::Audio)))
        .or_else(|| streams.first())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_core::Rational;
    use vaco_format_core::options::FormatOptions;
    use vaco_packet::Packet;

    /// A fake demuxer over an in-memory list of packet timestamps (in the
    /// stream's own time base, `1/1` -- microseconds, to keep the arithmetic
    /// in tests readable), so `SeekTrim`'s bound logic is tested without any
    /// real container.
    struct Fake {
        streams: Vec<Stream>,
        pts_us: Vec<i64>,
        next: usize,
        seeks: Vec<(SeekTarget, SeekFlags)>,
    }

    impl Fake {
        fn new(pts_us: Vec<i64>) -> Self {
            let mut s = Stream::new(0, MediaType::Audio, Rational::new(1, 1_000_000));
            s.start_time = Timestamp::ZERO;
            Self {
                streams: vec![s],
                pts_us,
                next: 0,
                seeks: Vec::new(),
            }
        }
    }

    impl Demuxer for Fake {
        fn streams(&self) -> &[Stream] {
            &self.streams
        }

        fn read_packet(&mut self) -> Result<Packet> {
            let Some(&us) = self.pts_us.get(self.next) else {
                return Err(Error::Eof);
            };
            self.next += 1;
            let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
            let mut pkt = Packet::from_slice(&mut budget, &[0]).unwrap();
            pkt.pts = Timestamp::new(us);
            Ok(pkt)
        }

        fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
            self.seeks.push((target, flags));
            let SeekTarget::Timestamp { ts, .. } = target else {
                return Ok(());
            };
            let target_us = ts.ticks().unwrap_or(0);
            // BACKWARD: land on the packet at-or-before the target -- unless
            // the target is past every packet this source has, which real
            // demuxers commonly resolve by moving straight to end-of-data
            // rather than replaying the last one again (OBSERVED: `ffmpeg
            // -ss 50 -i <10s wav>` produces zero packets, not the tail one;
            // see the module doc's `-ss` past-EOF section).
            self.next = if target_us > *self.pts_us.last().unwrap_or(&0) {
                self.pts_us.len()
            } else {
                self.pts_us
                    .iter()
                    .rposition(|&us| us <= target_us)
                    .unwrap_or(0)
            };
            Ok(())
        }
    }

    fn drain(d: &mut dyn Demuxer) -> Vec<i64> {
        let mut out = Vec::new();
        loop {
            match d.read_packet() {
                Ok(p) => out.push(p.pts.ticks().unwrap()),
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        out
    }

    #[test]
    fn neither_option_returns_the_inner_demuxer_unwrapped() {
        let fake = Box::new(Fake::new(vec![0, 1_000_000, 2_000_000]));
        let mut wrapped = SeekTrim::wrap(fake, None, None).unwrap();
        assert_eq!(drain(&mut *wrapped), vec![0, 1_000_000, 2_000_000]);
    }

    #[test]
    fn ss_seeks_once_at_construction() {
        let fake = Box::new(Fake::new(vec![0, 1_000_000, 2_000_000, 3_000_000]));
        let mut wrapped =
            SeekTrim::wrap(fake, Some(Duration::from_micros(2_000_000)), None).unwrap();
        assert_eq!(drain(&mut *wrapped), vec![2_000_000, 3_000_000]);
    }

    #[test]
    fn to_is_absolute_from_the_file_start_regardless_of_ss() {
        // -ss 1 -to 3: stop at the file's own 3s mark, i.e. one packet after
        // the seek target (2s of output), matching the reference's own
        // `-ss 2 -to 5` => `time=00:00:03.00` measurement in `crate::cli`.
        let fake = Box::new(Fake::new(vec![0, 1_000_000, 2_000_000, 3_000_000, 4_000_000]));
        let mut wrapped = SeekTrim::wrap(
            fake,
            Some(Duration::from_micros(1_000_000)),
            Some(EndBound::Absolute(Duration::from_micros(3_000_000))),
        )
        .unwrap();
        assert_eq!(drain(&mut *wrapped), vec![1_000_000, 2_000_000]);
    }

    #[test]
    fn t_is_relative_to_ss() {
        let fake = Box::new(Fake::new(vec![0, 1_000_000, 2_000_000, 3_000_000, 4_000_000]));
        let mut wrapped = SeekTrim::wrap(
            fake,
            Some(Duration::from_micros(1_000_000)),
            Some(EndBound::AfterSeek(Duration::from_micros(2_000_000))),
        )
        .unwrap();
        // Stop at 1 + 2 = 3s: packets at 1s and 2s qualify, 3s does not.
        assert_eq!(drain(&mut *wrapped), vec![1_000_000, 2_000_000]);
    }

    #[test]
    fn t_alone_is_relative_to_the_file_start() {
        let fake = Box::new(Fake::new(vec![0, 1_000_000, 2_000_000]));
        let mut wrapped = SeekTrim::wrap(
            fake,
            None,
            Some(EndBound::AfterSeek(Duration::from_micros(1_500_000))),
        )
        .unwrap();
        assert_eq!(drain(&mut *wrapped), vec![0, 1_000_000]);
    }

    #[test]
    fn ss_past_the_end_of_input_yields_immediate_eof() {
        let fake = Box::new(Fake::new(vec![0, 1_000_000]));
        let mut wrapped =
            SeekTrim::wrap(fake, Some(Duration::from_micros(50_000_000)), None).unwrap();
        assert_eq!(drain(&mut *wrapped), Vec::<i64>::new());
    }

    #[test]
    fn t_longer_than_the_remaining_stream_reaches_real_eof_first() {
        let fake = Box::new(Fake::new(vec![0, 1_000_000, 2_000_000]));
        let mut wrapped = SeekTrim::wrap(
            fake,
            None,
            Some(EndBound::AfterSeek(Duration::from_micros(100_000_000))),
        )
        .unwrap();
        assert_eq!(drain(&mut *wrapped), vec![0, 1_000_000, 2_000_000]);
    }

    #[test]
    fn other_demuxer_methods_forward_to_the_inner_demuxer() {
        let fake = Box::new(Fake::new(vec![0]));
        let wrapped = SeekTrim::wrap(fake, Some(Duration::ZERO), None).unwrap();
        assert_eq!(wrapped.streams().len(), 1);
        assert!(wrapped.programs().is_empty());
        assert!(wrapped.chapters().is_empty());
        assert!(wrapped.metadata().is_empty());
        assert_eq!(wrapped.duration(), None);
    }

    #[test]
    fn reconfigure_forwards_without_erroring() {
        let fake = Box::new(Fake::new(vec![0]));
        let mut wrapped = SeekTrim::wrap(fake, Some(Duration::ZERO), None).unwrap();
        let limits = Limits::permissive();
        assert!(wrapped.reconfigure(&limits, &FormatOptions::default()).is_ok());
    }
}
