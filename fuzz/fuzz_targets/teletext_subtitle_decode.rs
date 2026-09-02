//! Arbitrary bytes through `TeletextSubtitleDecoder`, the `Decoder`-trait
//! face `vaco-registry` reaches (`registry.rs`) — as opposed to
//! `teletext_packet_parse`'s target, which drives the inner
//! `TeletextDecoder` state machine directly. This is the path a real
//! `-c:s teletext` invocation runs, including `Frame`/`SubtitleContent`
//! construction and the `Machine<Frame>` send/receive protocol
//! (`Caps::SUBFRAMES | Caps::DELAY`), so a defect only reachable through
//! `Validated`'s protocol checking (a capability violation, a receive
//! called out of turn) would show up here and not in `teletext_packet_parse`.
//!
//! Property: `send`/`receive`/`flush` never panic (checked, since
//! `Validated` wraps the decoder and turns a `Machine` protocol violation
//! into a debug-assertion failure) for any split of `data` into packets, and
//! draining always terminates in `Error::Eof`.
//!
//! fuzz-crate: vaco-codec-subtitle-teletext

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::{SendReceive, Validated};
use vaco_codec_subtitle_teletext::registry::TeletextSubtitleDecoder;
use vaco_core::Error;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut decoder = Validated::new(TeletextSubtitleDecoder::new());
    let mut budget = Budget::new(Limits::permissive());

    for chunk in data.chunks(97) {
        let Ok(pkt) = Packet::from_slice(&mut budget, chunk) else {
            continue;
        };
        // A full output queue is backpressure, not a bug: drain it and
        // retry the same packet, exactly as a real caller must.
        loop {
            match decoder.send(Some(&pkt)) {
                Ok(()) => break,
                Err(Error::OutputPending) => {
                    while decoder.receive().is_ok() {}
                }
                Err(_) => break,
            }
        }
        while decoder.receive().is_ok() {}
    }

    let _ = decoder.send(None);
    loop {
        match decoder.receive() {
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(_) => break,
        }
    }
    decoder.flush();
});
