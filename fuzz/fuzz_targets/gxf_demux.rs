//! Whole-file GXF demuxing over arbitrary bytes.
//!
//! GXF's packets are length-prefixed at the outermost layer only (each
//! packet header states its own total length), but the MAP packet nests
//! two further length-prefixed sections (material data, track
//! description) and, inside each, a run of one-byte-length
//! tag/length/value items — reachable from a single `open` call, since
//! `GxfDemuxer::open` parses the file's first MAP packet unconditionally.
//! What this target checks beyond "does not panic":
//!
//! * **Reading terminates** for a file whose packet lengths chain into a
//!   very large or looping sequence of packets — bounded here by
//!   `MAX_PACKETS`, independent of `packet::MAX_PACKET_BYTES`'s own
//!   per-packet cap.
//! * **Every returned packet names a stream this demuxer actually built**,
//!   the same indexing-panic surface every other demux fuzz target in
//!   this workspace closes off.
//! * **`Eof` is stable**: a second `read_packet` after `Eof` returns `Eof`
//!   again.
//!
//! fuzz-crate: vaco-format-gxf

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_gxf::GxfDemuxer;
use vaco_io::MemorySource;

/// Packets read before a run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = GxfDemuxer::open(src, &NoParsers) else {
        return;
    };

    let streams = demux.streams().len() as u32;
    for (i, s) in demux.streams().iter().enumerate() {
        assert_eq!(s.index as usize, i, "stream index does not match its slot");
    }

    let mut n = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                assert!(p.stream_index < streams, "packet names stream {} of {streams}", p.stream_index);
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
