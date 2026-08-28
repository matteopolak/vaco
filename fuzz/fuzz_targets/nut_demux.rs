//! Whole-file NUT demux over arbitrary bytes.
//!
//! The attack surface is broader than most containers this crate's siblings
//! fuzz: `forward_ptr` sizes a header-packet allocation (`demux.rs`'s
//! `read_startcoded_packet`, budget-checked via `Budget::alloc`), `vb`'s
//! length prefix sizes `fourcc`/`codec_specific_data`/elision-header
//! allocations (`vlc.rs`'s `read_vb`), and a frame's `data_size` (built from
//! two attacker-influenced pieces — the frame-code table's `data_size_mul`
//! and a per-frame `data_size_msb`, both multiplied together with
//! `saturating_mul` specifically to avoid the overflow that combination
//! invites) sizes `Packet::alloc`. The frame-code table construction loop
//! (`header.rs`'s `read_frame_code_table`) has its own termination bound
//! independent of the outer file size, since a batch's declared `count`
//! does not have to make the `i<256` loop progress quickly.
//!
//! fuzz-crate: vaco-format-nut

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_nut::NutDemuxer;
use vaco_io::MemorySource;
use vaco_limits::Limits;

const MAX_PACKETS: u32 = 20_000;

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = NutDemuxer::open_with_limits(src, Limits::strict()) else {
        return;
    };

    let mut n = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                let streams = demux.streams().len() as u32;
                assert!(p.stream_index < streams, "packet names an unknown stream");
                assert!(p.len <= p.data.len());
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => break,
            Err(_) => break,
        }
    }
});
