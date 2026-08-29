//! [`HevcDecoder`] against a real `libx265`-encoded elementary stream,
//! measured plane by plane against `ffmpeg`'s own decode.
//!
//! # Fixture and its generation
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=size=64x64:rate=1:duration=1" -pix_fmt yuv420p \
//!        -c:v libx265 -x265-params "no-deblock=1:no-sao=1:qp=32:keyint=1" \
//!        -frames:v 1 tests/fixtures/qp32_64x64.hevc
//! ffmpeg -y -skip_loop_filter all -i tests/fixtures/qp32_64x64.hevc \
//!        -pix_fmt yuv420p -f rawvideo tests/fixtures/qp32_64x64.yuv
//! ```
//!
//! `no-deblock`/`no-sao` are the encoder's own switches — this crate's scope
//! excludes both in-loop filters (see the crate doc), so the fixture is
//! chosen to have nothing for either side to disagree about, rather than
//! relying on `-skip_loop_filter all` to paper over a gap. `-skip_loop_filter
//! all` is passed anyway, redundantly, as the second line of defence
//! `AGENT-CONSTRAINTS.md` asks for: a missing loop filter must not read as a
//! broken decoder.
//!
//! `x265`'s log for this fixture reports `tools: ... signhide tmvp ...
//! b-intra strong-intra-smoothing`, so sign-data-hiding and strong intra
//! smoothing are both genuinely exercised, not merely reachable.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    reason = "test code over a fixed, checked-in fixture: WIDTH/HEIGHT are compile-time-known \
              even powers of two, and a missing plane/row/frame-shape here is itself the test failing"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_core::Error;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const HEVC: &[u8] = include_bytes!("fixtures/qp32_64x64.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/qp32_64x64.yuv");
const WIDTH: usize = 64;
const HEIGHT: usize = 64;

fn packet(bytes: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::default());
    Packet::from_slice(&mut budget, bytes).unwrap()
}

/// Mean absolute per-sample difference and the count of exactly-matching
/// samples, reported separately — see `AGENT-CONSTRAINTS.md`: report Y, U
/// and V apart, always.
fn compare(name: &str, got: &[u8], want: &[u8]) -> (f64, usize) {
    assert_eq!(got.len(), want.len(), "{name}: plane size mismatch");
    let mut sum_abs = 0i64;
    let mut exact = 0usize;
    for (&g, &w) in got.iter().zip(want.iter()) {
        let d = i32::from(g) - i32::from(w);
        sum_abs += i64::from(d.abs());
        if d == 0 {
            exact += 1;
        }
    }
    let mean = sum_abs as f64 / got.len() as f64;
    println!(
        "{name}: {}/{} samples byte-exact, mean abs diff {mean:.4}",
        exact,
        got.len()
    );
    (mean, exact)
}

fn decode_and_compare() -> ((f64, usize), (f64, usize), (f64, usize)) {
    let mut d = HevcDecoder::new(Limits::default());
    let pkt = packet(HEVC);
    d.send_packet(Some(&pkt)).unwrap();
    d.send_packet(None).unwrap();

    let frame = match d.receive_frame() {
        Ok(f) => f,
        Err(Error::Unsupported(msg)) => panic!("decode refused as unsupported: {msg}"),
        Err(e) => panic!("unexpected decode error: {e:?}"),
    };

    let vaco_frame::FrameData::Video { width, height, .. } = &frame.data else {
        panic!("expected a video frame");
    };
    assert_eq!(*width as usize, WIDTH);
    assert_eq!(*height as usize, HEIGHT);

    let y_size = WIDTH * HEIGHT;
    let c_size = (WIDTH / 2) * (HEIGHT / 2);
    let (want_y, rest) = REF_YUV.split_at(y_size);
    let (want_u, want_v) = rest.split_at(c_size);

    let mut got_y = vec![0u8; y_size];
    let mut got_u = vec![0u8; c_size];
    let mut got_v = vec![0u8; c_size];
    blit(&frame, 0, WIDTH, HEIGHT, &mut got_y);
    blit(&frame, 1, WIDTH / 2, HEIGHT / 2, &mut got_u);
    blit(&frame, 2, WIDTH / 2, HEIGHT / 2, &mut got_v);

    (compare("Y", &got_y, want_y), compare("U", &got_u, want_u), compare("V", &got_v, want_v))
}

/// Decodes without error or panic and reports per-plane agreement against
/// `ffmpeg` — always passes, so it stays green as a smoke test regardless of
/// [`dense_content_is_byte_exact`]'s outcome.
#[test]
fn cabac_intra_frame_decodes_without_error() {
    let _ = decode_and_compare();
}

/// **Formerly a known, named gap — now root-caused and fixed.** Every 4x4
/// transform block (one coefficient group) always decoded byte-exact; every
/// 8x8-and-larger (multi-coefficient-group) block diverged. The cause was in
/// [`crate::residual::sig_ctx_inc`]'s caller: HM's `getSigCtxInc` returns a
/// literal `0` for the DC position (`(posX + posY) == 0`), which *bypasses*
/// `firstSignificanceMapContext` (this crate's `sig_base`/`sig_class_base`)
/// entirely rather than feeding `0` into it — DC is one context shared by
/// every transform size within a component, at the component's own base
/// index, not `comp_base + sig_class_base`. `residual_coding` was adding
/// `sig_class_base` unconditionally. That distinction is invisible at 4x4
/// (`sig_base` is `0` there), which is exactly why every 4x4 residual block
/// decoded byte-exact while every 8x8+ block desynchronised the whole
/// remainder of the CABAC stream from that bin onward.
///
/// Found by instrumenting a byte-for-byte CABAC bin trace (context index,
/// state, MPS, decoded value) in both this crate and a from-source,
/// locally-built HM 18.0 (BSD-3-Clause, Tier A) decoding this same fixture,
/// and diffing the two traces bin-for-bin: the first divergence landed
/// exactly on the `sig_coeff_flag` bin at the DC position of the first
/// multi-coefficient-group (8x8, 4-subset) luma transform block, matching
/// this bug precisely.
///
/// Two other real bugs were found and fixed in the same area in an earlier
/// pass, kept regardless of this fix: coefficient scanning used a flat
/// full-size scan instead of HM's `SCAN_GROUPED_4x4` (see
/// `crate::scan::generate_grouped`'s doc), and
/// [`crate::residual::sig_base`] used one shared base offset for every
/// `log2TrafoSize == 3` block regardless of scan order and let 32x32 luma
/// fall through to a reserved slot.
///
/// Kept under its own name (not `known_gap_*`) as the concrete regression
/// test for this exact defect, per this project's `vaco-codec-av1` and
/// `vaco-codec-theora` precedent for naming a gap specifically enough that
/// fixing it reads as fixing *this* test, not deleting it.
#[test]
fn dense_content_is_byte_exact() {
    let ((y_mean, y_exact), (u_mean, u_exact), (v_mean, v_exact)) = decode_and_compare();
    let y_size = WIDTH * HEIGHT;
    let c_size = (WIDTH / 2) * (HEIGHT / 2);
    assert_eq!(y_exact, y_size, "luma plane: not byte-exact, mean abs diff {y_mean}");
    assert_eq!(u_exact, c_size, "Cb plane: not byte-exact, mean abs diff {u_mean}");
    assert_eq!(v_exact, c_size, "Cr plane: not byte-exact, mean abs diff {v_mean}");
}

fn blit(frame: &vaco_frame::Frame, plane_index: usize, width: usize, height: usize, out: &mut [u8]) {
    let plane = frame.plane(plane_index).expect("plane present");
    for y in 0..height {
        let row = plane.row(y).expect("row in range");
        for x in 0..width {
            out[y * width + x] = row[x];
        }
    }
}
