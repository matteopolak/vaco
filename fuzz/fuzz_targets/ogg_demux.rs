//! Whole-file Ogg demux over arbitrary bytes.
//!
//! The broadest target this crate has, and the one that exercises what no
//! unit test reaches together: page resynchronisation after a bad capture
//! pattern or a failed CRC, packet reassembly across page continuations
//! (including a dangling continuation with nothing pending, and a page
//! flagged `CONTINUED` when nothing is), chained-stream discovery mid-file,
//! and every codec's granule mapping — all driven by attacker-chosen bytes,
//! with `NoParsers` so Opus always takes the page-anchored fallback path
//! rather than the exact `ParserProvider` one (a separate, narrower
//! surface `vaco-parse-opus`'s own fuzz targets already cover).
//!
//! What is asserted beyond "does not panic":
//!
//! * **Nothing allocates past the ceiling.** Opened with `Limits::strict`,
//!   so a page-spanning packet that never terminates — the format's natural
//!   amplification lever, since a lacing table can claim up to 65 025 body
//!   bytes per page forever — must produce `LimitExceeded` rather than
//!   unbounded growth.
//! * **`Eof` is stable.** The frozen `Demuxer` trait does not require it,
//!   but every caller assumes it.
//! * **Reading terminates.** A packet count cap turns a demuxer that
//!   returns packets without advancing into a localised assertion instead
//!   of a fuzzer timeout.
//! * **Every packet names a declared stream**, and the stream list only
//!   ever grows — chained and multiplexed discovery both add streams
//!   without ever removing or renumbering one, mirroring the invariant
//!   `vaco-demux-mpegts`'s fuzz target checks for its own stream-count
//!   growth.
//! * **A byte seek is total.** After a byte-position seek, reading either
//!   yields packets or ends cleanly; it never panics from landing mid-page.
//!
//! fuzz-crate: vaco-demux-ogg

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_ogg::OggDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::Demuxer;
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut OggDemuxer) -> u32 {
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
                // Every stream this crate can identify is audio or Theora
                // video; a packet's duration must never be negative
                // whatever the granule arithmetic did with hostile input.
                assert!(p.duration.as_micros() >= 0, "negative packet duration");
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)), "Eof is not sticky");
                return n;
            }
            Err(Error::LimitExceeded { .. }) => return n,
            Err(_) => return n,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = OggDemuxer::open_with_limits(src, &NoParsers, Limits::strict()) else {
        return;
    };

    for (i, s) in demux.streams().iter().enumerate() {
        assert_eq!(s.index as usize, i, "stream index does not match its slot");
    }

    let read = drain(&mut demux);

    if demux.streams().is_empty() {
        return;
    }
    // Only the byte-position path is implemented (see the docs file); a
    // timestamp seek must fail cleanly rather than do anything surprising.
    let target = SeekTarget::Timestamp {
        stream_index: 0,
        ts: vaco_core::Timestamp::new(0),
    };
    let _ = demux.seek(target, SeekFlags::empty());

    let byte_target = SeekTarget::Byte(data.len() as u64 / 2);
    if demux.seek(byte_target, SeekFlags::empty()).is_ok() {
        let after = drain(&mut demux);
        assert!(after <= read.saturating_add(MAX_PACKETS));
    }
    let _ = demux.stats();
});
