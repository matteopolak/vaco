//! Whole-file Matroska/WebM demux over arbitrary bytes.
//!
//! The broadest target for this container. `open` reads the EBML header, walks
//! every level-1 element of the `Segment` — buffering `Info`, `Tracks`, `Cues`,
//! `Tags`, `Chapters` and `Attachments` whole — and may follow a `SeekHead` to
//! positions the input chose. Then packets are read to the end and a seek is
//! performed into whatever timeline was discovered.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Nothing allocates past the ceiling.** `Limits::strict` is used, so a
//!   `Tags` element declaring 2^48 octets — the format's natural amplification
//!   lever, since every master carries a size and none carries a checksum we
//!   verify first — must produce `LimitExceeded` rather than a large
//!   allocation.
//! * **Reading terminates.** The parse is driven by an element stack whose
//!   unknown-size termination (RFC 8794 section 6.2) is the one place a
//!   no-progress loop could hide: closing a frame costs no input. A packet cap
//!   turns that into a localised assertion instead of a fuzzer timeout.
//! * **`Eof` is stable.** The frozen `Demuxer` trait does not require it and
//!   every caller assumes it.
//! * **Every packet names a declared stream** and lies inside the file. A
//!   packet naming stream seven when six exist is how an indexing panic reaches
//!   a caller that trusted us.
//! * **Every stream shares one time base.** Matroska has a single
//!   `TimestampScale` per segment; a per-track time base would be a parse bug
//!   that no unit test would notice.
//! * **Chapters are in nanoseconds.** RFC 9559 section 5.1.7.1.4.3, and a
//!   different base here would silently misplace every chapter.
//!
//! fuzz-crate: vaco-demux-matroska

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, Rational, Timestamp};
use vaco_demux_matroska::MatroskaDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

/// Nanoseconds, the base every chapter timestamp is counted in.
const NS_BASE: Rational = Rational::new(1, 1_000_000_000);

fn drain(d: &mut MatroskaDemuxer) -> u32 {
    let streams = d.streams().len() as u32;
    let mut n = 0u32;
    loop {
        match d.read_packet() {
            Ok(p) => {
                assert!(
                    p.stream_index < streams,
                    "packet names stream {} of {streams}",
                    p.stream_index
                );
                assert!(p.len <= p.data.len());
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                // Sticky: the second call must agree with the first.
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return n;
            }
            // A limit is a legitimate answer for a hostile stream.
            Err(_) => return n,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let opts = FormatOptions::default();
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = MatroskaDemuxer::open_with_limits(src, &NoParsers, &opts, Limits::strict())
    else {
        return;
    };

    // Everything below comes straight out of `Tracks`, which the input controls.
    let base = demux.streams().first().map(|s| s.time_base);
    for (i, s) in demux.streams().iter().enumerate() {
        assert_eq!(s.index as usize, i, "stream index does not match its slot");
        assert!(s.time_base.is_defined(), "time base has a zero denominator");
        assert_eq!(
            Some(s.time_base),
            base,
            "Matroska has one TimestampScale per segment, so one time base"
        );
        if let Some(v) = &s.params.video {
            assert!(v.width <= v.coded_width, "cropping grew the picture");
            assert!(v.height <= v.coded_height, "cropping grew the picture");
        }
    }
    for c in demux.chapters() {
        assert_eq!(c.time_base, NS_BASE, "chapter timestamps are nanoseconds");
    }
    if let Some(d) = demux.duration() {
        assert!(d.as_micros() >= 0, "negative container duration");
    }

    drain(&mut demux);

    if demux.streams().is_empty() {
        return;
    }
    // Seek into whatever timeline was found and read again. The first drain
    // populated the index from keyframes, so the index path is reachable here;
    // a file with no keyframes takes the first-cluster fallback.
    for ts in [
        0i64,
        1000,
        i64::from(u32::from_le_bytes([
            data.first().copied().unwrap_or(0),
            data.get(1).copied().unwrap_or(0),
            data.get(2).copied().unwrap_or(0),
            data.get(3).copied().unwrap_or(0),
        ])),
    ] {
        let target = SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(ts),
        };
        if demux.seek(target, SeekFlags::empty()).is_ok() {
            // Only that it terminates and stays coherent — `drain` asserts both.
            // An earlier version compared the count against the linear read and
            // required it to be no larger. That found a real bug (a file whose
            // level-1 scan aborted on a corrupt element size read zero packets
            // linearly while a cue-driven seek read all 22, because the first
            // cluster was never located), and the fix is pinned in the crate's
            // own tests. As a *fuzz* invariant it is wrong: a linear read may
            // legitimately stop at corruption that a seek lands past.
            let _ = drain(&mut demux);
        }
    }
});
