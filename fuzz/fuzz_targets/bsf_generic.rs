//! Every `vaco-bsf-generic` filter over an arbitrary packet stream.
//!
//! Every filter here runs on demuxed packets — attacker bytes, once removed
//! from the container — so a hang, a panic, or an unbounded queue growth is a
//! real finding, not a hypothetical one. One harness drives all of them
//! rather than one fuzz target per filter: the send/receive protocol
//! [`vaco_bsf_core::MappedFilter`] gives every filter is identical, so the
//! interesting variable is *which* filter and *what bytes*, not the driver
//! loop.
//!
//! Properties asserted beyond "does not panic":
//!
//! * **The driver terminates.** `MAX_STEPS` bounds the receive loop so a
//!   filter that never returns `NeedMoreInput`/`Eof` is a fuzz failure
//!   (timeout) rather than a hang nobody notices.
//! * **`chomp`'s output is never longer than its input.** The one filter here
//!   whose contract (drop trailing zero bytes) makes growth a bug by
//!   definition, so it is checked directly rather than left to "did not
//!   panic".
//!
//! fuzz-crate: vaco-bsf-generic

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bsf_generic::filters;
use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Caps how much of the fuzzer's input becomes one packet's payload.
const MAX_PAYLOAD: usize = 4096;

/// Caps how many packets one run feeds a filter.
const MAX_PACKETS: usize = 32;

/// Caps `receive_packet` calls per `send_packet`, so a filter that never
/// signals "nothing more for now" is a timeout, not a silent pass.
const MAX_STEPS: u32 = 10_000;

fn codec_params(codec_id: Option<CodecId>, has_extradata: bool, length_size: Option<u8>) -> CodecParameters {
    CodecParameters {
        codec_id,
        extradata: has_extradata.then(|| vec![1, 0x64, 0x00, 0x0a, 0xFF, 0xE1, 0, 0]),
        video: Some(VideoParameters {
            nal_length_size: length_size,
            ..VideoParameters::default()
        }),
        ..CodecParameters::video()
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&control, rest)) = data.split_first() else {
        return;
    };
    let descs = filters();
    let Some(desc) = descs.get(usize::from(control) % descs.len().max(1)) else {
        return;
    };

    let codec_id = match control & 0x03 {
        0 => Some(CodecId::H264),
        1 => Some(CodecId::Hevc),
        2 => Some(CodecId::Av1),
        _ => None,
    };
    let params = codec_params(
        codec_id,
        control & 0x04 != 0,
        (control & 0x08 != 0).then_some(4),
    );

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
        let sent_bytes = chunk.len() as u64;

        if filter.send_packet(Some(&pkt)).is_err() {
            return;
        }
        let mut steps = 0u32;
        let mut received_bytes: u64 = 0;
        loop {
            steps += 1;
            assert!(steps < MAX_STEPS, "{}: receive loop did not terminate", desc.name);
            match filter.receive_packet() {
                Ok(p) => {
                    received_bytes = received_bytes.saturating_add(p.len as u64);
                }
                Err(_) => break,
            }
        }
        if desc.name == "chomp" {
            assert!(
                received_bytes <= sent_bytes,
                "chomp must never grow a packet"
            );
        }
    }

    // Flush.
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
