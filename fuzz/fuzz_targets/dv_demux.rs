//! Whole-file DV demux over arbitrary bytes.
//!
//! DV has no PSI, no PES, no boxes — the entire attack surface is
//! [`vaco_format_dv::profile::Profile::detect`]'s 4-byte sniff and the
//! fixed-size frame reads that follow it, plus `DvDemuxer::open`'s
//! second-frame sanity check (see `profile.rs`'s docs on the
//! DVCPRO50/DVCPRO HD gap that check exists for). Small as that surface is,
//! it is exactly the kind of thing a byte the fuzzer controls can walk into
//! a division or a wraparound.
//!
//! fuzz-crate: vaco-format-dv

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::Demuxer;
use vaco_format_dv::DvDemuxer;
use vaco_io::MemorySource;
use vaco_limits::Limits;

const MAX_PACKETS: u32 = 20_000;

fn drain(d: &mut DvDemuxer) -> u32 {
    let streams = d.streams().len() as u32;
    let mut n = 0u32;
    loop {
        match d.read_packet() {
            Ok(p) => {
                assert!(p.stream_index < streams, "packet names an unknown stream");
                assert!(p.len <= p.data.len());
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return n;
            }
            Err(_) => return n,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let src = Box::new(MemorySource::new(data.to_vec()));
    let Ok(mut demux) = DvDemuxer::open_with_limits(src, Limits::strict()) else {
        return;
    };

    // Exactly two streams, always: DV always declares video and audio (see
    // `demux.rs`'s docs on why audio packets are never produced today).
    assert_eq!(demux.streams().len(), 2);

    let read = drain(&mut demux);
    assert!(read <= MAX_PACKETS);

    let _ = demux.seek(SeekTarget::Byte(data.len() as u64 / 2), SeekFlags::empty());
    let _ = drain(&mut demux);
});
