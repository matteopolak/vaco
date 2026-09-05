//! Scaling and conversion benchmarks.
//!
//! ```text
//! cargo bench -p vaco-scale
//! ```
//!
//! # Two kinds of number, and they answer different questions
//!
//! **`convert`** is absolute throughput on the conversions real pipelines run,
//! at real resolutions. It is what a `-vf scale` invocation costs.
//!
//! **`ab`** is the group that exists because of plan 12's PF-0.1 amendment: each
//! entry runs the *same work* two ways, side by side in one file, on more than
//! one input. Report the ratio, never the verdict — "1.76x" survives a different
//! machine and "faster" does not. Two confident assumptions in this project have
//! already measured backwards, so nothing here is optimised until it has been
//! measured, and the losing variant stays in the file so the next person can
//! re-measure rather than re-argue.
//!
//! # A caveat for Apple silicon
//!
//! macOS parks a new process on an efficiency core for the first few hundred
//! milliseconds. Divan's own warmup covers it, but a hand-rolled timer here
//! would not — see plan 12's PF-0.0 amendment, where an unchanged binary
//! reported 45 ns and 132 ns for the same row on consecutive runs.

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
    reason = "a benchmark that cannot allocate its input has nothing to measure"
)]

use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;
use vaco_scale::colour::{Affine, ColorStage};
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

fn main() {
    divan::main();
}

struct Image {
    planes: Vec<Vec<u8>>,
    strides: Vec<usize>,
}

impl Image {
    fn new(fmt: PixFmt, w: u32, h: u32) -> Self {
        let layout = fmt.plane_layout(w, h, 64).expect("layout");
        let mut planes = Vec::new();
        let mut strides = Vec::new();
        for p in 0..layout.planes {
            let mut data = vec![0u8; layout.sizes[p]];
            for (i, b) in data.iter_mut().enumerate() {
                *b = ((i * 37) ^ (i >> 5)) as u8;
            }
            planes.push(data);
            strides.push(layout.strides[p]);
        }
        Self { planes, strides }
    }
}

fn bench_convert(
    bencher: divan::Bencher<'_, '_>,
    s: PixFmt,
    sw: u32,
    sh: u32,
    d: PixFmt,
    dw: u32,
    dh: u32,
    opts: &str,
) {
    let mut o = ScaleOptions::default();
    if !opts.is_empty() {
        o.parse(opts).expect("options");
    }
    let mut scaler =
        Scaler::new(&ImageSpec::new(s, sw, sh), &ImageSpec::new(d, dw, dh), &o).expect("plans");
    let src = Image::new(s, sw, sh);
    let mut dst = Image::new(d, dw, dh);
    bencher
        .counter(divan::counter::ItemsCount::new(
            (dw as usize) * (dh as usize),
        ))
        .bench_local(|| {
            let ins: Vec<SrcPlane<'_>> = src
                .planes
                .iter()
                .zip(&src.strides)
                .map(|(p, st)| SrcPlane {
                    data: p,
                    stride: *st,
                })
                .collect();
            let mut outs: Vec<DstPlane<'_>> = dst
                .planes
                .iter_mut()
                .zip(&dst.strides)
                .map(|(p, st)| DstPlane {
                    data: p,
                    stride: *st,
                })
                .collect();
            scaler.scale_planes(&ins, &mut outs).expect("converts");
        });
}

mod convert {
    use super::*;

    /// The canonical playback conversion.
    #[divan::bench]
    fn yuv420p_to_rgb24_1080p(b: divan::Bencher<'_, '_>) {
        bench_convert(
            b,
            PixFmt::Yuv420p,
            1920,
            1080,
            PixFmt::Rgb24,
            1920,
            1080,
            "",
        );
    }

    /// The canonical hardware-encode feed; should be near a plane copy.
    #[divan::bench]
    fn yuv420p_to_nv12_1080p(b: divan::Bencher<'_, '_>) {
        bench_convert(b, PixFmt::Yuv420p, 1920, 1080, PixFmt::Nv12, 1920, 1080, "");
    }

    /// The canonical transcode downscale.
    #[divan::bench]
    fn yuv420p_downscale_bicubic(b: divan::Bencher<'_, '_>) {
        bench_convert(
            b,
            PixFmt::Yuv420p,
            1920,
            1080,
            PixFmt::Yuv420p,
            1280,
            720,
            "",
        );
    }

    /// The 2160p -> 1080p e2e benchmark scenario (`planning/E2E-GAPS.md` §9-11):
    /// same pixel format both sides, 2x down on both axes, default (bicubic)
    /// scaler -- exactly what `-vf scale=1920:1080` on a 4K `yuv420p` source runs.
    #[divan::bench]
    fn yuv420p_2160p_to_1080p_bicubic(b: divan::Bencher<'_, '_>) {
        bench_convert(
            b,
            PixFmt::Yuv420p,
            3840,
            2160,
            PixFmt::Yuv420p,
            1920,
            1080,
            "",
        );
    }

    /// Upscale with wide taps.
    #[divan::bench]
    fn yuv420p_upscale_lanczos(b: divan::Bencher<'_, '_>) {
        bench_convert(
            b,
            PixFmt::Yuv420p,
            1280,
            720,
            PixFmt::Yuv420p,
            1920,
            1080,
            "scaler=lanczos",
        );
    }

    /// Encode-side conversion: matrix plus chroma decimation.
    #[divan::bench]
    fn rgb24_to_yuv420p_1080p(b: divan::Bencher<'_, '_>) {
        bench_convert(
            b,
            PixFmt::Rgb24,
            1920,
            1080,
            PixFmt::Yuv420p,
            1920,
            1080,
            "",
        );
    }

    /// Bit-depth reduction with dither.
    #[divan::bench]
    fn yuv420p10le_to_yuv420p(b: divan::Bencher<'_, '_>) {
        bench_convert(
            b,
            PixFmt::Yuv420p10le,
            1920,
            1080,
            PixFmt::Yuv420p,
            1920,
            1080,
            "",
        );
    }

    /// Plan construction. Short CLI invocations pay this once and it must not
    /// dominate them.
    #[divan::bench]
    fn plan_construction(b: divan::Bencher<'_, '_>) {
        b.bench(|| {
            Scaler::new(
                &ImageSpec::new(PixFmt::Yuv420p, 1920, 1080),
                &ImageSpec::new(PixFmt::Rgb24, 1280, 720),
                &ScaleOptions::default(),
            )
            .expect("plans")
        });
    }

    /// Slice threading, one entry per worker count. The ratio between them is
    /// the scaling curve; the absolute numbers are not comparable with the
    /// single-threaded entries above because the pool is built per scaler.
    #[divan::bench(args = [1, 2, 4, 8])]
    fn threads(b: divan::Bencher<'_, '_>, n: i32) {
        bench_convert(
            b,
            PixFmt::Yuv420p,
            1920,
            1080,
            PixFmt::Yuv420p,
            1280,
            720,
            &format!("threads={n}"),
        );
    }
}

/// Side-by-side variants. Read the ratio, not the winner.
mod ab {
    use super::*;

    /// Two rows of the same length, so a difference cannot be a cache effect.
    fn rows(n: usize) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
        let mk = |seed: u32| -> Vec<i32> {
            (0..n)
                .map(|i| (((i as u32).wrapping_mul(2_654_435_761).wrapping_add(seed)) % 256) as i32)
                .collect()
        };
        (mk(1), mk(2), mk(3))
    }

    fn matrix() -> Affine {
        let src = ImageSpec::new(PixFmt::Yuv444p, 64, 64);
        let dst = ImageSpec::new(PixFmt::Rgb24, 64, 64);
        let mut budget = Budget::new(Limits::permissive());
        match vaco_scale::colour::build(&mut budget, &src, &dst, &ScaleOptions::default(), 8)
            .unwrap()
        {
            ColorStage::Affine(a) => a,
            ColorStage::None => unreachable!("a Y'CbCr to R'G'B' conversion has a matrix"),
            ColorStage::Float(_) => unreachable!("benchmark uses default matching colour metadata"),
        }
    }

    /// The colour matrix, scalar against dispatched SIMD, on two row widths.
    ///
    /// 1920 is a real luma row; 63 is deliberately not a multiple of any lane
    /// count, so the tail path is measured rather than assumed free.
    #[divan::bench(args = [63, 1920], consts = [false, true])]
    fn affine_row<const SIMD: bool>(b: divan::Bencher<'_, '_>, n: usize) {
        let a = matrix();
        let k = if SIMD {
            <vaco_scale::fast::ScaleKernels as vaco_simd::KernelSet>::select().affine_row
        } else {
            <vaco_scale::fast::ScaleKernels as vaco_simd::KernelSet>::reference().affine_row
        };
        let (r0, r1, r2) = rows(n);
        b.with_inputs(move || (r0.clone(), r1.clone(), r2.clone()))
            .bench_local_values(|(mut a0, mut a1, mut a2)| {
                k(&a, &mut a0, &mut a1, &mut a2);
            });
    }

    /// Bank layout: the row-major form the crate ships against the blocked form
    /// plan 17 §A.7.5 prescribes. Both compute the same output.
    ///
    /// The blocked form is *not* implemented — this entry measures the gather
    /// pattern alone, on a synthetic bank, so the cost of adopting it can be
    /// argued from a number rather than from the plan's assertion.
    #[divan::bench(args = [4, 8, 16])]
    fn gather_pattern(b: divan::Bencher<'_, '_>, taps: usize) {
        let n = 1920usize;
        let src: Vec<i32> = (0..n + 64).map(|i| (i % 251) as i32).collect();
        let coeffs: Vec<i32> = (0..n * taps).map(|i| ((i % 7) as i32) - 3).collect();
        let offsets: Vec<u32> = (0..n).map(|d| (d as u32) % 32).collect();
        let mut out = vec![0i32; n];
        b.bench_local(|| {
            for (d, o) in out.iter_mut().enumerate() {
                let off = offsets[d] as usize;
                let mut acc = 0i64;
                for t in 0..taps {
                    acc += i64::from(coeffs[d * taps + t]) * i64::from(src[off + t]);
                }
                *o = (acc >> 14) as i32;
            }
            divan::black_box(&out);
        });
    }
}
