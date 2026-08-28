//! Whole-file demux over arbitrary bytes, for all twenty `DemuxerDesc`s this
//! crate registers, tried against the same input in one run.
//!
//! Every format here either has a distinct fixed-offset signature (`wv`,
//! `tta`, `adx`, `#!AMR\n`, `NIST_1A\n`, `PVF1\n`) or none at all (the
//! headerless ITU/3GPP/Bluetooth codecs, `amrnb`/`amrwb`), so trying all
//! twenty `open`s against one input cannot make one format's parser see
//! bytes another format produced — each either rejects the input at its own
//! header check or, for the headerless ones, treats the whole input as its
//! own raw stream independently.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Every successful `open` reports exactly one audio stream** — the
//!   invariant every format module in this crate shares.
//! * **`Eof` is sticky**: a second `read_packet` after `Eof` must also
//!   report `Eof`.
//! * **Reading terminates**, via a packet-count cap. `wv`'s block chain and
//!   `sbc`/`g723_1`'s self-delimited frame walks are the paths here bounded
//!   by attacker-controlled per-record sizes rather than one declared
//!   length, so they are the most likely place a non-terminating loop would
//!   hide.
//! * **Every packet's payload fits inside its own buffer.**
//!
//! fuzz-crate: vaco-format-misc-audio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, DemuxerDesc};
use vaco_io::MediaSource;

use vaco_format_misc_audio::{adx, amr, g723, nistsphere, pvf, rawcodec, sbc, tta, wavpack};

const MAX_PACKETS: u32 = 50_000;

fn drain(d: &mut dyn Demuxer) {
    let mut n = 0u32;
    loop {
        match d.read_packet() {
            Ok(p) => {
                assert!(p.len <= p.data.len(), "packet payload longer than its buffer");
                assert_eq!(p.stream_index, 0, "exactly one stream is registered per format here");
                n += 1;
                assert!(n < MAX_PACKETS, "read did not terminate");
            }
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)), "Eof is not sticky");
                return;
            }
            Err(_) => return,
        }
    }
}

fn check(desc: &DemuxerDesc, data: &[u8]) {
    let src: Box<dyn MediaSource> = Box::new(vaco_io::MemorySource::new(data.to_vec()));
    let Ok(mut demux) = (desc.open)(src, &NoParsers) else {
        return;
    };
    let streams = demux.streams();
    assert_eq!(streams.len(), 1, "expected exactly one stream");
    assert!(
        streams.first().is_some_and(|s| s.params.audio.is_some()),
        "expected an audio stream"
    );
    drain(demux.as_mut());
}

fuzz_target!(|data: &[u8]| {
    for desc in [
        &wavpack::DEMUXER,
        &tta::DEMUXER,
        &amr::DEMUXER_AMR,
        &amr::DEMUXER_AMRNB,
        &amr::DEMUXER_AMRWB,
        &adx::DEMUXER,
        &nistsphere::DEMUXER,
        &pvf::DEMUXER,
        &g723::DEMUXER,
        &sbc::DEMUXER,
        &rawcodec::DEMUXER_GSM,
        &rawcodec::DEMUXER_SLN,
        &rawcodec::DEMUXER_DFPWM,
        &rawcodec::DEMUXER_G722,
        &rawcodec::DEMUXER_G726,
        &rawcodec::DEMUXER_G726LE,
        &rawcodec::DEMUXER_G728,
        &rawcodec::DEMUXER_G729,
        &rawcodec::DEMUXER_APTX,
        &rawcodec::DEMUXER_APTX_HD,
    ] {
        check(desc, data);
    }
});
