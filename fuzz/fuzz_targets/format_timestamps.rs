//! The timestamp model over arbitrary container values.
//!
//! Every number here comes from a file, which means every number here is
//! attacker-chosen: `i64::MIN` timestamps, a zero time base, a 33-bit clock
//! that jumps a full period, durations that overflow when added. The rules in
//! `vaco_format_core::time` are pure arithmetic over exactly that input, so
//! they are cheap to fuzz and expensive to get wrong — a panic here is
//! reachable from every demuxer in the project at once.
//!
//! The asserted properties:
//!
//! * `WrapState::correct` keeps a stream monotonic across wraps when the raw
//!   deltas are small, which is the whole point of the rule;
//! * `TimestampFixer` never produces a decreasing DTS on a format that does not
//!   declare `TS_DISCONT`;
//! * the seek index stays sorted whatever it is fed.
//! fuzz-crate: vaco-format-core

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Duration, Rational, Timestamp};
use vaco_format_core::seek::{IndexEntry, PacketIndex, SeekFlags};
use vaco_format_core::time::{
    TimestampFixer, WrapState, decode_ts, estimate_duration, quantise_duration,
};
use vaco_format_core::{FFlags, FormatFlags, FormatOptions};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    wrap_bits: u8,
    /// Raw container values, in order.
    raw: Vec<i64>,
    /// Small increments, for the monotonicity property.
    steps: Vec<u8>,
    tb_num: i32,
    tb_den: i32,
    rate_num: i32,
    rate_den: i32,
    genpts: bool,
    igndts: bool,
    nofillin: bool,
    discont: bool,
    delay: u8,
    reorders: bool,
    index_entries: Vec<(u64, i64, bool)>,
    search_for: i64,
}

fuzz_target!(|input: Input| {
    // ------------------------------------------------------ R21b quantisation
    //
    // Both arguments come from a file: the ratio is what a bitstream parser
    // read out of a packet, the base is what the container declared. Neither is
    // checked before it arrives here, and the multiplication is `num × tb.den`
    // over `den × tb.num` — the shape that overflows if it is done in `i64`.
    let seconds = Rational::new(input.rate_num, input.rate_den);
    let base = Rational::new(input.tb_num, input.tb_den);
    if let Some(d) = quantise_duration(seconds, base) {
        // A filled-in duration is a duration: positive, and never longer than
        // the exact value it came from. Native-tick truncation guarantees both.
        assert!(d > vaco_core::Duration::ZERO, "not a duration: {d:?}");
        assert!(seconds.num > 0 && seconds.den > 0);
        let exact = vaco_core::Duration::from_ticks(1, seconds).unwrap();
        assert!(d <= exact, "{d:?} exceeds {seconds:?} in {base:?}");
    }

    // ---------------------------------------------------------- wraparound
    let bits = u32::from(input.wrap_bits);
    let mut wrap = WrapState::new(bits);
    for &v in input.raw.iter().take(512) {
        wrap.observe(v);
        let _ = wrap.correct(decode_ts(v));
    }
    let _ = wrap.offset();
    wrap.reset();

    // The property the rule exists for: small raw steps over a wrapping clock
    // produce a strictly increasing corrected sequence.
    if (1..=48).contains(&bits) && !input.steps.is_empty() {
        let period = 1i64 << bits;
        let mut w = WrapState::new(bits);
        let mut raw = 0i64;
        let mut prev: Option<i64> = None;
        for &s in input.steps.iter().take(1024) {
            // A step of at most 255 is well under half of any period at least
            // 2^9; below that the rule cannot distinguish a wrap from a jump
            // and does not claim to.
            let step = i64::from(s) + 1;
            if period <= 2 * step {
                break;
            }
            raw = (raw + step) % period;
            let cur = w.correct(Timestamp::new(raw)).ticks().unwrap_or(i64::MIN);
            if let Some(p) = prev {
                assert!(cur > p, "wrap correction went backwards: {p} -> {cur}");
                assert_eq!(cur - p, step, "wrap correction changed the delta");
            }
            prev = Some(cur);
        }
    }

    // ---------------------------------------------------------- generation
    let mut opts = FormatOptions::default();
    if input.genpts {
        opts.fflags.insert(FFlags::GENPTS);
    }
    if input.igndts {
        opts.fflags.insert(FFlags::IGNDTS);
    }
    if input.nofillin {
        opts.fflags.insert(FFlags::NOFILLIN);
    }
    let flags = if input.discont {
        FormatFlags::TS_DISCONT
    } else {
        FormatFlags::empty()
    };

    let tb = Rational::new(input.tb_num, input.tb_den);
    let rate = Rational::new(input.rate_num, input.rate_den);
    let mut fixer = TimestampFixer::new(1, flags, &opts);
    fixer.set_stream_delay(0, input.delay, input.reorders);

    let mut budget = Budget::new(Limits::strict());
    let mut last_dts: Option<i64> = None;
    for &v in input.raw.iter().take(512) {
        let Ok(mut pkt) = Packet::alloc(&mut budget, 1) else {
            break;
        };
        pkt.pts = decode_ts(v);
        pkt.dts = decode_ts(v ^ 0x5555);
        pkt.duration = Duration::from_micros(v.rotate_left(7));
        let report = fixer.fix(&mut pkt, tb, rate);

        // R22: without TS_DISCONT and with fill-in on, DTS strictly increases
        // — unless the repair saturated, which the report has to admit to.
        if !input.discont && !input.nofillin {
            if let (Some(prev), Some(cur)) = (last_dts, pkt.dts.ticks()) {
                if report.dts_overflow {
                    assert!(cur >= prev, "a saturated repair still moved backwards");
                } else {
                    assert!(
                        cur > prev,
                        "dts went backwards after repair: {prev} -> {cur}"
                    );
                }
            }
            last_dts = pkt.dts.ticks().or(last_dts);
        }
    }
    fixer.flush();

    // ---------------------------------------------------------- duration
    let _ = estimate_duration(&Default::default(), &opts);

    // ---------------------------------------------------------- index
    let mut index = PacketIndex::with_options(&opts);
    for &(pos, ts, key) in input.index_entries.iter().take(2048) {
        index.add(if key {
            IndexEntry::keyframe(pos, decode_ts(ts))
        } else {
            IndexEntry::frame(pos, decode_ts(ts))
        });
        assert!(index.is_well_formed(), "the index lost its sort order");
    }
    for f in [
        SeekFlags::empty(),
        SeekFlags::BACKWARD,
        SeekFlags::ANY,
        SeekFlags::BACKWARD | SeekFlags::ANY,
    ] {
        if let Some(e) = index.search(Timestamp::new(input.search_for), f) {
            let found = e.timestamp.ticks().unwrap_or(i64::MIN);
            if f.contains(SeekFlags::BACKWARD) {
                assert!(found <= input.search_for, "backward search overshot");
            } else {
                assert!(found >= input.search_for, "forward search undershot");
            }
        }
    }
});
