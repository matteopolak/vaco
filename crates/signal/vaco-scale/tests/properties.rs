//! Property tests over the geometry, the banks and the pipeline.
//!
//! These are the tests that catch the class of bug this crate is most prone to:
//! a scaler that looks right on a 64×64 test pattern and quietly corrupts the
//! last row of a subsampled plane at an odd width.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    clippy::wildcard_imports,
    clippy::print_stdout,
    clippy::too_many_arguments,
    reason = "a failing assertion in a test is a failing test"
)]

use proptest::prelude::*;
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::filter::{COEFF_ONE, DEFAULT_MAX_TAPS, FilterSpec, Kernel, build_bank};
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

/// Allocate the planes a format needs at a size, filled with `fill`.
struct Image {
    planes: Vec<Vec<u8>>,
    strides: Vec<usize>,
}

impl Image {
    fn new(fmt: PixFmt, w: u32, h: u32, fill: impl Fn(usize, usize) -> u8) -> Self {
        let layout = fmt.plane_layout(w, h, 64).expect("layout");
        let mut planes = Vec::new();
        let mut strides = Vec::new();
        for p in 0..layout.planes {
            let stride = layout.strides[p];
            let rows = fmt.plane_height(h, p as u8) as usize;
            let mut data = vec![0u8; stride * rows.max(1)];
            for (i, b) in data.iter_mut().enumerate() {
                *b = fill(p, i);
            }
            planes.push(data);
            strides.push(stride);
        }
        Self { planes, strides }
    }

    fn src(&self) -> Vec<SrcPlane<'_>> {
        self.planes
            .iter()
            .zip(&self.strides)
            .map(|(d, s)| SrcPlane {
                data: d,
                stride: *s,
            })
            .collect()
    }

    fn dst(&mut self) -> Vec<DstPlane<'_>> {
        self.planes
            .iter_mut()
            .zip(&self.strides)
            .map(|(d, s)| DstPlane {
                data: d,
                stride: *s,
            })
            .collect()
    }
}

/// Formats the pipeline is expected to handle, spanning every family it claims.
const FORMATS: &[PixFmt] = &[
    PixFmt::Yuv420p,
    PixFmt::Yuv422p,
    PixFmt::Yuv444p,
    PixFmt::Yuv410p,
    PixFmt::Nv12,
    PixFmt::Nv21,
    PixFmt::Yuyv422,
    PixFmt::Uyvy422,
    PixFmt::Rgb24,
    PixFmt::Bgr24,
    PixFmt::Rgba,
    PixFmt::Bgra,
    PixFmt::Argb,
    PixFmt::Gray8,
    PixFmt::Yuv420p10le,
    PixFmt::Yuv444p16be,
    PixFmt::P010le,
    PixFmt::Rgb565le,
    PixFmt::Gbrp,
    PixFmt::Yuva420p,
];

fn convert(
    sfmt: PixFmt,
    sw: u32,
    sh: u32,
    dfmt: PixFmt,
    dw: u32,
    dh: u32,
    fill: impl Fn(usize, usize) -> u8,
    opts: &ScaleOptions,
) -> Option<Image> {
    let src_spec = ImageSpec::new(sfmt, sw, sh);
    let dst_spec = ImageSpec::new(dfmt, dw, dh);
    let mut scaler = Scaler::new(&src_spec, &dst_spec, opts).ok()?;
    let input = Image::new(sfmt, sw, sh, fill);
    let mut output = Image::new(dfmt, dw, dh, |_, _| 0);
    let planes = input.src();
    {
        let mut out = output.dst();
        scaler.scale_planes(&planes, &mut out).ok()?;
    }
    Some(output)
}

#[test]
fn every_format_pair_at_awkward_sizes_neither_panics_nor_errors() {
    let opts = ScaleOptions::default();
    for &s in FORMATS {
        for &d in FORMATS {
            for (sw, sh, dw, dh) in [
                (1, 1, 1, 1),
                (1, 1, 7, 5),
                (7, 5, 1, 1),
                (3, 3, 5, 5),
                (17, 9, 8, 16),
                (2, 2, 2, 2),
            ] {
                let out = convert(s, sw, sh, d, dw, dh, |_, i| (i * 7) as u8, &opts);
                assert!(
                    out.is_some(),
                    "{} {sw}x{sh} -> {} {dw}x{dh} failed",
                    s.name(),
                    d.name()
                );
            }
        }
    }
}

#[test]
fn a_constant_image_survives_every_kernel_and_every_ratio() {
    // The single most valuable property in the crate: it catches every
    // normalisation, edge-clamping and rounding bug at once.
    for kind in [
        vaco_scale::ScalerKind::Nearest,
        vaco_scale::ScalerKind::Bilinear,
        vaco_scale::ScalerKind::Bicubic,
        vaco_scale::ScalerKind::Lanczos,
        vaco_scale::ScalerKind::Gaussian,
        vaco_scale::ScalerKind::Area,
    ] {
        let mut opts = ScaleOptions::default();
        opts.scaler = kind;
        opts.scaler_sub = kind;
        for (sw, sh, dw, dh) in [
            (64, 64, 32, 32),
            (64, 64, 128, 128),
            (33, 17, 100, 7),
            (100, 7, 33, 17),
            (64, 64, 4, 4),
        ] {
            for value in [0u8, 17, 128, 255] {
                let out = convert(
                    PixFmt::Yuv444p,
                    sw,
                    sh,
                    PixFmt::Yuv444p,
                    dw,
                    dh,
                    |_, _| value,
                    &opts,
                )
                .expect("converts");
                for (p, plane) in out.planes.iter().enumerate() {
                    let stride = out.strides[p];
                    for y in 0..dh as usize {
                        for x in 0..dw as usize {
                            assert_eq!(
                                plane[y * stride + x],
                                value,
                                "{kind:?} {sw}x{sh}->{dw}x{dh} value {value} at ({x},{y})"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// A flat colour must survive a conversion that both resamples *and* changes
/// colour space — the combination where the chroma model is at its weakest, and
/// the one place a structural error would hide behind a plausible-looking
/// fidelity number rather than showing as garbage.
#[test]
fn a_flat_colour_survives_a_scaled_colour_conversion() {
    for (dw, dh) in [(64, 48), (128, 96), (61, 47)] {
        for (y, u, v) in [(128u8, 128u8, 128u8), (81, 90, 240), (145, 54, 34)] {
            let out = convert(
                PixFmt::Yuv420p,
                128,
                96,
                PixFmt::Rgb24,
                dw,
                dh,
                move |p, _| match p {
                    0 => y,
                    1 => u,
                    _ => v,
                },
                &ScaleOptions::default(),
            )
            .expect("converts");
            let stride = out.strides[0];
            let first: [u8; 3] = [out.planes[0][0], out.planes[0][1], out.planes[0][2]];
            for row in 0..dh as usize {
                for x in 0..dw as usize {
                    let at = row * stride + x * 3;
                    let got = [
                        out.planes[0][at],
                        out.planes[0][at + 1],
                        out.planes[0][at + 2],
                    ];
                    assert_eq!(
                        got, first,
                        "yuv({y},{u},{v}) -> rgb24 {dw}x{dh} is not flat at ({x},{row})"
                    );
                }
            }
        }
    }
}

#[test]
fn thread_count_never_changes_the_output() {
    for (sfmt, dfmt, sw, sh, dw, dh) in [
        (PixFmt::Yuv420p, PixFmt::Rgb24, 320, 200, 320, 200),
        (PixFmt::Yuv420p, PixFmt::Yuv420p, 320, 200, 213, 141),
        (PixFmt::Rgb24, PixFmt::Yuv420p, 161, 97, 320, 200),
    ] {
        let fill = |p: usize, i: usize| ((i * 31 + p * 97) % 251) as u8;
        let mut reference = None;
        for threads in [0, 2, 3, 5, 8] {
            let mut opts = ScaleOptions::default();
            opts.threads = threads;
            let out = convert(sfmt, sw, sh, dfmt, dw, dh, fill, &opts).expect("converts");
            match &reference {
                None => reference = Some(out.planes),
                Some(r) => assert_eq!(
                    &out.planes,
                    r,
                    "{} -> {} diverged at {threads} threads",
                    sfmt.name(),
                    dfmt.name()
                ),
            }
        }
    }
}

/// A nonlinear colour stage has no cross-band state either: the scalar `f64`
/// work must not make a conversion's bytes depend on scheduling.
#[test]
fn transfer_and_primaries_path_is_thread_deterministic() {
    use vaco_color::{
        ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
    };

    let src_color = ColorInfo {
        primaries: ColorPrimaries::Bt709,
        transfer: TransferCharacteristic::Bt709,
        matrix: MatrixCoefficients::Bt709,
        range: ColorRange::Limited,
        ..ColorInfo::default()
    };
    let dst_color = ColorInfo {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferCharacteristic::Bt2020_10,
        matrix: MatrixCoefficients::Bt2020Ncl,
        range: ColorRange::Limited,
        ..ColorInfo::default()
    };
    let source = Image::new(PixFmt::Yuv444p, 191, 107, |plane, i| {
        ((i * 17 + plane * 43) % 256) as u8
    });
    let src_spec = ImageSpec::new(PixFmt::Yuv444p, 191, 107).with_color(src_color);
    let dst_spec = ImageSpec::new(PixFmt::Yuv444p, 191, 107).with_color(dst_color);
    let mut reference = None;
    for threads in [0, 2, 5] {
        let mut options = ScaleOptions::default();
        options.threads = threads;
        let mut scaler = Scaler::new(&src_spec, &dst_spec, &options).expect("plan");
        let mut output = Image::new(PixFmt::Yuv444p, 191, 107, |_, _| 0);
        let planes = source.src();
        let mut dst = output.dst();
        scaler.scale_planes(&planes, &mut dst).expect("scale");
        match &reference {
            Some(expected) => assert_eq!(&output.planes, expected, "threads = {threads}"),
            None => reference = Some(output.planes),
        }
    }
}

#[test]
fn an_identity_conversion_is_a_copy() {
    let src = ImageSpec::new(PixFmt::Yuv420p, 64, 64);
    let scaler = Scaler::new(&src, &src, &ScaleOptions::default()).expect("plans");
    assert!(scaler.is_noop());
    let out = convert(
        PixFmt::Yuv420p,
        64,
        64,
        PixFmt::Yuv420p,
        64,
        64,
        |p, i| ((i * 13 + p) % 256) as u8,
        &ScaleOptions::default(),
    )
    .expect("converts");
    let input = Image::new(PixFmt::Yuv420p, 64, 64, |p, i| ((i * 13 + p) % 256) as u8);
    for p in 0..out.planes.len() {
        let stride = out.strides[p];
        let rows = PixFmt::Yuv420p.plane_height(64, p as u8) as usize;
        let bytes = PixFmt::Yuv420p.min_stride(64, p as u8);
        for y in 0..rows {
            assert_eq!(
                &out.planes[p][y * stride..y * stride + bytes],
                &input.planes[p][y * stride..y * stride + bytes],
                "plane {p} row {y}"
            );
        }
    }
}

#[test]
fn channel_permutations_are_exact_round_trips() {
    for (a, b) in [
        (PixFmt::Rgb24, PixFmt::Bgr24),
        (PixFmt::Rgba, PixFmt::Bgra),
        (PixFmt::Rgba, PixFmt::Argb),
        (PixFmt::Yuv420p, PixFmt::Nv12),
        (PixFmt::Nv12, PixFmt::Nv21),
        (PixFmt::Yuv422p, PixFmt::Yuyv422),
    ] {
        let fill = |p: usize, i: usize| ((i * 37 + p * 11) % 256) as u8;
        let opts = ScaleOptions::default();
        let mid = convert(a, 32, 32, b, 32, 32, fill, &opts).expect("forward");
        let src_spec = ImageSpec::new(b, 32, 32);
        let dst_spec = ImageSpec::new(a, 32, 32);
        let mut scaler = Scaler::new(&src_spec, &dst_spec, &opts).expect("plans");
        let mut back = Image::new(a, 32, 32, |_, _| 0);
        {
            let planes = mid.src();
            let mut out = back.dst();
            scaler.scale_planes(&planes, &mut out).expect("reverse");
        }
        let original = Image::new(a, 32, 32, fill);
        for (p, (got, want)) in back.planes.iter().zip(original.planes.iter()).enumerate() {
            let stride = back.strides[p];
            let rows = a.plane_height(32, p as u8) as usize;
            let bytes = a.min_stride(32, p as u8);
            for y in 0..rows {
                assert_eq!(
                    &got[y * stride..y * stride + bytes],
                    &want[y * stride..y * stride + bytes],
                    "{} <-> {} plane {p} row {y}",
                    a.name(),
                    b.name()
                );
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Every bank row sums to exactly one in fixed point, and never reads
    /// outside its source.
    #[test]
    fn banks_are_normalised_and_in_bounds(
        src in 1usize..300,
        dst in 1usize..300,
        kind in 0usize..6,
        phase in -2.0f64..2.0,
    ) {
        let kernel = [
            Kernel::Point,
            Kernel::Bilinear,
            Kernel::bicubic_default(),
            Kernel::Lanczos { a: 3.0 },
            Kernel::Gaussian { sigma: 1.0 },
            Kernel::Area,
        ][kind];
        let mut budget = Budget::new(Limits::permissive());
        let bank = build_bank(&mut budget, &FilterSpec {
            kernel,
            src_len: src,
            dst_len: dst,
            phase_src: phase,
            phase_dst: 0.0,
            max_taps: DEFAULT_MAX_TAPS,
        }).unwrap();
        prop_assert!(bank.taps <= src);
        for d in 0..dst {
            let row = bank.row(d).unwrap();
            let sum: i32 = row.iter().sum();
            prop_assert_eq!(sum, COEFF_ONE);
            prop_assert!(bank.offsets[d] as usize + bank.taps <= src);
        }
    }

    /// Arbitrary format pairs at arbitrary sizes plan-or-refuse and never panic.
    #[test]
    fn arbitrary_conversions_terminate_cleanly(
        si in 0usize..FORMATS.len(),
        di in 0usize..FORMATS.len(),
        sw in 1u32..40,
        sh in 1u32..40,
        dw in 1u32..40,
        dh in 1u32..40,
    ) {
        let out = convert(FORMATS[si], sw, sh, FORMATS[di], dw, dh, |_, i| (i % 256) as u8,
                          &ScaleOptions::default());
        prop_assert!(out.is_some());
    }

    /// A constant image is preserved for any format pair whose destination can
    /// represent the value.
    #[test]
    fn constant_luma_survives_subsampling_changes(
        value in 0u8..=255,
        w in 1u32..64,
        h in 1u32..64,
    ) {
        let out = convert(PixFmt::Yuv444p, w, h, PixFmt::Yuv420p, w, h,
                          move |p, _| if p == 0 { value } else { 128 },
                          &ScaleOptions::default()).unwrap();
        let stride = out.strides[0];
        for y in 0..h as usize {
            for x in 0..w as usize {
                prop_assert_eq!(out.planes[0][y * stride + x], value);
            }
        }
    }
}
