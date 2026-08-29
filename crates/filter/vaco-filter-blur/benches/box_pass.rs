//! `common::box_pass`'s fast `O(w*h)` sliding-window separable path against
//! the `O(w*h*(2rx+1)*(2ry+1))` brute-force rectangle sum it replaced.
//!
//! The brute-force version's own doc used to say "revisit with `divan`
//! before shipping this on real frame sizes" — this is that revisit.
//! `black_box` wraps every input and output: the two paths compute genuinely
//! different amounts of work per pixel, so a tie here would mean the
//! benchmark measured nothing, not that the two are equally fast.
//!
//! ```text
//! cargo bench -p vaco-filter-blur
//! ```

#![allow(
    clippy::integer_division,
    reason = "coarse checkerboard modulation for synthetic bench content; \
              precision is not the point"
)]

use divan::{Bencher, black_box};
use vaco_filter_blur::bench_support::{box_pass_fast, box_pass_naive};

fn main() {
    divan::main();
}

/// A synthetic plane with real structure (a diagonal ramp modulated by a
/// coarse checkerboard) rather than a flat field, so the sliding-window
/// accumulator sees varying deltas the way real footage would.
fn plane(w: usize, h: usize) -> Vec<Vec<u8>> {
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| {
                    let ramp = ((x + y) % 256) as u8;
                    let checker = if (x / 8 + y / 8) % 2 == 0 { 0 } else { 64 };
                    ramp.wrapping_add(checker)
                })
                .collect()
        })
        .collect()
}

const SIZES: &[(i32, i32)] = &[(176, 144), (640, 480), (1920, 1080)];
const RADII: &[i32] = &[1, 4, 16];

#[divan::bench(args = SIZES)]
fn fast_720p_like(bencher: Bencher<'_, '_>, size: (i32, i32)) {
    let (w, h) = size;
    let img = plane(w as usize, h as usize);
    let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
    bencher.bench_local(|| {
        black_box(box_pass_fast(
            black_box(&rows),
            black_box(w),
            black_box(h),
            black_box(4),
            black_box(4),
            false,
        ))
    });
}

#[divan::bench(args = SIZES)]
fn naive_720p_like(bencher: Bencher<'_, '_>, size: (i32, i32)) {
    let (w, h) = size;
    let img = plane(w as usize, h as usize);
    let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
    bencher.bench_local(|| {
        black_box(box_pass_naive(
            black_box(&rows),
            black_box(w),
            black_box(h),
            black_box(4),
            black_box(4),
            false,
        ))
    });
}

#[divan::bench(args = RADII)]
fn fast_by_radius(bencher: Bencher<'_, '_>, radius: i32) {
    let (w, h) = (640, 480);
    let img = plane(w as usize, h as usize);
    let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
    bencher.bench_local(|| {
        black_box(box_pass_fast(
            black_box(&rows),
            black_box(w),
            black_box(h),
            black_box(radius),
            black_box(radius),
            false,
        ))
    });
}

#[divan::bench(args = RADII)]
fn naive_by_radius(bencher: Bencher<'_, '_>, radius: i32) {
    let (w, h) = (640, 480);
    let img = plane(w as usize, h as usize);
    let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
    bencher.bench_local(|| {
        black_box(box_pass_naive(
            black_box(&rows),
            black_box(w),
            black_box(h),
            black_box(radius),
            black_box(radius),
            false,
        ))
    });
}
