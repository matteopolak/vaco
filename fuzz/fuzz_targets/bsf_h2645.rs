//! `h264_mp4toannexb`/`hevc_mp4toannexb` over an arbitrary length-prefixed
//! access unit and an arbitrary (possibly malformed) `avcC`/`hvcC` record,
//! plus `h264_metadata`/`hevc_metadata` (issue #353) over the same packet
//! bytes.
//!
//! Two untrusted inputs meet here: the packet bytes (always attacker-
//! controlled, demuxed from a file) and the configuration record used to
//! build the filter in the first place (equally attacker-controlled — an
//! `avcC`/`hvcC` box is container metadata, not something this filter's
//! caller validated). A malformed record must degrade to "no parameter sets
//! to splice", never panic.
//!
//! `h264_metadata`/`hevc_metadata` need no extradata, so they are driven over
//! the same packet payload as a second, independent check in this target
//! rather than a separate `[[bin]]`: one more `PacketMap::push` call per run
//! is cheap, and it means this crate's four filters are covered by two fuzz
//! targets total instead of needing a third just for the two newest ones.
//! At the bare-name default (`aud=pass`), both are still the measured
//! identity transform and this target still asserts it byte for byte.
//!
//! `h264_metadata`'s `aud=insert` (gap 12) is new attacker-adjacent parsing:
//! it scans the payload for NAL units and reads a slice header's first two
//! `ue(v)` fields on every one it finds, which is exactly the class of
//! untrusted-input parsing D6 exists to fuzz. `control`'s top bit selects it
//! (H.264 only, since `hevc_metadata` does not implement the option); the
//! byte-identity assertion does not apply once `aud` is no longer `pass`, so
//! the only property checked there is "does not panic and does not loop".
//!
//! fuzz-crate: vaco-bsf-h2645

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bsf_h2645::{h264_metadata, h264_mp4toannexb, hevc_metadata, hevc_mp4toannexb};
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

    // Second, independent check: `h264_metadata`/`hevc_metadata` are the
    // measured identity transform (no options are reachable — gap 12), so
    // over the same codec/payload this run already built, output must equal
    // input exactly, not merely "did not panic".
    let metadata_params = CodecParameters {
        codec_id: Some(if hevc { CodecId::Hevc } else { CodecId::H264 }),
        ..CodecParameters::video()
    };
    let metadata_built = if hevc {
        (hevc_metadata::DESC.build)(&metadata_params)
    } else {
        (h264_metadata::DESC.build)(&metadata_params)
    };
    if let Ok(mut mf) = metadata_built {
        // Bit 3: exercise `aud` (gap 12), H.264 only — `hevc_metadata` does
        // not implement it, so this is still the bare-name default there.
        let aud_set = !hevc && control & 0x08 != 0;
        let identity_expected = if aud_set {
            let value = if control & 0x10 != 0 { "insert" } else { "remove" };
            mf.set_option("aud", value).is_err()
        } else {
            true
        };
        if let Ok(pkt) = Packet::from_slice(&mut budget, payload) {
            if mf.send_packet(Some(&pkt)).is_ok() {
                let mut steps = 0u32;
                loop {
                    steps += 1;
                    assert!(steps < MAX_STEPS, "metadata receive loop did not terminate");
                    match mf.receive_packet() {
                        Ok(out) => {
                            if identity_expected {
                                assert_eq!(out.payload(), payload, "*_metadata must be identity");
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    }
});
