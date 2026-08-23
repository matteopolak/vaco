//! Property tests for the five models this crate owns.
//!
//! These were originally written against a fixed xorshift generator, because
//! adding a dev-dependency rewrote `Cargo.lock` and `--locked` refused it. That
//! restriction was lifted: `proptest` is pre-declared in
//! `[workspace.dependencies]`, so depending on it adds an edge and not a
//! package. Ported, because shrinking is worth most on exactly these
//! invariants — a minimal counterexample for "DTS went backwards" is a
//! two-packet sequence, and finding that by hand from a 500-packet xorshift
//! case is an afternoon.
//!
//! What is asserted here, and why each one matters:
//!
//! | Property | Breaking it looks like |
//! |---|---|
//! | wrap correction is strictly increasing with the delta preserved | a 30-hour recording whose timestamps jump 26.5 hours backwards |
//! | repaired DTS strictly increases unless it says it saturated | a scheduler that loops, or packets muxed out of order |
//! | the interleave queue loses, duplicates and reorders nothing | a remux that drops audio |
//! | the index stays sorted and searches in the right direction | a seek that lands after the point you asked for |
//! | probing is total and in range | a panic on `vaco -i <anything>` |
//! | a muxed file demuxes back to what went in | every remux |

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::field_reassign_with_default,
    clippy::match_same_arms,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Rational, Timestamp};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::interleave::{InterleaveQueue, MuxTimestamps};
use vaco_format_core::mux::MuxBuilder;
use vaco_format_core::probe::{Probe, ProbeData};
use vaco_format_core::seek::{IndexEntry, PacketIndex, SeekFlags, SeekTarget};
use vaco_format_core::time::{TimestampFixer, WrapState, decode_ts};
use vaco_format_core::vacoraw::{self, ForwardOnlySink, MemorySink, VacoRawDemuxer, VacoRawMuxer};
use vaco_format_core::{Demuxer, DemuxerDesc, FFlags, FormatFlags, FormatOptions, Muxer};
use vaco_io::{MediaSource, MemorySource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

// --------------------------------------------------------------- fixtures

/// One packet's worth of intent, so the input and the expectation come from the
/// same description.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    stream: u32,
    dts: i64,
    key: bool,
    payload: Vec<u8>,
}

/// A packet stream whose DTS is strictly increasing **per stream**, which is
/// the only thing a muxer may assume and the only thing the queue promises to
/// preserve.
fn specs(max_streams: u32) -> impl Strategy<Value = (u32, Vec<Spec>)> {
    (1..=max_streams).prop_flat_map(|streams| {
        (
            Just(streams),
            prop::collection::vec(
                (
                    0..streams,
                    1i64..1000,
                    any::<bool>(),
                    prop::collection::vec(any::<u8>(), 0..24),
                ),
                1..40,
            )
            .prop_map(move |raw| {
                let mut cursor = vec![0i64; streams as usize];
                raw.into_iter()
                    .map(|(stream, delta, key, payload)| {
                        let c = &mut cursor[stream as usize];
                        *c += delta;
                        Spec {
                            stream,
                            dts: *c,
                            key,
                            payload,
                        }
                    })
                    .collect()
            }),
        )
    })
}

fn build(specs: &[Spec], streams: u32, seekable: bool) -> Vec<u8> {
    let opts = FormatOptions::default();
    if seekable {
        let sink = MemorySink::new();
        let bytes = sink.shared();
        let mut mux = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
        write_all(&mut mux, specs, streams, &opts);
        bytes.snapshot()
    } else {
        let sink = ForwardOnlySink::new();
        let bytes = sink.shared();
        let mut mux = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
        write_all(&mut mux, specs, streams, &opts);
        bytes.snapshot()
    }
}

fn write_all(mux: &mut VacoRawMuxer, specs: &[Spec], streams: u32, opts: &FormatOptions) {
    let mut budget = Budget::new(Limits::permissive());
    for i in 0..streams {
        let params = if i == 0 {
            CodecParameters::video().with_codec(CodecId::H264)
        } else {
            CodecParameters::audio().with_codec(CodecId::Opus)
        };
        mux.add_stream(&params).unwrap();
    }
    mux.write_header().unwrap();
    let mut queue = InterleaveQueue::new(streams as usize, opts);
    for i in 0..streams {
        // The muxer now reports the base it chose, which is what M1 needs.
        let tb = mux
            .stream_time_base(i)
            .unwrap_or(vaco_format_core::time::TIME_BASE_Q);
        queue.set_time_base(i, tb);
    }
    for s in specs {
        let mut p = Packet::from_slice(&mut budget, &s.payload).unwrap();
        p.stream_index = s.stream;
        p.dts = Timestamp::new(s.dts);
        p.pts = Timestamp::new(s.dts);
        if s.key {
            p.flags = PacketFlags::KEY;
        }
        queue.push(p).unwrap();
        while let Some(out) = queue.next(false) {
            mux.write_packet(&out).unwrap();
        }
    }
    for i in 0..streams {
        queue.end_stream(i);
    }
    for out in queue.drain() {
        mux.write_packet(&out).unwrap();
    }
    mux.write_trailer().unwrap();
}

fn open(bytes: Vec<u8>) -> VacoRawDemuxer {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    VacoRawDemuxer::open(src, &NoParsers, &FormatOptions::default()).unwrap()
}

fn drain(d: &mut impl Demuxer) -> Vec<Spec> {
    let mut out = Vec::new();
    while let Ok(p) = d.read_packet() {
        out.push(Spec {
            stream: p.stream_index,
            dts: p.dts.ticks().unwrap_or(i64::MIN),
            key: p.is_key(),
            payload: p.payload().to_vec(),
        });
    }
    out
}

/// Read until end of stream, stepping over recoverable errors.
///
/// Returns whether `Eof` was actually reached — a stream that keeps reporting
/// recoverable corruption until the step cap has not ended, it has been given
/// up on, and asserting that `Eof` is stable would be asserting nothing.
///
/// Fails the property if reading does not terminate.
fn read_to_end(d: &mut impl Demuxer) -> Result<bool, TestCaseError> {
    for _ in 0..100_000 {
        match d.read_packet() {
            Ok(_) => {}
            Err(Error::Eof) => return Ok(true),
            // Recoverable: the demuxer skipped a bad header and will resync.
            Err(Error::InvalidData(_) | Error::Unsupported(_)) => {}
            Err(_) => return Ok(false),
        }
    }
    Err(TestCaseError::fail("reading did not terminate"))
}

fn packet(stream: u32, dts: i64, len: usize) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut p = Packet::from_slice(&mut budget, &vec![0u8; len]).unwrap();
    p.stream_index = stream;
    p.dts = Timestamp::new(dts);
    p.pts = p.dts;
    p
}

// ------------------------------------------------------------- the models

proptest! {
    /// R9: small raw steps over a wrapping clock produce a strictly increasing
    /// corrected sequence, with every delta preserved exactly.
    ///
    /// This is the property the whole wraparound model exists for, and the one
    /// the plan's original "apply the pivot to every value" formulation breaks.
    #[test]
    fn wrap_correction_is_monotonic_and_delta_preserving(
        bits in 9u32..40,
        start in 0i64..1_000_000,
        steps in prop::collection::vec(1i64..=255, 1..300),
    ) {
        let period = 1i64 << bits;
        let mut w = WrapState::new(bits);
        let mut raw = start % period;
        w.observe(raw);
        let mut prev = w.correct(Timestamp::new(raw)).ticks().unwrap();
        let first = prev;
        let mut total = 0i64;
        for step in steps {
            raw = (raw + step) % period;
            total += step;
            let cur = w.correct(Timestamp::new(raw)).ticks().unwrap();
            prop_assert!(cur > prev, "went backwards: {prev} -> {cur}");
            prop_assert_eq!(cur - prev, step, "the delta was not preserved");
            prev = cur;
        }
        prop_assert_eq!(prev - first, total);
    }

    /// A clock that cannot wrap is a pass-through, whatever it is fed.
    #[test]
    fn a_64_bit_clock_is_the_identity(raw in prop::collection::vec(any::<i64>(), 0..64)) {
        let mut w = WrapState::new(64);
        for v in raw {
            prop_assert_eq!(w.correct(decode_ts(v)).ticks(), decode_ts(v).ticks());
        }
    }

    /// R22: with fill-in on and no declared discontinuity, DTS strictly
    /// increases — unless the repair saturated, which the report has to admit.
    ///
    /// The saturation exception is not a weakening: it is what the
    /// `format_timestamps` fuzz target found, and reporting it is what lets a
    /// scheduler keep relying on the invariant everywhere else.
    #[test]
    fn repaired_dts_strictly_increases(
        raw in prop::collection::vec(any::<i64>(), 1..80),
        tb_den in 1i32..100_000,
        genpts in any::<bool>(),
    ) {
        let mut opts = FormatOptions::default();
        if genpts {
            opts.fflags.insert(FFlags::GENPTS);
        }
        let tb = Rational::new(1, tb_den);
        let mut fixer = TimestampFixer::new(1, FormatFlags::empty(), &opts);
        let mut last: Option<i64> = None;
        let mut budget = Budget::new(Limits::permissive());
        for v in raw {
            let mut p = Packet::from_slice(&mut budget, b"x").unwrap();
            p.pts = decode_ts(v);
            p.dts = decode_ts(v);
            let report = fixer.fix(&mut p, tb, Rational::ZERO);
            if let (Some(prev), Some(cur)) = (last, p.dts.ticks()) {
                if report.dts_overflow {
                    prop_assert!(cur >= prev, "a saturated repair moved backwards");
                } else {
                    prop_assert!(cur > prev, "dts went backwards: {} -> {}", prev, cur);
                }
            }
            last = p.dts.ticks().or(last);
        }
    }

    /// A format declaring `TS_DISCONT` gets its timestamps back untouched.
    /// Repairing a declared discontinuity destroys the evidence the CLI needs.
    #[test]
    fn ts_discont_passes_timestamps_through(
        raw in prop::collection::vec(any::<i64>(), 1..60),
    ) {
        let opts = FormatOptions::default();
        let mut fixer = TimestampFixer::new(1, FormatFlags::TS_DISCONT, &opts);
        let mut budget = Budget::new(Limits::permissive());
        for v in raw {
            let mut p = Packet::from_slice(&mut budget, b"x").unwrap();
            p.pts = decode_ts(v);
            p.dts = decode_ts(v);
            let report = fixer.fix(&mut p, Rational::new(1, 1000), Rational::ZERO);
            prop_assert!(!report.dts_repaired);
            prop_assert_eq!(p.dts.ticks(), decode_ts(v).ticks());
        }
    }

    /// N1 to N5: the queue orders *between* streams and never within one, and
    /// every packet that goes in comes out exactly once.
    #[test]
    fn interleaving_conserves_packets_and_per_stream_order(
        (streams, specs) in specs(4),
        max_delta in 0i64..20_000_000,
        chunk_size in 0i32..8,
    ) {
        let mut opts = FormatOptions::default();
        opts.max_interleave_delta = max_delta;
        opts.chunk_size = chunk_size;
        let mut queue = InterleaveQueue::new(streams as usize, &opts);
        for i in 0..streams {
            queue.set_time_base(i, Rational::new(1, 1000));
        }
        let mut popped: Vec<Vec<i64>> = vec![Vec::new(); streams as usize];
        for s in &specs {
            queue.push(packet(s.stream, s.dts, s.payload.len())).unwrap();
            while let Some(p) = queue.next(false) {
                popped[p.stream_index as usize].push(p.dts.ticks().unwrap());
            }
        }
        for p in queue.drain() {
            popped[p.stream_index as usize].push(p.dts.ticks().unwrap());
        }
        prop_assert!(queue.is_empty());
        for stream in 0..streams {
            let want: Vec<i64> = specs
                .iter()
                .filter(|s| s.stream == stream)
                .map(|s| s.dts)
                .collect();
            prop_assert_eq!(&want, &popped[stream as usize]);
        }
    }

    /// The queue emits in non-decreasing DTS order once it is allowed to
    /// commit, whatever order the streams arrived in.
    #[test]
    fn draining_is_ordered_by_dts((streams, specs) in specs(3)) {
        let opts = FormatOptions::default();
        let mut queue = InterleaveQueue::new(streams as usize, &opts);
        for i in 0..streams {
            queue.set_time_base(i, Rational::new(1, 1000));
        }
        for s in &specs {
            queue.push(packet(s.stream, s.dts, 1)).unwrap();
        }
        let out = queue.drain();
        prop_assert_eq!(out.len(), specs.len());
        let ts: Vec<i64> = out.iter().map(|p| p.dts.ticks().unwrap()).collect();
        for pair in ts.windows(2) {
            prop_assert!(pair[0] <= pair[1], "drain emitted out of order: {:?}", ts);
        }
    }

    /// M3: the `avoid_negative_ts` offset is computed once, from the first
    /// packet across all streams, and applied uniformly. A per-stream offset
    /// desynchronises them, which is the bug this pins down.
    #[test]
    fn the_output_shift_is_uniform_across_streams(
        first in -1_000_000i64..1_000_000,
        later in prop::collection::vec((0u32..3, 0i64..1_000_000), 1..20),
        mode in 0i32..3,
    ) {
        let mut opts = FormatOptions::default();
        opts.avoid_negative_ts = mode;
        let tb = Rational::new(1, 1000);
        let mut chain = MuxTimestamps::new(3, FormatFlags::TS_NONSTRICT, &opts);

        let mut p = packet(0, first, 1);
        chain.apply(&mut p, tb, tb).unwrap();
        let shift = p.dts.ticks().unwrap() - first;

        for (stream, dts) in later {
            // Keep this stream monotonic so M4 does not reject it for an
            // unrelated reason.
            let mut q = packet(stream, first + dts, 1);
            if chain.apply(&mut q, tb, tb).is_ok() {
                prop_assert_eq!(
                    q.dts.ticks().unwrap() - (first + dts),
                    shift,
                    "stream {} got a different offset", stream
                );
            }
        }
    }

    /// I1: the index is sorted, duplicate-free and bounded, whatever order and
    /// whatever volume it is fed.
    #[test]
    fn the_index_stays_well_formed(
        entries in prop::collection::vec((any::<u64>(), any::<i64>(), any::<bool>()), 0..400),
        indexmem in 64i32..8192,
    ) {
        let mut opts = FormatOptions::default();
        opts.indexmem = indexmem;
        let mut index = PacketIndex::with_options(&opts);
        for (pos, ts, key) in entries {
            index.add(if key {
                IndexEntry::keyframe(pos, decode_ts(ts))
            } else {
                IndexEntry::frame(pos, decode_ts(ts))
            });
            prop_assert!(index.is_well_formed());
        }
    }

    /// A search never returns an entry on the wrong side of the target, and
    /// never returns a non-keyframe unless `ANY` was asked for.
    #[test]
    fn index_search_respects_direction_and_keyframes(
        entries in prop::collection::vec((any::<u64>(), -10_000i64..10_000, any::<bool>()), 1..120),
        want in -12_000i64..12_000,
        backward in any::<bool>(),
        any_frame in any::<bool>(),
    ) {
        let mut index = PacketIndex::new();
        for (pos, ts, key) in &entries {
            index.add(if *key {
                IndexEntry::keyframe(*pos, Timestamp::new(*ts))
            } else {
                IndexEntry::frame(*pos, Timestamp::new(*ts))
            });
        }
        let mut flags = SeekFlags::empty();
        if backward {
            flags |= SeekFlags::BACKWARD;
        }
        if any_frame {
            flags |= SeekFlags::ANY;
        }
        if let Some(e) = index.search(Timestamp::new(want), flags) {
            let ts = e.timestamp.ticks().unwrap();
            if backward {
                prop_assert!(ts <= want, "backward search overshot: {} > {}", ts, want);
            } else {
                prop_assert!(ts >= want, "forward search undershot: {} < {}", ts, want);
            }
            prop_assert!(any_frame || e.is_key(), "returned a non-keyframe without ANY");
        }
    }

    /// Probing is total over arbitrary bytes, and a winning score is always in
    /// `1..=100`. Zero never wins (R5).
    #[test]
    fn probing_is_total_and_in_range(
        buf in prop::collection::vec(any::<u8>(), 0..256),
        filename in prop::option::of("[a-zA-Z0-9./]{0,32}"),
        mime in prop::option::of("[a-z]{1,10}/[a-z-]{1,10}"),
    ) {
        let opts = FormatOptions::default();
        let cands: &[&DemuxerDesc] = &[&vacoraw::DEMUXER];
        let probe = Probe::new(cands, &opts);
        let mut data = ProbeData::new(&buf);
        if let Some(f) = &filename {
            data = data.with_filename(f);
        }
        if let Some(m) = &mime {
            data = data.with_mime_type(m);
        }
        if let Some(found) = probe.best(&data) {
            prop_assert!(found.score.value() > 0);
            prop_assert!(found.score.value() <= 100);
        }
        let all = probe.score_all(&data);
        for pair in all.windows(2) {
            prop_assert!(pair[0].score >= pair[1].score);
        }
    }
}

// ------------------------------------------------- the container end to end

proptest! {
    // A file is built and parsed per case, so run fewer of them than the pure
    // models get.
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Every packet written comes back, byte for byte, with its timestamps and
    /// its keyframe flag, whether or not the sink could seek to write an index.
    #[test]
    fn muxing_then_demuxing_is_the_identity((streams, specs) in specs(3), seekable in any::<bool>()) {
        let bytes = build(&specs, streams, seekable);
        let mut d = open(bytes);
        let got = drain(&mut d);
        prop_assert_eq!(got.len(), specs.len());
        for stream in 0..streams {
            let want: Vec<&Spec> = specs.iter().filter(|s| s.stream == stream).collect();
            let have: Vec<&Spec> = got.iter().filter(|s| s.stream == stream).collect();
            prop_assert_eq!(want, have);
        }
    }

    /// An indexed seek lands on exactly the last keyframe at or before the
    /// target — not merely one that is close.
    #[test]
    fn indexed_seek_lands_on_the_right_keyframe(
        (_, specs) in specs(1),
        target_frac in 0u32..=100,
    ) {
        let bytes = build(&specs, 1, true);
        let keys: Vec<i64> = specs.iter().filter(|s| s.key).map(|s| s.dts).collect();
        let last = specs.last().map_or(0, |s| s.dts);
        let want = i64::from(target_frac) * last / 100;

        let mut d = open(bytes);
        let expect = keys.iter().filter(|&&k| k <= want).max().copied();
        let first = specs.first().map_or(0, |s| s.dts);
        match d.seek(
            SeekTarget::Timestamp { stream_index: 0, ts: Timestamp::new(want) },
            SeekFlags::BACKWARD,
        ) {
            Ok(()) => {
                let landed = d.read_packet().unwrap().dts.ticks().unwrap();
                if let Some(k) = expect {
                    // The index path must be exact: not "close to" the last
                    // keyframe at or before the target, but that keyframe.
                    prop_assert_eq!(landed, k);
                } else {
                    // Nothing precedes the target. A file with no keyframes at
                    // all carries no index, so this is the bisection path, and
                    // its documented fallback is the first sync point in the
                    // range — the best a backward seek can do when there is
                    // nothing behind you.
                    prop_assert_eq!(landed, first);
                }
            }
            Err(_) => prop_assert!(expect.is_none(), "refused a reachable seek to {}", want),
        }
    }

    /// A single flipped bit anywhere in a valid file never panics and never
    /// makes reading fail to terminate.
    #[test]
    fn single_bit_corruption_is_survivable(
        (streams, specs) in specs(2),
        at in any::<prop::sample::Index>(),
        bit in 0u8..8,
    ) {
        let bytes = build(&specs, streams, true);
        let mut corrupt = bytes.clone();
        let i = at.index(corrupt.len());
        corrupt[i] ^= 1 << bit;
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(corrupt));
        if let Ok(mut d) = VacoRawDemuxer::open(src, &NoParsers, &FormatOptions::default()) {
            // `InvalidData` is recoverable by design — the demuxer skips the
            // bad header and resynchronises — so reading past one is correct
            // and only `Eof` is terminal. Reaching it must still take a
            // bounded number of steps.
            if read_to_end(&mut d)? {
                prop_assert!(
                    matches!(d.read_packet(), Err(Error::Eof)),
                    "end of stream was not stable"
                );
            }
        }
    }

    /// Every prefix of a valid file either opens and reads to a terminating
    /// end, or refuses to open. Never a panic, never a hang.
    #[test]
    fn every_truncation_is_survivable((streams, specs) in specs(2), keep in 0u32..=100) {
        let bytes = build(&specs, streams, true);
        let n = (bytes.len() * keep as usize) / 100;
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes[..n].to_vec()));
        if let Ok(mut d) = VacoRawDemuxer::open(src, &NoParsers, &FormatOptions::default()) {
            read_to_end(&mut d)?;
        }
    }
}

proptest! {
    /// `display_rotation` is total: no matrix panics it, and every answer is a
    /// finite angle in `(-180, 180]`.
    ///
    /// The all-zero matrix is the case that makes this non-obvious — both
    /// column scales are zero, and an unguarded normalisation returns NaN,
    /// which then truncates to a value the reference never prints.
    #[test]
    fn display_rotation_is_total_and_bounded(m in proptest::array::uniform9(any::<i32>())) {
        let deg = vaco_format_core::display_rotation(&m);
        prop_assert!(deg.is_finite(), "{m:?} -> {deg}");
        prop_assert!((-180.0..=180.0).contains(&deg), "{m:?} -> {deg}");
    }

    /// The identity is the only matrix reported as the identity, and it is the
    /// one a demuxer must not turn into side data.
    #[test]
    fn only_the_identity_is_the_identity(m in proptest::array::uniform9(any::<i32>())) {
        let identity = [1 << 16, 0, 0, 0, 1 << 16, 0, 0, 0, 1 << 30];
        prop_assert_eq!(vaco_format_core::is_identity_matrix(&m), m == identity);
    }

    /// `duration_ts` is the stored truth and `duration()` the derived view, so
    /// the tick count must survive whatever the microsecond view loses.
    ///
    /// The concrete case behind this: 25 500 ticks at 1/12800 is
    /// 1 992 187.5 µs. Storing microseconds and converting back gives 25 499
    /// or 25 500 depending on the rounding, and `ffprobe` prints `25500`.
    #[test]
    fn duration_ts_is_never_lost_to_the_microsecond_view(
        ticks in 0i64..1_000_000_000,
        den in 1i32..=1_000_000,
    ) {
        let mut s = vaco_format_core::Stream::new(
            0,
            vaco_core::MediaType::Video,
            Rational::new(1, den),
        );
        s.set_duration_ts(ticks);
        prop_assert_eq!(s.duration_ts, Some(ticks));
        // The derived view may round; the field may not.
        if let Some(d) = s.duration() {
            prop_assert!(d.as_micros() >= 0);
        }
    }

    /// The state machine is not a filter: everything accepted reaches the
    /// muxer, exactly once, in per-stream order, and the report agrees with
    /// what came out the other end.
    ///
    /// This is the property that makes `MuxBuilder` safe to adopt. The queue
    /// and the M-chain each have their own conservation property above; this
    /// one is about the composition, which is where a packet gets dropped in
    /// practice — a drain that stops one short, a filter flush that never
    /// runs, an `end_stream` that discards instead of emitting.
    #[test]
    fn the_session_conserves_every_packet((streams, specs) in specs(3)) {
        let opts = FormatOptions::default();
        let tb = Rational::new(1, 1_000_000);
        let sink = MemorySink::new();
        let written = sink.shared();
        let muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
        let mut builder = MuxBuilder::new(Box::new(muxer), &opts);
        for _ in 0..streams {
            builder
                .add_stream(&CodecParameters::video().with_codec(CodecId::H264), tb)
                .unwrap();
        }
        let mut writer = builder.open().unwrap();
        let mut accepted = 0u64;
        for spec in &specs {
            let mut budget = Budget::new(Limits::strict());
            let mut p = Packet::from_slice(&mut budget, &spec.payload).unwrap();
            p.stream_index = spec.stream;
            p.dts = Timestamp::new(spec.dts);
            p.pts = p.dts;
            if spec.key {
                p.flags = PacketFlags::KEY;
            }
            if writer.write_packet(p).is_ok() {
                accepted += 1;
            }
        }
        let report = writer.finish().unwrap();
        prop_assert_eq!(report.packets, accepted);
        prop_assert!(report.trailer_written);
        let summed: u64 = report.per_stream_packets.iter().sum();
        prop_assert_eq!(summed, report.packets);

        // And the file itself holds them, in per-stream order.
        let mut d = open(written.snapshot());
        let got = drain(&mut d);
        prop_assert_eq!(got.len() as u64, accepted);
        for stream in 0..streams {
            let want: Vec<i64> = specs.iter().filter(|s| s.stream == stream).map(|s| s.dts).collect();
            let have: Vec<i64> = got.iter().filter(|s| s.stream == stream).map(|s| s.dts).collect();
            prop_assert_eq!(want, have);
        }
    }

    /// A negative duration is refused rather than clamped: no container states
    /// one, so it means the arithmetic that produced it was wrong, and `N/A`
    /// keeps that visible.
    #[test]
    fn a_negative_duration_is_refused(ticks in i64::MIN..0) {
        let mut s = vaco_format_core::Stream::new(
            0,
            vaco_core::MediaType::Video,
            Rational::new(1, 1000),
        );
        s.set_duration_ts(ticks);
        prop_assert_eq!(s.duration_ts, None);
        prop_assert_eq!(s.duration(), None);
    }
}
