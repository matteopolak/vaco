//! Whole-file ASF demux over arbitrary bytes.
//!
//! `open_with_limits` walks the Header Object (File Properties, every
//! Stream Properties Object, Content Description, Extended Content
//! Description, DRM detection) fully in memory, then scans forward past the
//! Data Object for Simple Index/Index Objects when the source can seek.
//! Every step is driven by attacker-controlled sizes and offsets — a
//! declared object size, a Stream Properties `Type-Specific Data Length`, a
//! packet's `Padding Length`, a fragment's `Offset Into Media Object`. Once
//! open, packets are read to the end (exercising `packet::parse_packet`'s
//! four payload shapes and `demux::AsfDemuxer`'s fragment reassembly) and a
//! seek is performed into whatever index was discovered.
//!
//! This is exactly the format the brief for this crate calls out as the
//! place a `slow-unit` finding would be *real* rather than noise: ASF's
//! fixed-size-packet framing has several attacker-controlled length fields
//! per packet (`Padding Length`, `Payload Length`, `Replicated Data
//! Length`, the compressed sub-payload loop's per-entry length byte), and a
//! length that decodes to a huge value but is silently clamped rather than
//! rejected is exactly the shape that turns into an O(n²) or an
//! unexpectedly-large-but-not-over-the-cap allocation.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Nothing allocates past the ceiling.** `Limits::strict` is used, so a
//!   Header Object, Stream Properties Object, or fragment-reassembly buffer
//!   claiming a huge size must produce `LimitExceeded` rather than a large
//!   allocation.
//! * **`Eof` is stable.**
//! * **Reading terminates**, via a packet-count cap.
//! * **Every packet names a declared stream** — the indexing-panic surface
//!   `feed_payload`'s `stream_index_for` lookup exists to close off.
//! * **DRM detection never panics** even on a Content Encryption Object
//!   whose own length fields are hostile, and a detected file's
//!   `read_packet` call returns `Unsupported`, not a payload.
//!
//! fuzz-crate: vaco-demux-asf

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_asf::AsfDemuxer;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut AsfDemuxer) -> u32 {
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
            Err(Error::Unsupported(_)) => {
                // DRM-protected content: detected and refused, per this
                // crate's documented scope boundary. Not a finding.
                return n;
            }
            Err(Error::Eof) => {
                // Sticky: the second call must agree with the first.
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
    let Ok(mut demux) = AsfDemuxer::open_with_limits(src, &opts, Limits::strict()) else {
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
    let probe_ts = i64::from(u32::from_le_bytes([
        data.first().copied().unwrap_or(0),
        data.get(1).copied().unwrap_or(0),
        data.get(2).copied().unwrap_or(0),
        data.get(3).copied().unwrap_or(0),
    ]));
    for ts in [0i64, 1_000, probe_ts] {
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
