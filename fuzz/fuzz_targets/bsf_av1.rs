//! Every `vaco-bsf-av1` filter over an arbitrary packet stream.
//!
//! Same shape as `bsf_generic`'s harness: one driver over every filter
//! `vaco_bsf_av1::filters()` registers, feeding chunks of the fuzzer's input
//! as packets and draining until each filter signals it has nothing more.
//!
//! fuzz-crate: vaco-bsf-av1

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bsf_av1::filters;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

const MAX_PAYLOAD: usize = 4096;
const MAX_PACKETS: usize = 32;
const MAX_STEPS: u32 = 10_000;

fuzz_target!(|data: &[u8]| {
    let Some((&control, rest)) = data.split_first() else {
        return;
    };
    let descs = filters();
    let Some(desc) = descs.get(usize::from(control) % descs.len().max(1)) else {
        return;
    };
    let params = CodecParameters::video().with_codec(CodecId::Av1);
    let Ok(mut filter) = (desc.build)(&params) else {
        return;
    };

    let mut budget = Budget::new(Limits::permissive());
    for chunk in rest.chunks(MAX_PAYLOAD).take(MAX_PACKETS) {
        let Ok(mut pkt) = Packet::from_slice(&mut budget, chunk) else {
            break;
        };
        pkt.flags = if chunk.first().is_some_and(|b| b & 1 != 0) {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        if filter.send_packet(Some(&pkt)).is_err() {
            return;
        }
        let mut steps = 0u32;
        loop {
            steps += 1;
            assert!(steps < MAX_STEPS, "{}: receive loop did not terminate", desc.name);
            if filter.receive_packet().is_err() {
                break;
            }
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
