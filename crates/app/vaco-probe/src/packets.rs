//! The packet read loop: `-show_packets`, `-count_packets`, `-select_streams`
//! and `-read_intervals`, all of which are the same pass over the file.
//!
//! # What it is
//!
//! One function, [`read`], that walks the demuxer once and does three things at
//! the same time — emits `[PACKET]` sections, counts packets per stream, and
//! stops where `-read_intervals` says to. They are one pass because the
//! reference makes them one pass, and the observable consequence is that
//! `-count_packets -read_intervals '%+#3'` reports **3**, not the file's total.
//!
//! # How it works
//!
//! ```text
//! for interval in intervals:
//!     seek if it has a start
//!     loop:
//!         packet = demuxer.read_packet()      -- an error or EOF ends everything
//!         skip it unless -select_streams admits it
//!         cursor.admit(pts) -> Show: emit and count
//!                           -> Stop: this packet is DROPPED, next interval
//! ```
//!
//! The dropped packet is the part that is not obvious and is measured:
//! `-read_intervals '%+#1,%+#1'` on a two-stream MP4 prints the packets at file
//! offsets 48 and **7675**, skipping the one at 5219. See [`crate::intervals`].
//!
//! # Bounding the work (D6)
//!
//! Two bounds, and they answer different questions.
//!
//! * `-read_intervals` is the **user's** bound. It is the reason a packet dump
//!   on a two-hour file is usable at all.
//! * A [`Budget`] is the **safety** bound. One unit of fuel per packet, so a
//!   demuxer that returns packets without consuming input terminates instead of
//!   spinning. `Limits::permissive()` allows 2³² packets, which is four orders
//!   of magnitude above any real file; the fuzz target passes
//!   `Limits::tiny()` (2¹⁶) so a hostile input is bounded in milliseconds
//!   rather than minutes.
//!
//! Note what does *not* need a bound: a read error. The reference stops the
//! whole read on any `av_read_frame` failure, so a corrupt file terminates by
//! the same path a well-formed one does.
//!
//! # How to change it
//!
//! The three measured rules — one dropped packet per interval boundary,
//! counting only selected packets, and counting only what was shown — are
//! pinned in this module's own tests, and the field-level output is pinned in
//! `tests/packets.rs` against captured reference bytes. A change to any of
//! them needs a new `ffprobe` run in the commit.

use std::io::Write;

use vaco_core::{Error, Result, TimeBase};
use vaco_format_core::{Demuxer, SeekFlags, SeekTarget, Stream};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_textformat::sections::SectionId;

use crate::emit::Emit;
use crate::intervals::{Admission, Bound, Cursor, ReadInterval};
use crate::show::{self, PayloadOpts};

/// Everything the loop needs that is not the demuxer.
#[derive(Clone, Debug)]
pub struct ReadOpts<'a> {
    /// From `-read_intervals`; a single [`ReadInterval::ALL`] when absent.
    pub intervals: &'a [ReadInterval],
    /// Stream indices `-select_streams` admits, in container order.
    pub selected: &'a [u32],
    /// Whether to emit `[PACKET]` sections. `-count_packets` alone reads the
    /// file without printing a single one.
    pub emit_packets: bool,
    /// `-show_data` / `-show_data_hash`.
    pub payload: PayloadOpts,
    /// The safety bound. See the module note.
    pub limits: Limits,
}

/// Read the file once. Returns the per-stream packet counts, indexed the same
/// way as `streams`.
///
/// # Errors
/// Propagates the sink's I/O error. A demuxer error is **not** propagated: it
/// ends the read, exactly as `av_read_frame` returning a negative value ends
/// the reference's. Budget exhaustion ends it the same way, because a bound
/// that turned a long file into an error would be a worse failure than a
/// truncated dump.
pub fn read<W: Write>(
    e: &mut Emit<'_, W>,
    demuxer: &mut dyn Demuxer,
    streams: &[Stream],
    opts: ReadOpts<'_>,
) -> Result<Vec<u64>> {
    let mut counts = vec![0u64; streams.len()];
    let mut budget = Budget::new(opts.limits);

    if opts.emit_packets {
        e.tf().open(SectionId::PACKETS)?;
    }

    'intervals: for interval in opts.intervals {
        if let Some(start) = interval.start
            && seek(demuxer, streams, opts.selected, start).is_err()
        {
            // The reference gives up on the whole read when a seek fails
            // ("could not seek to position"), rather than falling back to a
            // linear scan. Ours does the same rather than silently reading
            // from wherever it happens to be.
            break 'intervals;
        }
        let mut cursor = Cursor::new(*interval);

        loop {
            if budget.consume_fuel(1).is_err() {
                break 'intervals;
            }
            let Ok(pkt) = demuxer.read_packet() else {
                break 'intervals;
            };
            if !opts.selected.contains(&pkt.stream_index) {
                continue;
            }
            let stream = stream_of(streams, pkt.stream_index);
            if cursor.admit(micros(&pkt, stream)) == Admission::Stop {
                // Consumed and not shown. The next interval starts after it.
                continue 'intervals;
            }
            // Indexed by *position* in `streams`, not by `stream_index`. They
            // agree for every container measured, but the trait does not
            // promise it and a container that renumbers would silently
            // mis-attribute every count.
            if let Some(slot) = streams
                .iter()
                .position(|s| s.index == pkt.stream_index)
                .and_then(|i| counts.get_mut(i))
            {
                *slot = slot.saturating_add(1);
            }
            if opts.emit_packets {
                show::packet(e, &pkt, stream, opts.payload)?;
            }
        }
    }

    if opts.emit_packets {
        e.tf().close()?;
    }
    Ok(counts)
}

/// A packet's position on the interval timeline, in microseconds.
///
/// `pts` when it has one, `dts` otherwise. Measured: `-read_intervals '%+0.04'`
/// on an MP4 whose second packet has `pts_time=0.160000` and
/// `dts_time=-0.040000` stops **before** it, so the comparison is against the
/// presentation timestamp and not the decode one — the opposite of what a
/// demux-order loop suggests.
fn micros(pkt: &Packet, stream: Option<&Stream>) -> Option<i64> {
    let tb: TimeBase = stream.map_or(TimeBase::MICROSECONDS, |s| s.time_base);
    let ts = if pkt.pts.is_some() { pkt.pts } else { pkt.dts };
    ts.to_duration(tb).map(|d| d.0)
}

fn stream_of(streams: &[Stream], index: u32) -> Option<&Stream> {
    streams.iter().find(|s| s.index == index)
}

/// Seek to an interval start.
///
/// # The seek follows `-select_streams`, which is measurable
///
/// The reference seeks with stream `-1` and lets libavformat pick, which looked
/// like an approximation nothing could pin down. It is not — two probes settle
/// it. On a five-second file with one video keyframe at 0:
///
/// ```text
/// -read_intervals '2%+#6'                     -> video 0.000000, audio -0.023220
/// -read_intervals '2%+#3' -select_streams a   -> audio 1.996916
/// ```
///
/// Without a selection everything rewinds to the video keyframe at 0, audio
/// included. Narrow the selection to audio and the audio lands at ~2 s instead.
/// So the reference seeks **in the selected stream**, and "the first selected
/// stream" reproduces both rows: with no `-select_streams` the first selected
/// stream *is* the first video stream.
///
/// This used to seek the first video stream unconditionally, which is right for
/// the common case and wrong for every `-select_streams a` with a start bound —
/// 420 invocations of one option matrix, all the same shape.
///
/// `Bound::Relative` is a `+OFFSET` start, which is defined as "from the
/// current position" and therefore performs no seek at all (plan 14 §5.3).
fn seek(
    demuxer: &mut dyn Demuxer,
    streams: &[Stream],
    selected: &[u32],
    start: Bound,
) -> Result<()> {
    let Bound::Absolute(micros) = start else {
        return Ok(());
    };
    let reference = streams
        .iter()
        .find(|s| selected.contains(&s.index) && s.time_base.is_defined())
        .or_else(|| {
            streams
                .iter()
                .find(|s| s.media_type() == Some(vaco_core::MediaType::Video))
        })
        .or_else(|| streams.first())
        .ok_or(Error::NotSeekable)?;
    let ts = vaco_core::Duration(micros)
        .to_ticks(reference.time_base)
        .ok_or(Error::NotSeekable)?;
    demuxer.seek(
        SeekTarget::Timestamp {
            stream_index: reference.index,
            ts: ts.into(),
        },
        SeekFlags::BACKWARD,
    )
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use vaco_core::{Duration, MediaType, Rational, Timestamp};
    use vaco_format_core::SeekTarget;
    use vaco_packet::PacketFlags;
    use vaco_textformat::{FormatOpts, OptionalFields, TextFormat, writers};

    /// A demuxer over a fixed packet list, so the loop can be tested without a
    /// container. `seek` records the request and rewinds to the start, which is
    /// the worst realistic case — every seek lands on the first keyframe.
    struct Canned {
        streams: Vec<Stream>,
        packets: Vec<Packet>,
        at: usize,
        seeks: Vec<i64>,
    }

    impl Demuxer for Canned {
        fn streams(&self) -> &[Stream] {
            &self.streams
        }
        fn read_packet(&mut self) -> Result<Packet> {
            let pkt = self.packets.get(self.at).cloned().ok_or(Error::Eof)?;
            self.at += 1;
            Ok(pkt)
        }
        fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
            if let SeekTarget::Timestamp { ts, .. } = target {
                self.seeks.push(ts.ticks().unwrap_or(0));
            }
            self.at = 0;
            Ok(())
        }
    }

    /// One never-ending demuxer, for the bound.
    struct Endless(u32);

    impl Demuxer for Endless {
        fn streams(&self) -> &[Stream] {
            &[]
        }
        fn read_packet(&mut self) -> Result<Packet> {
            let mut p = Packet::empty();
            p.stream_index = self.0;
            Ok(p)
        }
        fn seek(&mut self, _t: SeekTarget, _f: SeekFlags) -> Result<()> {
            Ok(())
        }
    }

    fn stream(index: u32, media: MediaType) -> Stream {
        Stream::new(index, media, Rational { num: 1, den: 1000 })
    }

    fn packet(index: u32, pts: i64) -> Packet {
        let mut p = Packet::empty();
        p.stream_index = index;
        p.pts = Timestamp::new(pts);
        p.dts = Timestamp::new(pts);
        p.duration = Duration(1000);
        p.flags = PacketFlags::KEY;
        p
    }

    /// Two video packets, then one audio, then two video — the interleaving
    /// that makes "selected packets only" observable.
    fn canned() -> Canned {
        Canned {
            streams: vec![stream(0, MediaType::Video), stream(1, MediaType::Audio)],
            packets: vec![
                packet(0, 0),
                packet(0, 40),
                packet(1, 0),
                packet(0, 80),
                packet(1, 20),
            ],
            at: 0,
            seeks: Vec::new(),
        }
    }

    fn run(demuxer: &mut Canned, opts: ReadOpts<'_>) -> (String, Vec<u64>) {
        let mut sink = Vec::new();
        let streams = demuxer.streams.clone();
        let counts = {
            let w = writers::make("compact=p=0:nk=1").expect("writer");
            let mut tf = TextFormat::new(w, &mut sink, FormatOpts::default());
            tf.open(SectionId::ROOT).expect("root");
            let counts = {
                let mut e = Emit::new(&mut tf, OptionalFields::Auto);
                read(&mut e, demuxer, &streams, opts).expect("read")
            };
            tf.close().expect("close");
            let _ = tf.finish().expect("finish");
            counts
        };
        (String::from_utf8_lossy(&sink).into_owned(), counts)
    }

    fn opts<'a>(intervals: &'a [ReadInterval], selected: &'a [u32]) -> ReadOpts<'a> {
        ReadOpts {
            intervals,
            selected,
            emit_packets: true,
            payload: PayloadOpts::default(),
            limits: Limits::permissive(),
        }
    }

    #[test]
    fn every_packet_when_nothing_bounds_it() {
        let (out, counts) = run(&mut canned(), opts(&[ReadInterval::ALL], &[0, 1]));
        assert_eq!(out.lines().count(), 5);
        assert_eq!(counts, [3, 2]);
    }

    #[test]
    fn an_interval_boundary_drops_exactly_one_packet() {
        // The measured `%+#1,%+#1` rule. Packets are (v0, v40, a0, v80, a20);
        // the first interval shows v0, the second must show a0 — v40 is eaten.
        let two = [
            ReadInterval {
                start: None,
                end: Some(crate::intervals::EndBound::Packets(1)),
            },
            ReadInterval {
                start: None,
                end: Some(crate::intervals::EndBound::Packets(1)),
            },
        ];
        let (out, counts) = run(&mut canned(), opts(&two, &[0, 1]));
        assert_eq!(out.lines().count(), 2);
        assert_eq!(counts, [1, 1], "the second shown packet is the audio one");
    }

    #[test]
    fn select_streams_bounds_the_count_as_well_as_the_output() {
        // `-select_streams v -read_intervals '%+#1,%+#1'` skips the second
        // *video* packet, not the second packet. Counting follows the same
        // rule: an unselected packet is never counted.
        let two = [
            ReadInterval {
                start: None,
                end: Some(crate::intervals::EndBound::Packets(1)),
            },
            ReadInterval {
                start: None,
                end: Some(crate::intervals::EndBound::Packets(1)),
            },
        ];
        let (_, counts) = run(&mut canned(), opts(&two, &[0]));
        assert_eq!(counts, [2, 0]);
    }

    #[test]
    fn count_packets_alone_reads_without_printing() {
        let mut d = canned();
        let mut opts = opts(&[ReadInterval::ALL], &[0, 1]);
        opts.emit_packets = false;
        let (out, counts) = run(&mut d, opts);
        assert_eq!(out, "");
        assert_eq!(counts, [3, 2]);
    }

    #[test]
    fn a_start_seeks_and_a_relative_start_does_not() {
        let mut d = canned();
        let _ = run(
            &mut d,
            opts(
                &[ReadInterval {
                    start: Some(Bound::Absolute(1_000_000)),
                    end: None,
                }],
                &[0, 1],
            ),
        );
        assert_eq!(d.seeks, [1000], "1 s at a 1/1000 time base");

        let mut d = canned();
        let _ = run(
            &mut d,
            opts(
                &[ReadInterval {
                    start: Some(Bound::Relative(1_000_000)),
                    end: None,
                }],
                &[0, 1],
            ),
        );
        assert!(d.seeks.is_empty(), "+OFFSET is from the current position");
    }

    #[test]
    fn a_demuxer_that_never_ends_is_bounded_by_the_budget() {
        // Not hypothetical: a demuxer that returns packets without consuming
        // input is a live denial-of-service on untrusted media, and this is
        // the bound that keeps the fuzz target's iterations short.
        let mut sink = Vec::new();
        let w = writers::make("compact").expect("writer");
        let mut tf = TextFormat::new(w, &mut sink, FormatOpts::default());
        tf.open(SectionId::ROOT).expect("root");
        {
            let mut e = Emit::new(&mut tf, OptionalFields::Auto);
            let counts = read(
                &mut e,
                &mut Endless(0),
                &[stream(0, MediaType::Video)],
                ReadOpts {
                    intervals: &[ReadInterval::ALL],
                    selected: &[0],
                    emit_packets: false,
                    payload: PayloadOpts::default(),
                    limits: Limits::tiny(),
                },
            )
            .expect("read");
            // `Limits::tiny()` is 2^16 fuel and the loop charges one per read.
            assert_eq!(counts, [u64::from(u16::MAX) + 1]);
        }
        tf.close().expect("close");
        let _ = tf.finish().expect("finish");
    }
}
