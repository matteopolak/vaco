//! Benchmarks. `divan`, per plan 19 §"no criterion".
//!
//! # What is being compared, and why side by side
//!
//! Plan 12's PF-0.1 amendment records two confident performance assumptions on
//! this project that measured *backwards* — a branchless CABAC decision at
//! 1.76x slower than the spec's literal shape, and D12's widening-MAC gap at
//! 0.79x against an assumed ~6x. The polyphase inner loop here is exactly a
//! widening multiply-accumulate, so the same discipline applies: write the
//! obvious version, put the alternatives in one file, run them on more than one
//! input, and report ratios.
//!
//! Three groups:
//!
//! 1. `dot` — one, four and eight accumulators, at three tap counts, in `f32`
//!    and `f64`. The PF-0.0 rule "never carry a single accumulator" claims up to
//!    4x here; this measures whether it holds for *this* loop.
//! 2. `walk` — the stride-1 specialisation in `convert_elems` against the
//!    general `step_by` path doing the same work. The specialisation exists on
//!    the theory that a runtime `step_by` blocks vectorisation; if it does not,
//!    the specialisation should go.
//! 3. `pipeline` — the §B.15 scenarios, end to end.

#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::field_reassign_with_default,
    clippy::collapsible_if,
    clippy::unwrap_used,
    reason = "benchmark code, not a shipped path"
)]

use divan::Bencher;
use vaco_chlayout::ChannelLayout;
use vaco_limits::{Budget, Limits};
use vaco_resample::convert::{Elem, convert_elems};
use vaco_resample::rate::kernel::{dot_naive, dot4, dot8};
use vaco_resample::{AudioMut, AudioRef, AudioSpec, ResampleOptions, Resampler};
use vaco_sampfmt::SampleFmt;

fn main() {
    divan::main();
}

// ---------------------------------------------------------------------------
// 1. The convolution kernel
// ---------------------------------------------------------------------------

const TAPS: [usize; 3] = [32, 50, 256];

fn ramp<T: From<u16>>(n: usize) -> Vec<T> {
    (0..n).map(|i| T::from((i % 97) as u16)).collect()
}

fn taps_f32(n: usize) -> (Vec<f32>, Vec<f32>) {
    let x: Vec<f32> = (0..n).map(|i| ((i % 97) as f32) * 0.01 - 0.5).collect();
    let h: Vec<f32> = (0..n).map(|i| ((i % 31) as f32) * 0.002 - 0.03).collect();
    (x, h)
}

fn taps_f64(n: usize) -> (Vec<f64>, Vec<f64>) {
    let (x, h) = taps_f32(n);
    (
        x.iter().map(|v| f64::from(*v)).collect(),
        h.iter().map(|v| f64::from(*v)).collect(),
    )
}

#[divan::bench(args = TAPS)]
fn dot_f32_naive(bencher: Bencher<'_, '_>, taps: usize) {
    let (x, h) = taps_f32(taps);
    bencher.bench(|| dot_naive(divan::black_box(&x), divan::black_box(&h)));
}

#[divan::bench(args = TAPS)]
fn dot_f32_acc4(bencher: Bencher<'_, '_>, taps: usize) {
    let (x, h) = taps_f32(taps);
    bencher.bench(|| dot4(divan::black_box(&x), divan::black_box(&h)));
}

#[divan::bench(args = TAPS)]
fn dot_f32_acc8(bencher: Bencher<'_, '_>, taps: usize) {
    let (x, h) = taps_f32(taps);
    bencher.bench(|| dot8(divan::black_box(&x), divan::black_box(&h)));
}

#[divan::bench(args = TAPS)]
fn dot_f64_naive(bencher: Bencher<'_, '_>, taps: usize) {
    let (x, h) = taps_f64(taps);
    bencher.bench(|| dot_naive(divan::black_box(&x), divan::black_box(&h)));
}

#[divan::bench(args = TAPS)]
fn dot_f64_acc4(bencher: Bencher<'_, '_>, taps: usize) {
    let (x, h) = taps_f64(taps);
    bencher.bench(|| dot4(divan::black_box(&x), divan::black_box(&h)));
}

#[divan::bench(args = TAPS)]
fn dot_f64_acc8(bencher: Bencher<'_, '_>, taps: usize) {
    let (x, h) = taps_f64(taps);
    bencher.bench(|| dot8(divan::black_box(&x), divan::black_box(&h)));
}

// ---------------------------------------------------------------------------
// 2. Element conversion: stride-1 specialisation versus the general walk
// ---------------------------------------------------------------------------

const SAMPLES: usize = 8192;

#[divan::bench]
fn walk_s16_to_f32_contiguous(bencher: Bencher<'_, '_>) {
    let src: Vec<u8> = ramp::<u16>(SAMPLES)
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut dst = vec![0u8; SAMPLES * 4];
    bencher.bench_local(|| {
        convert_elems(Elem::S16, &src, 1, Elem::F32, &mut dst, 1, SAMPLES);
    });
}

/// The same element count through the strided path, gathering every second
/// sample from a twice-as-long source. This is the packed-to-planar shape.
#[divan::bench]
fn walk_s16_to_f32_stride2(bencher: Bencher<'_, '_>) {
    let src: Vec<u8> = ramp::<u16>(SAMPLES * 2)
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut dst = vec![0u8; SAMPLES * 4];
    bencher.bench_local(|| {
        convert_elems(Elem::S16, &src, 2, Elem::F32, &mut dst, 1, SAMPLES);
    });
}

#[divan::bench]
fn walk_f32_to_s16_contiguous(bencher: Bencher<'_, '_>) {
    let src: Vec<u8> = (0..SAMPLES)
        .map(|i| ((i % 97) as f32) * 0.01 - 0.5)
        .flat_map(f32::to_le_bytes)
        .collect();
    let mut dst = vec![0u8; SAMPLES * 2];
    bencher.bench_local(|| {
        convert_elems(Elem::F32, &src, 1, Elem::S16, &mut dst, 1, SAMPLES);
    });
}

#[divan::bench]
fn walk_s32_to_s16_contiguous(bencher: Bencher<'_, '_>) {
    let src: Vec<u8> = (0..SAMPLES)
        .map(|i| (i as i32).wrapping_mul(65_537))
        .flat_map(i32::to_le_bytes)
        .collect();
    let mut dst = vec![0u8; SAMPLES * 2];
    bencher.bench_local(|| {
        convert_elems(Elem::S32, &src, 1, Elem::S16, &mut dst, 1, SAMPLES);
    });
}

// ---------------------------------------------------------------------------
// 3. End-to-end pipeline scenarios (plan 17 §B.15)
// ---------------------------------------------------------------------------

struct Case {
    name: &'static str,
    in_rate: u32,
    out_rate: u32,
    in_fmt: SampleFmt,
    out_fmt: SampleFmt,
    in_layout: fn() -> ChannelLayout,
    out_layout: fn() -> ChannelLayout,
}

fn l51() -> ChannelLayout {
    ChannelLayout::from_name("5.1").unwrap_or(ChannelLayout::STEREO)
}
fn l71() -> ChannelLayout {
    ChannelLayout::from_name("7.1").unwrap_or(ChannelLayout::STEREO)
}
fn stereo() -> ChannelLayout {
    ChannelLayout::STEREO
}

const CASES: &[Case] = &[
    Case {
        name: "1-44k1-48k-s16-stereo",
        in_rate: 44100,
        out_rate: 48000,
        in_fmt: SampleFmt::S16,
        out_fmt: SampleFmt::S16,
        in_layout: stereo,
        out_layout: stereo,
    },
    Case {
        name: "2-48k-44k1-f32-stereo",
        in_rate: 48000,
        out_rate: 44100,
        in_fmt: SampleFmt::F32,
        out_fmt: SampleFmt::F32,
        in_layout: stereo,
        out_layout: stereo,
    },
    Case {
        name: "3-44k1-48k-f32p-s16",
        in_rate: 44100,
        out_rate: 48000,
        in_fmt: SampleFmt::F32P,
        out_fmt: SampleFmt::S16,
        in_layout: stereo,
        out_layout: stereo,
    },
    Case {
        name: "4-format-only-s16-f32",
        in_rate: 48000,
        out_rate: 48000,
        in_fmt: SampleFmt::S16,
        out_fmt: SampleFmt::F32,
        in_layout: stereo,
        out_layout: stereo,
    },
    Case {
        name: "5-downmix-51-stereo",
        in_rate: 48000,
        out_rate: 48000,
        in_fmt: SampleFmt::F32,
        out_fmt: SampleFmt::F32,
        in_layout: l51,
        out_layout: stereo,
    },
    Case {
        name: "6-downmix-71-51",
        in_rate: 48000,
        out_rate: 48000,
        in_fmt: SampleFmt::F32,
        out_fmt: SampleFmt::F32,
        in_layout: l71,
        out_layout: l51,
    },
    Case {
        name: "7-full-51-44k1-48k-s16",
        in_rate: 44100,
        out_rate: 48000,
        in_fmt: SampleFmt::F32,
        out_fmt: SampleFmt::S16,
        in_layout: l51,
        out_layout: stereo,
    },
    Case {
        name: "8-96k-48k-f32-long-filter",
        in_rate: 96000,
        out_rate: 48000,
        in_fmt: SampleFmt::F32,
        out_fmt: SampleFmt::F32,
        in_layout: stereo,
        out_layout: stereo,
    },
];

fn case_names() -> Vec<&'static str> {
    CASES.iter().map(|c| c.name).collect()
}

fn build(case: &Case, filter_size: i32) -> Option<Resampler> {
    let mut opts = ResampleOptions::default();
    opts.filter_size = filter_size;
    let mut budget = Budget::new(Limits::permissive());
    Resampler::new(
        &AudioSpec::new(case.in_rate, case.in_fmt, (case.in_layout)()).ok()?,
        &AudioSpec::new(case.out_rate, case.out_fmt, (case.out_layout)()).ok()?,
        &opts,
        &mut budget,
    )
    .ok()
}

const FRAMES: usize = 4096;

fn planes_for(fmt: SampleFmt, channels: usize, frames: usize) -> Vec<Vec<u8>> {
    let per = fmt.bytes_per_sample();
    if fmt.is_planar() {
        (0..channels).map(|_| vec![0x11u8; frames * per]).collect()
    } else {
        vec![vec![0x11u8; frames * per * channels]]
    }
}

#[divan::bench(args = case_names())]
fn pipeline(bencher: Bencher<'_, '_>, name: &str) {
    let Some(case) = CASES.iter().find(|c| c.name == name) else {
        return;
    };
    let Some(mut rs) = build(case, 32) else {
        return;
    };
    let in_ch = (case.in_layout)().channels as usize;
    let out_ch = (case.out_layout)().channels as usize;
    let src_planes = planes_for(case.in_fmt, in_ch, FRAMES);
    let mut dst_planes = planes_for(case.out_fmt, out_ch, FRAMES * 2);
    let refs: Vec<&[u8]> = src_planes.iter().map(Vec::as_slice).collect();
    bencher
        .counter(divan::counter::ItemsCount::new(FRAMES))
        .bench_local(|| {
            let src = if case.in_fmt.is_planar() {
                AudioRef::planar(case.in_fmt, &refs)
            } else {
                AudioRef::packed(
                    case.in_fmt,
                    in_ch as u32,
                    refs.first().copied().unwrap_or(&[]),
                )
            };
            let Ok(src) = src else { return };
            if case.out_fmt.is_planar() {
                let mut split: Vec<&mut [u8]> =
                    dst_planes.iter_mut().map(Vec::as_mut_slice).collect();
                if let Ok(mut dst) = AudioMut::planar(case.out_fmt, &mut split) {
                    let _ = rs.convert(Some(src), &mut dst);
                }
            } else if let Some(first) = dst_planes.first_mut() {
                if let Ok(mut dst) = AudioMut::packed(case.out_fmt, out_ch as u32, first) {
                    let _ = rs.convert(Some(src), &mut dst);
                }
            }
        });
}

/// Coefficient-bank generation: `P·T` transcendental evaluations, which matter
/// for short CLI runs (§B.15 scenario 10).
#[divan::bench(args = [32, 64, 256])]
fn bank_setup(bencher: Bencher<'_, '_>, filter_size: i32) {
    let case = Case {
        name: "setup",
        in_rate: 44100,
        out_rate: 48000,
        in_fmt: SampleFmt::F32,
        out_fmt: SampleFmt::F32,
        in_layout: stereo,
        out_layout: stereo,
    };
    bencher.bench_local(|| divan::black_box(build(&case, filter_size)).is_some());
}

// ---------------------------------------------------------------------------
// 4. What the reference's overflow behaviour costs
// ---------------------------------------------------------------------------
//
// `elem::f32_to_i16` reproduces the reference exactly, including the `i64`
// saturate / `i32` truncate / clamp sequence its clip helper performs on an
// out-of-range value. That is three integer ops per sample on a path that would
// otherwise be one convert and one clamp. This pair measures the price, so a
// future decision to drop the emulation is made against a number rather than a
// feeling.

/// The shipped converter: bit-identical to the reference on every input.
#[divan::bench]
fn clip_exact(bencher: Bencher<'_, '_>) {
    let src: Vec<f32> = (0..SAMPLES)
        .map(|i| ((i % 97) as f32) * 0.01 - 0.5)
        .collect();
    let mut dst = vec![0i16; SAMPLES];
    bencher.bench_local(|| {
        for (s, d) in src.iter().zip(dst.iter_mut()) {
            *d = vaco_resample::convert::elem::f32_to_i16(*s);
        }
    });
}

/// The obvious version: round, clamp in `f32`, convert. Differs from the
/// reference only for `|x| > 65536`, which is +96 dBFS.
#[divan::bench]
fn clip_naive(bencher: Bencher<'_, '_>) {
    let src: Vec<f32> = (0..SAMPLES)
        .map(|i| ((i % 97) as f32) * 0.01 - 0.5)
        .collect();
    let mut dst = vec![0i16; SAMPLES];
    bencher.bench_local(|| {
        for (s, d) in src.iter().zip(dst.iter_mut()) {
            *d = (s * 32768.0 + 0.5).floor().clamp(-32768.0, 32767.0) as i16;
        }
    });
}
