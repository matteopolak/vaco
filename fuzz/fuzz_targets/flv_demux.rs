//! Whole-file FLV demux over arbitrary bytes.
//!
//! Stream discovery is progressive here — there is no header to bound the
//! work up front, only the tag stream itself, including the `onMetaData`
//! AMF0 script tag this crate decodes on every open. What is asserted beyond
//! "does not panic":
//!
//! * **Nothing allocates past the ceiling.** `Limits::strict` bounds both the
//!   demuxer's own tag-body reads and AMF0's recursion/item counters — a
//!   `Tags` value nesting objects thousands deep, or an ECMA array claiming a
//!   huge count, must produce an error rather than a large allocation or a
//!   stack overflow.
//! * **`Eof` is stable.**
//! * **Reading terminates**, bounded by a packet cap.
//! * **Every packet names a declared stream** — meaningful here specifically
//!   because the stream list can *grow* mid-read (the first video/audio tag
//!   creates it), which is the one thing that must never let a packet name a
//!   slot that does not exist yet.
//! * **Seeking, including the heuristic byte-seek resync, never panics** on
//!   adversarial tag headers.
//!
//! fuzz-crate: vaco-demux-flv

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_flv::FlvDemuxer;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut FlvDemuxer) -> u32 {
    let mut streams = d.streams().len() as u32;
    let mut n = 0u32;
    loop {
        let now = d.streams().len() as u32;
        assert!(now >= streams, "the stream list shrank while reading");
        streams = now;
        match d.read_packet() {
            Ok(p) => {
                let live = d.streams().len() as u32;
                assert!(
                    p.stream_index < live,
                    "packet names stream {} of {live}",
                    p.stream_index
                );
                assert!(p.len <= p.data.len());
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return n;
            }
            Err(_) => return n,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let opts = FormatOptions::default();
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = FlvDemuxer::open_with_limits(src, &opts, Limits::strict()) else {
        return;
    };

    if let Some(d) = demux.duration() {
        assert!(d.as_micros() >= 0, "negative container duration");
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
        if demux.seek(target, SeekFlags::empty()).is_ok() {
            let after = drain(&mut demux);
            assert!(after <= read.saturating_add(MAX_PACKETS));
        }
    }
    let _ = demux.seek(SeekTarget::Byte(data.len() as u64 / 2), SeekFlags::empty());
    let _ = drain(&mut demux);
});
