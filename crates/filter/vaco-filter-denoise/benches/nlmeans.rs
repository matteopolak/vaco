//! `nlmeans`'s integral-image fast path against the brute-force reference
//! it replaced.
//!
//! The reference recomputes every candidate patch's `(2*pr+1)^2` squared
//! differences from scratch for each of the `(2*rr+1)^2` search offsets;
//! the fast path builds one integral image per offset and answers every
//! pixel's patch sum in `O(1)`. `black_box` wraps every input and output —
//! the two paths do genuinely different amounts of work, so a tie would
//! mean the harness measured nothing.
//!
//! ```text
//! cargo bench -p vaco-filter-denoise --bench nlmeans
//! ```

use divan::{Bencher, black_box};
use vaco_filter_denoise::bench_support::{nlmeans_fast, nlmeans_naive};

fn main() {
    divan::main();
}

fn plane(w: usize, h: usize) -> Vec<f32> {
    (0..h)
        .flat_map(|y| {
            (0..w).map(move |x| {
                let ramp = ((x * 7 + y * 13) % 200) as f32;
                let checker = if (x + 2 * y) % 5 == 0 { 40.0 } else { 0.0 };
                ramp + checker
            })
        })
        .collect()
}

/// `(width, height, patch_radius, research_radius)` -- the reference's own
/// default `p=7`/`r=15` gives `pr=3`/`rr=7`; a smaller pair is included
/// since the naive path is prohibitively slow at the default on anything
/// but a tiny frame.
const CASES: &[(usize, usize, i64, i64)] = &[(48, 48, 1, 2), (48, 48, 3, 7)];

#[divan::bench(args = CASES)]
fn fast(bencher: Bencher<'_, '_>, case: (usize, usize, i64, i64)) {
    let (w, h, pr, rr) = case;
    let data = plane(w, h);
    bencher.bench_local(|| {
        black_box(nlmeans_fast(
            black_box(&data),
            w,
            h,
            255.0,
            8.0,
            black_box(pr),
            black_box(rr),
        ))
    });
}

#[divan::bench(args = CASES)]
fn naive(bencher: Bencher<'_, '_>, case: (usize, usize, i64, i64)) {
    let (w, h, pr, rr) = case;
    let data = plane(w, h);
    bencher.bench_local(|| {
        black_box(nlmeans_naive(
            black_box(&data),
            w,
            h,
            255.0,
            8.0,
            black_box(pr),
            black_box(rr),
        ))
    });
}
