//! Whole-file `s337m` demux over arbitrary bytes.
//!
//! `S337mDemuxer` is a thin wrapper around `SpdifDemuxer` today (see
//! `s337m.rs`'s module docs for why), so this shares `spdif_demux.rs`'s
//! attack surface exactly. Kept as its own target rather than folded into
//! that one so a future divergence between the two demuxers gets its own
//! coverage rather than silently losing it.
//!
//! fuzz-crate: vaco-format-spdif

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_spdif::S337mDemuxer;
use vaco_io::MemorySource;
use vaco_limits::Limits;

const MAX_PACKETS: u32 = 20_000;

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = S337mDemuxer::open_with_limits(src, Limits::strict()) else {
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
