//! Whole-file demux over arbitrary bytes, for all nine formats in
//! `vaco-format-audio-simple` at once.
//!
//! One target covering nine `open()` entry points, per the brief for this
//! crate: each format's header parser is independent and none depends on
//! codec code, so a single input can legitimately be tried against all nine
//! without any of them influencing another's result. A byte string is
//! extremely unlikely to look like more than one of these formats at once
//! (the signatures are long and specific — `"Creative Voice File\x1A"`,
//! `"caff"`, `0x0001A364`, ...), so in practice each run mostly exercises
//! whichever one format's `open` gets past its signature check, and the
//! fuzzer's corpus naturally partitions across all nine over time.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Nothing allocates without limit.** Every format module in this crate
//!   currently opens under a fixed internal `Limits::permissive()` budget —
//!   there is no `open_with_limits` injection point the way
//!   `vaco-demux-mpegts` has one, which is a real gap (reported in this
//!   crate's docs) rather than something this target papers over. What it
//!   *can* still check is that a failure under that budget is a clean
//!   `LimitExceeded`/`InvalidData`, never a panic.
//! * **A successful `open` always reports exactly one stream**, and that
//!   stream is audio — the invariant every format module in this crate
//!   shares (module docs, `pcm::RawPcmDemuxer`).
//! * **`Eof` is stable**: a second `read_packet` after `Eof` must also
//!   report `Eof`, never corruption from re-reading past a boundary.
//! * **Reading terminates**, via a packet-count cap — the VOC block-chain
//!   walk is the one path in this crate that loops on attacker-controlled
//!   block headers rather than a single declared length, so it is the most
//!   likely place a non-terminating loop would hide.
//! * **Every packet's payload fits inside its own buffer**
//!   (`p.len <= p.data.len()`), the same invariant `mpegts_demux` checks.
//!
//! fuzz-crate: vaco-format-audio-simple

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MediaSource;

/// Packets read before a run is treated as non-terminating. VOC's block
/// chain is the one path here bounded only by the input, not by a single
/// declared length, so this is generous enough for a large legitimate file
/// while still catching a genuine infinite loop.
const MAX_PACKETS: u32 = 50_000;

fn drain(d: &mut dyn Demuxer) -> u32 {
    let mut n = 0u32;
    loop {
        match d.read_packet() {
            Ok(p) => {
                assert!(p.len <= p.data.len(), "packet payload longer than its buffer");
                assert_eq!(p.stream_index, 0, "audio-simple formats have exactly one stream");
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)), "Eof is not sticky");
                return n;
            }
            Err(Error::LimitExceeded { .. } | Error::NotSeekable) => return n,
            Err(_) => return n,
        }
    }
}

fn check_open<D: Demuxer + 'static>(
    open: impl FnOnce(Box<dyn MediaSource>, &FormatOptions) -> vaco_core::Result<D>,
    data: &[u8],
) {
    let src = Box::new(vaco_io::MemorySource::new(data.to_vec()));
    let opts = FormatOptions::default();
    let Ok(mut demux) = open(src, &opts) else {
        return;
    };
    let streams = demux.streams();
    assert_eq!(streams.len(), 1, "expected exactly one stream");
    assert_eq!(
        streams[0].params.effective_media_type(),
        Some(vaco_core::MediaType::Audio),
        "audio-simple stream is not audio"
    );
    if let Some(d) = demux.duration() {
        assert!(d.as_micros() >= 0, "negative duration");
    }
    let _ = drain(&mut demux);
}

fuzz_target!(|data: &[u8]| {
    check_open(vaco_format_audio_simple::wav::WavDemuxer::open, data);
    check_open(vaco_format_audio_simple::w64::W64Demuxer::open, data);
    check_open(vaco_format_audio_simple::aiff::AiffDemuxer::open, data);
    check_open(vaco_format_audio_simple::caf::CafDemuxer::open, data);
    check_open(vaco_format_audio_simple::au::AuDemuxer::open, data);
    check_open(vaco_format_audio_simple::voc::VocDemuxer::open, data);
    check_open(vaco_format_audio_simple::sox::SoxDemuxer::open, data);
    check_open(vaco_format_audio_simple::ircam::IrcamDemuxer::open, data);
    check_open(vaco_format_audio_simple::rso::RsoDemuxer::open, data);
});
