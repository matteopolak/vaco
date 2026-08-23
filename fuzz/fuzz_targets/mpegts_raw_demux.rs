//! `mpegtsraw` over arbitrary bytes.
//!
//! Distinct from `mpegts_demux`: this is a different `DemuxerDesc` (never
//! auto-probed) with its own open path, its own resynchronisation loop bounded
//! by a different constant (`RESYNC_SIZE`, not `MAX_RESYNC_BYTES`), and no PSI
//! or PES layer at all — every byte of untrusted input reaches the demuxer
//! through stride detection and the raw-packet resync scan alone, so it needs
//! its own target rather than riding on `mpegts_demux`'s coverage.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Every packet is exactly 188 bytes.** The one invariant this demuxer
//!   exists to provide: whatever the stride, the M2TS prefix (if any) is
//!   stripped before the caller ever sees the payload.
//! * **`Eof` is stable**, matching `vaco-demux-mpegts::MpegTsDemuxer`.
//! * **Reading terminates**, via a packet-count cap.
//! * **Seeking is total**: after a byte seek, reading either yields packets
//!   or ends cleanly.
//!
//! fuzz-crate: vaco-demux-mpegts

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_mpegts::MpegTsRawDemuxer;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::Limits;

/// Packets read before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut MpegTsRawDemuxer) -> u32 {
    let mut n = 0u32;
    loop {
        match d.read_packet() {
            Ok(p) => {
                assert_eq!(p.len, 188, "mpegtsraw must always emit the stripped 188-byte body");
                assert_eq!(p.stream_index, 0);
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)), "Eof must be sticky");
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
    let Ok(mut demux) = MpegTsRawDemuxer::open_with_limits(src, &opts, Limits::strict()) else {
        return;
    };
    assert_eq!(demux.streams().len(), 1, "mpegtsraw always reports exactly one stream");
    assert!(demux.duration().is_none());

    let _ = drain(&mut demux);
    let _ = demux.seek(SeekTarget::Byte(data.len() as u64 / 2), SeekFlags::empty());
    let _ = drain(&mut demux);
});
