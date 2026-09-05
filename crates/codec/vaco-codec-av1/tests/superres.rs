//! AV1 super-resolution regressions against independently decoded fixtures.
//!
//! The fixture was encoded with `libsvtav1` and decoded with the pinned BSD
//! `dav1d` build recorded in `provenance/vaco-codec-av1-superres.toml`.
//! It has a 96x64 display frame whose coded width is smaller. The flat source
//! keeps this black-box check exact despite the decoder's separate incomplete
//! intra-prediction coverage; the non-flat scalar coverage is in the dav1d
//! vector oracle below.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "fixed checked-in test fixtures"
)]

use vaco_codec_av1::Av1Decoder;
use vaco_codec_core::Decoder;
use vaco_core::Error;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn read_u16(input: &[u8], offset: &mut usize) -> u16 {
    let bytes: [u8; 2] = input[*offset..*offset + 2].try_into().expect("oracle u16");
    *offset += 2;
    u16::from_le_bytes(bytes)
}

fn next_state(state: u32) -> u32 {
    let state = state ^ (state << 13);
    let state = state ^ (state >> 17);
    state ^ (state << 5)
}

/// The 18 records come from the pinned scalar dav1d oracle, not from this
/// crate's table or arithmetic. Several cases deliberately have a visible
/// source width smaller than its Mi-padded source plane.
#[test]
fn spec_resampler_matches_every_pinned_dav1d_oracle_case() {
    let fixture: &[u8] = include_bytes!("fixtures/superres-dav1d.u16le");
    let mut offset = 0usize;
    let mut case_id = 0u32;
    while offset < fixture.len() {
        let bit_depth = read_u16(fixture, &mut offset) as u8;
        let visible_width = usize::from(read_u16(fixture, &mut offset));
        let padded_width = usize::from(read_u16(fixture, &mut offset));
        let output_width = usize::from(read_u16(fixture, &mut offset));
        let height = usize::from(read_u16(fixture, &mut offset));
        let mut budget = Budget::new(Limits::default());
        let mut input = vaco_codec_av1::framebuf::Plane::new(&mut budget, padded_width, height)
            .expect("oracle input allocation");
        let max = (1u16 << bit_depth) - 1;
        let mut state = 0x6d2b_79f5u32 ^ case_id;
        for y in 0..height {
            for x in 0..padded_width {
                state = next_state(state);
                input.set(
                    x,
                    y,
                    (state.wrapping_add((x * 37 + y * 91) as u32) as u16) & max,
                );
            }
        }
        let actual = vaco_codec_av1::superres::upscale_plane(
            &input,
            vaco_codec_av1::superres::PlaneConfig {
                visible_width,
                output_width,
                height,
                bit_depth,
            },
            &mut budget,
        )
        .expect("spec resample");
        for &sample in actual.as_slice() {
            assert_eq!(
                sample,
                read_u16(fixture, &mut offset),
                "oracle case {case_id}"
            );
        }
        case_id += 1;
    }
    assert_eq!(case_id, 18, "all oracle cases consumed");
}

#[test]
fn flat_superres_keyframe_matches_dav1d_on_every_output_plane() {
    const WIDTH: usize = 96;
    const HEIGHT: usize = 64;
    let fixture: &[u8] = include_bytes!("fixtures/superres-96x64.obu");
    let reference: &[u8] = include_bytes!("fixtures/superres-96x64_ref.yuv");

    let mut decoder = Av1Decoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());
    let packet = Packet::from_slice(&mut budget, fixture).expect("fixture packet allocation");
    decoder.send_packet(Some(&packet)).expect("fixture decode");
    decoder.send_packet(None).expect("fixture drain signal");
    let frame = decoder.receive_frame().expect("one decoded frame");

    let mut actual = Vec::new();
    for plane_index in 0..3 {
        let plane_width = if plane_index == 0 {
            WIDTH
        } else {
            WIDTH.div_ceil(2)
        };
        let plane_height = if plane_index == 0 {
            HEIGHT
        } else {
            HEIGHT.div_ceil(2)
        };
        let plane = frame.plane(plane_index).expect("YUV420 output plane");
        for y in 0..plane_height {
            actual.extend_from_slice(
                plane
                    .row(y)
                    .expect("fixture output row")
                    .get(..plane_width)
                    .expect("fixture output width"),
            );
        }
    }

    let chroma_samples = WIDTH.div_ceil(2) * HEIGHT.div_ceil(2);
    assert_eq!(
        actual.len(),
        WIDTH * HEIGHT + 2 * chroma_samples,
        "Y/U/V byte count"
    );
    let mismatch = actual
        .iter()
        .zip(reference)
        .enumerate()
        .find(|(_, (actual, expected))| actual != expected);
    assert!(
        mismatch.is_none(),
        "all super-resolved Y/U/V samples must match; first mismatch: {mismatch:?}"
    );
    assert!(
        matches!(decoder.receive_frame(), Err(Error::Eof)),
        "exactly one output frame"
    );
}

/// The real P frame following `superres-96x64.obu`. Its §5.9.5 inter syntax
/// reaches `frame_size_with_refs()`, which needs the prior frame's retained
/// dimensions; this bounded intra-only decoder names that missing
/// reference-store/inter-prediction boundary rather than inventing a size.
const ACTIVE_SUPERRES_INTER: &[u8] = &[
    0x12, 0x00, 0x32, 0x0f, 0x30, 0x02, 0x00, 0x00, 0x00, 0x00, 0x1d, 0x48, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x9c, 0x4e,
];

#[test]
fn active_superres_reference_frame_is_exact_before_inter_refusal() {
    const WIDTH: usize = 96;
    const HEIGHT: usize = 64;
    let key: &[u8] = include_bytes!("fixtures/superres-96x64.obu");
    let reference: &[u8] = include_bytes!("fixtures/superres-96x64_ref.yuv");
    let mut decoder = Av1Decoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());
    let key_packet = Packet::from_slice(&mut budget, key).expect("key fixture packet allocation");
    decoder
        .send_packet(Some(&key_packet))
        .expect("active superres reference frame must decode");
    let frame = decoder
        .receive_frame()
        .expect("one active superres reference frame");

    let mut actual = Vec::new();
    for plane_index in 0..3 {
        let (plane_width, plane_height) = if plane_index == 0 {
            (WIDTH, HEIGHT)
        } else {
            (WIDTH.div_ceil(2), HEIGHT.div_ceil(2))
        };
        let plane = frame.plane(plane_index).expect("YUV420 reference plane");
        for y in 0..plane_height {
            actual.extend_from_slice(
                plane
                    .row(y)
                    .expect("reference output row")
                    .get(..plane_width)
                    .expect("reference output width"),
            );
        }
    }
    assert_eq!(actual.len(), 9_216, "one full Y/U/V reference frame");
    assert_eq!(actual, reference, "all reference-frame bytes match dav1d");

    let inter_packet = Packet::from_slice(&mut budget, ACTIVE_SUPERRES_INTER)
        .expect("inter fixture packet allocation");
    let error = decoder
        .send_packet(Some(&inter_packet))
        .expect_err("inter frame requires a reference store and inter prediction");
    assert!(matches!(
        error,
        Error::Unsupported(
            "vaco-codec-av1: inter frame uses frame_size_with_refs; reference-store/inter prediction is not decoded"
        )
    ));
}
