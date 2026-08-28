//! C5 support: measuring a deviation's **shape**, not just its size.
//!
//! # Why this exists
//!
//! `planning/AGENT-CONSTRAINTS.md`'s "Byte-exactness is a check, not the bar"
//! (owner ruling, 2026-08-28) supersedes every bit-exact acceptance criterion
//! in this project: the reference binary is a sanity check, not a pass/fail
//! oracle, and a small deviation from it is expected and fine. What still
//! matters — the thing worth building machinery for — is the distinction the
//! ruling draws:
//!
//! > Small and unstructured is fine. Structured is a bug. [...] max deviation
//! > 1-2 spread across a frame → rounding, ship it. [...] error concentrated
//! > in specific blocks, or on every row but the first [...] → a real defect,
//! > however small the average.
//!
//! A comparator that only asserts `max_abs <= tolerance` cannot see that
//! distinction: a deviation of 1 on every pixel of one macroblock and a
//! deviation of 1 scattered independently across the whole frame have the
//! same `max_abs`, and only one of them is a bug. This module is the missing
//! half — [`measure`] reports magnitude (`max_abs`/`mean_abs`/`rms`, for
//! [`crate::case::Tolerance`] to judge) **and** [`Shape`], a plain-language
//! description of where the deviation sits, so a human reading a case's
//! output sees "structured, concentrated in rows 4-4 of 8" rather than a
//! bare number that already looked fine on average.
//!
//! # The two examples this project has on record
//!
//! Both are named directly in the constraints file, and both are what
//! [`Shape::Structured`] is built to catch, via one mechanism
//! ([`largest_connected_component`]) rather than two bespoke heuristics:
//!
//! * **"Every row but the first"**: the differing pixels form one contiguous
//!   region spanning nearly the whole frame minus a clean strip. A connected-
//!   component pass over the "did this pixel differ" mask finds this as one
//!   dominant component and reports its bounding box.
//! * **"Concentrated in specific blocks" / "every macroblock of one type"**:
//!   either a single dominant region (same case as above, different shape)
//!   or several same-sized regions scattered around the frame — caught by
//!   the companion signal, "what fraction of differing pixels sit in a
//!   multi-pixel component at all" (see [`ClusterStats::clustered_fraction`]):
//!   independent rounding noise is almost all isolated singletons, so a high
//!   clustered fraction is itself evidence of structure even before any one
//!   component dominates.
//!
//! # What this module does not do
//!
//! It does not decide pass/fail — that stays [`crate::case::Tolerance`]'s
//! job (magnitude) plus a human or a future C5 policy reading [`Shape`]
//! (structure). It does not know about codecs, planes, or containers; it
//! takes two equal-length byte buffers and an optional 2-D [`Geometry`], the
//! same shape [`crate::compare::raw`]'s C4 already works with. And it works
//! byte-for-byte, not sample-for-sample: a 16-bit sample's high byte
//! differing by one looks like a much larger jump than the underlying sample
//! actually moved. Correct for 8-bit planes and byte streams; a caller
//! comparing wider samples should reinterpret before calling this, which is
//! not done here to keep this module's contract to "two byte buffers plus
//! optional geometry", matching C4's own.

/// The 2-D shape a byte buffer represents, when it has one. Audio and other
/// data with no useful spatial structure passes `None` and still gets
/// magnitude statistics — only [`Shape`]'s structural half needs this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
}

/// Where a deviation sits, in plain language — the half a raw `max_abs`
/// number cannot express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// No geometry was given, there was nothing to compare, or the
    /// structural checks found no concentration: differing samples are
    /// scattered independently, consistent with rounding noise.
    Unstructured,
    /// A human-readable description of the concentration found — a
    /// bounding box, a size, a fraction. Structured regardless of how small
    /// `max_abs` is; per the ruling, that is what makes it a bug rather than
    /// imprecision.
    Structured(String),
}

/// Magnitude plus shape for one comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct Deviation {
    /// Bytes actually compared (the shorter of the two buffers' lengths).
    pub compared: usize,
    /// Bytes that differed at all.
    pub differing: usize,
    pub max_abs: f64,
    pub mean_abs: f64,
    pub rms: f64,
    pub shape: Shape,
}

impl Deviation {
    /// Whether every one of `compared` bytes was identical.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        self.differing == 0
    }

    /// Judge magnitude against `tolerance`, ignoring [`Deviation::shape`]
    /// entirely — a caller that also wants the structural veto should check
    /// `shape` itself; this only answers "is the size acceptable".
    #[must_use]
    pub fn within_magnitude(&self, tolerance: &crate::case::Tolerance) -> bool {
        self.max_abs <= tolerance.max_abs && self.rms <= tolerance.max_rms
    }

    /// Whether [`Deviation::shape`] found a structural pattern — the ruling's
    /// veto: true here means "a real defect, however small the average".
    #[must_use]
    pub const fn is_structured(&self) -> bool {
        matches!(self.shape, Shape::Structured(_))
    }
}

/// Compare `ours` against `theirs` byte for byte, then classify the shape of
/// whatever differs using `geometry` when it is given and fits.
#[must_use]
pub fn measure(ours: &[u8], theirs: &[u8], geometry: Option<Geometry>) -> Deviation {
    let compared = ours.len().min(theirs.len());
    let mut differing = 0usize;
    let mut max_abs = 0.0_f64;
    let mut sum_abs = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut diffs = Vec::new();
    for (&oa, &ob) in ours.iter().zip(theirs.iter()) {
        let a = f64::from(oa);
        let b = f64::from(ob);
        let d = (a - b).abs();
        diffs.push(d);
        if d > 0.0 {
            differing += 1;
        }
        max_abs = max_abs.max(d);
        sum_abs += d;
        sum_sq += d * d;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "conformance statistics, not a codec bound — compared is bytes, not attacker-controlled dimensions"
    )]
    let n = compared as f64;
    let mean_abs = if compared == 0 { 0.0 } else { sum_abs / n };
    let rms = if compared == 0 { 0.0 } else { (sum_sq / n).sqrt() };

    let shape = geometry
        .filter(|g| g.width.saturating_mul(g.height) == compared && g.width > 0 && g.height > 0)
        .map_or(Shape::Unstructured, |g| classify_shape(&diffs, g));

    Deviation {
        compared,
        differing,
        max_abs,
        mean_abs,
        rms,
        shape,
    }
}

/// Structural classification over a fully-populated `width * height` diff
/// grid: connected-component analysis of "did this pixel differ at all",
/// per this module's top doc.
fn classify_shape(diffs: &[f64], g: Geometry) -> Shape {
    // Too few differing pixels for a shape claim to mean anything; treat as
    // unstructured rather than over-interpreting noise.
    const MIN_FOR_A_CLAIM: usize = 8;
    // One region dominates: this is the "every row but the first" / "one
    // bad block" shape.
    const DOMINANT_COMPONENT_THRESHOLD: f64 = 0.3;
    // No single region dominates, but most differing pixels still sit in
    // multi-pixel clumps rather than isolated singletons — several
    // same-shaped bad blocks scattered around the frame, not one, is still
    // "concentrated", just not in one place.
    const CLUSTERED_THRESHOLD: f64 = 0.6;

    let mask: Vec<bool> = diffs.iter().map(|&d| d > 0.0).collect();
    let total_differing = mask.iter().filter(|&&d| d).count();
    if total_differing < MIN_FOR_A_CLAIM {
        return Shape::Unstructured;
    }

    let components = connected_components(&mask, g);
    let Some(largest) = components.iter().max_by_key(|c| c.size) else {
        return Shape::Unstructured;
    };

    #[expect(clippy::cast_precision_loss, reason = "conformance reporting ratio, small counts")]
    let largest_fraction = largest.size as f64 / total_differing as f64;
    #[expect(clippy::cast_precision_loss, reason = "conformance reporting ratio, small counts")]
    let clustered_fraction = components
        .iter()
        .filter(|c| c.size >= 4)
        .map(|c| c.size)
        .sum::<usize>() as f64
        / total_differing as f64;

    if largest_fraction >= DOMINANT_COMPONENT_THRESHOLD {
        return Shape::Structured(format!(
            "one contiguous region covers {:.0}% of the {total_differing} differing byte(s): \
             rows {}-{}, columns {}-{} (of {}x{})",
            largest_fraction * 100.0,
            largest.min_row,
            largest.max_row,
            largest.min_col,
            largest.max_col,
            g.width,
            g.height
        ));
    }

    if clustered_fraction >= CLUSTERED_THRESHOLD {
        let clustered_components = components.iter().filter(|c| c.size >= 4).count();
        return Shape::Structured(format!(
            "{:.0}% of the {total_differing} differing byte(s) sit in {clustered_components} \
             multi-pixel region(s) rather than scattered independently ({}x{})",
            clustered_fraction * 100.0,
            g.width,
            g.height
        ));
    }

    Shape::Unstructured
}

struct Component {
    size: usize,
    min_row: usize,
    max_row: usize,
    min_col: usize,
    max_col: usize,
}

/// 4-connected flood fill over `mask` (row-major, `width * height`),
/// iterative (a stack, not recursion) so a large frame cannot blow the call
/// stack.
fn connected_components(mask: &[bool], g: Geometry) -> Vec<Component> {
    let mut visited = vec![false; mask.len()];
    let mut out = Vec::new();
    let index = |r: usize, c: usize| r * g.width + c;

    for start_r in 0..g.height {
        for start_c in 0..g.width {
            let start = index(start_r, start_c);
            let already_visited = visited.get(start).copied().unwrap_or(true);
            let differs = mask.get(start).copied().unwrap_or(false);
            if already_visited || !differs {
                continue;
            }
            let mut stack = vec![(start_r, start_c)];
            if let Some(v) = visited.get_mut(start) {
                *v = true;
            }
            let (mut min_row, mut max_row) = (start_r, start_r);
            let (mut min_col, mut max_col) = (start_c, start_c);
            let mut size = 0usize;
            while let Some((r, c)) = stack.pop() {
                size += 1;
                min_row = min_row.min(r);
                max_row = max_row.max(r);
                min_col = min_col.min(c);
                max_col = max_col.max(c);
                let neighbours = [
                    (r.checked_sub(1), Some(c)),
                    (Some(r + 1).filter(|&r2| r2 < g.height), Some(c)),
                    (Some(r), c.checked_sub(1)),
                    (Some(r), Some(c + 1).filter(|&c2| c2 < g.width)),
                ];
                for (nr, nc) in neighbours {
                    let (Some(nr), Some(nc)) = (nr, nc) else {
                        continue;
                    };
                    let ni = index(nr, nc);
                    let ni_visited = visited.get(ni).copied().unwrap_or(true);
                    let ni_differs = mask.get(ni).copied().unwrap_or(false);
                    if !ni_visited && ni_differs {
                        if let Some(v) = visited.get_mut(ni) {
                            *v = true;
                        }
                        stack.push((nr, nc));
                    }
                }
            }
            out.push(Component {
                size,
                min_row,
                max_row,
                min_col,
                max_col,
            });
        }
    }
    out
}

/// Find the single largest connected component's size, for callers that only
/// need the headline number (used by this module's own tests and available
/// for a future report that wants it without the full [`measure`] path).
#[must_use]
pub fn largest_connected_component(mask: &[bool], g: Geometry) -> usize {
    connected_components(mask, g)
        .iter()
        .map(|c| c.size)
        .max()
        .unwrap_or(0)
}

/// Per-frame clustering summary, exposed for a future report that wants the
/// numbers without the prose [`Shape`] renders them into.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClusterStats {
    pub total_differing: usize,
    pub largest_component: usize,
    pub clustered: usize,
}

impl ClusterStats {
    /// Fraction of differing pixels in a component of size `>= 4`. High
    /// values are evidence of structure even before one component
    /// dominates outright — see this module's top doc.
    #[must_use]
    pub fn clustered_fraction(&self) -> f64 {
        if self.total_differing == 0 {
            return 0.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "conformance reporting ratio, small counts")]
        {
            self.clustered as f64 / self.total_differing as f64
        }
    }
}


#[cfg(test)]
#[expect(clippy::indexing_slicing, reason = "test fixture construction over known-in-bounds indices")]
mod tests {
    use super::*;
    use crate::case::Tolerance;

    fn grid(g: Geometry, set: &[(usize, usize)]) -> Vec<u8> {
        let mut v = vec![0u8; g.width * g.height];
        for &(r, c) in set {
            v[r * g.width + c] = 1;
        }
        v
    }

    #[test]
    fn identical_buffers_have_zero_deviation() {
        let d = measure(&[1, 2, 3], &[1, 2, 3], None);
        assert!(d.is_exact());
        assert_eq!(d.shape, Shape::Unstructured);
    }

    #[test]
    fn scattered_independent_noise_is_unstructured() {
        let g = Geometry { width: 16, height: 16 };
        // Isolated single-pixel differences, spread out with no two adjacent
        // — the "±1 rounding scatter" case.
        let set: Vec<(usize, usize)> = (0..16).map(|i| (i, i)).collect();
        let ours = vec![0u8; g.width * g.height];
        let mut theirs = ours.clone();
        for &(r, c) in &set {
            theirs[r * g.width + c] = 1;
        }
        let d = measure(&ours, &theirs, Some(g));
        assert_eq!(d.shape, Shape::Unstructured, "{:?}", d.shape);
        assert_eq!(d.differing, set.len());
    }

    #[test]
    fn every_row_but_the_first_is_structured() {
        let g = Geometry { width: 8, height: 8 };
        let ours = vec![0u8; g.width * g.height];
        let mut theirs = ours.clone();
        for r in 1..g.height {
            for c in 0..g.width {
                theirs[r * g.width + c] = 5;
            }
        }
        let d = measure(&ours, &theirs, Some(g));
        assert!(d.is_structured(), "{:?}", d.shape);
        let Shape::Structured(msg) = &d.shape else {
            unreachable!("checked is_structured above")
        };
        assert!(msg.contains("rows 1-7"), "{msg}");
    }

    #[test]
    fn a_contiguous_block_is_structured() {
        let g = Geometry { width: 16, height: 16 };
        let ours = vec![0u8; g.width * g.height];
        let mut theirs = ours.clone();
        for r in 4..8 {
            for c in 4..8 {
                theirs[r * g.width + c] = 3;
            }
        }
        let d = measure(&ours, &theirs, Some(g));
        assert!(d.is_structured(), "{:?}", d.shape);
    }

    #[test]
    fn several_scattered_same_sized_blocks_are_structured() {
        // Four separate 4x4 blocks, none touching, none dominating alone —
        // the "every macroblock of one type" shape: no single component is
        // even close to 30% of the total, but almost everything is
        // clustered rather than isolated.
        let g = Geometry { width: 32, height: 32 };
        let ours = vec![0u8; g.width * g.height];
        let mut theirs = ours.clone();
        for &(br, bc) in &[(0usize, 0usize), (0, 16), (16, 0), (16, 16)] {
            for r in br..br + 4 {
                for c in bc..bc + 4 {
                    theirs[r * g.width + c] = 2;
                }
            }
        }
        let d = measure(&ours, &theirs, Some(g));
        assert!(d.is_structured(), "{:?}", d.shape);
    }

    #[test]
    fn magnitude_and_shape_are_independent_axes() {
        // Small max_abs, but structured -- the ruling's whole point: size
        // alone must not be allowed to wave this through.
        let g = Geometry { width: 8, height: 8 };
        let ours = vec![0u8; g.width * g.height];
        let mut theirs = ours.clone();
        for b in theirs.iter_mut().take(g.width) {
            *b = 1; // row 0 only, off by 1
        }
        let d = measure(&ours, &theirs, Some(g));
        let tolerance = Tolerance { max_abs: 2.0, max_ulp: 0, max_rms: 2.0 };
        assert!(d.within_magnitude(&tolerance), "magnitude alone looks fine");
        assert!(d.is_structured(), "but the shape check must still catch it: {:?}", d.shape);
    }

    #[test]
    fn geometry_that_does_not_fit_the_buffer_falls_back_to_unstructured() {
        let d = measure(&[1, 2, 3, 4], &[1, 2, 3, 5], Some(Geometry { width: 3, height: 3 }));
        assert_eq!(d.shape, Shape::Unstructured);
    }

    #[test]
    fn no_geometry_still_reports_magnitude() {
        let d = measure(&[0, 0, 0], &[1, 1, 1], None);
        assert!((d.max_abs - 1.0).abs() < f64::EPSILON);
        assert!((d.mean_abs - 1.0).abs() < f64::EPSILON);
        assert_eq!(d.shape, Shape::Unstructured);
    }

    #[test]
    fn largest_connected_component_matches_a_hand_built_grid() {
        let g = Geometry { width: 4, height: 4 };
        let raw = grid(g, &[(0, 0), (0, 1), (1, 0), (3, 3)]);
        let mask: Vec<bool> = raw.iter().map(|&b| b == 1).collect();
        assert_eq!(largest_connected_component(&mask, g), 3);
    }
}
