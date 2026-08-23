//! Shared video kernels that cross filter-crate boundaries.
//!
//! Plan `16-filters.md` SS4.1 places `scene_sad` here because it is used by
//! `framerate` (motion), `freezedetect` (temporal), `identity`/`msad`
//! (analysis), `minterpolate` (motion), `scdet` (analysis) and `select`
//! (multimedia) — filters that land in five different category crates. This
//! crate did not exist yet when `vaco-filter-temporal` (GitHub #475) needed
//! `scene_sad` for `decimate`, `mpdecimate` and `freezedetect`, so it is
//! created here, minimally, rather than duplicating the kernel inside that
//! crate (D19: one definition per concept). Whoever implements `framerate`'s
//! real motion-compensated blend, `scdet`, `identity`/`msad` or
//! `minterpolate` should extend this crate rather than re-deriving the same
//! sum, and `edge_common`/`motion_estimation`/the box-blur core/`transform`
//! (the rest of plan SS4.1's `vdsp` kernel set) still need to be added here by
//! whichever agent needs them first.
//!
//! # What is here
//!
//! [`plane_sad`] — the sum of absolute per-sample differences between two
//! same-sized 8-bit planes, with an `f64` normalisation
//! ([`normalised_sad`]) to a `0.0..=1.0` "fraction of full-scale difference"
//! scale, and [`block_sad`] for the same sum restricted to one rectangular
//! block, which `decimate` and `mpdecimate` need for their per-block metric.
//!
//! # Why 8-bit only, for now
//!
//! Every filter that needs this today (`decimate`, `mpdecimate`,
//! `freezedetect`) is being written against `vaco-filter-temporal`'s own
//! byte-oriented plane access, which itself only handles 8-bit samples
//! cleanly without a depth parameter. A `u16` variant is a mechanical
//! addition (same loop, `u16` accumulator) whenever a caller needs one — not
//! a redesign — so it is left until there is a real caller rather than
//! speculatively generalised now.
#![forbid(unsafe_code)]

use vaco_frame::PlaneRef;

/// Sum of `|a[i] - b[i]|` over every sample the two planes have in common
/// (the overlap of their `(rows, row_bytes)`, so mismatched geometry degrades
/// rather than panics).
///
/// # Independent oracle
///
/// Two identical planes score `0` (any correct absolute-difference sum does);
/// a plane compared against its own bitwise complement (`255 - x` for every
/// 8-bit sample) scores `255 * sample_count`, the maximum possible value —
/// both are algebraic identities of "sum of absolute differences", not
/// properties of this particular implementation.
#[must_use]
pub fn plane_sad(a: PlaneRef<'_>, b: PlaneRef<'_>) -> u64 {
    let rows = a.rows().min(b.rows());
    let mut sad: u64 = 0;
    for y in 0..rows {
        let (Some(ra), Some(rb)) = (a.row(y), b.row(y)) else {
            continue;
        };
        let width = ra.len().min(rb.len());
        for x in 0..width {
            let (Some(&sa), Some(&sb)) = (ra.get(x), rb.get(x)) else {
                continue;
            };
            sad = sad.saturating_add(u64::from(sa.abs_diff(sb)));
        }
    }
    sad
}

/// [`plane_sad`] restricted to the rectangle `(bx, by)..(bx+bw, by+bh)`,
/// clipped to both planes' bounds. `decimate` and `mpdecimate` divide the
/// frame into blocks and threshold each block's SAD independently.
#[must_use]
pub fn block_sad(a: PlaneRef<'_>, b: PlaneRef<'_>, bx: usize, by: usize, bw: usize, bh: usize) -> u64 {
    let rows = a.rows().min(b.rows()).min(by.saturating_add(bh));
    let mut sad: u64 = 0;
    for y in by..rows {
        let (Some(ra), Some(rb)) = (a.row(y), b.row(y)) else {
            continue;
        };
        let width = ra.len().min(rb.len()).min(bx.saturating_add(bw));
        for x in bx..width {
            let (Some(&sa), Some(&sb)) = (ra.get(x), rb.get(x)) else {
                continue;
            };
            sad = sad.saturating_add(u64::from(sa.abs_diff(sb)));
        }
    }
    sad
}

/// [`plane_sad`] divided by `255 * sample_count`, so two 8-bit planes that
/// differ maximally everywhere score `1.0` and identical planes score `0.0`
/// regardless of resolution — the form `freezedetect`'s noise threshold and
/// `decimate`'s scene-change threshold both compare against.
#[must_use]
pub fn normalised_sad(a: PlaneRef<'_>, b: PlaneRef<'_>) -> f64 {
    let rows = a.rows().min(b.rows());
    let cols = (0..rows)
        .filter_map(|y| Some(a.row(y)?.len().min(b.row(y)?.len())))
        .min()
        .unwrap_or(0);
    let samples = (rows as u64).saturating_mul(cols as u64);
    if samples == 0 {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "SAD and sample counts are far below 2^53; this is a display-scale ratio"
    )]
    let ratio = plane_sad(a, b) as f64 / (255.0 * samples as f64);
    ratio.clamp(0.0, 1.0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn plane_of(value: u8, w: u32, h: u32) -> vaco_frame::Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    #[test]
    fn identical_planes_score_zero() {
        let a = plane_of(100, 8, 8);
        let b = plane_of(100, 8, 8);
        assert_eq!(plane_sad(a.plane(0).unwrap(), b.plane(0).unwrap()), 0);
        assert!(normalised_sad(a.plane(0).unwrap(), b.plane(0).unwrap()) < 1e-12);
    }

    #[test]
    fn full_complement_scores_the_algebraic_maximum() {
        let a = plane_of(0, 4, 4);
        let b = plane_of(255, 4, 4);
        assert_eq!(
            plane_sad(a.plane(0).unwrap(), b.plane(0).unwrap()),
            255 * 16
        );
        assert!((normalised_sad(a.plane(0).unwrap(), b.plane(0).unwrap()) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn block_sad_only_counts_the_block() {
        let mut a = plane_of(0, 4, 4);
        let b = plane_of(0, 4, 4);
        // Perturb one pixel outside a 2x2 block at (0,0); block score must stay 0.
        if let Some(mut p) = a.plane_mut(0)
            && let Some(row) = p.row_mut(3)
            && let Some(byte) = row.get_mut(3)
        {
            *byte = 255;
        }
        let sad = block_sad(a.plane(0).unwrap(), b.plane(0).unwrap(), 0, 0, 2, 2);
        assert_eq!(sad, 0, "perturbation outside the block must not count");
        let full = block_sad(a.plane(0).unwrap(), b.plane(0).unwrap(), 0, 0, 4, 4);
        assert_eq!(full, 255);
    }
}
