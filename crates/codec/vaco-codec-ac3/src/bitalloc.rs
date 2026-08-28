//! The parametric bit-allocation model: turns exponents plus a handful of
//! per-block side-info values into one `bap` (bit-allocation pointer, 0..=15)
//! per coefficient. ATSC A/52:2012 §7.2, the seven-step procedure of
//! §7.2.2.1 through §7.2.2.7, transcribed directly from the pseudocode
//! rather than reconstructed from the algorithm's general shape — see
//! `crate::tables_bitalloc`'s module docs for how the tables were obtained.
//!
//! This is the single highest-risk module in this crate. The encoder never
//! transmits `bap` — the decoder derives it with the *exact* algorithm the
//! encoder used, because `bap` is what tells [`crate::mantissa`] how many
//! bits the next coefficient occupies. A constant or a comparison off by one
//! does not degrade quality here the way a lossy-codec bug usually would; it
//! desyncs the mantissa bit reader for the rest of the block.

use crate::tables_bitalloc::{
    BAPTAB, BNDSZ, DBKNEE, FASTDECAY, FASTGAIN, FLOOR, HTH, LATAB, MASKTAB, SLOWDECAY, SLOWGAIN,
};

/// Number of fixed masking bands spanning the transform's spectral bins.
pub const NBANDS: usize = 50;

/// Per-band starting bin, derived once from [`BNDSZ`] (`bndtab[]`, Table
/// 7.12 — stated directly in the spec but redundant with `bndsz[]`'s
/// cumulative sum, which the spec itself notes).
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

/// §7.2.2.2: exponent (0..=24) to power-spectral-density units.
/// `psd[bin] = 3072 - (exp[bin] << 7)`.
#[must_use]
pub const fn psd_of_exp(exp: u8) -> i32 {
    3072 - (exp as i32) * 128
}

/// §7.2.2.3's `logadd`: `c = a - b; addr = min(|c|>>1, 255); a>=b ? a +
/// latab[addr] : b + latab[addr]`.
fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let addr = usize::try_from(c.unsigned_abs() >> 1).unwrap_or(usize::MAX).min(255);
    let corr = i32::from(*LATAB.get(addr).unwrap_or(&0));
    if c >= 0 { a + corr } else { b + corr }
}

/// Inputs the bit-allocation model needs beyond the exponents themselves —
/// every one of these is a per-block or per-frame BSI/audblk field, never
/// derived. Field names match the spec's own (`sdcycod` etc.) so the
/// pseudocode-to-code correspondence in `compute_bap` stays checkable.
#[derive(Debug, Clone, Copy)]
pub struct AllocParams {
    pub sdcycod: u8,
    pub fdcycod: u8,
    pub sgaincod: u8,
    pub dbpbcod: u8,
    pub floorcod: u8,
    pub fscod: u8,
    pub fgaincod: u8,
    /// `snroffset`, already combined from `csnroffst` and this channel's own
    /// fine offset per §7.2.2.1.1:
    /// `(((csnroffst - 15) << 4) + fine_offset) << 2`.
    pub snroffset: i32,
    /// `start`/`end` per §7.2.2.1: `strtmant[ch]`/`endmant[ch]` for a full-
    /// bandwidth channel, `cplstrtmant`/`cplendmant` for the coupling
    /// channel, or `0`/`7` for LFE.
    pub start_bin: usize,
    pub end_bin: usize,
    /// Delta bit allocation, applied in §7.2.2.6: `(band_offset, run_length,
    /// deltba)` triples, already expanded from `deltoffst`/`deltlen`/
    /// `deltba` segments into absolute band positions by the caller. Empty
    /// when `deltbaie == 0` or this channel's `deltbae` says reuse/none.
    pub delta: &'static [(u8, u8, u8)],
}

impl Default for AllocParams {
    fn default() -> Self {
        Self {
            sdcycod: 2,
            fdcycod: 1,
            sgaincod: 1,
            dbpbcod: 2,
            floorcod: 7,
            fscod: 0,
            fgaincod: 4,
            snroffset: 0,
            start_bin: 0,
            end_bin: 0,
            delta: &[],
        }
    }
}

/// §7.2.2.4's `calc_lowcomp`.
const fn calc_lowcomp(a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
    if bin < 7 {
        if b0 + 256 == b1 {
            384
        } else if b0 > b1 {
            if a - 64 > 0 { a - 64 } else { 0 }
        } else {
            a
        }
    } else if bin < 20 {
        if b0 + 256 == b1 {
            320
        } else if b0 > b1 {
            if a - 64 > 0 { a - 64 } else { 0 }
        } else {
            a
        }
    } else if a - 128 > 0 {
        a - 128
    } else {
        0
    }
}

/// Run the model over one channel's exponents (§7.2.2.1 through §7.2.2.7),
/// returning one `bap` per bin in `0..params.end_bin`. `exps` must be at
/// least `end_bin` long; shorter is treated as silence past its end.
#[must_use]
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "one seven-step spec procedure, §7.2.2.1-7.2.2.7 — variable names (i, j, m, p) match the pseudocode's own so the correspondence stays checkable"
)]
pub fn compute_bap(exps: &[u8], params: &AllocParams) -> Vec<u8> {
    let start = params.start_bin;
    let end = params.end_bin.max(start);
    let n = end.max(exps.len());
    let mut bap = vec![0u8; n];
    if start >= end {
        return bap;
    }

    // §7.2.2.2: exponent mapping into psd.
    let mut psd = vec![0i32; n];
    for bin in start..end {
        let e = exps.get(bin).copied().unwrap_or(24);
        if let Some(slot) = psd.get_mut(bin) {
            *slot = psd_of_exp(e);
        }
    }

    // §7.2.2.3: PSD integration into `bndpsd[band]`, log-domain.
    let bndtab = band_start();
    let bndstrt = usize::from(*MASKTAB.get(start).unwrap_or(&0));
    let bndend = usize::from(*MASKTAB.get(end - 1).unwrap_or(&0)) + 1;
    let mut bndpsd = [0i32; NBANDS];
    {
        let mut j = start;
        let mut k = bndstrt;
        loop {
            let band_start_bin = usize::from(*bndtab.get(k).unwrap_or(&0));
            let band_sz = usize::from(*BNDSZ.get(k).unwrap_or(&0));
            let lastbin = (band_start_bin.saturating_add(band_sz)).min(end);
            let mut acc = psd.get(j).copied().unwrap_or(0);
            j = j.saturating_add(1);
            while j < lastbin {
                acc = logadd(acc, psd.get(j).copied().unwrap_or(0));
                j = j.saturating_add(1);
            }
            if let Some(slot) = bndpsd.get_mut(k) {
                *slot = acc;
            }
            k = k.saturating_add(1);
            if end <= lastbin || k >= NBANDS {
                break;
            }
        }
    }

    // §7.2.2.4: excitation function, with the low-frequency "lowcomp"
    // compensation applying only when this allocation starts at bin 0 (full
    // bandwidth or LFE channels; the coupling channel never does).
    let fgain = i32::from(*FASTGAIN.get(usize::from(params.fgaincod)).unwrap_or(&0));
    let sgain = i32::from(*SLOWGAIN.get(usize::from(params.sgaincod)).unwrap_or(&0));
    let fdecay = i32::from(*FASTDECAY.get(usize::from(params.fdcycod)).unwrap_or(&0));
    let sdecay = i32::from(*SLOWDECAY.get(usize::from(params.sdcycod)).unwrap_or(&0));

    let bp = |idx: usize| bndpsd.get(idx).copied().unwrap_or(0);

    let mut excite = [0i32; NBANDS];
    let mut fastleak;
    let mut slowleak;
    let mut lowcomp = 0i32;
    let begin;
    if bndstrt == 0 {
        let is_lfe_last = bndend == 7; // "do not call calc_lowcomp for bin 6 of lfe"
        lowcomp = calc_lowcomp(lowcomp, bp(0), bp(1), 0);
        if let Some(slot) = excite.get_mut(0) {
            *slot = bp(0) - fgain - lowcomp;
        }
        lowcomp = calc_lowcomp(lowcomp, bp(1), bp(2), 1);
        if let Some(slot) = excite.get_mut(1) {
            *slot = bp(1) - fgain - lowcomp;
        }
        fastleak = bp(1) - fgain;
        slowleak = bp(1) - sgain;
        let mut b = 7usize;
        for bin in 2..7 {
            if !(is_lfe_last && bin == 6) {
                lowcomp = calc_lowcomp(lowcomp, bp(bin), bp(bin + 1), bin);
            }
            fastleak = bp(bin) - fgain;
            slowleak = bp(bin) - sgain;
            if let Some(slot) = excite.get_mut(bin) {
                *slot = fastleak - lowcomp;
            }
            if !(is_lfe_last && bin == 6) && bp(bin) <= bp(bin + 1) {
                b = bin + 1;
                break;
            }
        }
        let bound = bndend.min(22);
        let mut bin = b;
        while bin < bound {
            if !(is_lfe_last && bin == 6) {
                lowcomp = calc_lowcomp(lowcomp, bp(bin), bp(bin + 1), bin);
            }
            fastleak -= fdecay;
            fastleak = fastleak.max(bp(bin) - fgain);
            slowleak -= sdecay;
            slowleak = slowleak.max(bp(bin) - sgain);
            if let Some(slot) = excite.get_mut(bin) {
                *slot = (fastleak - lowcomp).max(slowleak);
            }
            bin += 1;
        }
        begin = 22;
    } else {
        // Coupling channel: no lowcomp, leak state seeded by the caller's
        // `cplfleak`/`cplsleak` in the general case — approximated here at
        // the same starting psd the fbw path would use, since this crate
        // does not reconstruct coupling and the value is otherwise unused.
        fastleak = bndpsd.get(bndstrt).copied().unwrap_or(0) - fgain;
        slowleak = bndpsd.get(bndstrt).copied().unwrap_or(0) - sgain;
        begin = bndstrt;
    }
    let mut bin = begin;
    while bin < bndend {
        fastleak -= fdecay;
        fastleak = fastleak.max(bndpsd.get(bin).copied().unwrap_or(0) - fgain);
        slowleak -= sdecay;
        slowleak = slowleak.max(bndpsd.get(bin).copied().unwrap_or(0) - sgain);
        if let Some(slot) = excite.get_mut(bin) {
            *slot = fastleak.max(slowleak);
        }
        bin += 1;
    }

    // §7.2.2.5: masking curve.
    let dbknee = i32::from(*DBKNEE.get(usize::from(params.dbpbcod)).unwrap_or(&0));
    let hth_row = HTH.get(usize::from(params.fscod)).copied().unwrap_or(HTH[0]);
    let mut mask = [0i32; NBANDS];
    for bin in bndstrt..bndend {
        let mut e = excite.get(bin).copied().unwrap_or(0);
        let bp = bndpsd.get(bin).copied().unwrap_or(0);
        if bp < dbknee {
            e += (dbknee - bp) >> 2;
        }
        if let Some(slot) = mask.get_mut(bin) {
            *slot = e.max(i32::from(*hth_row.get(bin).unwrap_or(&0)));
        }
    }

    // §7.2.2.6: delta bit allocation, adjustments of ±6 dB multiples applied
    // directly to `mask[band]`.
    for &(band_off, run, deltba) in params.delta {
        let delta = if deltba >= 4 {
            i32::from(deltba) - 3
        } else {
            i32::from(deltba) - 4
        } << 7;
        for b in usize::from(band_off)..usize::from(band_off).saturating_add(usize::from(run)) {
            if let Some(slot) = mask.get_mut(b) {
                *slot += delta;
            }
        }
    }

    // §7.2.2.7: compute bit allocation.
    let floor = FLOOR.get(usize::from(params.floorcod)).copied().unwrap_or(0);
    let mut i = start;
    let mut j = bndstrt;
    loop {
        let band_start_bin = usize::from(*bndtab.get(j).unwrap_or(&0));
        let band_sz = usize::from(*BNDSZ.get(j).unwrap_or(&0));
        let lastbin = (band_start_bin.saturating_add(band_sz)).min(end);

        let mut m = mask.get(j).copied().unwrap_or(0);
        m -= params.snroffset;
        m -= floor;
        if m < 0 {
            m = 0;
        }
        m &= 0x1fe0;
        m += floor;

        while i < lastbin {
            let p = psd.get(i).copied().unwrap_or(0);
            let address = ((p - m) >> 5).clamp(0, 63);
            let idx = usize::try_from(address).unwrap_or(0);
            if let Some(slot) = bap.get_mut(i) {
                *slot = *BAPTAB.get(idx).unwrap_or(&0);
            }
            i = i.saturating_add(1);
        }
        j = j.saturating_add(1);
        if end <= lastbin || j >= NBANDS {
            break;
        }
    }

    bap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn louder_bins_never_get_a_stricter_bap_than_quieter_ones_in_the_same_band() {
        // Within one band the mask is identical for every bin in it (§7.2.2.7
        // computes `mask[j]` once per band, before the inner bin loop), so
        // `address = (psd[bin]-mask)>>5` is monotonic in `psd[bin]` there —
        // unlike *across* bands, where the leaky-integrator excitation
        // function makes "louder bin" and "stricter bap" not comparable at
        // all (a loud, isolated bin self-masks; a quiet bin far past it can
        // legitimately get more bits once the mask has decayed below it).
        // Band 28 (bins 28..31) is the first band wider than one bin.
        let mut exps = vec![14u8; 256];
        if let Some(e) = exps.get_mut(29) {
            *e = 2; // much louder than its band-mates at bins 28 and 30
        }
        let params = AllocParams {
            end_bin: 256,
            ..AllocParams::default()
        };
        let bap = compute_bap(&exps, &params);
        let quiet = bap.get(28).copied().unwrap_or(0);
        let loud = bap.get(29).copied().unwrap_or(0);
        assert!(loud >= quiet, "loud={loud} quiet={quiet}");
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

    #[test]
    fn never_panics_on_the_full_253_bin_range() {
        let exps = vec![5u8; 253];
        let params = AllocParams {
            end_bin: 253,
            ..AllocParams::default()
        };
        let bap = compute_bap(&exps, &params);
        assert_eq!(bap.len(), 253);
    }

    #[test]
    fn lfe_range_never_panics() {
        let exps = vec![8u8; 7];
        let params = AllocParams {
            end_bin: 7,
            ..AllocParams::default()
        };
        let bap = compute_bap(&exps, &params);
        assert_eq!(bap.len(), 7);
    }
}
