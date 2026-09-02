//! Regression test for the *other* half of clause 6.4.11.7's availability
//! rule: a `B_8x8` macroblock whose four quadrants do not all predict from
//! the same reference picture list.
//!
//! Clause 6.4's availability is a property of the **macroblock**, not of a
//! list. `MvInfo::mb_available` therefore has to be set by
//! `decode_sub_mb_pred_cabac`/`_cavlc`'s `ref_idx_l0`/`ref_idx_l1` passes,
//! which between them visit every quadrant that predicts at all, and *not*
//! by the later `mvd_l0`/`mvd_l1` passes -- those run list 0 across all four
//! quadrants before list 1, so setting it there makes availability per-list.
//! A `B_L1_8x8` quadrant is then still "unavailable" while a later
//! quadrant's own list-0 prediction reads it as `A`, `B` or `D`, and clause
//! 8.4.1.3.1's "if `B` and `C` are both not available and `A` is available,
//! `mvLXB = mvLXA`" shortcut fires (or fails to fire) against inputs the
//! specification does not give it. Clause 8.4.1.3.2 is explicit that this is
//! a *different* condition: a partition whose `predFlagLX` is 0 gets
//! `mvLXN = (0, 0)` and `refIdxLXN = -1` and **stays available**.
//!
//! `f970c23` moved that flag into the `mvd` passes, reaching clause
//! 6.4.11.7's genuine "not yet decoded" case the wrong way. It is invisible
//! in a P slice -- every partition there predicts from list 0, so the two
//! passes coincide -- which is why it left baseline-profile and `-bf 0`
//! output byte-exact while regressing every stock `libx264` stream that
//! carries B-frames, up to and including 4K. The real "not yet decoded"
//! rule is positional and lives in `mb.rs`'s own `resolve_c`;
//! `tests/cabac_p_8x8_same_mb_c_neighbour.rs` is the case that covers it,
//! and `mb.rs`'s `only_c_can_reach_a_not_yet_decoded_partition` proves it
//! is the only neighbour that needs it.
//!
//! # What this fixture is, and that it can actually fail
//!
//! Six pictures of `testsrc2` at 160x128, High profile, CABAC, `-bf 3`,
//! encoded by a stock `libx264` with no tuning beyond the profile and the
//! B-frame count -- deliberately ordinary output, since that is the
//! population that regressed. It was **selected by measurement, not by
//! plausibility**: of eight candidate clips generated across three frame
//! sizes and four lengths, this is the smallest whose decode differs
//! between a build with the defect and one without. Against the defective
//! build four of its six output pictures diverge, by 725 to 4,266 bytes and
//! up to 222 in a single sample; the all-intra picture and one other stay
//! byte-exact. A fixture that could not fail would be worse than no
//! fixture, so that check is the reason this particular clip is here.
//!
//! Driven through [`H264Decoder`]'s own public [`Decoder`] surface, with the
//! real `ffmpeg` decode of the same bitstream embedded once (D7 / plan 13
//! Sec1b), exactly as `tests/decoder_output_matches_ffmpeg.rs` and
//! `tests/cabac_p_8x8_same_mb_c_neighbour.rs` already do.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    clippy::format_collect,
    clippy::cast_precision_loss,
    reason = "test code over a fixed fixture: every division is by the constant 2 (4:2:0 \
              chroma subsampling) over compile-time constants, and the failure message is \
              built once, only on the failing path"
)]

use vaco_bitstream::annexb;
use vaco_codec_core::Decoder;
use vaco_codec_h264::H264Decoder;
use vaco_core::Error;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const WIDTH: usize = 160;
const HEIGHT: usize = 128;
const LUMA: usize = WIDTH * HEIGHT;
const CHROMA: usize = (WIDTH / 2) * (HEIGHT / 2);
const FRAME: usize = LUMA + 2 * CHROMA;
const FRAMES: usize = 6;

fn packed(plane: &vaco_frame::Plane, row_bytes: usize, rows: usize) -> Vec<u8> {
    let data = plane.data.as_slice();
    let mut out = Vec::new();
    for r in 0..rows {
        let start = r * plane.stride;
        out.extend_from_slice(&data[start..start + row_bytes]);
    }
    out
}

#[test]
fn b_8x8_quadrants_using_different_lists_are_byte_exact_against_ffmpeg() {
    let stream: &[u8] = include_bytes!("fixtures/cabac_b_8x8_mixed_list_quadrants.264");
    let reference: &[u8] = include_bytes!("fixtures/cabac_b_8x8_mixed_list_quadrants_ref.yuv");
    assert_eq!(
        reference.len(),
        FRAMES * FRAME,
        "reference fixture is not {FRAMES} whole frames"
    );

    let mut d = H264Decoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());

    let mut extradata = Vec::new();
    let mut slices: Vec<Vec<u8>> = Vec::new();
    for nal in annexb::nal_units(stream) {
        match nal.first().map(|b| b & 0x1F) {
            Some(7 | 8) => {
                extradata.extend_from_slice(&[0, 0, 0, 1]);
                extradata.extend_from_slice(nal);
            }
            Some(1 | 5) => {
                let mut framed = vec![0u8, 0, 0, 1];
                framed.extend_from_slice(nal);
                slices.push(framed);
            }
            _ => {}
        }
    }
    assert_eq!(
        slices.len(),
        FRAMES,
        "fixture should carry one slice per picture, {FRAMES} pictures"
    );
    d.set_extradata(&extradata).unwrap();

    // B slices reorder, so this drives the real send/receive protocol and
    // takes output strictly in the order the decoder emits it -- which is
    // display order, and therefore the order the reference YUV is in.
    let mut frames: Vec<vaco_frame::Frame> = Vec::new();
    for slice in &slices {
        let pkt = Packet::from_slice(&mut budget, slice).unwrap();
        loop {
            match d.send_packet(Some(&pkt)) {
                Ok(()) => break,
                Err(Error::OutputPending) => frames.push(d.receive_frame().unwrap()),
                Err(e) => panic!("send_packet failed: {e:?}"),
            }
        }
        while let Ok(frame) = d.receive_frame() {
            frames.push(frame);
        }
    }
    d.send_packet(None).unwrap();
    loop {
        match d.receive_frame() {
            Ok(frame) => frames.push(frame),
            Err(Error::Eof) => break,
            Err(e) => panic!("receive_frame failed while draining end of stream: {e:?}"),
        }
    }
    assert_eq!(
        frames.len(),
        FRAMES,
        "expected {FRAMES} frames out, got {}",
        frames.len()
    );

    let mut report: Vec<(usize, [(usize, u8); 3])> = Vec::new();
    for (idx, frame) in frames.iter().enumerate() {
        let FrameData::Video { format, planes, .. } = &frame.data else {
            panic!("frame {idx}: expected video, got {:?}", frame.data);
        };
        assert_eq!(format.name(), "yuv420p", "frame {idx}");
        assert_eq!(planes.len(), 3, "frame {idx}");

        let got = [
            packed(&planes[0], WIDTH, HEIGHT),
            packed(&planes[1], WIDTH / 2, HEIGHT / 2),
            packed(&planes[2], WIDTH / 2, HEIGHT / 2),
        ];
        let base = idx * FRAME;
        let want: [&[u8]; 3] = [
            &reference[base..base + LUMA],
            &reference[base + LUMA..base + LUMA + CHROMA],
            &reference[base + LUMA + CHROMA..base + FRAME],
        ];

        let mut per_plane = [(0usize, 0u8); 3];
        for p in 0..3 {
            assert_eq!(
                got[p].len(),
                want[p].len(),
                "frame {idx} plane {p}: wrong size"
            );
            for (g, w) in got[p].iter().zip(want[p].iter()) {
                if g != w {
                    per_plane[p].0 += 1;
                    per_plane[p].1 = per_plane[p].1.max(g.abs_diff(*w));
                }
            }
        }
        report.push((idx, per_plane));
    }

    let first_bad = report.iter().find(|(_, p)| p.iter().any(|(n, _)| *n > 0));
    if let Some((idx, per_plane)) = first_bad {
        let total: usize = report
            .iter()
            .map(|(_, p)| p.iter().map(|(n, _)| n).sum::<usize>())
            .sum();
        let summary: String = report
            .iter()
            .filter(|(_, p)| p.iter().any(|(n, _)| *n > 0))
            .map(|(i, p)| {
                format!(
                    "\n  frame {i}: Y {} bytes (max delta {}), Cb {} (max {}), Cr {} (max {})",
                    p[0].0, p[0].1, p[1].0, p[1].1, p[2].0, p[2].1
                )
            })
            .collect();
        panic!(
            "first differing frame is {idx}: Y {} bytes differ (max delta {}), \
             Cb {} (max {}), Cr {} (max {}). {total} of {} bytes differ across the \
             whole sequence ({:.4}%).{summary}",
            per_plane[0].0,
            per_plane[0].1,
            per_plane[1].0,
            per_plane[1].1,
            per_plane[2].0,
            per_plane[2].1,
            FRAMES * FRAME,
            100.0 * total as f64 / (FRAMES * FRAME) as f64,
        );
    }
}
