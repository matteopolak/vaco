//! Two rules a stock `libx265` encode never reaches, against one that does:
//! the residual quad-tree's depth limit (§7.3.8.8 `transform_tree`, §7.4.9.8's
//! `MaxTrafoDepth`/`interSplitFlag`) and §8.5.3.2.1's uni-prediction
//! restriction on a bi-predictive merge candidate for an 8x4/4x8 PU
//! (`nOrigPbW + nOrigPbH == 12`). Measured plane by plane against `ffmpeg`'s
//! own decode.
//!
//! # Why no other fixture in this crate covers either
//!
//! Measured by reading the SPS/PPS back out of the encodes, not assumed: a
//! stock `libx265` invocation writes `max_transform_hierarchy_depth_inter = 0`,
//! `max_transform_hierarchy_depth_intra = 0` and `amp_enabled_flag = 0`, and
//! leaves `--rect` off. So every inter CU it emits is `PART_2Nx2N` — which
//! makes `interSplitFlag` `0` either way, and makes the smallest PU an 8x8
//! (`8 + 8 == 16`, never `12`) — and no CU of either kind is allowed more than
//! one transform split. All three rules are dead in every other fixture here.
//!
//! It caught two separate defects, in that order: before either fix it failed
//! on frame 1 with 3558 of 6144 luma samples byte-exact, and with only the
//! transform-tree fix applied it still failed, on frame 2, with 6085 of 6144.
//!
//! # Fixture and its generation
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=size=96x64:rate=25:duration=0.24" -pix_fmt yuv420p \
//!        -c:v libx265 \
//!        -x265-params "rect=1:amp=1:tu-inter-depth=3:tu-intra-depth=3:qp=30:wpp=0:no-sao=1:no-deblock=1:bframes=2:ref=2" \
//!        -f hevc tests/fixtures/rqt_96x64.hevc
//! ffmpeg -y -skip_loop_filter all -i tests/fixtures/rqt_96x64.hevc \
//!        -pix_fmt yuv420p -f rawvideo tests/fixtures/rqt_96x64.yuv
//! ```
//!
//! The in-loop filters are off at the encoder so a residual/parse regression
//! cannot hide behind a filtering one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    reason = "test code over a fixed, checked-in fixture: a missing plane/row/frame shape here is \
              itself the test failing"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_core::Error;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const HEVC: &[u8] = include_bytes!("fixtures/rqt_96x64.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/rqt_96x64.yuv");
const WIDTH: usize = 96;
const HEIGHT: usize = 64;
const FRAMES: usize = 6;

/// Split Annex-B `data` into access units: a VCL NAL (type <= 31) starts a
/// new one whenever the unit being accumulated already holds a VCL NAL.
/// Every picture in this fixture is a single slice segment, so "a VCL NAL"
/// and "a picture" coincide.
fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut seen_vcl = false;
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let vcl = ((data[i + 3] >> 1) & 0x3f) <= 31;
            if starts.is_empty() || (vcl && seen_vcl) {
                starts.push(i);
                seen_vcl = false;
            }
            seen_vcl |= vcl;
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    starts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

#[test]
fn deep_transform_trees_and_rectangular_inter_partitions_are_byte_exact() {
    let mut budget = Budget::new(Limits::default());
    let mut decoder = HevcDecoder::new(Limits::default());
    let y_size = WIDTH * HEIGHT;
    let c_size = (WIDTH / 2) * (HEIGHT / 2);
    let frame_size = y_size + 2 * c_size;

    let mut got = Vec::new();
    for au in access_units(HEVC) {
        let pkt = Packet::from_slice(&mut budget, au).unwrap();
        match decoder.send_packet(Some(&pkt)) {
            Ok(()) => {}
            Err(Error::Unsupported(msg)) => panic!("decode refused as unsupported: {msg}"),
            Err(e) => panic!("unexpected decode error: {e:?}"),
        }
        while let Ok(frame) = decoder.receive_frame() {
            append_planes(&frame, &mut got);
        }
    }
    decoder.send_packet(None).unwrap();
    while let Ok(frame) = decoder.receive_frame() {
        append_planes(&frame, &mut got);
    }

    assert_eq!(
        got.len(),
        FRAMES * frame_size,
        "frame count: decoded {} frames, fixture has {FRAMES}",
        got.len() / frame_size
    );
    assert_eq!(REF_YUV.len(), FRAMES * frame_size, "fixture ref YUV size");

    for (f, (ours, theirs)) in got
        .chunks_exact(frame_size)
        .zip(REF_YUV.chunks_exact(frame_size))
        .enumerate()
    {
        let mut offset = 0;
        for (name, size) in [("Y", y_size), ("U", c_size), ("V", c_size)] {
            let a = &ours[offset..offset + size];
            let b = &theirs[offset..offset + size];
            offset += size;
            let exact = a.iter().zip(b).filter(|(x, y)| x == y).count();
            assert_eq!(
                exact, size,
                "frame {f} plane {name}: {exact} of {size} samples byte-exact"
            );
        }
    }
}

fn append_planes(frame: &vaco_frame::Frame, out: &mut Vec<u8>) {
    let vaco_frame::FrameData::Video { width, height, .. } = &frame.data else {
        panic!("expected a video frame");
    };
    assert_eq!(*width as usize, WIDTH);
    assert_eq!(*height as usize, HEIGHT);
    for (plane_index, (w, h)) in [
        (0, (WIDTH, HEIGHT)),
        (1, (WIDTH / 2, HEIGHT / 2)),
        (2, (WIDTH / 2, HEIGHT / 2)),
    ] {
        let plane = frame.plane(plane_index).expect("plane present");
        for y in 0..h {
            out.extend_from_slice(&plane.row(y).expect("row in range")[..w]);
        }
    }
}
