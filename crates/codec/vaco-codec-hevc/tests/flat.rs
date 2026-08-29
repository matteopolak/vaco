//! A minimal diagnostic fixture: a flat grey 64x64 frame, encoded by
//! `libx265` at low complexity. Every sample should decode to the same
//! constant value (126) — this isolates whether the CTU walk, CABAC
//! contexts and reconstruction are even structurally sound before chasing
//! anything content-dependent.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "test code over a fixed, checked-in fixture: the (x, y) pair is used for both the \
              value lookup and the diagnostic eprintln!, which an iterator/enumerate rewrite \
              would make less readable here, not more"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const HEVC: &[u8] = include_bytes!("fixtures/flat_gray_64x64.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/flat_gray_64x64.yuv");

fn packet(bytes: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::default());
    Packet::from_slice(&mut budget, bytes).unwrap()
}

#[test]
fn flat_frame_decodes_to_a_constant() {
    let mut d = HevcDecoder::new(Limits::default());
    let pkt = packet(HEVC);
    d.send_packet(Some(&pkt)).unwrap();
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();

    let want = REF_YUV[0];
    eprintln!("reference constant = {want}");

    let plane = frame.plane(0).unwrap();
    let mut wrong = 0;
    for y in 0..64 {
        let row = plane.row(y).unwrap();
        for x in 0..64 {
            if row[x] != want {
                if wrong < 20 {
                    eprintln!("Y[{x},{y}] = {} (want {want})", row[x]);
                }
                wrong += 1;
            }
        }
    }
    eprintln!("{wrong}/4096 luma samples wrong");
    assert_eq!(wrong, 0, "flat frame did not decode to a constant");
}
