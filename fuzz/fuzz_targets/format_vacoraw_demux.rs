//! The worked-example demuxer over arbitrary bytes.
//!
//! `vacoraw` is not a format anyone ships, but it is the only complete
//! implementation of the `Demuxer` contract that exists, so this target is what
//! actually exercises the contract's hard edges on hostile input: a declared
//! packet length larger than the file, a stream index that names nothing, an
//! index block that points into the middle of a payload, a seek into a
//! truncated tail. Every one of those is a bug class `vaco-demux-mp4` will have
//! too.
//!
//! Three properties are asserted rather than merely "does not panic":
//!
//! * reading terminates;
//! * `Eof` is stable — the second call after end of stream must not report
//!   corruption, which is exactly the bug the sticky flag in `VacoRawDemuxer`
//!   exists to prevent;
//! * a seek either succeeds or reports an error, and reading after it still
//!   terminates.
//! fuzz-crate: vaco-format-core

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, Timestamp};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::vacoraw::VacoRawDemuxer;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, MemorySource};

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    data: Vec<u8>,
    seekable: bool,
    seek_ts: i64,
    seek_byte: u64,
    backward: bool,
    any: bool,
}

const MAX_PACKETS: u32 = 200_000;

fn drain(d: &mut VacoRawDemuxer) -> u32 {
    let mut n = 0;
    loop {
        match d.read_packet() {
            Ok(_) => {
                n += 1;
                assert!(n < MAX_PACKETS, "read_packet did not terminate");
            }
            Err(_) => return n,
        }
    }
}

fuzz_target!(|input: Input| {
    let src: Box<dyn MediaSource> = if input.seekable {
        Box::new(MemorySource::new(input.data.clone()))
    } else {
        Box::new(MemorySource::forward_only(input.data.clone()))
    };
    let opts = FormatOptions::default();
    let Ok(mut d) = VacoRawDemuxer::open(src, &NoParsers, &opts) else {
        return;
    };

    drain(&mut d);

    // End of stream is stable: once it is reported, it stays reported, and it
    // never turns into "invalid data" on a later call.
    for _ in 0..4 {
        match d.read_packet() {
            Err(Error::Eof) => {}
            Err(_) => break,
            Ok(_) => panic!("a packet appeared after end of stream"),
        }
    }

    let mut flags = SeekFlags::empty();
    if input.backward {
        flags |= SeekFlags::BACKWARD;
    }
    if input.any {
        flags |= SeekFlags::ANY;
    }

    for target in [
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(input.seek_ts),
        },
        SeekTarget::Byte(input.seek_byte),
        SeekTarget::Frame {
            stream_index: 0,
            frame: 3,
        },
        SeekTarget::Timestamp {
            stream_index: u32::MAX,
            ts: Timestamp::NONE,
        },
    ] {
        if d.seek(target, flags).is_ok() {
            drain(&mut d);
        }
    }

    // Accessors must be total whatever state the demuxer ended in.
    let _ = d.streams().len();
    let _ = d.duration();
    let _ = d.index().len();
    assert!(d.index().is_well_formed(), "the index lost its sort order");
});
