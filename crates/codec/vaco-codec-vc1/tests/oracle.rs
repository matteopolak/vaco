//! Differential verification against a real Main-profile VC-1 stream and
//! its `ffmpeg 8.1`-decoded output.
//!
//! # Fixture provenance
//!
//! `fixtures/smm0015_frame0.vc1` is the first (I) frame's `FRAMEDATA`
//! extracted from `SMM0015.rcv`, a public sample from `FFmpeg`'s own FATE
//! sample corpus (`fate-suite.ffmpeg.org/vc1/SMM0015.rcv`) — the same
//! corpus this project's Theora crate already checks fixtures in from.
//! `SMM0015.rcv`'s own sequence-layer header
//! (`STRUCT_C == 0x41F3_8001`, `VERT_SIZE == 576`, `HORIZ_SIZE == 720`) was
//! decoded bit-by-bit by hand against Annex J.2/L before any decoder code
//! in this crate was written (see `header.rs`'s
//! `real_fixture_struct_c_decodes_as_measured` test) — `PROFILE == Main`,
//! `OVERLAP == 0`, `LOOPFILTER == 0`, `MULTIRES == 1` (this frame's own
//! `RESPIC == 0`, full resolution).
//!
//! `fixtures/smm0015_frame0_ffmpeg.yuv420p` is `ffmpeg 8.1`'s own decode of
//! that file's first frame (`ffmpeg -i SMM0015.rcv -vframes 1 -pix_fmt
//! yuv420p -f rawvideo`), confirming `wmv3 (Main), yuv420p, 720x576` —
//! exactly this crate's extradata parse.
//!
//! # Result
//!
//! Measured Y/U/V **separately**, per this project's own "measure every
//! plane" rule. This crate transcribes only the High Rate Intra/Inter AC
//! coding sets (see the crate doc); this real fixture's I frame happens to
//! use exactly that pair (`PQINDEX == 2 <= 8`, `TRANSACFRM == TRANSACFRM2
//! == 0`), which is why it decodes at all rather than returning
//! `Error::Unsupported`.

use vaco_codec_core::Decoder;
use vaco_codec_vc1::Vc1Decoder;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const EXTRADATA: [u8; 12] = {
    let struct_c = 0x41F3_8001u32.to_be_bytes();
    let vert = 576u32.to_le_bytes();
    let horiz = 720u32.to_le_bytes();
    [
        struct_c[0], struct_c[1], struct_c[2], struct_c[3],
        vert[0], vert[1], vert[2], vert[3],
        horiz[0], horiz[1], horiz[2], horiz[3],
    ]
};

struct PlaneStats {
    max_abs_diff: i64,
    mean_abs_diff: f64,
}

fn compare_plane(got: &[u8], want: &[u8]) -> PlaneStats {
    let mut max_abs_diff = 0i64;
    let mut sum = 0i64;
    let n = got.len().min(want.len());
    for i in 0..n {
        let g = i64::from(got.get(i).copied().unwrap_or(0));
        let w = i64::from(want.get(i).copied().unwrap_or(0));
        let d = (g - w).abs();
        max_abs_diff = max_abs_diff.max(d);
        sum += d;
    }
    PlaneStats {
        max_abs_diff,
        mean_abs_diff: if n == 0 { 0.0 } else { sum as f64 / n as f64 },
    }
}

#[test]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a differential test that cannot set up its own fixtures is a failed test, not a skip"
)]
fn i_frame_matches_ffmpeg_per_plane() {
    let payload = include_bytes!("fixtures/smm0015_frame0.vc1");
    let reference = include_bytes!("fixtures/smm0015_frame0_ffmpeg.yuv420p");

    let mut dec = Vc1Decoder::new(Limits::permissive());
    dec.set_extradata(&EXTRADATA).expect("valid extradata");

    let mut budget = Budget::new(Limits::permissive());
    let pkt = Packet::from_slice(&mut budget, payload).unwrap();
    dec.send_packet(Some(&pkt)).expect("decode must not fail on this real fixture");
    let frame = dec.receive_frame().expect("one frame must be produced");

    let FrameData::Video { planes, width, height, .. } = &frame.data else {
        panic!("decoded frame has no video data");
    };
    assert_eq!(*width, 720);
    assert_eq!(*height, 576);

    let (w, h) = (720usize, 576usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y_ref = reference.get(..w * h).unwrap();
    let u_ref = reference.get(w * h..w * h + cw * ch).unwrap();
    let v_ref = reference.get(w * h + cw * ch..w * h + 2 * cw * ch).unwrap();

    let plane_bytes = |idx: usize, plane_w: usize, plane_h: usize| -> Vec<u8> {
        let p = planes.get(idx).expect("plane must exist");
        let data = p.data.as_slice();
        let mut out = vec![0u8; plane_w * plane_h];
        for row in 0..plane_h {
            let src_off = row * p.stride;
            let Some(src) = data.get(src_off..src_off + plane_w) else { continue };
            let dst_off = row * plane_w;
            if let Some(dst) = out.get_mut(dst_off..dst_off + plane_w) {
                dst.copy_from_slice(src);
            }
        }
        out
    };

    let y_got = plane_bytes(0, w, h);
    let u_got = plane_bytes(1, cw, ch);
    let v_got = plane_bytes(2, cw, ch);

    let y_stats = compare_plane(&y_got, y_ref);
    let u_stats = compare_plane(&u_got, u_ref);
    let v_stats = compare_plane(&v_got, v_ref);

    eprintln!(
        "Y: max={} mean={:.3}  U: max={} mean={:.3}  V: max={} mean={:.3}",
        y_stats.max_abs_diff, y_stats.mean_abs_diff, u_stats.max_abs_diff, u_stats.mean_abs_diff,
        v_stats.max_abs_diff, v_stats.mean_abs_diff,
    );

    // Per 705779d/D17: byte-exactness is not the bar, a small unstructured
    // deviation is fine, and a structured one is a bug. These thresholds
    // are deliberately loose (the transform's own +/-1 rounding aside, this
    // is the first real bitstream this crate has ever decoded) but do
    // assert the *shape* that matters: no channel is wildly wrong, which
    // is what a swapped table, a wrong scan, or a sign error would produce.
    assert!(y_stats.mean_abs_diff < 8.0, "luma mean deviation too high: {:.3}", y_stats.mean_abs_diff);
    assert!(u_stats.mean_abs_diff < 8.0, "Cb mean deviation too high: {:.3}", u_stats.mean_abs_diff);
    assert!(v_stats.mean_abs_diff < 8.0, "Cr mean deviation too high: {:.3}", v_stats.mean_abs_diff);
}
