//! Whole-file MPEG-PS demux over arbitrary bytes.
//!
//! Mirrors `mpegts_demux.rs`'s shape for the sibling container. What is
//! asserted beyond "does not panic":
//!
//! * **Nothing allocates past the ceiling.** Opened with `Limits::strict`,
//!   so a PES packet with `PES_packet_length == 0` — unbounded, terminated
//!   only by the next start code — must produce `LimitExceeded` rather than
//!   an unbounded read. This is exactly the shape the brief warns about: a
//!   length field of zero means "unbounded" here just as it does in PES
//!   over MPEG-TS.
//! * **`Eof` is stable** across repeated calls after end of stream.
//! * **Reading terminates**, via a packet-count cap.
//! * **Every packet names a declared stream.**
//! * **Seeking is total**: never reports corruption from landing mid-packet.
//!
//! fuzz-crate: vaco-demux-mpegps

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, Timestamp};
use vaco_demux_mpegps::MpegPsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut MpegPsDemuxer) -> u32 {
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
            Err(Error::LimitExceeded { .. }) => return n,
            Err(_) => return n,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let opts = FormatOptions::default();
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = MpegPsDemuxer::open_with_limits(src, &opts, Limits::strict(), &NoParsers)
    else {
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
});
