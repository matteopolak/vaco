//! Wires `vaco-codec-dsp-mecmp`'s vectorised comparison kernels (D-12,
//! #144, #145/PF-3.9) through the harness: the scalar reference in that
//! crate against `MecmpKernels::select()`'s dispatched path, over the
//! vector-width tails and saturation boundaries `crate::edge` generates.
//! All four of `sad`/`ssd`/`variance`/`satd` are wired — `satd` joined the
//! other three once its vector path measured a genuine win (~1.29x on this
//! machine, see that crate's module doc) rather than the loss its 4x4
//! Hadamard shuffle was originally expected to be.

use vaco_codec_dsp_mecmp::{MecmpKernels, Plane};
use vaco_simd::KernelSet;

use crate::Kernel;
use crate::edge;

/// One comparison case: two same-shaped, tightly-packed (`stride == width`)
/// planes.
#[derive(Debug, Clone)]
pub struct MecmpCase {
    cur: Vec<u8>,
    refb: Vec<u8>,
    width: usize,
    height: usize,
}

impl MecmpCase {
    fn cur_plane(&self) -> Plane<'_> {
        Plane::new(&self.cur, self.width.max(1), self.width, self.height)
    }

    fn ref_plane(&self) -> Plane<'_> {
        Plane::new(&self.refb, self.width.max(1), self.width, self.height)
    }
}

/// Every case shape: widths that straddle every SIMD tier's tail (in
/// 1-byte `u8` lanes) crossed with a handful of representative heights,
/// each filled with several patterns designed to disagree under different
/// bugs — a uniform block would hide a tail bug the same way a flat image
/// hides a border rule.
fn cases() -> Vec<MecmpCase> {
    let widths = edge::lengths_around(&edge::element_widths(1));
    let heights = [1usize, 2, 3, 5, 8];
    let boundaries = edge::boundaries_u8();
    let mut cases = Vec::new();

    for &w in &widths {
        for &h in &heights {
            let len = w * h;
            let zero = vec![0u8; len];
            let max = vec![255u8; len];
            let ramp: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            let reverse_ramp: Vec<u8> = ramp.iter().rev().copied().collect();
            let alternating: Vec<u8> =
                (0..len).map(|i| if i % 2 == 0 { 0 } else { 255 }).collect();
            let boundary_tiled: Vec<u8> = (0..len)
                .map(|i| *boundaries.get(i % boundaries.len().max(1)).unwrap_or(&0))
                .collect();

            let patterns: [(&[u8], &[u8]); 5] = [
                (&zero, &max),
                (&max, &zero),
                (&ramp, &reverse_ramp),
                (&alternating, &boundary_tiled),
                (&ramp, &ramp), // identical: every metric must be exactly zero
            ];
            for (cur, refb) in patterns {
                cases.push(MecmpCase {
                    cur: cur.to_vec(),
                    refb: refb.to_vec(),
                    width: w,
                    height: h,
                });
            }
        }
    }
    cases
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_mecmp::sad`].
#[derive(Debug, Clone, Copy)]
pub struct SadKernel;

impl Kernel for SadKernel {
    const NAME: &'static str = "vaco-codec-dsp-mecmp::sad";
    type Case = MecmpCase;
    type Lane = u32;

    fn cases() -> Vec<Self::Case> {
        cases()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::reference().sad)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::select().sad)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_mecmp::ssd`].
#[derive(Debug, Clone, Copy)]
pub struct SsdKernel;

impl Kernel for SsdKernel {
    const NAME: &'static str = "vaco-codec-dsp-mecmp::ssd";
    type Case = MecmpCase;
    type Lane = u64;

    fn cases() -> Vec<Self::Case> {
        cases()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::reference().ssd)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::select().ssd)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_mecmp::variance`].
#[derive(Debug, Clone, Copy)]
pub struct VarianceKernel;

impl Kernel for VarianceKernel {
    const NAME: &'static str = "vaco-codec-dsp-mecmp::variance";
    type Case = MecmpCase;
    type Lane = u32;

    fn cases() -> Vec<Self::Case> {
        cases()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::reference().variance)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::select().variance)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_mecmp::satd`].
#[derive(Debug, Clone, Copy)]
pub struct SatdKernel;

impl Kernel for SatdKernel {
    const NAME: &'static str = "vaco-codec-dsp-mecmp::satd";
    type Case = MecmpCase;
    type Lane = u32;

    fn cases() -> Vec<Self::Case> {
        cases()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::reference().satd)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        vec![(MecmpKernels::select().satd)(
            case.cur_plane(),
            case.ref_plane(),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn sad_vector_agrees_with_scalar() {
        let report = Differential::<SadKernel>::run();
        assert!(report.cases_run() > 0);
        report.assert_clean();
    }

    #[test]
    fn ssd_vector_agrees_with_scalar() {
        let report = Differential::<SsdKernel>::run();
        assert!(report.cases_run() > 0);
        report.assert_clean();
    }

    #[test]
    fn variance_vector_agrees_with_scalar() {
        let report = Differential::<VarianceKernel>::run();
        assert!(report.cases_run() > 0);
        report.assert_clean();
    }

    #[test]
    fn satd_vector_agrees_with_scalar() {
        let report = Differential::<SatdKernel>::run();
        assert!(report.cases_run() > 0);
        report.assert_clean();
    }
}
