//! Whole-file MPJPEG demux over arbitrary bytes.
//!
//! The attack surface: `Content-length` is an attacker-chosen number parsed
//! straight out of a header line and used to size one allocation
//! (`Packet::alloc` in `demux.rs`), and the header-line scanner grows its
//! peek window on an unterminated line. Both are exactly the "declared
//! length vs actual remaining bytes" shape `planning/AGENT-CONSTRAINTS.md`
//! calls out for container demuxers.
//!
//! fuzz-crate: vaco-format-mpjpeg

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_mpjpeg::MpjpegDemuxer;
use vaco_io::MemorySource;
use vaco_limits::Limits;

const MAX_PACKETS: u32 = 20_000;

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = MpjpegDemuxer::open_with_limits(src, Limits::strict()) else {
        return;
    };

    let streams = demux.streams().len() as u32;
    let mut n = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                assert!(p.stream_index < streams, "packet names an unknown stream");
                assert!(p.len <= p.data.len());
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(demux.read_packet(), Err(Error::Eof)));
                break;
            }
            Err(_) => break,
        }
    }
});
