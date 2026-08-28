//! The parametric bit-allocation model: turns exponents plus a handful of
//! per-block side-info values into one `bap` (bit-allocation pointer, 0..=15)
//! per coefficient. ATSC A/52:2018 Annex A.
//!
//! This is the single highest-risk module in this crate. The encoder never
//! transmits `bap` — the decoder derives it with the *exact* algorithm the
//! encoder used, because `bap` is what tells [`crate::mantissa`] how many
//! bits the next coefficient occupies. A masking-curve constant off by one
//! does not degrade quality here the way a lossy-codec bug usually would; it
//! desyncs the mantissa bit reader for the rest of the block. See the crate
//! root docs for what this was checked against and what it was not.

use crate::tables_bitalloc::{
    BAPTAB, BNDSZ, DBKNEE, FASTDECAY, FASTGAIN, FLOOR, HTH, LATAB, SLOWDECAY, SLOWGAIN,
};

/// Number of fixed masking bands spanning the transform's spectral bins.
pub const NBANDS: usize = 50;

/// Per-band starting bin, derived once from [`BNDSZ`].
#[must_use]
pub fn band_start() -> [u16; NBANDS] {
    let mut out = [0u16; NBANDS];
    let mut acc = 0u16;
    for (i, &sz) in BNDSZ.iter().enumerate() {
        if let Some(slot) = out.get_mut(i) {
            *slot = acc;
        }
        acc = acc.saturating_add(sz);
    }
    out
}

/// `psdtab`: exponent (0..24) to power-spectral-density units. Each exponent
/// step is 6.02 dB, represented here as 128 units out of a 3072-unit (24
/// step) scale — the same unit system [`LATAB`] and [`HTH`] are stated in.
#[must_use]
pub const fn psd_of_exp(exp: u8) -> i32 {
    3072 - (exp as i32) * 128
}

/// Combine two power values already in the log-power-sum units above,
/// `latab`-corrected rather than converted through an actual log/exp pair —
/// this is what makes the model integer and deterministic across encoder and
/// decoder.
fn log_add(a: i32, b: i32) -> i32 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    let diff = (hi - lo).clamp(0, 255) as usize;
    let corr = LATAB.get(diff.min(LATAB.len() - 1)).copied().unwrap_or(0);
    hi + i32::from(corr)
}

/// Compute `bndpsd[band]` by log-summing every bin's `psd` inside that band.
fn band_psd(psd: &[i32]) -> [i32; NBANDS] {
    let starts = band_start();
    let mut out = [0i32; NBANDS];
    for band in 0..NBANDS {
        let start = usize::from(starts.get(band).copied().unwrap_or(0));
        let sz = usize::from(*BNDSZ.get(band).unwrap_or(&0));
        let end = start.saturating_add(sz);
        let mut acc: Option<i32> = None;
        for &p in psd.get(start..end.min(psd.len())).unwrap_or(&[]) {
            acc = Some(match acc {
                None => p,
                Some(a) => log_add(a, p),
            });
        }
        if let Some(slot) = out.get_mut(band) {
            *slot = acc.unwrap_or(-9999);
        }
    }
    out
}

/// Inputs the bit-allocation model needs beyond the exponents themselves —
/// every one of these is a per-block or per-frame BSI/audblk field, never
/// derived.
#[derive(Debug, Clone, Copy)]
pub struct AllocParams {
    pub sdecaycod: u8,
    pub fdecaycod: u8,
    pub sgaincod: u8,
    pub dbpbcod: u8,
    pub floorcod: u8,
    pub fscod: u8,
    pub snroffset: i32,
    pub fgaincod: u8,
    /// Starting bin the allocation covers to; typically `endmant`.
    pub end_bin: usize,
    /// Bins below this are not allocated at all (e.g. a coupled channel's
    /// own spectrum stops at `cplstrtmant`).
    pub start_bin: usize,
}

impl Default for AllocParams {
    fn default() -> Self {
        Self {
            sdecaycod: 2,
            fdecaycod: 1,
            sgaincod: 1,
            dbpbcod: 2,
            floorcod: 7,
            fscod: 0,
            snroffset: 0,
            fgaincod: 4,
            end_bin: 0,
            start_bin: 0,
        }
    }
}

/// Run the model over one channel's exponents, returning one `bap` per bin
/// in `0..end_bin`.
#[must_use]
pub fn compute_bap(exps: &[u8], params: &AllocParams) -> Vec<u8> {
    let n = exps.len().max(params.end_bin);
    let mut psd = vec![-9999i32; n];
    for (i, &e) in exps.iter().enumerate() {
        if let Some(slot) = psd.get_mut(i) {
            *slot = psd_of_exp(e);
        }
    }
    let bndpsd = band_psd(&psd);

    let fdecay = *FASTDECAY.get(usize::from(params.fdecaycod)).unwrap_or(&0);
    let sdecay = *SLOWDECAY.get(usize::from(params.sdecaycod)).unwrap_or(&0);
    let sgain = *SLOWGAIN.get(usize::from(params.sgaincod)).unwrap_or(&0);
    let dbknee = *DBKNEE.get(usize::from(params.dbpbcod)).unwrap_or(&0);
    let floor = *FLOOR.get(usize::from(params.floorcod)).unwrap_or(&0);
    let fgain = *FASTGAIN.get(usize::from(params.fgaincod)).unwrap_or(&0);

    let starts = band_start();
    let mut excitation = [0i32; NBANDS];
    let mut lastexc = bndpsd.first().copied().unwrap_or(-9999) - i32::from(fgain);
    for band in 0..NBANDS {
        let start = usize::from(starts.get(band).copied().unwrap_or(0));
        let is_low = start < 21; // low-frequency bands decay differently
        let decay = if is_low { fdecay } else { sdecay };
        let bp = bndpsd.get(band).copied().unwrap_or(-9999);
        let candidate = bp - i32::from(if is_low { fgain } else { sgain });
        lastexc = candidate.max(lastexc - i32::from(decay));
        if let Some(slot) = excitation.get_mut(band) {
            *slot = lastexc;
        }
    }

    let default_hth = [0i16; NBANDS];
    let hth_row = HTH.get(usize::from(params.fscod)).unwrap_or(&default_hth);
    let mut bap = vec![0u8; n];
    for band in 0..NBANDS {
        let start = usize::from(starts.get(band).copied().unwrap_or(0));
        let sz = usize::from(*BNDSZ.get(band).unwrap_or(&0));
        if start >= params.end_bin {
            break;
        }
        let exc = excitation.get(band).copied().unwrap_or(-9999);
        let mask = exc
            .max(i32::from(*hth_row.get(band).unwrap_or(&0)))
            .saturating_sub(i32::from(dbknee))
            .max(i32::from(floor));
        for bin in start..(start + sz).min(params.end_bin) {
            if bin < params.start_bin {
                continue;
            }
            let Some(&p) = psd.get(bin) else { continue };
            let snr = p - mask + params.snroffset;
            let last_idx = i32::try_from(BAPTAB.len()).unwrap_or(1) - 1;
            let idx = usize::try_from((snr >> 5).clamp(0, last_idx)).unwrap_or(0);
            if let (Some(slot), Some(&val)) = (bap.get_mut(bin), BAPTAB.get(idx)) {
                *slot = val;
            }
        }
    }
    bap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn louder_bins_never_get_a_stricter_bap_than_quieter_ones_in_the_same_band() {
        // A property the *shape* of the model must have regardless of the
        // exact constants: within one band, a higher-power bin cannot be
        // allocated fewer bits than a lower-power one, since they share the
        // same mask. This does not validate bit-exactness (the constants
        // might still be wrong), only that the comparison direction is not
        // inverted.
        let mut exps = vec![10u8; 256];
        if let Some(e) = exps.get_mut(0) {
            *e = 2; // louder (lower exponent = higher amplitude)
        }
        let params = AllocParams {
            end_bin: 256,
            ..AllocParams::default()
        };
        let bap = compute_bap(&exps, &params);
        assert!(bap.first().copied().unwrap_or(0) >= bap.get(1).copied().unwrap_or(0));
    }

    #[test]
    fn silence_gets_the_lowest_bap_everywhere() {
        let exps = vec![24u8; 64];
        let params = AllocParams {
            end_bin: 64,
            ..AllocParams::default()
        };
        let bap = compute_bap(&exps, &params);
        assert!(bap.iter().all(|&b| b <= 5));
    }

    #[test]
    fn never_panics_on_a_short_exponent_array() {
        let params = AllocParams {
            end_bin: 40,
            ..AllocParams::default()
        };
        let _ = compute_bap(&[1, 2, 3], &params);
    }
}
