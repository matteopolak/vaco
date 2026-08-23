//! `h264_mp4toannexb`/`hevc_mp4toannexb` over an arbitrary length-prefixed
//! access unit and an arbitrary (possibly malformed) `avcC`/`hvcC` record.
//!
//! Two untrusted inputs meet here: the packet bytes (always attacker-
//! controlled, demuxed from a file) and the configuration record used to
//! build the filter in the first place (equally attacker-controlled — an
//! `avcC`/`hvcC` box is container metadata, not something this filter's
//! caller validated). A malformed record must degrade to "no parameter sets
//! to splice", never panic.
//!
//! fuzz-crate: vaco-bsf-h2645

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bsf_h2645::{h264_mp4toannexb, hevc_mp4toannexb};
use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const MAX_PAYLOAD: usize = 4096;
const MAX_STEPS: u32 = 10_000;

fuzz_target!(|data: &[u8]| {
    let Some((&control, rest)) = data.split_first() else {
        return;
    };
    // Split the rest into a configuration record and a packet payload at an
    // attacker-chosen point, so both "tiny record, huge packet" and "huge
    // record, tiny packet" are reachable from the same corpus.
    let split = rest.first().copied().unwrap_or(0) as usize % (rest.len() + 1).max(1);
    let (extradata, payload) = rest.split_at(split.min(rest.len()));
    let payload = payload.get(..payload.len().min(MAX_PAYLOAD)).unwrap_or(payload);

    let hevc = control & 0x01 != 0;
    let length_size = if control & 0x02 != 0 { Some(4) } else { None };
    let params = CodecParameters {
        codec_id: Some(if hevc { CodecId::Hevc } else { CodecId::H264 }),
        extradata: (control & 0x04 != 0).then(|| extradata.to_vec()),
        video: Some(VideoParameters {
            nal_length_size: length_size,
            ..VideoParameters::default()
        }),
        ..CodecParameters::video()
    };

    let built = if hevc {
        (hevc_mp4toannexb::DESC.build)(&params)
    } else {
        (h264_mp4toannexb::DESC.build)(&params)
    };
    let Ok(mut filter) = built else {
        return;
    };

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
        assert!(steps < MAX_STEPS, "receive loop did not terminate");
        if filter.receive_packet().is_err() {
            break;
        }
    }
    if filter.send_packet(None).is_ok() {
        let mut steps = 0u32;
        loop {
            steps += 1;
            assert!(steps < MAX_STEPS, "flush did not terminate");
            if filter.receive_packet().is_err() {
                break;
            }
        }
    }
});
