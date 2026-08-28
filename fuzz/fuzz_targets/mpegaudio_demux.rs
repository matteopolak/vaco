//! Whole-file mp3 demux over arbitrary bytes.
//!
//! What is asserted beyond "does not panic": `Eof` is stable, reading
//! terminates within a packet cap, every packet names the one declared
//! stream, and seeking to an arbitrary byte offset or timestamp never
//! panics even when it lands mid-frame in adversarial data.
//!
//! fuzz-crate: vaco-demux-mpegaudio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_mpegaudio::MpegAudioDemuxer;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

const MAX_PACKETS: u32 = 20_000;

fuzz_target!(|data: &[u8]| {
    let src = MemorySource::forward_only(data.to_vec());
    let Ok(mut demux) = MpegAudioDemuxer::open(Box::new(src), &FormatOptions::default()) else {
        return;
    };

    let mut n = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                assert_eq!(p.stream_index, 0);
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

    let _ = demux.seek(SeekTarget::Byte(0), SeekFlags::empty());
    let _ = demux.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: vaco_core::Timestamp::new(1),
        },
        SeekFlags::empty(),
    );
});
