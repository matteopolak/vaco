//! Quantization parameters and matrix construction (`Vaco-Spec-Ref:
//! theora-spec-20170603 sections 6.4.2 and 6.4.3`).

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

/// A code needs `floor(log2(x)) + 1` bits to represent any value `0..=x`;
/// `ilog(0)` is `0` (the spec's own definition, used throughout the setup
/// header to size fields against how much room the largest possible value
/// needs).
const fn ilog(x: u32) -> u32 {
    32 - x.leading_zeros()
}

/// No table may declare more base matrices than this (section 6.4.2, step
/// 5's own `MUST` bound) — checked explicitly so `bms`'s length is trusted
/// before it is used to size anything.
const MAX_BASE_MATRICES: u32 = 384;

#[derive(Debug, Clone)]
pub(crate) struct QuantParams {
    acscale: [u32; 64],
    dcscale: [u32; 64],
    bms: Vec<[u8; 64]>,
    nqrs: [[u32; 3]; 2],
    qrsizes: [[[u32; 63]; 3]; 2],
    qrbmis: [[[u32; 64]; 3]; 2],
}

impl QuantParams {
    /// Section 6.4.2: the complete quantization-parameter decode.
    #[allow(
        clippy::integer_division,
        reason = "the spec's own qtj formula, (3*qti+pli-1)//3, is exact integer division on small non-negative operands, not a rounding shortcut"
    )]
    pub(crate) fn parse(r: &mut BitReader<'_>) -> Result<Self> {
        let mut acscale = [0u32; 64];
        let nbits = r.get(4) + 1;
        for v in &mut acscale {
            *v = r.get(nbits);
        }
        let mut dcscale = [0u32; 64];
        let nbits = r.get(4) + 1;
        for v in &mut dcscale {
            *v = r.get(nbits);
        }

        let nbms = r.get(9) + 1;
        if nbms > MAX_BASE_MATRICES {
            return Err(Error::InvalidData(
                "theora: setup header declares more than 384 base matrices",
            ));
        }
        let mut bms = Vec::new();
        bms.try_reserve_exact(nbms as usize).map_err(|_| {
            Error::InvalidData("theora: base matrix table allocation would be too large")
        })?;
        for _ in 0..nbms {
            let mut row = [0u8; 64];
            for slot in &mut row {
                *slot = u8::try_from(r.get(8)).unwrap_or(0);
            }
            bms.push(row);
        }

        let mut nqrs = [[0u32; 3]; 2];
        let mut qrsizes = [[[0u32; 63]; 3]; 2];
        let mut qrbmis = [[[0u32; 64]; 3]; 2];
        for qti in 0..2usize {
            for pli in 0..3usize {
                let newqr = if qti > 0 || pli > 0 { r.get(1) } else { 1 };
                if newqr == 0 {
                    let rpqr = if qti > 0 { r.get(1) } else { 0 };
                    let (qtj, plj) = if rpqr == 1 {
                        (qti.saturating_sub(1), pli)
                    } else {
                        (
                            (3 * qti + pli).saturating_sub(1) / 3,
                            (pli + 2).checked_rem(3).unwrap_or(0),
                        )
                    };
                    let copy_nqrs = nqrs.get(qtj).and_then(|p| p.get(plj)).copied().unwrap_or(0);
                    let copy_sizes = qrsizes
                        .get(qtj)
                        .and_then(|p| p.get(plj))
                        .copied()
                        .unwrap_or([0; 63]);
                    let copy_bmis = qrbmis
                        .get(qtj)
                        .and_then(|p| p.get(plj))
                        .copied()
                        .unwrap_or([0; 64]);
                    if let Some(slot) = nqrs.get_mut(qti).and_then(|p| p.get_mut(pli)) {
                        *slot = copy_nqrs;
                    }
                    if let Some(slot) = qrsizes.get_mut(qti).and_then(|p| p.get_mut(pli)) {
                        *slot = copy_sizes;
                    }
                    if let Some(slot) = qrbmis.get_mut(qti).and_then(|p| p.get_mut(pli)) {
                        *slot = copy_bmis;
                    }
                } else {
                    // Step C: the left endpoint of the first range is read
                    // once, before the loop; every subsequent bmi comes from
                    // step G below, as the right endpoint of the range that
                    // was just closed.
                    let bmi0 = r.get(ilog(nbms.saturating_sub(1)));
                    if bmi0 >= nbms {
                        return Err(Error::InvalidData(
                            "theora: quant range base matrix index out of range",
                        ));
                    }
                    let Some(first) = qrbmis
                        .get_mut(qti)
                        .and_then(|p| p.get_mut(pli))
                        .and_then(|row| row.first_mut())
                    else {
                        return Err(Error::InvalidData("theora: quant range table is empty"));
                    };
                    *first = bmi0;

                    let mut qri = 0usize;
                    let mut qi = 0u32;
                    loop {
                        let Some(size_slot) = qrsizes
                            .get_mut(qti)
                            .and_then(|p| p.get_mut(pli))
                            .and_then(|row| row.get_mut(qri))
                        else {
                            return Err(Error::InvalidData(
                                "theora: too many quant ranges in setup header",
                            ));
                        };
                        // Step D.
                        let size = r.get(ilog(62u32.saturating_sub(qi))) + 1;
                        *size_slot = size;
                        // Step E/F.
                        qi = qi.saturating_add(size);
                        qri += 1;
                        // Step G: right endpoint of the range just closed.
                        let Some(slot) = qrbmis
                            .get_mut(qti)
                            .and_then(|p| p.get_mut(pli))
                            .and_then(|row| row.get_mut(qri))
                        else {
                            return Err(Error::InvalidData(
                                "theora: too many quant ranges in setup header",
                            ));
                        };
                        let bmi = r.get(ilog(nbms.saturating_sub(1)));
                        if bmi >= nbms {
                            return Err(Error::InvalidData(
                                "theora: quant range base matrix index out of range",
                            ));
                        }
                        *slot = bmi;
                        // Step H/I/J.
                        if qi < 63 {
                            continue;
                        }
                        if qi > 63 {
                            return Err(Error::InvalidData("theora: quant ranges overshoot qi 63"));
                        }
                        break;
                    }
                    let final_qri = u32::try_from(qri).unwrap_or(0);
                    if let Some(slot) = nqrs.get_mut(qti).and_then(|p| p.get_mut(pli)) {
                        *slot = final_qri;
                    }
                }
            }
        }
        r.check()
            .map_err(|_| Error::InvalidData("theora: truncated quantization parameters"))?;

        Ok(Self {
            acscale,
            dcscale,
            bms,
            nqrs,
            qrsizes,
            qrbmis,
        })
    }

    /// Section 6.4.3: compute the 64-entry quantization matrix (natural
    /// order) for one `(qti, pli, qi)` triple.
    #[allow(
        clippy::too_many_lines,
        reason = "one procedure transcribed directly from the spec; splitting it would separate steps that share the same running state"
    )]
    #[allow(
        clippy::integer_division,
        reason = "the spec's own base-matrix interpolation and 100ths-of-a-pixel-value scale-down are both exact integer divisions on non-negative operands (section 6.4.3), not rounding shortcuts"
    )]
    pub(crate) fn matrix(&self, qti: usize, pli: usize, qi: u32) -> [i32; 64] {
        let nqrs = self
            .nqrs
            .get(qti)
            .and_then(|p| p.get(pli))
            .copied()
            .unwrap_or(0);
        let qrsizes = self
            .qrsizes
            .get(qti)
            .and_then(|p| p.get(pli))
            .copied()
            .unwrap_or([0; 63]);
        let qrbmis = self
            .qrbmis
            .get(qti)
            .and_then(|p| p.get(pli))
            .copied()
            .unwrap_or([0; 64]);

        // Find the quant range qi falls in, and its [QISTART, QIEND] bounds:
        // walk cumulative range sizes until qi sits at or before the running
        // end (section 6.4.3, step 1). If qi lies exactly on a boundary
        // between two ranges, either produces the same interpolated matrix
        // (the spec's own note), so taking the first match is fine.
        let mut qistart = 0u32;
        let mut qiend = 0u32;
        let mut qri = 0usize;
        let mut found = false;
        for (i, &size) in qrsizes.iter().take(nqrs as usize).enumerate() {
            let end = qistart.saturating_add(size);
            if qi <= end {
                qiend = end;
                qri = i;
                found = true;
                break;
            }
            qistart = end;
        }
        if !found {
            // Malformed setup header (ranges do not cover qi); fall back to
            // the last defined range rather than indexing past it.
            qri = (nqrs.saturating_sub(1)) as usize;
            qiend = qistart.saturating_add(qrsizes.get(qri).copied().unwrap_or(1));
        }
        let range_size = qiend.saturating_sub(qistart).max(1);
        let bmi = qrbmis.get(qri).copied().unwrap_or(0) as usize;
        let bmj = qrbmis.get(qri.saturating_add(1)).copied().unwrap_or(0) as usize;
        let bm_i = self.bms.get(bmi).copied().unwrap_or([0; 64]);
        let bm_j = self.bms.get(bmj).copied().unwrap_or([0; 64]);

        let mut out = [0i32; 64];
        for ci in 0..64usize {
            let a = i64::from(bm_i.get(ci).copied().unwrap_or(0));
            let b = i64::from(bm_j.get(ci).copied().unwrap_or(0));
            let bm = (2 * i64::from(qiend.saturating_sub(qi)) * a
                + 2 * i64::from(qi.saturating_sub(qistart)) * b
                + i64::from(range_size))
                / (2 * i64::from(range_size));
            let qmin: i64 = match (qti, ci) {
                (0, 0) => 16,
                (0, _) => 8,
                (_, 0) => 32,
                _ => 16,
            };
            let qscale = i64::from(if ci == 0 {
                self.dcscale.get(qi as usize).copied().unwrap_or(0)
            } else {
                self.acscale.get(qi as usize).copied().unwrap_or(0)
            });
            let scaled = (qscale * bm / 100) * 4;
            let clamped = qmin.max(scaled.min(4096));
            if let Some(slot) = out.get_mut(ci) {
                *slot = i32::try_from(clamped).unwrap_or(i32::MAX);
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn ilog_matches_the_spec_definition() {
        assert_eq!(ilog(0), 0);
        assert_eq!(ilog(1), 1);
        assert_eq!(ilog(2), 2);
        assert_eq!(ilog(3), 2);
        assert_eq!(ilog(4), 3);
        assert_eq!(ilog(383), 9);
    }
}
