#![forbid(unsafe_code)]
//! Motion-estimation search patterns: full search, diamond search, and
//! three-step search.
//!
//! D-13 (#260): given a current-frame block and a candidate reference
//! plane, find the displacement that minimises a cost from
//! [`vaco_codec_dsp_mecmp`] (D-12) over some search radius. This crate
//! decides *where* to look; `vaco-codec-dsp-mecmp` decides *how good* a
//! candidate is — kept as two crates because D-13 is written entirely
//! against that crate's public API, never against a specific cost
//! function's internals.
//!
//! # The three patterns
//!
//! | Pattern | Cost | When it is the right choice |
//! |---|---|---|
//! | [`Searcher::full_search`] | O(range²) evaluations | ground truth for testing the other two, or a small range where exhaustive is cheap enough to just do |
//! | [`Searcher::diamond_search`] | typically a small constant multiple of the number of steps to the optimum | real-time encoding; this is the standard choice for a general-purpose fast search |
//! | [`Searcher::three_step_search`] | `O(log(range))` rounds of 8 evaluations | a coarser, faster search when diamond's per-step cost is not affordable |
//!
//! Diamond search here is the classical Large-Diamond/Small-Diamond
//! pattern (LDSP/SDSP): repeatedly probe an 8-point diamond of radius 2
//! around the current best, recentre on any improvement, and finish with a
//! 4-point radius-1 diamond once the large pattern stops improving.
//! Three-step search probes a 3×3 grid (minus the already-known centre) at
//! a halving step size. Both are textbook block-matching algorithms
//! (Tham/Ranganath 1998 for diamond search; Koga et al. 1981 for
//! three-step search) — academic literature on motion estimation, not
//! anything read from a specific implementation.
//!
//! None of the three is normative: an encoder is free to pick any
//! displacement it likes, so there is no bit-exactness question here, only
//! "does it find something at least as good as where it started, and does
//! the fast pattern get close to what full search finds." See
//! `tests::full_search_and_diamond_search_find_the_true_shift` for
//! how that is actually checked — full search is the independent oracle, on
//! synthetic content constructed to have a single, unambiguous best match.
//!
//! # Untrusted-input posture
//!
//! Every candidate offset is bounds-checked through
//! [`vaco_codec_dsp_mecmp::Plane::sub`] before any cost is computed; an
//! out-of-bounds candidate is simply skipped, never a panic. A block whose
//! own origin is out of bounds degrades to [`SearchResult::cost`] of
//! [`u32::MAX`] at the starting vector rather than failing outright, so a
//! caller can still detect "nothing here was evaluable" from the result
//! rather than from an `Option`/`Result` layer this crate does not need
//! anywhere else.

use vaco_codec_dsp_mecmp::{MecmpKernels, Plane};
use vaco_simd::KernelSet;

/// A displacement, in whole samples, from a block's own position to a
/// candidate position in the reference plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Displacement {
    /// Horizontal displacement.
    pub x: i32,
    /// Vertical displacement.
    pub y: i32,
}

impl Displacement {
    /// No displacement.
    pub const ZERO: Self = Self { x: 0, y: 0 };

    /// A new vector `dx`/`dy` samples away from this one, saturating rather
    /// than overflowing — a search never legitimately needs a vector
    /// outside `i32`'s range, and saturating keeps every step of every
    /// pattern panic-free regardless of a pathological `range`.
    #[must_use]
    fn offset(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
        }
    }

    /// Squared Euclidean length, in `i64` so no `i32` vector's square can
    /// overflow. Used only to break exact cost ties toward the smaller
    /// (cheaper-to-code) vector — never to rank distinct costs.
    fn magnitude_sq(self) -> i64 {
        let x = i64::from(self.x);
        let y = i64::from(self.y);
        x * x + y * y
    }

    /// Whether this vector is within `range` of `origin` in both axes —
    /// the Chebyshev (square) search window every pattern here respects,
    /// matching how `range` is documented on [`SearchConfig`].
    fn within(self, origin: Self, range: u32) -> bool {
        let range = i64::from(range);
        (i64::from(self.x) - i64::from(origin.x)).abs() <= range
            && (i64::from(self.y) - i64::from(origin.y)).abs() <= range
    }
}

/// Which [`vaco_codec_dsp_mecmp`] cost function ranks candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Sum of absolute differences — cheapest, used by default.
    Sad,
    /// Hadamard-domain cost — several times more expensive; a refinement
    /// metric, not usually the one a wide search runs at every candidate.
    Satd,
}

/// One search's parameters.
#[derive(Debug, Clone, Copy)]
pub struct SearchConfig {
    /// Which cost ranks candidates.
    pub metric: Metric,
    /// How far from the search's starting vector a candidate may be, in
    /// each axis independently (a square window, not a circular one).
    pub range: u32,
}

/// A search's outcome: the best vector found, and its cost. A vector's own
/// [`Default`] cost is meaningless on its own — always look at `cost`
/// alongside `mv`, since [`u32::MAX`] means "nothing evaluable was found",
/// not "a real cost of `u32::MAX`".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchResult {
    /// The best displacement found.
    pub mv: Displacement,
    /// Its cost under the search's configured [`Metric`]. [`u32::MAX`]
    /// means no evaluable candidate was found at all — every offset this
    /// search tried was out of the reference plane's bounds.
    pub cost: u32,
}

impl SearchResult {
    const NONE: Self = Self {
        mv: Displacement::ZERO,
        cost: u32::MAX,
    };
}

/// One block's fixed position and size in `cur`'s coordinate space, shared
/// by every candidate a search evaluates.
#[derive(Debug, Clone, Copy)]
pub struct BlockOrigin {
    /// Column of the block's top-left sample.
    pub x: usize,
    /// Row of the block's top-left sample.
    pub y: usize,
    /// Block width.
    pub width: usize,
    /// Block height.
    pub height: usize,
}

/// Runs [`SearchConfig`]-driven searches with one resolved
/// [`MecmpKernels`] table, so the scalar-vs-vector choice is made once per
/// [`Searcher`] rather than once per candidate.
#[derive(Debug, Clone, Copy)]
pub struct Searcher {
    kernels: MecmpKernels,
}

impl Searcher {
    /// A searcher using the runtime-detected best `vaco-simd` tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            kernels: MecmpKernels::select(),
        }
    }

    /// A searcher over an explicitly chosen kernel table — the scalar
    /// reference, for a deterministic test, or a specific tier.
    #[must_use]
    pub fn with_kernels(kernels: MecmpKernels) -> Self {
        Self { kernels }
    }

    fn cost(&self, metric: Metric, a: Plane<'_>, b: Plane<'_>) -> u32 {
        match metric {
            Metric::Sad => (self.kernels.sad)(a, b),
            Metric::Satd => (self.kernels.satd)(a, b),
        }
    }

    /// The cost of one candidate vector, or `None` if the resulting
    /// reference block would read outside `refp`'s bounds. This is the one
    /// place bounds-checking happens; every search pattern below routes
    /// through it.
    fn candidate_cost(
        &self,
        cur: Plane<'_>,
        refp: Plane<'_>,
        block: BlockOrigin,
        metric: Metric,
        mv: Displacement,
    ) -> Option<u32> {
        let rx = block.x.checked_add_signed(isize::try_from(mv.x).ok()?)?;
        let ry = block.y.checked_add_signed(isize::try_from(mv.y).ok()?)?;
        let curb = cur.sub(block.x, block.y, block.width, block.height)?;
        let refb = refp.sub(rx, ry, block.width, block.height)?;
        Some(self.cost(metric, curb, refb))
    }

    /// Considers `mv` as a candidate, updating `best` if it evaluates and
    /// beats (or exactly ties, toward the smaller vector) the current best.
    /// Returns whether `best` changed, which every iterative pattern below
    /// uses as its improvement signal.
    fn consider(
        &self,
        cur: Plane<'_>,
        refp: Plane<'_>,
        block: BlockOrigin,
        metric: Metric,
        mv: Displacement,
        best: &mut SearchResult,
    ) -> bool {
        let Some(cost) = self.candidate_cost(cur, refp, block, metric, mv) else {
            return false;
        };
        let better =
            cost < best.cost || (cost == best.cost && mv.magnitude_sq() < best.mv.magnitude_sq());
        if better {
            *best = SearchResult { mv, cost };
        }
        better
    }

    /// Exhaustively evaluates every vector within `cfg.range` of `start`
    /// (a `(2·range+1)²` grid) and returns the cheapest. The ground truth
    /// every faster pattern is measured against — see the crate doc.
    #[must_use]
    pub fn full_search(
        &self,
        cur: Plane<'_>,
        refp: Plane<'_>,
        block: BlockOrigin,
        cfg: &SearchConfig,
        start: Displacement,
    ) -> SearchResult {
        let mut best = SearchResult::NONE;
        self.consider(cur, refp, block, cfg.metric, start, &mut best);
        let range = i32::try_from(cfg.range).unwrap_or(i32::MAX);
        for dy in -range..=range {
            for dx in -range..=range {
                if dx == 0 && dy == 0 {
                    continue; // `start` itself, already evaluated above
                }
                self.consider(
                    cur,
                    refp,
                    block,
                    cfg.metric,
                    start.offset(dx, dy),
                    &mut best,
                );
            }
        }
        best
    }

    /// Large-diamond/small-diamond search (LDSP/SDSP): probe an 8-point
    /// radius-2 diamond around the current best, recentre on any
    /// improvement, and finish with a 4-point radius-1 diamond once the
    /// large pattern stops improving. Bounded to at most
    /// `2 * range + 8` large-diamond rounds so a search always terminates
    /// even if a future change to the improvement rule stopped guaranteeing
    /// forward progress on its own.
    #[must_use]
    pub fn diamond_search(
        &self,
        cur: Plane<'_>,
        refp: Plane<'_>,
        block: BlockOrigin,
        cfg: &SearchConfig,
        start: Displacement,
    ) -> SearchResult {
        const LARGE_DIAMOND: [(i32, i32); 8] = [
            (0, -2),
            (0, 2),
            (-2, 0),
            (2, 0),
            (-1, -1),
            (-1, 1),
            (1, -1),
            (1, 1),
        ];
        const SMALL_DIAMOND: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];

        let mut best = SearchResult::NONE;
        self.consider(cur, refp, block, cfg.metric, start, &mut best);

        let mut center = start;
        let max_rounds = (cfg.range as usize).saturating_mul(2).saturating_add(8);
        for _ in 0..max_rounds {
            let mut moved = false;
            for &(dx, dy) in &LARGE_DIAMOND {
                let mv = center.offset(dx, dy);
                if !mv.within(start, cfg.range) {
                    continue;
                }
                if self.consider(cur, refp, block, cfg.metric, mv, &mut best) {
                    moved = true;
                }
            }
            if !moved {
                break;
            }
            center = best.mv;
        }

        for &(dx, dy) in &SMALL_DIAMOND {
            let mv = center.offset(dx, dy);
            if mv.within(start, cfg.range) {
                self.consider(cur, refp, block, cfg.metric, mv, &mut best);
            }
        }
        best
    }

    /// Three-step search (TSS): probe a 3×3 grid at a halving step size
    /// (starting at the largest power of two not exceeding `range`),
    /// recentring on the best point found at each step.
    #[must_use]
    pub fn three_step_search(
        &self,
        cur: Plane<'_>,
        refp: Plane<'_>,
        block: BlockOrigin,
        cfg: &SearchConfig,
        start: Displacement,
    ) -> SearchResult {
        let mut best = SearchResult::NONE;
        self.consider(cur, refp, block, cfg.metric, start, &mut best);

        let range = i32::try_from(cfg.range).unwrap_or(i32::MAX);
        if range < 1 {
            return best;
        }
        let mut step: i32 = 1;
        while step.saturating_mul(2) <= range {
            step = step.saturating_mul(2);
        }

        let mut center = start;
        while step >= 1 {
            for &dy in &[-step, 0, step] {
                for &dx in &[-step, 0, step] {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let mv = center.offset(dx, dy);
                    if mv.within(start, cfg.range) {
                        self.consider(cur, refp, block, cfg.metric, mv, &mut best);
                    }
                }
            }
            center = best.mv;
            if step == 1 {
                break;
            }
            step = step.checked_div(2).unwrap_or(1);
        }
        best
    }
}

impl Default for Searcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code: a panic is the assertion mechanism, and fixture generation has no untrusted denominator"
)]
mod tests {
    use super::*;

    /// A textured reference plane large enough to search in, and a current
    /// block that is an exact copy of one window of it — so the true best
    /// match is the vector that recovers the offset between them.
    ///
    /// The texture is an anisotropic quadratic "bowl" centred on the plane:
    /// `255 - (3·dx² + 5·dy²)/40`, clamped to `0..=255`. Squared distance
    /// (rather than, say, an L1 pyramid) makes the surface strictly convex
    /// with no flat direction to plateau along, and the unequal `3`/`5`
    /// axis weights break the residual symmetry a plain circular bowl would
    /// have (two different offsets landing on the same radius would
    /// otherwise tie). Both properties matter: a flat direction or a
    /// symmetric tie would let a search "find the true shift" by landing on
    /// a *different*, equally-good point, which would validate nothing
    /// about its actual descent behaviour (`AGENT-CONSTRAINTS.md`'s point
    /// about an input that cannot separate two candidate answers).
    fn textured_plane(w: usize, h: usize) -> Vec<u8> {
        let cx = i64::try_from(w / 2).unwrap_or(0);
        let cy = i64::try_from(h / 2).unwrap_or(0);
        (0..w * h)
            .map(|i| {
                let x = i64::try_from(i % w).unwrap_or(0);
                let y = i64::try_from(i / w).unwrap_or(0);
                let dx = x - cx;
                let dy = y - cy;
                let bowl = (3 * dx * dx + 5 * dy * dy) / 40;
                u8::try_from((255 - bowl).clamp(0, 255)).unwrap_or(0)
            })
            .collect()
    }

    struct Fixture {
        refbuf: Vec<u8>,
        ref_w: usize,
        ref_h: usize,
        curbuf: Vec<u8>,
        block: BlockOrigin,
    }

    /// Builds a fixture where the true best match for the block at
    /// `(bx, by)` in the current frame is exactly `(dx, dy)` in the
    /// reference frame.
    fn fixture(bx: usize, by: usize, bw: usize, bh: usize, dx: i32, dy: i32) -> Fixture {
        let ref_w = 64;
        let ref_h = 64;
        let refbuf = textured_plane(ref_w, ref_h);
        let ref_plane = Plane::new(&refbuf, ref_w, ref_w, ref_h);

        let rx = bx
            .checked_add_signed(isize::try_from(dx).unwrap_or(0))
            .unwrap_or(0);
        let ry = by
            .checked_add_signed(isize::try_from(dy).unwrap_or(0))
            .unwrap_or(0);
        let src = ref_plane
            .sub(rx, ry, bw, bh)
            .expect("fixture parameters must stay in-bounds");

        // The current frame is otherwise blank; only the block itself
        // matters, since every search here only ever reads that block.
        let mut curbuf = vec![0u8; ref_w * ref_h];
        for row in 0..bh {
            let srow = src.row(row);
            let dst_start = (by + row) * ref_w + bx;
            if let Some(dst) = curbuf.get_mut(dst_start..dst_start + bw) {
                let n = dst.len().min(srow.len());
                if let (Some(d), Some(s)) = (dst.get_mut(..n), srow.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
        }

        Fixture {
            refbuf,
            ref_w,
            ref_h,
            curbuf,
            block: BlockOrigin {
                x: bx,
                y: by,
                width: bw,
                height: bh,
            },
        }
    }

    #[test]
    fn full_search_finds_the_exact_known_shift_at_zero_cost() {
        let f = fixture(20, 20, 8, 8, 3, -2);
        let cur = Plane::new(&f.curbuf, f.ref_w, f.ref_w, f.ref_h);
        let refp = Plane::new(&f.refbuf, f.ref_w, f.ref_w, f.ref_h);
        let searcher = Searcher::with_kernels(MecmpKernels::reference());
        let cfg = SearchConfig {
            metric: Metric::Sad,
            range: 8,
        };
        let result = searcher.full_search(cur, refp, f.block, &cfg, Displacement::ZERO);
        assert_eq!(result.cost, 0);
        assert_eq!(result.mv, Displacement { x: 3, y: -2 });
    }

    #[test]
    fn full_search_and_diamond_search_find_the_true_shift() {
        // Diamond search's radius-2/radius-1 diamonds are fine-grained
        // enough to reliably descend to the true optimum on a convex
        // surface. Three-step search is checked separately, and more
        // weakly, below — see that test's doc for why.
        for &(dx, dy) in &[(0, 0), (5, 5), (-6, 3), (8, -8), (1, -7)] {
            let f = fixture(24, 24, 8, 8, dx, dy);
            let cur = Plane::new(&f.curbuf, f.ref_w, f.ref_w, f.ref_h);
            let refp = Plane::new(&f.refbuf, f.ref_w, f.ref_w, f.ref_h);
            let searcher = Searcher::with_kernels(MecmpKernels::reference());
            let cfg = SearchConfig {
                metric: Metric::Sad,
                range: 8,
            };

            let full = searcher.full_search(cur, refp, f.block, &cfg, Displacement::ZERO);
            let diamond = searcher.diamond_search(cur, refp, f.block, &cfg, Displacement::ZERO);

            assert_eq!(
                full.cost, 0,
                "full search must find the exact shift ({dx},{dy})"
            );
            assert_eq!(
                diamond.cost, 0,
                "diamond search must recover the true shift ({dx},{dy}), found {:?}",
                diamond.mv
            );
            assert_eq!(diamond.mv, full.mv);
        }
    }

    #[test]
    fn three_step_search_finds_shifts_that_land_on_its_coarse_grid() {
        // TSS's first round only ever probes points at ±(a power of two) in
        // each axis, and every later round is a halving of that — so a true
        // shift outside the sequence of grids the algorithm actually visits
        // (e.g. (1, -7) under range 8: TSS commits to an x near 0 or ±8 at
        // step 8 long before a step-1 round could ever explore x=1) is a
        // documented, real limitation of the algorithm, not a bug in this
        // implementation. This is exactly why D-13's search patterns are a
        // *family*: diamond search exists for the cases TSS's coarse
        // commitment misses.
        for &(dx, dy) in &[(0, 0), (8, 0), (0, -8), (4, -4)] {
            let f = fixture(24, 24, 8, 8, dx, dy);
            let cur = Plane::new(&f.curbuf, f.ref_w, f.ref_w, f.ref_h);
            let refp = Plane::new(&f.refbuf, f.ref_w, f.ref_w, f.ref_h);
            let searcher = Searcher::with_kernels(MecmpKernels::reference());
            let cfg = SearchConfig {
                metric: Metric::Sad,
                range: 8,
            };
            let tss = searcher.three_step_search(cur, refp, f.block, &cfg, Displacement::ZERO);
            assert_eq!(
                tss.cost, 0,
                "three-step search must recover the true shift ({dx},{dy}), found {:?}",
                tss.mv
            );
            assert_eq!(tss.mv, Displacement { x: dx, y: dy });
        }
    }

    #[test]
    fn a_search_never_returns_a_result_worse_than_its_own_starting_vector() {
        let f = fixture(24, 24, 8, 8, 4, 4);
        let cur = Plane::new(&f.curbuf, f.ref_w, f.ref_w, f.ref_h);
        let refp = Plane::new(&f.refbuf, f.ref_w, f.ref_w, f.ref_h);
        let searcher = Searcher::with_kernels(MecmpKernels::reference());
        let cfg = SearchConfig {
            metric: Metric::Sad,
            range: 8,
        };
        let start_cost =
            searcher.candidate_cost(cur, refp, f.block, cfg.metric, Displacement::ZERO);
        for search in [
            Searcher::diamond_search,
            Searcher::three_step_search,
            Searcher::full_search,
        ] {
            let result = search(&searcher, cur, refp, f.block, &cfg, Displacement::ZERO);
            if let Some(start_cost) = start_cost {
                assert!(result.cost <= start_cost);
            }
        }
    }

    #[test]
    fn out_of_bounds_candidates_are_skipped_not_panicking() {
        // A block flush against the reference plane's top-left corner: half
        // of every search pattern's candidates are out of bounds.
        let f = fixture(0, 0, 8, 8, 0, 0);
        let cur = Plane::new(&f.curbuf, f.ref_w, f.ref_w, f.ref_h);
        let refp = Plane::new(&f.refbuf, f.ref_w, f.ref_w, f.ref_h);
        let searcher = Searcher::with_kernels(MecmpKernels::reference());
        let cfg = SearchConfig {
            metric: Metric::Sad,
            range: 8,
        };
        let result = searcher.diamond_search(cur, refp, f.block, &cfg, Displacement::ZERO);
        assert_eq!(result.cost, 0);
        assert_eq!(result.mv, Displacement::ZERO);
    }

    #[test]
    fn zero_range_only_ever_returns_the_starting_vector() {
        let f = fixture(20, 20, 8, 8, 0, 0);
        let cur = Plane::new(&f.curbuf, f.ref_w, f.ref_w, f.ref_h);
        let refp = Plane::new(&f.refbuf, f.ref_w, f.ref_w, f.ref_h);
        let searcher = Searcher::with_kernels(MecmpKernels::reference());
        let cfg = SearchConfig {
            metric: Metric::Sad,
            range: 0,
        };
        for search in [
            Searcher::diamond_search,
            Searcher::three_step_search,
            Searcher::full_search,
        ] {
            let result = search(&searcher, cur, refp, f.block, &cfg, Displacement::ZERO);
            assert_eq!(result.mv, Displacement::ZERO);
        }
    }

    #[test]
    fn satd_metric_also_recovers_an_exact_shift() {
        // An exact pixel match has an all-zero residual under any metric,
        // so SATD (Hadamard of the residual) is exactly 0 at the true shift
        // too. Checked with full_search, the reliable oracle: SATD's cost
        // surface is not the SAD's simple bowl (it is a transform of the
        // residual, not the residual itself), so a greedy pattern is not
        // guaranteed to reach the exact optimum under it from an arbitrary
        // start -- this test is about the `Metric` wiring being correct,
        // not about diamond/TSS's behaviour under SATD specifically.
        let f = fixture(16, 16, 8, 8, -2, 3);
        let cur = Plane::new(&f.curbuf, f.ref_w, f.ref_w, f.ref_h);
        let refp = Plane::new(&f.refbuf, f.ref_w, f.ref_w, f.ref_h);
        let searcher = Searcher::with_kernels(MecmpKernels::reference());
        let cfg = SearchConfig {
            metric: Metric::Satd,
            range: 8,
        };
        let result = searcher.full_search(cur, refp, f.block, &cfg, Displacement::ZERO);
        assert_eq!(result.cost, 0);
        assert_eq!(result.mv, Displacement { x: -2, y: 3 });
    }
}
