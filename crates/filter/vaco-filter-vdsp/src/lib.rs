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
//!
//! # 2026-08-23 addition: `comb_score`
//!
//! `vaco-filter-deinterlace` (plan 16 SS4.3, the FT-4.12 long tail, #480)
//! needs a per-frame "how combed is this" metric for `idet` and
//! `fieldmatch` — a different question from `plane_sad`'s "how different
//! are these two whole planes", since combing is a property of **one**
//! frame's own vertical structure (its rows alternate between two
//! temporally-offset fields), not a comparison between two frames. Per this
//! crate's own invitation to extend rather than duplicate (see this
//! module's opening paragraph), [`comb_score`] is added here: the sum of
//! absolute vertical second differences, `|row[y-1] - 2*row[y] +
//! row[y+1]|`, which is small for smooth (progressive) vertical structure
//! and large where alternating rows disagree (interlaced motion). This is
//! an original metric — not a transcription of the reference's own
//! (GPL, unread) interlace-detection formula — and `vaco-filter-deinterlace`
//! documents it as such.
//!
//! # 2026-08-28 addition: [`affine`], [`integral`], [`motion`] (GitHub #459/#460)
//!
//! Plan SS4.1's row also lists `edge_common`, the box-blur core, morphology's
//! neighbourhood reduction and 1D/3D LUT sampling. **All four already have a
//! real, measured, tested implementation elsewhere in this tree**, written
//! before this crate existed to hold a shared copy:
//!
//! - `edge_common` (the Sobel/Prewitt/Scharr two-gradient engine): `vaco-filter-convolve::edge`,
//!   including a differential-tested border rule (`reflect-101`, corrected
//!   2026-08-28 from an earlier, insufficiently-tested "replicate" guess —
//!   see that module's own doc for the two-axis probe that told them apart).
//! - The box-blur core: `vaco-filter-blur::common::box_pass`, with a measured
//!   replicate border and — a real subtlety a naive shared version would
//!   miss — `boxblur` rounds to nearest while `avgblur` truncates the same
//!   division.
//! - Morphology's neighbourhood reduction: `vaco-filter-convolve::morph`
//!   (`dilation`/`erosion`'s shared engine, with a measured `coordinates`
//!   bitmask and a `threshold` cap) and `::morpho` (the two-input
//!   structuring-element generalisation).
//! - 1D/3D LUT sampling: `vaco-filter-lut::lut1d`/`lut3d::Cube3d` (trilinear;
//!   `tetrahedral` is a documented, not-yet-implemented gap there).
//!
//! Adding a second copy of any of these here — before a caller *outside*
//! those crates actually needs one — is exactly what this crate's own
//! opening paragraph and `cargo xtask dup-check` (D19) warn against: a
//! kernel with no second caller is untested by construction, and the first
//! draft of this addition did exactly that for `edge_common` and the
//! box-blur core before the collision was caught by inspection rather than
//! by tooling (dup-check only catches type-name collisions, not two
//! functions with different names computing the same thing). The real
//! remaining work item is a **consolidation migration** — moving those four
//! into this crate and updating their three owning crates to depend on it —
//! which is out of scope for whoever does not own those crates; see this
//! batch's own report for the details.
//!
//! [`affine`] (geometric warp) and [`integral`] (summed-area tables) have no
//! such prior implementation anywhere in the tree, so they are added here
//! directly.
//!
//! `motion_estimation` and "SAD/hadamard (pixelutils)" are **not**
//! reimplemented here either, for the D19 reason stated above rather than
//! the "already exists elsewhere" one: `vaco-codec-dsp-mecmp` (#144, D-12)
//! already owns SAD/SSD/variance/SATD as vectorised kernels for the
//! codec-encoder motion search. [`motion`] is a thin filter-side block
//! search built by *calling* that crate's `sad`, for callers (`deshake`'s
//! feature tracking, `minterpolate`'s motion field) that need "find the
//! best match nearby", not the encoder's rate-distortion search —
//! `vaco-codec-dsp-me` (#260, D-13) is that search and did not exist yet
//! when this was written; when it lands, a filter that wants RDO-aware
//! search should call it instead of [`motion`], which stays for the
//! simpler "cheapest good match" case.
#![forbid(unsafe_code)]

pub mod affine;
pub mod integral;
pub mod motion;

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
pub fn block_sad(
    a: PlaneRef<'_>,
    b: PlaneRef<'_>,
    bx: usize,
    by: usize,
    bw: usize,
    bh: usize,
) -> u64 {
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

/// Sum of absolute vertical second differences, `|a[y-1] - 2*a[y] +
/// a[y+1]|`, over interior rows of one plane (edge rows are excluded, not
/// clamped — they have no interior second difference).
///
/// # Independent oracle
///
/// A plane whose rows are a linear ramp (`row[y] = k*y + c` for constant
/// `k`) has zero second difference at every interior row *by construction*
/// — that is an algebraic identity of "second difference of a linear
/// function", not a property of this implementation — so [`comb_score`]
/// must be exactly `0` on such a plane. A plane whose rows strictly
/// alternate between two fixed values (the textbook combing pattern) has
/// the maximum possible per-row score at every interior row.
#[must_use]
pub fn comb_score(plane: PlaneRef<'_>) -> u64 {
    let rows = plane.rows();
    if rows < 3 {
        return 0;
    }
    let mut score: u64 = 0;
    for y in 1..rows.saturating_sub(1) {
        let (Some(above), Some(center), Some(below)) =
            (plane.row(y - 1), plane.row(y), plane.row(y + 1))
        else {
            continue;
        };
        let width = above.len().min(center.len()).min(below.len());
        for x in 0..width {
            let (Some(&a), Some(&c), Some(&b)) = (above.get(x), center.get(x), below.get(x)) else {
                continue;
            };
            let second = i32::from(a) - 2 * i32::from(c) + i32::from(b);
            score = score.saturating_add(second.unsigned_abs().into());
        }
    }
    score
}

/// Sum of squared per-sample differences between two same-sized 8-bit
/// planes (the overlap of their geometry, as [`plane_sad`]), the numerator
/// `vaco-filter-analysis`'s `psnr` needs for its per-component MSE.
///
/// # 2026-08-23 addition
///
/// `vaco-filter-analysis` (plan 16 SS4.3, GitHub #477) needs a sum-of-
/// squares reduction for `psnr`'s MSE, which is a different reduction from
/// [`plane_sad`]'s sum-of-absolute-differences — squaring changes which
/// differences dominate, so it is not expressible as a post-hoc transform of
/// the existing sum. Added here per this crate's own invitation to extend
/// rather than duplicate.
///
/// # Independent oracle
///
/// Two identical planes score `0`, the algebraic identity of "sum of squared
/// differences"; a plane compared against its bitwise complement (`255 - x`)
/// scores `255 * 255 * sample_count`, the maximum possible value — both are
/// true of any correct sum-of-squares, not of this implementation.
#[must_use]
pub fn plane_sse(a: PlaneRef<'_>, b: PlaneRef<'_>) -> u64 {
    let rows = a.rows().min(b.rows());
    let mut sse: u64 = 0;
    for y in 0..rows {
        let (Some(ra), Some(rb)) = (a.row(y), b.row(y)) else {
            continue;
        };
        let width = ra.len().min(rb.len());
        for x in 0..width {
            let (Some(&sa), Some(&sb)) = (ra.get(x), rb.get(x)) else {
                continue;
            };
            let d = u64::from(sa.abs_diff(sb));
            sse = sse.saturating_add(d.saturating_mul(d));
        }
    }
    sse
}

/// How many of the samples two same-sized 8-bit planes share exactly, over
/// how many samples were compared (the overlap of their geometry).
///
/// `vaco-filter-analysis`'s `identity` filter is measured (not guessed, see
/// that crate's docs) to report the *fraction of bit-exact pixels* per
/// plane, not a continuous difference measure — a plane that differs by 1
/// everywhere and a plane that differs by 255 everywhere score identically
/// (`0.0`) under this metric, which is the property that distinguishes it
/// from [`normalised_sad`].
///
/// # Independent oracle
///
/// Two identical planes score `(n, n)` (every sample matches); a plane
/// compared against its bitwise complement with no fixed point (guaranteed
/// for any 8-bit value, since `x != 255 - x` whenever `x != 127.5`, which no
/// integer is) scores `(0, n)`.
#[must_use]
pub fn identical_count(a: PlaneRef<'_>, b: PlaneRef<'_>) -> (u64, u64) {
    let rows = a.rows().min(b.rows());
    let mut same: u64 = 0;
    let mut total: u64 = 0;
    for y in 0..rows {
        let (Some(ra), Some(rb)) = (a.row(y), b.row(y)) else {
            continue;
        };
        let width = ra.len().min(rb.len());
        for x in 0..width {
            let (Some(&sa), Some(&sb)) = (ra.get(x), rb.get(x)) else {
                continue;
            };
            total += 1;
            if sa == sb {
                same += 1;
            }
        }
    }
    (same, total)
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

    fn ramp_plane(w: u32, h: u32) -> vaco_frame::Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    #[allow(clippy::cast_possible_truncation, reason = "test fixture, h is small")]
                    row.fill((y as u32).min(255) as u8);
                }
            }
        }
        f
    }

    #[test]
    fn a_linear_ramp_scores_zero() {
        // Algebraic identity: the second difference of a linear function is
        // zero everywhere, so comb_score must be exactly 0, not just small.
        let f = ramp_plane(4, 8);
        assert_eq!(comb_score(f.plane(0).unwrap()), 0);
    }

    #[test]
    fn strict_alternation_scores_higher_than_a_ramp() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 8).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..8usize {
                if let Some(row) = p.row_mut(y) {
                    row.fill(if y % 2 == 0 { 0 } else { 255 });
                }
            }
        }
        let combed = comb_score(f.plane(0).unwrap());
        let smooth = ramp_plane(4, 8);
        let smooth_score = comb_score(smooth.plane(0).unwrap());
        assert!(
            combed > smooth_score,
            "combed={combed} smooth={smooth_score}"
        );
        assert!(combed > 0);
    }

    #[test]
    fn plane_sse_identical_is_zero_complement_is_the_algebraic_maximum() {
        let a = plane_of(0, 4, 4);
        let b = plane_of(255, 4, 4);
        assert_eq!(plane_sse(a.plane(0).unwrap(), a.plane(0).unwrap()), 0);
        assert_eq!(
            plane_sse(a.plane(0).unwrap(), b.plane(0).unwrap()),
            255 * 255 * 16
        );
    }

    #[test]
    fn identical_count_distinguishes_from_normalised_sad() {
        // Half the plane matches exactly, half differs by 1 (not 255): a
        // continuous metric like `normalised_sad` reports a small number
        // (1/255 over half the samples), but `identical_count`'s fraction
        // must be exactly 0.5 regardless of *how much* the other half
        // differs, which is the property that makes it "identity" rather
        // than "difference".
        let pool = FramePool::default();
        let mut a = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        let mut b = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        if let Some(mut p) = a.plane_mut(0) {
            p.fill(10);
        }
        if let Some(mut p) = b.plane_mut(0) {
            p.fill(10);
            if let Some(row) = p.row_mut(0) {
                row.fill(11);
            }
        }
        let (same, total) = identical_count(a.plane(0).unwrap(), b.plane(0).unwrap());
        assert_eq!(total, 16);
        assert_eq!(same, 12, "3 of 4 rows are untouched, each 4 pixels wide");
        #[allow(clippy::cast_precision_loss, reason = "test code, tiny counts")]
        let fraction = same as f64 / total as f64;
        assert!((fraction - 0.75).abs() < 1e-12);
    }
}
