//! Every `vaco-bsf-audio` filter over an arbitrary packet stream.
//!
//! Same shape as `bsf_generic`/`bsf_av1`/`bsf_vpx`'s harness. Each filter
//! here refuses at construction unless handed its own codec id (`aac`,
//! `opus`, a PCM variant), so the codec is picked to match `desc.name`
//! rather than fixed once for the whole crate — a fixed choice would make
//! two of the three filters return `Unsupported` on every input and never
//! actually get fuzzed.
//!
//! fuzz-crate: vaco-bsf-audio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bsf_audio::filters;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

const MAX_PAYLOAD: usize = 4096;
const MAX_PACKETS: usize = 32;
const MAX_STEPS: u32 = 10_000;

fn params_for(name: &str) -> CodecParameters {
    let codec_id = match name {
        "aac_adtstoasc" => CodecId::Aac,
        "opus_metadata" => CodecId::Opus,
        _ => CodecId::PcmS16le,
    };
    let mut p = CodecParameters::audio().with_codec(codec_id);
    p.audio = Some(AudioParameters {
        sample_rate: 44_100,
        layout: Some(ChannelLayout::STEREO),
        ..AudioParameters::default()
    });
    p
}

fuzz_target!(|data: &[u8]| {
    let Some((&control, rest)) = data.split_first() else {
        return;
    };
    let descs = filters();
    let Some(desc) = descs.get(usize::from(control) % descs.len().max(1)) else {
        return;
    };
    let params = params_for(desc.name);
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
        // A filter reporting a shape it does not cover (e.g.
        // `aac_adtstoasc` on a non-ADTS payload) is a legitimate refusal,
        // not a fuzz finding.
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
