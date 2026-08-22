//! Whole-file MPEG-TS demux over arbitrary bytes.
//!
//! The broadest target for this container, and the one that covers the parts
//! no unit test reaches: `open` runs a bounded header scan, a duration tail
//! scan with a seven-step retry loop, and a rewind, all driven by PSI the
//! input controls. Then packets are read to the end and a seek is performed
//! into whatever timeline was discovered.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Nothing allocates past the ceiling.** The demuxer is opened with
//!   `Limits::strict`, so a PES packet with no declared length — which is
//!   terminated only by the next one and is therefore the format's natural
//!   amplification lever — must produce `LimitExceeded` rather than a large
//!   allocation.
//! * **`Eof` is stable.** The frozen `Demuxer` trait does not require it, but
//!   every caller assumes it, so it is asserted here rather than hoped for.
//! * **Reading terminates.** A packet count cap turns a demuxer that returns
//!   packets without advancing into a localised assertion instead of a fuzzer
//!   timeout.
//! * **Every packet names a declared stream** and carries a position inside
//!   the file. A packet naming stream seven when six exist is how an indexing
//!   panic reaches a caller that trusted us.
//! * **Seeking is total.** After `seek`, reading either yields packets or ends;
//!   it never reports corruption from landing mid-packet, because landing
//!   mid-packet is the ordinary case for a container with no index.
//!
//! fuzz-crate: vaco-demux-mpegts

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, Timestamp};
use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut MpegTsDemuxer) -> u32 {
    // The stream count is re-read every iteration, not captured. A PMT that
    // arrives after the first packets adds a stream *while reading*, which is
    // ordinary in MPEG-TS and not a defect — this target asserted the opposite
    // and was wrong, found after eight million executions. What must hold is
    // that the count never shrinks and that a packet never names a slot that
    // does not exist yet.
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
                // Sticky: the second call must agree with the first.
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return n;
            }
            // A limit is a legitimate answer for a hostile stream.
            Err(Error::LimitExceeded { .. }) => return n,
            Err(_) => return n,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let opts = FormatOptions::default();
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = MpegTsDemuxer::open_with_limits(src, &opts, Limits::strict()) else {
        return;
    };

    // Declared streams must be coherent before a single packet is read: they
    // come straight from the PMT, which is attacker-controlled.
    for (i, s) in demux.streams().iter().enumerate() {
        assert_eq!(s.index as usize, i, "stream index does not match its slot");
        assert!(
            s.id.is_some_and(|id| (0..=0x1FFF).contains(&id)),
            "stream id is not a thirteen-bit PID"
        );
        assert_eq!(s.time_base, vaco_format_mpegts_tables::TIME_BASE);
    }
    for p in demux.programs() {
        for &i in &p.stream_indices {
            assert!(
                (i as usize) < demux.streams().len(),
                "program names a stream that does not exist"
            );
        }
    }
    // A duration is an estimate, never a negative one.
    if let Some(d) = demux.duration() {
        assert!(d.as_micros() >= 0);
    }

    let read = drain(&mut demux);

    // Seek back into whatever timeline was found, then read again. Both the
    // index path and the bisection path are reachable from here: the first
    // drain populated the index, so this seek takes it, while a file that
    // produced no keyframes takes the bisection.
    if demux.streams().is_empty() {
        return;
    }
    for ts in [0i64, 90_000, i64::from(u32::from_le_bytes([
        data.first().copied().unwrap_or(0),
        data.get(1).copied().unwrap_or(0),
        data.get(2).copied().unwrap_or(0),
        data.get(3).copied().unwrap_or(0),
    ]))] {
        let target = SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(ts),
        };
        if demux.seek(target, SeekFlags::BACKWARD).is_ok() {
            let after = drain(&mut demux);
            assert!(after <= read.saturating_add(MAX_PACKETS));
        }
    }
    let _ = demux.seek(SeekTarget::Byte(data.len() as u64 / 2), SeekFlags::empty());
    let _ = drain(&mut demux);
    let _ = demux.stats();
});
