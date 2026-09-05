//! `-read_intervals`, the packet read loop, and the payload dumps.
//!
//! `probe_argv` already drives the whole binary from an arbitrary argument
//! vector, but it never reaches a packet: the paths it invents do not exist, so
//! the run stops at the open. This target starts *after* the open, with an
//! arbitrary interval spec, an arbitrary packet stream and an arbitrary writer,
//! which is the combination `-show_packets` actually runs.
//!
//! Four properties beyond "does not panic":
//!
//! * **`intervals::parse` is total.** It takes a raw option value, so it is
//!   untrusted input in the plainest sense, and it has to return an error
//!   rather than reach a slice index or an integer overflow.
//! * **The read loop terminates**, on every interval list, for every writer.
//!   It runs under `Limits::tiny()`, so the budget is a live backstop rather
//!   than a theoretical one.
//! * **The counts are bounded by the intervals.** The sum of what was counted
//!   can never exceed what the intervals allow, which is the invariant that
//!   `nb_read_packets` rests on.
//! * **The hexdump geometry holds for every payload length.** Every non-empty
//!   line puts the ASCII column at byte 51. That is one measured constant
//!   standing in for a whole family of off-by-one padding bugs.
//!
//! # Why the packet source is finite
//!
//! The first version used a demuxer that never ends, so every iteration ran
//! the full `Limits::tiny()` budget — 65 536 packets, each emitting a section
//! — and the fuzzer managed **1 exec/s**. libFuzzer needs thousands. That is
//! not a finding about `vaco-probe`; it is the harness paying the bound's full
//! price on every input, and the bound is already pinned deterministically by
//! `packets::tests::a_demuxer_that_never_ends_is_bounded_by_the_budget`. The
//! source is now finite and derived from the input length, which is what a
//! real file looks like, and the target explores option shapes instead of
//! re-proving one constant.
//!
//! fuzz-crate: vaco-probe

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_probe::dump::{DumpFormat, HashAlg};
use vaco_probe::emit::Emit;
use vaco_probe::intervals::{self, Cursor, EndBound, ReadInterval};
use vaco_probe::packets::{self, ReadOpts};
use vaco_probe::show::PayloadOpts;

/// Enough writers to cover the two that escape values and the four that do not.
const WRITERS: &[&str] = &[
    "default",
    "compact",
    "csv",
    "flat",
    "ini",
    "json",
    "xml",
    "compact=e=csv",
    "json=c=1",
];

/// Cap the interval spec: past a few hundred bytes the fuzzer is only
/// rediscovering that a long comma list is a long comma list.
const MAX_SPEC: usize = 256;

/// One packet's worth of arbitrary bytes.
const MAX_PAYLOAD: usize = 4096;

/// Cap the packet count so an iteration stays in the microsecond range. See
/// the module note on why this is finite.
const MAX_PACKETS: i64 = 64;

/// A finite packet source. Timestamps advance so a timed interval can close,
/// and the two stream indices alternate so `-select_streams` has something to
/// do.
struct Canned {
    streams: Vec<vaco_format_core::Stream>,
    payload: Vec<u8>,
    next: i64,
    end: i64,
}

impl vaco_format_core::Demuxer for Canned {
    fn streams(&self) -> &[vaco_format_core::Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> vaco_core::Result<vaco_packet::Packet> {
        if self.next >= self.end {
            return Err(vaco_core::Error::Eof);
        }
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let mut p = vaco_packet::Packet::from_slice(&mut budget, &self.payload)?;
        p.stream_index = u32::try_from(self.next & 1).unwrap_or(0);
        p.pts = vaco_core::Timestamp::new(self.next);
        p.dts = vaco_core::Timestamp::new(self.next);
        p.duration = vaco_core::Duration::from_micros(1000);
        p.pos = Some(u64::try_from(self.next).unwrap_or(0));
        self.next = self.next.saturating_add(1);
        Ok(p)
    }

    fn seek(
        &mut self,
        _target: vaco_format_core::SeekTarget,
        _flags: vaco_format_core::SeekFlags,
    ) -> vaco_core::Result<()> {
        self.next = 0;
        Ok(())
    }
}

fn stream(index: u32, media: vaco_core::MediaType) -> vaco_format_core::Stream {
    vaco_format_core::Stream::new(
        index,
        media,
        vaco_core::Rational {
            num: 1,
            den: 1_000,
        },
    )
}

fuzz_target!(|data: &[u8]| {
    // Layout: <writer><flags><NUL-separated: interval spec, payload>
    let writer = WRITERS
        .get(usize::from(data.first().copied().unwrap_or(0)) % WRITERS.len())
        .copied()
        .unwrap_or("default");
    let flags = data.get(1).copied().unwrap_or(0);
    let rest = data.get(2..).unwrap_or_default();
    let mut fields = rest.splitn(2, |b| *b == 0);
    let spec_bytes = fields.next().unwrap_or_default();
    let payload = fields.next().unwrap_or_default();

    let spec = String::from_utf8_lossy(
        spec_bytes
            .get(..MAX_SPEC.min(spec_bytes.len()))
            .unwrap_or_default(),
    )
    .into_owned();
    let payload = payload
        .get(..MAX_PAYLOAD.min(payload.len()))
        .unwrap_or_default()
        .to_vec();

    // ---- 1. the parser is total, and every interval it produces is usable.
    let intervals = match intervals::parse(&spec) {
        Ok((intervals, _warnings)) => intervals,
        Err(_) => vec![ReadInterval::ALL],
    };
    for interval in &intervals {
        // A cursor must reach a verdict for any timestamp, including none and
        // both extremes, without overflowing when it adds the offset.
        let mut cursor = Cursor::new(*interval);
        for ts in [None, Some(i64::MIN), Some(0), Some(i64::MAX)] {
            let _ = cursor.admit(ts);
        }
    }

    // ---- 2. the hexdump geometry holds at every length.
    let dumped = vaco_probe::dump::xxd(&payload);
    for line in dumped.lines().filter(|l| !l.is_empty()) {
        assert_eq!(
            line.as_bytes().get(50),
            Some(&b' '),
            "the ASCII column moved at payload length {}",
            payload.len()
        );
    }
    let b64 = vaco_probe::dump::base64(&payload);
    for line in b64.lines().filter(|l| !l.is_empty()) {
        assert!(line.len() <= 80, "base64 line exceeded the wrap width");
    }
    for (_, alg) in vaco_probe::dump::HASH_NAMES {
        assert_eq!(
            alg.digest_hex(&payload).is_some(),
            alg.implemented(),
            "{} disagrees with its own implemented() flag",
            alg.label()
        );
    }

    // ---- 3. the read loop terminates, and honours the interval bound.
    let streams = vec![
        stream(0, vaco_core::MediaType::Video),
        stream(1, vaco_core::MediaType::Audio),
    ];
    let selected: Vec<u32> = if flags & 1 == 0 {
        vec![0, 1]
    } else {
        vec![0]
    };
    let payload_opts = PayloadOpts {
        data: (flags & 2 != 0).then(|| {
            if flags & 4 == 0 {
                DumpFormat::Xxd
            } else {
                DumpFormat::Base64
            }
        }),
        hash: (flags & 8 != 0).then_some(HashAlg::Crc32),
    };

    let end = 1 + i64::try_from(payload.len()).unwrap_or(0) % MAX_PACKETS;
    let mut demuxer = Canned {
        streams: streams.clone(),
        payload,
        next: 0,
        end,
    };
    let mut sink = Vec::new();
    let counts = {
        let Ok(w) = vaco_textformat::writers::make(writer) else {
            return;
        };
        let mut tf =
            vaco_textformat::TextFormat::new(w, &mut sink, vaco_textformat::FormatOpts::default());
        if tf.open(vaco_textformat::sections::SectionId::ROOT).is_err() {
            return;
        }
        let counts = {
            let mut e = Emit::new(&mut tf, vaco_textformat::OptionalFields::Auto);
            packets::read(&mut e, &mut demuxer, &streams, ReadOpts {
                intervals: &intervals,
                selected: &selected,
                emit_packets: flags & 16 == 0,
                payload: payload_opts,
                // A live backstop rather than a theoretical one: `tiny` is
                // 2^16 fuel and the loop charges one unit per packet read.
                limits: vaco_limits::Limits::tiny(),
                format_flags: vaco_format_core::flags::FormatFlags::empty(),
                format_options: vaco_format_core::options::FormatOptions::default(),
            })
        };
        let _ = tf.close();
        let _ = tf.finish();
        counts.unwrap_or_default()
    };

    // Every writer emits text. A packet payload is arbitrary bytes, and the
    // hexdump and base64 forms are what keep them out of the output — a
    // non-UTF-8 byte reaching the sink means one of them was bypassed.
    assert!(
        std::str::from_utf8(&sink).is_ok(),
        "a writer emitted non-UTF-8"
    );

    // The counts can never exceed what the intervals allow. An interval with no
    // `#N` is unbounded, so the check only applies when every one has a count.
    let allowed: Option<u64> = intervals
        .iter()
        .map(|i| match i.end {
            Some(EndBound::Packets(n)) => Some(n),
            _ => None,
        })
        .try_fold(0u64, |acc, n| Some(acc.saturating_add(n?)));
    if let Some(allowed) = allowed {
        let total: u64 = counts.iter().copied().fold(0, u64::saturating_add);
        assert!(
            total <= allowed,
            "counted {total} packets against a bound of {allowed}"
        );
    }
});
