//! Whole-file AVI demux over arbitrary bytes.
//!
//! `open_with_limits` walks `RIFF`/`hdrl`/`strl` fully in memory, then a
//! bounded scan past `movi` looking for `idx1` when the source can seek —
//! every one of those steps is driven by attacker-controlled sizes and
//! offsets. Once open, packets are read to the end and a seek is performed
//! into whatever timeline was discovered.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Nothing allocates past the ceiling.** `Limits::strict` is used, so a
//!   chunk declaring a huge size, or an `idx1`/`indx`/`ix##` claiming a huge
//!   entry count, must produce `LimitExceeded` rather than a large
//!   allocation.
//! * **`Eof` is stable.** The frozen `Demuxer` trait does not require it, but
//!   every caller assumes it.
//! * **Reading terminates.** A packet count cap turns a demuxer that returns
//!   packets without advancing into a localised assertion instead of a
//!   fuzzer timeout.
//! * **Every packet names a declared stream.** A packet naming a stream that
//!   does not exist is how an indexing panic reaches a caller that trusted
//!   this crate's own bounds checks.
//! * **`idx1`'s offset-ambiguity probe never panics on a corrupt or
//!   adversarial index** — `detect_offset_base` and `resolve_opendml` both
//!   seek around based entirely on file content.
//!
//! fuzz-crate: vaco-demux-avi

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_avi::AviDemuxer;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut AviDemuxer) -> u32 {
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
    let Ok(mut demux) = AviDemuxer::open_with_limits(src, &opts, Limits::strict()) else {
        return;
    };

    for (i, s) in demux.streams().iter().enumerate() {
        assert_eq!(s.index as usize, i, "stream index does not match its slot");
    }
    if let Some(d) = demux.duration() {
        assert!(d.as_micros() >= 0);
    }

    let read = drain(&mut demux);

    if demux.streams().is_empty() {
        return;
    }
    for ts in [0i64, 1_000, i64::from(u32::from_le_bytes([
        data.first().copied().unwrap_or(0),
        data.get(1).copied().unwrap_or(0),
        data.get(2).copied().unwrap_or(0),
        data.get(3).copied().unwrap_or(0),
    ]))] {
        let target = SeekTarget::Timestamp {
            stream_index: 0,
            ts: vaco_core::Timestamp::new(ts),
        };
        if demux.seek(target, SeekFlags::BACKWARD).is_ok() {
            let after = drain(&mut demux);
            assert!(after <= read.saturating_add(MAX_PACKETS));
        }
    }
    let _ = demux.seek(SeekTarget::Byte(data.len() as u64 / 2), SeekFlags::empty());
    let _ = drain(&mut demux);
});
