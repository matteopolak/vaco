//! The `Kernel` trait, the `Differential` runner, and mismatch reporting.
//!
//! A [`Kernel`] impl is the adapter between one optimised, runtime-dispatched
//! function (or [`vaco_simd::KernelSet`] field) and this crate: it names the
//! corpus, and it knows how to run both the scalar reference and the
//! optimised path over one case and flatten the result into comparable lanes.
//! [`Differential::run`] does the rest — walk the corpus, compare lane for
//! lane, and produce a [`Report`] that says exactly which case and which lane
//! diverged, not merely that something did.

use core::fmt;

/// One kernel under differential test.
///
/// Implement this once per kernel, not once per instruction-set tier: the
/// tier that actually runs [`Kernel::vector`] is whichever one
/// [`vaco_simd::Caps::detect`] resolves to on the machine running the test,
/// which is the point — dispatch itself is exercised, not simulated. Forcing
/// a *weaker* tier than the CPU actually has would need a capability token
/// fabricated without evidence (`Avx2::assume_supported` and its siblings,
/// all `unsafe`), which is closed to us by D2. So coverage of every tier
/// accumulates across the machines CI runs on, not within one process.
pub trait Kernel {
    /// Stable name, used in every report line and by the CLI's `list`/`verify`
    /// subcommands. Convention: `"<crate>::<field or fn>"`, e.g.
    /// `"vaco-scale::affine_row"`.
    const NAME: &'static str;

    /// One self-contained input. Owned rather than borrowed so a mismatch can
    /// be captured, printed and (by a human, from the report) replayed after
    /// the corpus itself has gone out of scope.
    type Case: Clone + fmt::Debug;

    /// One comparable output element. Multi-output kernels (e.g. three
    /// interleaved planes) flatten to one `Vec<Lane>` in [`Kernel::scalar`]
    /// and [`Kernel::vector`]; pick a flattening and use it in both, so a
    /// reported lane index means the same position in either.
    type Lane: fmt::Debug + Clone;

    /// The deterministic corpus. Always the same, no seed to lose — build it
    /// from [`crate::edge`] plus whatever domain knowledge the kernel needs
    /// (a valid coefficient matrix, a valid tap count, and so on).
    ///
    /// # Panics
    ///
    /// Implementations should not panic here; an empty corpus is preferable
    /// to a panicking generator, since [`Differential::run`] treats zero
    /// cases as zero evidence rather than as an error.
    #[must_use]
    fn cases() -> Vec<Self::Case>;

    /// Run the scalar reference on one case.
    #[must_use]
    fn scalar(case: &Self::Case) -> Vec<Self::Lane>;

    /// Run the optimised, runtime-dispatched implementation on the same case.
    #[must_use]
    fn vector(case: &Self::Case) -> Vec<Self::Lane>;

    /// Whether two output lanes agree.
    ///
    /// Defaults to bit-for-bit equality via `PartialEq`, which is the
    /// correct rule for every integer kernel (D6/D17: byte-exactness is the
    /// check, and for a kernel that is meant to be a pure reformulation of
    /// its own reference there is no daylight to allow). Override this for a
    /// float kernel that legitimately wants `NaN` to match `NaN`, and say in
    /// the override why that is the right rule for that kernel rather than a
    /// tolerance chosen to make a real divergence disappear.
    fn lanes_match(a: &Self::Lane, b: &Self::Lane) -> bool
    where
        Self::Lane: PartialEq,
    {
        a == b
    }
}

/// Where one case diverged.
#[derive(Debug, Clone)]
pub enum Divergence<L> {
    /// The scalar and vector paths produced different numbers of lanes —
    /// always a bug, since both are meant to compute the same function.
    LengthMismatch {
        /// Lanes the scalar reference produced.
        scalar_len: usize,
        /// Lanes the optimised implementation produced.
        vector_len: usize,
    },
    /// One lane disagreed.
    Lane {
        /// Index into the flattened output, per [`Kernel::Lane`]'s doc.
        lane: usize,
        /// What the scalar reference produced at this lane.
        scalar: L,
        /// What the optimised implementation produced at this lane.
        vector: L,
    },
}

/// One case that diverged, with enough context to reproduce it.
///
/// Debug and Clone are hand-written rather than derived: `#[derive]` on a
/// struct generic over `K` would bound the impls on `K: Debug + Clone`, which
/// says nothing about whether `K` (typically a zero-sized marker type) has
/// either — the bound that actually matters is on `K::Case`/`K::Lane`.
pub struct Mismatch<K: Kernel> {
    /// Index of the case in [`Kernel::cases`]'s corpus, for cross-referencing
    /// a rerun.
    pub case_index: usize,
    /// The input itself.
    pub case: K::Case,
    /// What went wrong.
    pub divergence: Divergence<K::Lane>,
}

impl<K: Kernel> fmt::Debug for Mismatch<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mismatch")
            .field("case_index", &self.case_index)
            .field("case", &self.case)
            .field("divergence", &self.divergence)
            .finish()
    }
}

impl<K: Kernel> Clone for Mismatch<K> {
    fn clone(&self) -> Self {
        Self {
            case_index: self.case_index,
            case: self.case.clone(),
            divergence: self.divergence.clone(),
        }
    }
}

/// How many mismatches a [`Report`] keeps in full. Beyond this the report
/// still counts every divergence (see [`Report::total_mismatches`]) but stops
/// cloning cases and lanes into memory — a kernel broken badly enough to
/// diverge on most of its corpus should not make the harness itself the slow
/// or memory-heavy part of the run.
const MAX_REPORTED: usize = 16;

/// The result of running a [`Kernel`]'s whole corpus once.
///
/// See [`Mismatch`]'s doc for why Debug is hand-written rather than derived.
pub struct Report<K: Kernel> {
    name: &'static str,
    cases_run: usize,
    total_mismatches: usize,
    mismatches: Vec<Mismatch<K>>,
}

impl<K: Kernel> fmt::Debug for Report<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Report")
            .field("name", &self.name)
            .field("cases_run", &self.cases_run)
            .field("total_mismatches", &self.total_mismatches)
            .field("mismatches", &self.mismatches)
            .finish()
    }
}

impl<K: Kernel> Report<K> {
    /// Whether every case agreed.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.total_mismatches == 0
    }

    /// How many cases [`Kernel::cases`] produced.
    #[must_use]
    pub fn cases_run(&self) -> usize {
        self.cases_run
    }

    /// How many individual divergences were found, which may exceed
    /// `mismatches().len()` — see [`MAX_REPORTED`].
    #[must_use]
    pub fn total_mismatches(&self) -> usize {
        self.total_mismatches
    }

    /// The first divergences found, each with the case and lane that produced
    /// it. Capped at [`MAX_REPORTED`]; [`Report::total_mismatches`] has the
    /// true count.
    #[must_use]
    pub fn mismatches(&self) -> &[Mismatch<K>] {
        &self.mismatches
    }

    /// Panics with the formatted report if any case diverged.
    ///
    /// # Panics
    ///
    /// Panics when [`Report::is_clean`] is `false`.
    pub fn assert_clean(&self) {
        assert!(self.is_clean(), "{self}");
    }
}

impl<K: Kernel> fmt::Display for Report<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            return writeln!(f, "{}: OK ({} cases)", self.name, self.cases_run);
        }
        writeln!(
            f,
            "{}: FAIL — {} mismatch(es) across {} case(s), {} shown",
            self.name,
            self.total_mismatches,
            self.cases_run,
            self.mismatches.len()
        )?;
        for m in &self.mismatches {
            match &m.divergence {
                Divergence::LengthMismatch {
                    scalar_len,
                    vector_len,
                } => writeln!(
                    f,
                    "  case {}: length mismatch — scalar produced {scalar_len} lanes, vector produced {vector_len}\n    input: {:?}",
                    m.case_index, m.case
                )?,
                Divergence::Lane {
                    lane,
                    scalar,
                    vector,
                } => writeln!(
                    f,
                    "  case {}: lane {lane} diverged — scalar={scalar:?} vector={vector:?}\n    input: {:?}",
                    m.case_index, m.case
                )?,
            }
        }
        if self.total_mismatches > self.mismatches.len() {
            writeln!(
                f,
                "  ... {} more mismatch(es) not shown",
                self.total_mismatches - self.mismatches.len()
            )?;
        }
        Ok(())
    }
}

/// Runs one [`Kernel`]'s corpus and builds its [`Report`].
///
/// Zero-sized; every method is an associated function reached through the
/// type parameter, so there is nothing to construct or store.
pub struct Differential<K> {
    _kernel: core::marker::PhantomData<fn() -> K>,
}

impl<K> fmt::Debug for Differential<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Differential").finish()
    }
}

impl<K: Kernel> Differential<K>
where
    K::Lane: PartialEq,
{
    /// Run every case in [`Kernel::cases`], comparing scalar against vector
    /// lane for lane with [`Kernel::lanes_match`].
    #[must_use]
    pub fn run() -> Report<K> {
        let cases = K::cases();
        let cases_run = cases.len();
        let mut total_mismatches = 0usize;
        let mut mismatches = Vec::new();

        for (case_index, case) in cases.into_iter().enumerate() {
            let scalar = K::scalar(&case);
            let vector = K::vector(&case);

            if scalar.len() != vector.len() {
                total_mismatches += 1;
                if mismatches.len() < MAX_REPORTED {
                    mismatches.push(Mismatch {
                        case_index,
                        case: case.clone(),
                        divergence: Divergence::LengthMismatch {
                            scalar_len: scalar.len(),
                            vector_len: vector.len(),
                        },
                    });
                }
                continue;
            }

            for (lane, (s, v)) in scalar.into_iter().zip(vector).enumerate() {
                if K::lanes_match(&s, &v) {
                    continue;
                }
                total_mismatches += 1;
                if mismatches.len() < MAX_REPORTED {
                    mismatches.push(Mismatch {
                        case_index,
                        case: case.clone(),
                        divergence: Divergence::Lane {
                            lane,
                            scalar: s,
                            vector: v,
                        },
                    });
                }
            }
        }

        Report {
            name: K::NAME,
            cases_run,
            total_mismatches,
            mismatches,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;

    /// A kernel that always agrees, to prove a clean run reports clean.
    struct AlwaysAgrees;

    impl Kernel for AlwaysAgrees {
        const NAME: &'static str = "test::always_agrees";
        type Case = i32;
        type Lane = i32;

        fn cases() -> Vec<Self::Case> {
            vec![-1, 0, 1, i32::MIN, i32::MAX]
        }

        fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
            vec![*case, case.wrapping_mul(2)]
        }

        fn vector(case: &Self::Case) -> Vec<Self::Lane> {
            vec![*case, case.wrapping_mul(2)]
        }
    }

    /// A kernel whose "vector" side has a seeded, single-value bug: it
    /// reports `0` instead of `i32::MIN` — a plausible saturating-clip bug —
    /// on exactly one case. This is the induced-mismatch proof: a harness
    /// that cannot catch this is not known to work.
    struct SaturationBug;

    impl Kernel for SaturationBug {
        const NAME: &'static str = "test::saturation_bug";
        type Case = i32;
        type Lane = i32;

        fn cases() -> Vec<Self::Case> {
            crate::edge::boundaries_i32()
        }

        fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
            vec![*case]
        }

        fn vector(case: &Self::Case) -> Vec<Self::Lane> {
            // The seeded bug: everything agrees except the very boundary a
            // real saturating-clip off-by-one would miss.
            if *case == i32::MIN {
                vec![0]
            } else {
                vec![*case]
            }
        }
    }

    /// A kernel whose two sides disagree on length — the other divergence
    /// shape `Differential` must catch.
    struct LengthBug;

    impl Kernel for LengthBug {
        const NAME: &'static str = "test::length_bug";
        type Case = usize;
        type Lane = u8;

        fn cases() -> Vec<Self::Case> {
            vec![0, 1, 5]
        }

        fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
            vec![0u8; *case]
        }

        fn vector(case: &Self::Case) -> Vec<Self::Lane> {
            // Off-by-one tail bug: drops the last lane whenever there is one.
            vec![0u8; case.saturating_sub(1)]
        }
    }

    #[test]
    fn a_clean_kernel_reports_clean() {
        let report = Differential::<AlwaysAgrees>::run();
        assert!(report.is_clean());
        assert_eq!(report.total_mismatches(), 0);
        assert_eq!(report.cases_run(), 5);
        report.assert_clean();
    }

    #[test]
    fn the_harness_catches_an_induced_saturation_mismatch() {
        let report = Differential::<SaturationBug>::run();
        assert!(!report.is_clean(), "seeded bug must be caught");
        assert_eq!(report.total_mismatches(), 1);
        let m = report.mismatches().first().expect("one mismatch recorded");
        assert_eq!(m.case, i32::MIN, "must name the exact input that diverged");
        match &m.divergence {
            Divergence::Lane {
                lane,
                scalar,
                vector,
            } => {
                assert_eq!(*lane, 0, "must name the diverging lane");
                assert_eq!(*scalar, i32::MIN);
                assert_eq!(*vector, 0);
            }
            other @ Divergence::LengthMismatch { .. } => {
                panic!("expected a lane divergence, got {other:?}")
            }
        }
        // Every other boundary must still agree — the report should not cry
        // wolf on inputs the seeded bug does not touch.
        assert!(
            crate::edge::boundaries_i32()
                .iter()
                .filter(|&&c| c != i32::MIN)
                .all(|c| SaturationBug::scalar(c) == SaturationBug::vector(c))
        );
    }

    #[test]
    fn the_harness_catches_an_induced_length_mismatch() {
        let report = Differential::<LengthBug>::run();
        assert!(!report.is_clean());
        // cases 1 and 5 both trigger the off-by-one; case 0 (empty either way) does not.
        assert_eq!(report.total_mismatches(), 2);
        assert!(
            report
                .mismatches()
                .iter()
                .any(|m| matches!(m.divergence, Divergence::LengthMismatch { .. }))
        );
    }

    #[test]
    fn display_names_the_case_and_lane_not_just_that_something_failed() {
        let report = Differential::<SaturationBug>::run();
        let text = report.to_string();
        assert!(text.contains("lane 0"));
        assert!(text.contains("-2147483648"), "must show the actual input");
        assert!(text.contains("scalar=-2147483648"));
        assert!(text.contains("vector=0"));
    }

    #[test]
    fn a_kernel_with_no_cases_is_reported_clean_not_erroneous() {
        struct Empty;
        impl Kernel for Empty {
            const NAME: &'static str = "test::empty";
            type Case = ();
            type Lane = ();
            fn cases() -> Vec<Self::Case> {
                Vec::new()
            }
            fn scalar(_case: &Self::Case) -> Vec<Self::Lane> {
                Vec::new()
            }
            fn vector(_case: &Self::Case) -> Vec<Self::Lane> {
                Vec::new()
            }
        }
        let report = Differential::<Empty>::run();
        assert!(report.is_clean());
        assert_eq!(report.cases_run(), 0);
    }
}
