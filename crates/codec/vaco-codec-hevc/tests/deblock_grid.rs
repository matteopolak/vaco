//! §8.7.2's deblocking edge grid is 8 luma samples whatever `MinCbSizeY`
//! is, against a real `libx265` encode with `MinCbSizeY == 16` and a
//! transform tree deep enough to put real edges at odd multiples of 8,
//! measured plane by plane against `ffmpeg`'s own decode.
//!
//! # Why no other fixture in this crate covers it
//!
//! Measured, not assumed: a stock `libx265` invocation writes
//! `log2_min_luma_coding_block_size_minus3 = 0` (`MinCbSizeY == 8`) and
//! `max_transform_hierarchy_depth_intra/inter = 0`. With `MinCbSizeY == 8`
//! a grid derived from it *is* 8, so the derivation and the constant agree,
//! and with a depth of 0 there is no transform edge inside a coding block
//! to disagree about. This fixture sets `min-cu-size=16` and
//! `tu-intra-depth=3:tu-inter-depth=3` — its SPS was read back to confirm
//! `log2_min_luma_coding_block_size_minus3 = 1` and both
//! `max_transform_hierarchy_depth` fields = 2 — so transform units land on
//! odd multiples of 8 and the two disagree.
//!
//! Deriving the grid from `MinCbSizeY` left every such edge unfiltered.
//! Perturbing `deblock::DEBLOCK_GRID` from 8 back to 16 fails this on frame
//! 0 with 645 of 16384 luma samples wrong, in 8-wide bands centred on each
//! skipped edge — 3973 samples over the whole clip. SAO is off at the
//! encoder so a deblocking regression cannot hide behind a SAO one.
//!
//! # Fixture and its generation
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=size=128x128:rate=25:duration=0.2" -pix_fmt yuv420p \
//!        -c:v libx265 \
//!        -x265-params "qp=30:wpp=0:no-sao=1:min-cu-size=16:tu-intra-depth=3:tu-inter-depth=3" \
//!        -f hevc tests/fixtures/deblock_grid_128x128.hevc
//! ffmpeg -y -i tests/fixtures/deblock_grid_128x128.hevc \
//!        -pix_fmt yuv420p -f rawvideo tests/fixtures/deblock_grid_128x128.yuv
//! ```

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

const HEVC: &[u8] = include_bytes!("fixtures/deblock_grid_128x128.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/deblock_grid_128x128.yuv");
const WIDTH: usize = 128;
const HEIGHT: usize = 128;
const FRAMES: usize = 5;

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
fn transform_edges_off_the_min_cb_grid_are_still_deblocked() {
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
