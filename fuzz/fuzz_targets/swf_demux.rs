//! Whole-file SWF demux over arbitrary bytes.
//!
//! The attack surface: every tag's length is attacker-chosen (the 6-bit
//! short form or the escaped `u32` long form, `tags.rs`), used to size a
//! `Budget`-checked allocation (`Packet::alloc`/`budget.alloc` in
//! `demux.rs`) before the payload is read — and the `RECT`'s `Nbits` field
//! (`header.rs`) controls how many bits four fields are read as, which a
//! malformed value could otherwise walk arbitrarily far past the buffer.
//!
//! fuzz-crate: vaco-format-swf

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_swf::SwfDemuxer;
use vaco_io::MemorySource;
use vaco_limits::Limits;

const MAX_PACKETS: u32 = 20_000;

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = SwfDemuxer::open_with_limits(src, Limits::strict()) else {
        return;
    };

    let mut n = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                // Unlike a fixed-header container, SWF (like FLV) declares
                // a stream only when the tag that needs it first appears,
                // so the count can grow between `open` and a later packet
                // — re-read it each time rather than caching it once.
                let streams = demux.streams().len() as u32;
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
