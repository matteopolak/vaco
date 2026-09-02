//! Regression test for the `decode_sub_mb_pred_cabac`/`decode_sub_mb_pred_cavlc`
//! premature-`mb_available` bug: a `P_8x8` macroblock whose bottom-left
//! quadrant (`mbPartIdx == 2`) is split into four `P_L0_4x4` sub-partitions
//! needs, for its own bottom-right sub-partition (`subMbPartIdx == 3`),
//! clause 8.4.1.3.2's `C` (above-right) motion-vector-prediction neighbour --
//! which resolves to the bottom-right quadrant's own top-left 4x4
//! (`mbPartIdx == 3`), not yet decoded at that point in partition-scan
//! order. Clause 8.4.1.3.2's own "not yet decoded" case requires falling
//! back to `D` (above-left) instead of using `C` directly.
//!
//! `decode_sub_mb_pred_cabac`'s own `ref_idx_l0`/`ref_idx_l1` pass used to
//! mark **every** quadrant `mb_available` the moment it finished, before
//! the later `mvd_l0`/`mvd_l1` pass had derived any quadrant's actual
//! motion vector -- so `resolve_c` saw the not-yet-decoded quadrant as
//! available (a real `ref_idx`, but a motion vector still at the grid's
//! `(0, 0)` default) and used it raw instead of falling back to `D`,
//! corrupting the median predictor for that one sub-partition. Confirmed
//! against a real decode of the JVT conformance stream `CANL3_SVA_B`
//! (`CABAC`, QCIF Foreman, `intra_period == 10`): frame 1 (the first
//! P-frame) diverged from `ffmpeg`'s own decode by exactly the 4x4 block at
//! this local grid position, in two independent macroblocks, both with
//! `coded_block_pattern == 0` (no residual to mask it) and `mvd == (0, 0)`
//! (the wrong predictor decoded unmodified) -- see `mb.rs`'s own fix
//! comment on `decode_sub_mb_pred_cabac`'s `ref_idx` pass for the full
//! account.
//!
//! Driven through [`H264Decoder`]'s own public [`Decoder`] surface, exactly
//! as `tests/decoder_output_matches_ffmpeg.rs` is, and using that same
//! file's own per-plane byte-exactness comparison shape -- this fixture
//! just happens to be a real third-party conformance stream instead of an
//! x264-encoded one.

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

/// `CANL3_SVA_B`: 176x144 (QCIF), Main profile, CABAC, I + P slices (no B,
/// no weighted prediction, no direct-8x8-inference, loop filter off,
/// `num_ref_frames == 5`, POC type 0), one slice per picture, 17 frames.
const WIDTH: usize = 176;
const HEIGHT: usize = 144;
const LUMA: usize = WIDTH * HEIGHT;
const CHROMA: usize = (WIDTH / 2) * (HEIGHT / 2);
const FRAME: usize = LUMA + 2 * CHROMA;
const FRAMES: usize = 17;

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
fn canl3_sva_b_p_8x8_macroblocks_are_byte_exact_against_ffmpeg() {
    let stream: &[u8] = include_bytes!("fixtures/cabac_p_8x8_same_mb_c_neighbour.264");
    let reference: &[u8] = include_bytes!("fixtures/cabac_p_8x8_same_mb_c_neighbour_ref.yuv");
    assert_eq!(
        reference.len(),
        FRAMES * FRAME,
        "reference fixture is not 17 whole frames"
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
        "fixture should carry one slice per picture, 17 pictures"
    );
    d.set_extradata(&extradata).unwrap();

    // No B slices in this fixture, so decode order and display order
    // coincide -- but the *test* still drives the real send/receive
    // protocol rather than assume strict packet-for-frame lockstep, the
    // same way `decoder_output_matches_ffmpeg.rs` does.
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
