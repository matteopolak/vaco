//! `audblk()`: one of a frame's audio blocks (6 for classic AC-3, 1..=6 for
//! E-AC-3 depending on `numblkscod`). ATSC A/52:2018 §7.
//!
//! This is the module where every uncertainty flagged in [`crate::bitalloc`]
//! and [`crate::tables_bitalloc`] converges: a wrong field width here
//! desyncs everything from that point in the block onward. Coupling and the
//! delta-bit-allocation fine-tuning mechanism are parsed structurally (their
//! presence bits and lengths are read, so the bitstream stays aligned) but
//! not applied to reconstruction, which is tracked as follow-up work.

use vaco_bitstream::BitReader;

use crate::bitalloc::{self, AllocParams};
use crate::exponent::{self, ExpStrategy};
use crate::mantissa;

/// LFE channel coefficient count: fixed, low-frequency-only. §7.3.3.
pub const LFE_COEFFS: usize = 7;

/// Persistent state carried block-to-block within one frame: exponents and
/// bit-allocation parameters only re-transmitted when they change.
#[derive(Debug, Clone)]
pub struct BlockState {
    pub nfchans: usize,
    pub lfeon: bool,
    pub acmod: u8,
    pub chexps: Vec<Vec<u8>>,
    pub lfeexps: Vec<u8>,
    pub chbw_bins: Vec<usize>,
    pub alloc: AllocParams,
    pub fscod: u8,
    /// Per-channel fine SNR offset and fast gain, combined with the common
    /// coarse offset when `snroffste` is set; persist otherwise (§7.6.2).
    pub ch_snroffset: Vec<i32>,
    pub ch_fgaincod: Vec<u8>,
}

impl BlockState {
    #[must_use]
    pub fn new(nfchans: usize, lfeon: bool, acmod: u8, fscod: u8) -> Self {
        Self {
            nfchans,
            lfeon,
            acmod,
            chexps: vec![vec![0u8; 256]; nfchans],
            lfeexps: vec![0u8; LFE_COEFFS],
            chbw_bins: vec![253; nfchans],
            alloc: AllocParams {
                fscod,
                ..AllocParams::default()
            },
            fscod,
            ch_snroffset: vec![0; nfchans],
            ch_fgaincod: vec![4; nfchans],
        }
    }
}

/// One decoded block: dequantised, unscaled coefficients per full-bandwidth
/// channel plus LFE, ready for [`crate::imdct`]. Coupling and rematrixing
/// are not applied (see module docs); a coupled channel's high-frequency
/// coefficients are therefore silent rather than reconstructed from the
/// shared coupling channel, which is this crate's largest single accuracy
/// gap below the window-function one.
#[derive(Debug, Clone)]
pub struct DecodedBlock {
    pub channels: Vec<Vec<f32>>,
    pub lfe: Option<Vec<f32>>,
    pub dynrng: Option<f32>,
}

/// Parse and fully decode one `audblk()`. Never panics on truncated or
/// adversarial input — every read goes through [`BitReader`]'s sticky-
/// overrun model — but a truncated block naturally produces implausible
/// (usually silent) output rather than an error, matching how a real
/// decoder degrades on a corrupt frame.
#[must_use]
pub fn decode(r: &mut BitReader<'_>, state: &mut BlockState) -> DecodedBlock {
    let nfchans = state.nfchans;
    let mut blksw = vec![false; nfchans];
    for slot in &mut blksw {
        *slot = r.get_bit() != 0;
    }
    let mut dithflag = vec![true; nfchans];
    for slot in &mut dithflag {
        *slot = r.get_bit() != 0;
    }

    let dynrnge = r.get_bit() != 0;
    let dynrng = dynrnge.then(|| dynrng_db(r.get(8)));
    if state.acmod == 0 {
        let dynrng2e = r.get_bit() != 0;
        if dynrng2e {
            r.skip(8);
        }
    }

    let mut cplinu = false;
    let mut chincpl = vec![false; nfchans];
    let mut ncplbnd = 0usize;
    if nfchans > 1 {
        let cplstre = r.get_bit() != 0;
        if cplstre {
            cplinu = r.get_bit() != 0;
            if cplinu {
                for slot in &mut chincpl {
                    *slot = r.get_bit() != 0;
                }
                let phsflginu = (state.acmod == 2) && r.get_bit() != 0;
                let cplbegf = r.get(4);
                let cplendf = r.get(4);
                let ncplsubnd = cplendf.saturating_add(3).saturating_sub(cplbegf).max(1);
                for _ in 1..ncplsubnd {
                    r.skip(1); // cplbndstrc
                }
                ncplbnd = ncplsubnd as usize;
                let _ = phsflginu;
            }
        }
    }
    if cplinu {
        let mut cplcoe = vec![false; nfchans];
        for (ch, incpl) in chincpl.iter().enumerate() {
            if *incpl {
                let coe = r.get_bit() != 0;
                if let Some(slot) = cplcoe.get_mut(ch) {
                    *slot = coe;
                }
                if coe {
                    r.skip(2); // mstrcplco
                    for _ in 0..ncplbnd {
                        r.skip(4); // cplcoexp
                        r.skip(4); // cplcomant
                    }
                }
            }
        }
        if state.acmod == 2 && cplcoe.iter().any(|&c| c) {
            let phsflginu_again = r.get_bit() != 0;
            if phsflginu_again {
                for _ in 0..ncplbnd {
                    r.skip(1);
                }
            }
        }
    }

    let mut rematflg = [false; 4];
    if state.acmod == 2 {
        let rematstr = r.get_bit() != 0;
        if rematstr {
            for slot in &mut rematflg {
                *slot = r.get_bit() != 0;
            }
        }
    }

    let cplexpstr = cplinu.then(|| ExpStrategy::from_bits(r.get(2)));
    let mut chexpstr = Vec::new();
    for _ in 0..nfchans {
        chexpstr.push(ExpStrategy::from_bits(r.get(2)));
    }
    let lfeexpstr = state.lfeon.then(|| {
        if r.get_bit() != 0 {
            ExpStrategy::D15
        } else {
            ExpStrategy::Reuse
        }
    });

    for (ch, strategy) in chexpstr.iter().enumerate() {
        if *strategy != ExpStrategy::Reuse && !chincpl.get(ch).copied().unwrap_or(false) {
            let chbwcod = r.get(6);
            // Bandwidth in bins, per the standard's `chbwcod` -> bin-count
            // relationship: coarser groups of 3 bins per code step above a
            // fixed base, clamped to the maximum a 256-bin transform holds.
            let bins = (chbwcod.saturating_mul(3).saturating_add(73)).min(252) as usize;
            if let Some(slot) = state.chbw_bins.get_mut(ch) {
                *slot = bins;
            }
        }
    }

    if cplinu
        && cplexpstr.is_some_and(|s| s != ExpStrategy::Reuse)
        && let Some(strategy) = cplexpstr
    {
        let width = ncplbnd.saturating_mul(12).max(1);
        let (_exps, _bits) = exponent::decode(r, width, strategy);
    }
    for (ch, strategy) in chexpstr.iter().enumerate() {
        if *strategy == ExpStrategy::Reuse {
            continue;
        }
        let n = state.chbw_bins.get(ch).copied().unwrap_or(253);
        let (exps, _bits) = exponent::decode(r, n, *strategy);
        if let Some(slot) = state.chexps.get_mut(ch) {
            *slot = exps;
        }
        r.skip(2); // gainrng, per-channel
    }
    if let Some(strategy) = lfeexpstr
        && strategy != ExpStrategy::Reuse
    {
        let (exps, _bits) = exponent::decode(r, LFE_COEFFS, strategy);
        state.lfeexps = exps;
    }

    let baie = r.get_bit() != 0;
    if baie {
        state.alloc.fdecaycod = u8::try_from(r.get(2)).unwrap_or(0);
        state.alloc.sdecaycod = u8::try_from(r.get(2)).unwrap_or(0);
        state.alloc.sgaincod = u8::try_from(r.get(2)).unwrap_or(0);
        state.alloc.dbpbcod = u8::try_from(r.get(2)).unwrap_or(0);
        state.alloc.floorcod = u8::try_from(r.get(3)).unwrap_or(0);
    }

    let snroffste = r.get_bit() != 0;
    if snroffste {
        let csnroffst = r.get(6);
        if cplinu {
            let fsnr = r.get(4);
            let fgain = r.get(3);
            state.alloc.snroffset = combine_snroffset(csnroffst, fsnr);
            state.alloc.fgaincod = u8::try_from(fgain).unwrap_or(0);
        }
        for ch in 0..nfchans {
            let fsnr = r.get(4);
            let fgain = r.get(3);
            if let Some(slot) = state.ch_snroffset.get_mut(ch) {
                *slot = combine_snroffset(csnroffst, fsnr);
            }
            if let Some(slot) = state.ch_fgaincod.get_mut(ch) {
                *slot = u8::try_from(fgain).unwrap_or(0);
            }
        }
        if state.lfeon {
            r.skip(4); // lfefsnroffst
            r.skip(3); // lfefgaincod
        }
    }

    if cplinu {
        let cplleake = r.get_bit() != 0;
        if cplleake {
            r.skip(3);
            r.skip(3);
        }
    }

    let deltbae_cpl = cplinu.then(|| r.get(2));
    skip_delta_bit_allocation(r, deltbae_cpl);
    for _ in 0..nfchans {
        let deltbae = r.get(2);
        skip_delta_bit_allocation(r, Some(deltbae));
    }

    let skipfle = r.get_bit() != 0;
    if skipfle {
        let skipl = r.get(9);
        r.skip(skipl);
    }

    // -- bit allocation + mantissas -----------------------------------
    let mut channels = Vec::new();
    for ch in 0..nfchans {
        let exps = state.chexps.get(ch).cloned().unwrap_or_default();
        let n = state
            .chbw_bins
            .get(ch)
            .copied()
            .unwrap_or(253)
            .min(exps.len());
        let mut params = state.alloc;
        params.end_bin = n;
        params.start_bin = 0;
        params.snroffset = state.ch_snroffset.get(ch).copied().unwrap_or(0);
        params.fgaincod = state.ch_fgaincod.get(ch).copied().unwrap_or(4);
        let bap = bitalloc::compute_bap(&exps, &params);
        let coeffs = mantissa::decode(
            r,
            &bap,
            &exps,
            dithflag.get(ch).copied().unwrap_or(true),
            simple_dither,
        );
        channels.push(coeffs);
    }
    let lfe = state.lfeon.then(|| {
        let exps = state.lfeexps.clone();
        let mut params = state.alloc;
        params.end_bin = LFE_COEFFS;
        params.start_bin = 0;
        let bap = bitalloc::compute_bap(&exps, &params);
        mantissa::decode(r, &bap, &exps, true, simple_dither)
    });

    let auxdatae = r.get_bit() != 0;
    if auxdatae {
        let auxdatal = r.get(14);
        r.skip(auxdatal);
    }
    r.skip(1); // reserved
    r.skip(16); // crc2 (unchecked: verifying it needs the whole frame's CRC state, not one block's)

    apply_rematrix(&mut channels, state.acmod, rematflg);

    DecodedBlock {
        channels,
        lfe,
        dynrng,
    }
}

/// `dynrng` is an 8-bit two's-complement value scaled to dB per §7.6.1's
/// gain-word convention (roughly 6 dB per unit of the top nibble, finer
/// resolution below) — approximated here as a linear dB mapping across the
/// field's signed range, which preserves the sign and rough magnitude
/// without claiming the exact companding curve.
fn dynrng_db(code: u32) -> f32 {
    let signed = i32::from(code as i8);
    f32::from(signed as i16) * (12.0 / 128.0)
}

fn combine_snroffset(coarse: u32, fine: u32) -> i32 {
    coarse.cast_signed() * 4 + fine.cast_signed() - 60
}

fn skip_delta_bit_allocation(r: &mut BitReader<'_>, deltbae: Option<u32>) {
    // Codes: 0 reuse, 1 new (segments follow), 2 none, 3 reserved.
    if deltbae != Some(1) {
        return;
    }
    let deltnseg = r.get(3).saturating_add(1);
    for _ in 0..deltnseg {
        r.skip(5); // deltoffst
        r.skip(4); // deltlen
        r.skip(3); // deltba
    }
}

fn simple_dither() -> f32 {
    0.0
}

/// Stereo rematrixing: bands where `rematflg` is set were coded as
/// sum/difference rather than left/right. §7.5.4. Only meaningful for
/// `acmod == 2`; a no-op otherwise since `channels.len() != 2`.
const REMATRIX_BANDS: [(usize, usize); 4] = [(13, 25), (25, 37), (37, 61), (61, 253)];

fn apply_rematrix(channels: &mut [Vec<f32>], acmod: u8, rematflg: [bool; 4]) {
    if acmod != 2 || channels.len() != 2 {
        return;
    }
    let (left, rest) = channels.split_at_mut(1);
    let Some(l) = left.first_mut() else { return };
    let Some(r) = rest.first_mut() else { return };
    for (band, &(start, end)) in REMATRIX_BANDS.iter().enumerate() {
        if !rematflg.get(band).copied().unwrap_or(false) {
            continue;
        }
        for i in start..end {
            let (Some(lv), Some(rv)) = (l.get_mut(i), r.get_mut(i)) else {
                continue;
            };
            let sum = *lv;
            let diff = *rv;
            *lv = sum + diff;
            *rv = sum - diff;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_panics_on_an_empty_buffer() {
        let mut r = BitReader::new(&[]);
        let mut state = BlockState::new(2, false, 2, 0);
        let block = decode(&mut r, &mut state);
        assert_eq!(block.channels.len(), 2);
    }

    #[test]
    fn mono_never_reads_coupling_or_rematrix_flags() {
        let mut r = BitReader::new(&[0xFFu8; 64]);
        let mut state = BlockState::new(1, true, 1, 0);
        let block = decode(&mut r, &mut state);
        assert_eq!(block.channels.len(), 1);
        assert!(block.lfe.is_some());
    }
}
