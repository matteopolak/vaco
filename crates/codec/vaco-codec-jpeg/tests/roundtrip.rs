//! Encode/decode round trips: build a frame with this crate's own encoder,
//! decode it with this crate's own decoder, and check the pixels came back.
//!
//! This is the differential loop the crate has available without a
//! reference binary in this environment: not a substitute for measuring
//! against `ffmpeg`/`libjpeg-turbo`, but it does catch a large class of
//! bugs (geometry mismatches between encode and decode, sign errors in the
//! quantizer, a Huffman table built inconsistently) that unit tests over
//! either half alone cannot.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code building fixtures, not the untrusted-input surface these lints protect"
)]

use vaco_codec_jpeg::{EncodeOptions, decode, encode};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

fn flat_frame(fmt: PixFmt, width: u32, height: u32, luma: u8, chroma: u8) -> Frame {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_video(&mut budget, fmt, width, height).unwrap();
    let FrameData::Video { planes, .. } = &mut frame.data else {
        unreachable!()
    };
    for (i, plane) in planes.iter_mut().enumerate() {
        let value = if i == 0 { luma } else { chroma };
        let rows = plane.rows();
        let stride = plane.stride;
        let data = plane.data.make_mut();
        for y in 0..rows {
            if let Some(row) = data.get_mut(y * stride..y * stride + stride) {
                row.fill(value);
            }
        }
    }
    frame
}

fn gradient_frame(fmt: PixFmt, width: u32, height: u32) -> Frame {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_video(&mut budget, fmt, width, height).unwrap();
    let FrameData::Video { planes, .. } = &mut frame.data else {
        unreachable!()
    };
    for (i, plane) in planes.iter_mut().enumerate() {
        let rows = plane.rows();
        let stride = plane.stride;
        let data = plane.data.make_mut();
        for y in 0..rows {
            if let Some(row) = data.get_mut(y * stride..y * stride + stride) {
                for (x, b) in row.iter_mut().enumerate() {
                    *b = if i == 0 {
                        ((x * 7 + y * 3) % 256) as u8
                    } else {
                        128u8.wrapping_add(((x + y) % 32) as u8)
                    };
                }
            }
        }
    }
    frame
}

fn plane_bytes(frame: &Frame, index: usize) -> Vec<u8> {
    let plane = frame.plane(index).unwrap();
    let mut out = Vec::new();
    for y in 0..plane.rows() {
        out.extend_from_slice(plane.row(y).unwrap());
    }
    out
}

#[test]
fn a_perfectly_flat_image_round_trips_exactly_at_quality_100() {
    let fmt = PixFmt::from_name("yuvj420p").unwrap();
    let src = flat_frame(fmt, 16, 16, 128, 128);
    let options = EncodeOptions {
        quality: 100,
        restart_interval: 0,
        progressive: false,
    };
    let bytes = encode(&src, &options).unwrap();
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();

    let FrameData::Video {
        width,
        height,
        format,
        ..
    } = decoded.data
    else {
        unreachable!()
    };
    assert_eq!((width, height), (16, 16));
    assert_eq!(format, fmt);
    for i in 0..3 {
        let want = plane_bytes(&src, i);
        let got = plane_bytes(&decoded, i);
        assert_eq!(got, want, "plane {i} differs");
    }
}

#[test]
fn a_gradient_round_trips_within_one_level_at_quality_100() {
    let fmt = PixFmt::from_name("yuvj444p").unwrap();
    let src = gradient_frame(fmt, 32, 16);
    let options = EncodeOptions {
        quality: 100,
        restart_interval: 0,
        progressive: false,
    };
    let bytes = encode(&src, &options).unwrap();
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();

    let mut max_abs = 0i32;
    let mut sq_err = 0f64;
    let mut n = 0usize;
    for i in 0..3 {
        let want = plane_bytes(&src, i);
        let got = plane_bytes(&decoded, i);
        assert_eq!(want.len(), got.len());
        for (a, b) in want.iter().zip(got.iter()) {
            let d = i32::from(*a) - i32::from(*b);
            max_abs = max_abs.max(d.abs());
            sq_err += f64::from(d * d);
            n += 1;
        }
    }
    let rms = (sq_err / n as f64).sqrt();
    assert!(max_abs <= 2, "max abs deviation {max_abs} exceeds 2 LSB");
    assert!(rms <= 1.0, "RMS error {rms} exceeds 1.0");
}

#[test]
fn grayscale_round_trips() {
    let fmt = PixFmt::from_name("gray").unwrap();
    let src = gradient_frame(fmt, 24, 24);
    let options = EncodeOptions::default();
    let bytes = encode(&src, &options).unwrap();
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();
    let FrameData::Video { format, .. } = decoded.data else {
        unreachable!()
    };
    assert_eq!(format, fmt);
}

#[test]
fn restart_intervals_round_trip() {
    let fmt = PixFmt::from_name("yuvj420p").unwrap();
    let src = gradient_frame(fmt, 64, 32);
    let options = EncodeOptions {
        quality: 90,
        restart_interval: 2,
        progressive: false,
    };
    let bytes = encode(&src, &options).unwrap();
    // A real DRI segment and at least one RSTn marker should be present.
    assert!(bytes.windows(4).any(|w| w == [0xFF, 0xDD, 0x00, 0x04]));
    assert!(
        bytes
            .windows(2)
            .any(|w| w[0] == 0xFF && (0xD0..=0xD7).contains(&w[1]))
    );

    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();
    let FrameData::Video { width, height, .. } = decoded.data else {
        unreachable!()
    };
    assert_eq!((width, height), (64, 32));
}

#[test]
fn subsampling_variants_all_round_trip() {
    for name in ["yuvj420p", "yuvj422p", "yuvj444p", "yuvj440p"] {
        let fmt = PixFmt::from_name(name).unwrap();
        let src = gradient_frame(fmt, 40, 24);
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let decoded = decode(&bytes, &mut budget).unwrap();
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = decoded.data
        else {
            unreachable!()
        };
        assert_eq!(format, fmt, "{name}");
        assert_eq!((width, height), (40, 24), "{name}");
    }
}

#[test]
fn a_non_mcu_aligned_size_still_decodes_to_the_right_dimensions() {
    let fmt = PixFmt::from_name("yuvj420p").unwrap();
    let src = gradient_frame(fmt, 37, 21);
    let bytes = encode(&src, &EncodeOptions::default()).unwrap();
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();
    let FrameData::Video { width, height, .. } = decoded.data else {
        unreachable!()
    };
    assert_eq!((width, height), (37, 21));
}

#[test]
fn progressive_encode_emits_sof2_and_round_trips_exactly_at_quality_100() {
    let fmt = PixFmt::from_name("yuvj420p").unwrap();
    let src = flat_frame(fmt, 32, 32, 128, 128);
    let options = EncodeOptions {
        quality: 100,
        restart_interval: 0,
        progressive: true,
    };
    let bytes = encode(&src, &options).unwrap();
    assert!(
        bytes.windows(2).any(|w| w == [0xFF, 0xC2]),
        "progressive output must contain an SOF2 marker"
    );
    assert!(
        !bytes.windows(2).any(|w| w == [0xFF, 0xC0]),
        "progressive output must not also contain an SOF0 marker"
    );
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();
    for i in 0..3 {
        assert_eq!(
            plane_bytes(&decoded, i),
            plane_bytes(&src, i),
            "plane {i} differs"
        );
    }
}

#[test]
fn progressive_encode_round_trips_a_gradient_within_one_level() {
    let fmt = PixFmt::from_name("yuvj444p").unwrap();
    let src = gradient_frame(fmt, 32, 16);
    let options = EncodeOptions {
        quality: 100,
        restart_interval: 0,
        progressive: true,
    };
    let bytes = encode(&src, &options).unwrap();
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();

    let mut max_abs = 0i32;
    for i in 0..3 {
        let want = plane_bytes(&src, i);
        let got = plane_bytes(&decoded, i);
        assert_eq!(want.len(), got.len());
        for (a, b) in want.iter().zip(got.iter()) {
            max_abs = max_abs.max((i32::from(*a) - i32::from(*b)).abs());
        }
    }
    assert!(max_abs <= 2, "max abs deviation {max_abs} exceeds 2 LSB");
}

#[test]
fn progressive_encode_round_trips_every_subsampling_variant_and_restart_intervals() {
    // Progressive and baseline share the exact same forward-DCT/quantize
    // pipeline (`compute_coeffs`) and differ only in how the resulting
    // coefficients are split across scans, so decoding either must produce
    // pixel-identical output — a stronger, content-independent check than
    // an absolute error bound against the source, which this sawtooth
    // pattern's sharp edges can legitimately exceed under lossy
    // quantization alone (baseline already does, at this quality).
    for name in ["yuvj420p", "yuvj422p", "yuvj444p", "yuvj440p", "gray"] {
        let fmt = PixFmt::from_name(name).unwrap();
        let src = gradient_frame(fmt, 40, 24);
        for restart_interval in [0, 3] {
            let options = EncodeOptions {
                quality: 90,
                restart_interval,
                progressive: true,
            };
            let baseline_options = EncodeOptions {
                quality: 90,
                restart_interval,
                progressive: false,
            };
            let bytes = encode(&src, &options).unwrap();
            let baseline_bytes = encode(&src, &baseline_options).unwrap();
            let mut budget = Budget::new(Limits::permissive());
            let mut baseline_budget = Budget::new(Limits::permissive());
            let decoded = decode(&bytes, &mut budget).unwrap();
            let baseline_decoded = decode(&baseline_bytes, &mut baseline_budget).unwrap();
            let FrameData::Video {
                format,
                width,
                height,
                ..
            } = decoded.data
            else {
                unreachable!()
            };
            assert_eq!(format, fmt, "{name} restart={restart_interval}");
            assert_eq!(
                (width, height),
                (40, 24),
                "{name} restart={restart_interval}"
            );
            let planes = if name == "gray" { 1 } else { 3 };
            for i in 0..planes {
                let got = plane_bytes(&decoded, i);
                let want_baseline = plane_bytes(&baseline_decoded, i);
                assert_eq!(
                    got, want_baseline,
                    "{name} restart={restart_interval} plane {i}: progressive disagrees with baseline"
                );
            }
        }
    }
}

#[test]
fn progressive_encode_handles_non_mcu_aligned_dimensions() {
    let fmt = PixFmt::from_name("yuvj420p").unwrap();
    let src = gradient_frame(fmt, 37, 21);
    let options = EncodeOptions {
        quality: 90,
        restart_interval: 0,
        progressive: true,
    };
    let bytes = encode(&src, &options).unwrap();
    let mut budget = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget).unwrap();
    let FrameData::Video { width, height, .. } = decoded.data else {
        unreachable!()
    };
    assert_eq!((width, height), (37, 21));
}
