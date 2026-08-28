//! Whole-file `spdif` demux over arbitrary bytes.
//!
//! The attack surface: `Pd` is an attacker-chosen 16-bit length field used
//! to size the AC-3 payload read out of a fixed 6144-byte burst
//! (`BurstHeader::ac3_payload_len_bytes` in `iec61937.rs`), and
//! `SpdifDemuxer::read_burst` in `demux.rs` must reject a payload length
//! that would overrun the burst before reading it.
//!
//! fuzz-crate: vaco-format-spdif

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_spdif::SpdifDemuxer;
use vaco_io::MemorySource;
use vaco_limits::Limits;

const MAX_PACKETS: u32 = 20_000;

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = SpdifDemuxer::open_with_limits(src, Limits::strict()) else {
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
