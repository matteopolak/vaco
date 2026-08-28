//! Every `vaco-bsf-legacy` filter over an arbitrary packet stream.
//!
//! Same shape as `bsf_generic`: both `mpeg2_metadata` and `prores_metadata`
//! are the measured identity transform (see each module's docs), so beyond
//! "does not panic and terminates" this also asserts the one property their
//! whole design rests on — output equals input exactly, for every codec
//! gate and every payload the fuzzer finds.
//!
//! fuzz-crate: vaco-bsf-legacy

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bsf_legacy::filters;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const MAX_PAYLOAD: usize = 4096;
const MAX_STEPS: u32 = 10_000;

fuzz_target!(|data: &[u8]| {
    let Some((&control, rest)) = data.split_first() else {
        return;
    };
    let descs = filters();
    let Some(desc) = descs.get(usize::from(control) % descs.len().max(1)) else {
        return;
    };

    let codec_id = match control & 0x03 {
        0 => Some(CodecId::Mpeg2video),
        1 => Some(CodecId::Prores),
        2 => Some(CodecId::H264),
        _ => None,
    };
    let params = CodecParameters {
        codec_id,
        ..CodecParameters::video()
    };

    let Ok(mut filter) = (desc.build)(&params) else {
        return;
    };

    let payload = rest.get(..rest.len().min(MAX_PAYLOAD)).unwrap_or(rest);
    let mut budget = Budget::new(Limits::permissive());
    let Ok(pkt) = Packet::from_slice(&mut budget, payload) else {
        return;
    };
    if filter.send_packet(Some(&pkt)).is_err() {
        return;
    }
    let mut steps = 0u32;
    loop {
        steps += 1;
        assert!(steps < MAX_STEPS, "{}: receive loop did not terminate", desc.name);
        match filter.receive_packet() {
            Ok(out) => assert_eq!(out.payload(), payload, "{}: must be identity", desc.name),
            Err(_) => break,
        }
    }
    if filter.send_packet(None).is_ok() {
        let mut steps = 0u32;
        loop {
            steps += 1;
            assert!(steps < MAX_STEPS, "{}: flush did not terminate", desc.name);
            if filter.receive_packet().is_err() {
                break;
            }
        }
    }
});
