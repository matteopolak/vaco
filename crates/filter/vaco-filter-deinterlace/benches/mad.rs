//! Throughput benchmark for the shared motion-adaptive deinterlace kernel.
//!
//! The benchmark intentionally keeps input frame construction outside the
//! timed closure. It measures the row/pixel kernel, common output allocation,
//! and output writes; only the row/pixel work changes in `mad.rs`.
//!
//! ```text
//! cargo bench -p vaco-filter-deinterlace --bench mad
//! ```

#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "benchmark fixture dimensions and allocations are fixed by construction"
)]

use divan::{Bencher, black_box};
use std::time::{Duration, Instant};
use vaco_filter_deinterlace::bench_support::deinterlace_frame;
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;

fn main() {
    for &(width, height) in SIZES {
        let (pool, frames) = frames(width, height);
        let baseline = legacy_deinterlace_frame(
            &pool,
            Some(&frames[0]),
            &frames[1],
            Some(&frames[2]),
            width,
            height,
        );
        let candidate =
            deinterlace_frame(&pool, Some(&frames[0]), &frames[1], Some(&frames[2]), true).unwrap();
        assert_same_pixels(&baseline, &candidate);
    }
    interleaved_measurement();
    divan::main();
}

const SIZES: &[(u32, u32)] = &[(176, 144), (640, 480), (1_920, 1_080)];

fn frames(width: u32, height: u32) -> (FramePool, Vec<Frame>) {
    let pool = FramePool::default();
    let frames = (0..3)
        .map(|frame_index| {
            let mut frame = pool.acquire_video(PixFmt::Yuv420p, width, height).unwrap();
            for plane_index in 0..PixFmt::Yuv420p.plane_count() {
                let mut plane = frame.plane_mut(plane_index).unwrap();
                for y in 0..plane.rows() {
                    let row = plane.row_mut(y).unwrap();
                    for (x, sample) in row.iter_mut().enumerate() {
                        *sample =
                            (x.wrapping_mul(17)
                                .wrapping_add(y.wrapping_mul(31))
                                .wrapping_add(frame_index * 47)) as u8;
                    }
                }
            }
            frame
        })
        .collect();
    (pool, frames)
}

fn old_sample(plane: vaco_frame::PlaneRef<'_>, x: usize, y: usize) -> Option<u8> {
    plane.row(y)?.get(x).copied()
}

fn old_estimate(plane: vaco_frame::PlaneRef<'_>, x: usize, y: usize, rows: usize) -> Option<u16> {
    let above = y.checked_sub(1).and_then(|ay| old_sample(plane, x, ay));
    let below = if y.saturating_add(1) < rows {
        old_sample(plane, x, y + 1)
    } else {
        None
    };
    match (above, below) {
        (Some(a), Some(b)) => Some((u16::from(a) + u16::from(b)).div_ceil(2)),
        (Some(a), None) => Some(u16::from(a)),
        (None, Some(b)) => Some(u16::from(b)),
        (None, None) => None,
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::many_single_char_names,
    reason = "benchmark-only baseline preserves the former per-pixel call shape"
)]
fn old_blend(
    cur: vaco_frame::PlaneRef<'_>,
    prev: Option<vaco_frame::PlaneRef<'_>>,
    next: Option<vaco_frame::PlaneRef<'_>>,
    x: usize,
    y: usize,
    rows: usize,
) -> u8 {
    let spatial = old_estimate(cur, x, y, rows);
    let p = prev.and_then(|plane| old_estimate(plane, x, y, rows));
    let n = next.and_then(|plane| old_estimate(plane, x, y, rows));
    let temporal = match (p, n) {
        (Some(a), Some(b)) => Some((a + b).div_ceil(2)),
        _ => None,
    };
    let motion = match (p, n) {
        (Some(a), Some(b)) => a.abs_diff(b),
        _ => 0,
    };
    let value = match (temporal, spatial) {
        (Some(t), Some(s)) if motion <= 4 => (t + s).div_ceil(2),
        (Some(_), Some(s)) => s,
        (Some(t), None) => t,
        (None, Some(s)) => s,
        (None, None) => 128,
    };
    u8::try_from(value.min(255)).unwrap_or(255)
}

fn legacy_deinterlace_frame(
    pool: &FramePool,
    prev: Option<&Frame>,
    cur: &Frame,
    next: Option<&Frame>,
    width: u32,
    height: u32,
) -> Frame {
    let format = PixFmt::Yuv420p;
    let mut out = pool.acquire_video(format, width, height).unwrap();
    for plane_index in 0..format.plane_count() {
        let rows = format.plane_height(height, plane_index as u8) as usize;
        let cols = format.plane_width(width, plane_index as u8) as usize;
        let cur_plane = cur.plane(plane_index).unwrap();
        let prev_plane = prev.and_then(|frame| frame.plane(plane_index));
        let next_plane = next.and_then(|frame| frame.plane(plane_index));
        let mut dst_plane = out.plane_mut(plane_index).unwrap();
        for y in 0..rows {
            let dst_row = dst_plane.row_mut(y).unwrap();
            if y.is_multiple_of(2) {
                dst_row.copy_from_slice(cur_plane.row(y).unwrap());
                continue;
            }
            for x in 0..cols.min(dst_row.len()) {
                dst_row[x] = old_blend(cur_plane, prev_plane, next_plane, x, y, rows);
            }
        }
    }
    out
}

fn assert_same_pixels(left: &Frame, right: &Frame) {
    for plane_index in 0..PixFmt::Yuv420p.plane_count() {
        let left_plane = left.plane(plane_index).unwrap();
        let right_plane = right.plane(plane_index).unwrap();
        assert_eq!(left_plane.rows(), right_plane.rows());
        for y in 0..left_plane.rows() {
            assert_eq!(
                left_plane.row(y),
                right_plane.row(y),
                "plane={plane_index}, row={y}"
            );
        }
    }
}

/// Emit a same-process A/B measurement with ten alternating rounds. Divan's
/// per-function statistics remain useful for distribution shape, while this
/// explicit sequence prevents thermal drift or allocator state from being
/// mistaken for a kernel improvement in the headline comparison.
fn interleaved_measurement() {
    const ROUNDS: usize = 10;
    let (width, height) = (640, 480);
    let (pool, frames) = frames(width, height);
    let mut baseline_total = Duration::ZERO;
    let mut candidate_total = Duration::ZERO;
    for round in 0..ROUNDS {
        let run_baseline = |pool: &FramePool, frames: &[Frame]| {
            legacy_deinterlace_frame(
                pool,
                Some(&frames[0]),
                &frames[1],
                Some(&frames[2]),
                width,
                height,
            )
        };
        let run_candidate = |pool: &FramePool, frames: &[Frame]| {
            deinterlace_frame(pool, Some(&frames[0]), &frames[1], Some(&frames[2]), true).unwrap()
        };
        if round.is_multiple_of(2) {
            let start = Instant::now();
            black_box(run_baseline(&pool, &frames));
            baseline_total += start.elapsed();
            let start = Instant::now();
            black_box(run_candidate(&pool, &frames));
            candidate_total += start.elapsed();
        } else {
            let start = Instant::now();
            black_box(run_candidate(&pool, &frames));
            candidate_total += start.elapsed();
            let start = Instant::now();
            black_box(run_baseline(&pool, &frames));
            baseline_total += start.elapsed();
        }
    }
    let baseline_ns = baseline_total.as_secs_f64() * 1e9 / ROUNDS as f64;
    let candidate_ns = candidate_total.as_secs_f64() * 1e9 / ROUNDS as f64;
    eprintln!(
        "interleaved mad 640x480 rounds={ROUNDS}: baseline={baseline_ns:.0} ns candidate={candidate_ns:.0} ns speedup={:.3}x",
        baseline_ns / candidate_ns
    );
}

#[divan::bench(args = SIZES)]
fn production(bencher: Bencher<'_, '_>, size: (u32, u32)) {
    let (width, height) = size;
    let (pool, frames) = frames(width, height);
    bencher.bench_local(|| {
        let out = deinterlace_frame(
            black_box(&pool),
            Some(black_box(&frames[0])),
            black_box(&frames[1]),
            Some(black_box(&frames[2])),
            true,
        )
        .unwrap();
        black_box(out)
    });
}

#[divan::bench(args = SIZES)]
fn baseline(bencher: Bencher<'_, '_>, size: (u32, u32)) {
    let (width, height) = size;
    let (pool, frames) = frames(width, height);
    bencher.bench_local(|| {
        black_box(legacy_deinterlace_frame(
            black_box(&pool),
            Some(black_box(&frames[0])),
            black_box(&frames[1]),
            Some(black_box(&frames[2])),
            width,
            height,
        ))
    });
}
