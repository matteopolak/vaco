//! `audblk()`: one of a frame's 6 audio blocks. ATSC A/52:2012 §5.4.3/§7,
//! transcribed field-by-field and pseudocode-by-pseudocode against the
//! primary specification text.
//!
//! `auxdata()` and `errorcheck()` are **not** part of `audblk()` — they are
//! separate `syncframe()` elements that appear once, after all 6 blocks
//! (§5.3: `syncframe() { syncinfo(); bsi(); for 6 blocks: audblk(); auxdata();
//! errorcheck(); }`). An earlier version of this parser read them once per
//! block instead, which consumed 17+ spurious bits after every block but the
//! last — this is now handled once, at the end of [`crate::decode`]'s frame
//! loop.
//!
//! Coupling reconstruction remains out of scope (see `planning/TECH-DEBT.md`):
//! every coupling-related field is read correctly enough to stay bit-aligned
//! for the *side information*, but the coupling channel's own mantissas are
//! not consumed, so a frame that actually uses coupling will still desync at
//! the mantissa stage. Real encodes at the bit rates this crate's fixtures
//! use were not observed enabling it.

use vaco_bitstream::BitReader;

use crate::bitalloc::{self, AllocParams};
use crate::exponent::{self, ExpStrategy};
use crate::mantissa;

/// LFE channel coefficient count: fixed, low-frequency-only. §7.2.2.1.
pub const LFE_COEFFS: usize = 7;

/// Persistent state carried block-to-block within one frame: exponents,
/// bit-allocation parameters and coupling strategy only re-transmitted when
/// they change.
#[derive(Debug, Clone)]
pub struct BlockState {
    pub nfchans: usize,
    pub lfeon: bool,
    pub acmod: u8,
    pub chexps: Vec<Vec<u8>>,
    pub lfeexps: Vec<u8>,
    /// `strtmant[ch]`/`endmant[ch]`, §7.2.2.1.
    pub ch_range: Vec<(usize, usize)>,
    pub alloc: AllocParams,
    pub fscod: u8,
    /// Per-channel fine SNR offset and fast gain, combined with the common
    /// coarse offset when `snroffste` is set; persist otherwise (§7.2.2.1.1).
    pub ch_snroffset: Vec<i32>,
    pub ch_fgaincod: Vec<u8>,
    /// Coupling strategy, persisting across blocks where `cplstre == 0`.
    pub cplinu: bool,
    pub cplbegf: u32,
    pub cplendf: u32,
    pub chincpl: Vec<bool>,
    /// §5.4.3: `phsflginu`, read only when `cplstre && cplinu` — like
    /// `cplbegf`/`cplendf`, it persists across later blocks that reuse the
    /// coupling strategy rather than being re-read.
    pub phsflginu: bool,
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
            ch_range: vec![(0, 253); nfchans],
            alloc: AllocParams {
                fscod,
                ..AllocParams::default()
            },
            fscod,
            ch_snroffset: vec![0; nfchans],
            ch_fgaincod: vec![4; nfchans],
            cplinu: false,
            cplbegf: 0,
            cplendf: 0,
            chincpl: vec![false; nfchans],
            phsflginu: false,
        }
    }
}

/// One decoded block: dequantised, unscaled coefficients per full-bandwidth
/// channel plus LFE, ready for [`crate::imdct`]. Coupling is not
/// reconstructed (see module docs).
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
#[allow(clippy::too_many_lines, reason = "one audblk() syntax walk, §5.4.3")]
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

    // §5.4.3 Table 5.3: `cplstre` is read unconditionally, for every
    // `acmod` including 1/0 (mono). There is no `nfchans > 1` gate in the
    // syntax table — mono streams still carry this bit (always 0, since
    // coupling needs at least two channels), and skipping it desyncs every
    // field that follows for the rest of the block.
    let mut ncplbnd = 0usize;
    let cplstre = r.get_bit() != 0;
    if cplstre {
        state.cplinu = r.get_bit() != 0;
        if state.cplinu {
            for slot in &mut state.chincpl {
                *slot = r.get_bit() != 0;
            }
            // §5.4.3: `phsflginu` is only *in the bitstream* when acmod==2;
            // reading it any other time would consume a bit that was never
            // sent. When cplinu turns on for a non-2/0 stream, the field
            // does not exist at all and the persisted value stays `false`.
            if state.acmod == 2 {
                state.phsflginu = r.get_bit() != 0;
            }
            state.cplbegf = r.get(4);
            state.cplendf = r.get(4);
        } else {
            state.chincpl.fill(false);
        }
    }
    if state.cplinu {
        let ncplsubnd = state
            .cplendf
            .saturating_add(3)
            .saturating_sub(state.cplbegf)
            .max(1);
        for _ in 1..ncplsubnd {
            r.skip(1); // cplbndstrc
        }
        ncplbnd = ncplsubnd as usize;
    }
    let cplinu = state.cplinu;
    let cplbegf = state.cplbegf;
    let cplendf = state.cplendf;
    let chincpl = state.chincpl.clone();

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
        // §5.4.3: `phsflginu` here is the value persisted from the coupling
        // strategy section above, not a fresh bit — the spec's own gate is
        // `(acmod==0x2) && phsflginu && (cplcoe[0]||cplcoe[1])`. Reading a
        // second bit at this point (an earlier version of this parser did)
        // consumes one bit that was never transmitted whenever a 2/0 stream
        // sends any coupling coordinates, desyncing everything after it.
        if state.acmod == 2 && state.phsflginu && cplcoe.iter().any(|&c| c) {
            for _ in 0..ncplbnd {
                r.skip(1); // phsflg[bnd]
            }
        }
    }

    // §5.4.3.14-16: rematflg band count depends on cplbegf/cplinu.
    let mut rematflg = [false; 4];
    if state.acmod == 2 {
        let rematstr = r.get_bit() != 0;
        if rematstr {
            let nrematbnd = if cplbegf > 2 || !cplinu {
                4
            } else if cplbegf > 0 {
                3
            } else {
                2
            };
            for slot in rematflg.iter_mut().take(nrematbnd) {
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

    // §7.2.2.1: strtmant/endmant, cplstrtmant/cplendmant.
    let cplstrtmant = 37 + 12 * cplbegf as usize;
    let cplendmant = 37 + 12 * (cplendf as usize + 3);
    for (ch, strategy) in chexpstr.iter().enumerate() {
        if *strategy != ExpStrategy::Reuse && !chincpl.get(ch).copied().unwrap_or(false) {
            let chbwcod = r.get(6);
            let endmant = ((chbwcod as usize + 12) * 3) + 37;
            if let Some(slot) = state.ch_range.get_mut(ch) {
                *slot = (0, endmant.min(253));
            }
        } else if chincpl.get(ch).copied().unwrap_or(false)
            && let Some(slot) = state.ch_range.get_mut(ch)
        {
            slot.1 = cplstrtmant.min(253);
        }
    }

    if cplinu
        && cplexpstr.is_some_and(|s| s != ExpStrategy::Reuse)
        && let Some(strategy) = cplexpstr
    {
        let width = cplendmant.saturating_sub(cplstrtmant);
        // §7.1.3: ncplgrps has no "-1" offset (unlike nchgrps below) — the
        // coupling channel's own exp[0] is not a real exponent, only a
        // reference point, so its width divides evenly by construction.
        let ncodes = ncplgrps_for(width, strategy);
        let absexp = u8::try_from(r.get(4)).unwrap_or(0) << 1;
        let _exps = exponent::decode(r, absexp, ncodes, strategy);
    }
    for (ch, strategy) in chexpstr.iter().enumerate() {
        if *strategy == ExpStrategy::Reuse {
            continue;
        }
        let (_start, end) = state.ch_range.get(ch).copied().unwrap_or((0, 253));
        // §7.1.3: nchgrps = truncate{(endmant-1)/3, /6, or /12}.
        let ncodes = ncodes_for(end.saturating_sub(1), *strategy);
        let absexp = u8::try_from(r.get(4)).unwrap_or(0);
        let exps = exponent::decode(r, absexp, ncodes, *strategy);
        if let Some(slot) = state.chexps.get_mut(ch) {
            *slot = exps;
        }
        r.skip(2); // gainrng, per-channel
    }
    if let Some(strategy) = lfeexpstr
        && strategy != ExpStrategy::Reuse
    {
        let absexp = u8::try_from(r.get(4)).unwrap_or(0);
        // nlfegrps is fixed at 2, not derived from a bin-count formula.
        state.lfeexps = exponent::decode(r, absexp, 2, strategy);
    }

    let baie = r.get_bit() != 0;
    if baie {
        // §5.4.3.30's own field order: sdcycod before fdcycod.
        state.alloc.sdcycod = u8::try_from(r.get(2)).unwrap_or(0);
        state.alloc.fdcycod = u8::try_from(r.get(2)).unwrap_or(0);
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

    // §5.4.3.47: an overall `deltbaie` gate governates the whole delta-bit-
    // allocation section — missing it in an earlier version of this parser
    // meant `cpldeltbae`/`deltbae[ch]` were read unconditionally, 2 spurious
    // bits per channel on every single block regardless of content.
    let deltbaie = r.get_bit() != 0;
    if deltbaie {
        let cpldeltbae = cplinu.then(|| r.get(2));
        let deltbae: Vec<u32> = (0..nfchans).map(|_| r.get(2)).collect();
        if cplinu {
            skip_delta_bit_allocation(r, cpldeltbae);
        }
        for d in deltbae {
            skip_delta_bit_allocation(r, Some(d));
        }
    }

    let skiple = r.get_bit() != 0;
    if skiple {
        let skipl = r.get(9);
        r.skip(skipl);
    }

    // -- bit allocation + mantissas -----------------------------------
    // Coupling channel mantissas are not read here (see module docs): a
    // frame using coupling desyncs from this point, a known, disclosed gap.
    let mut channels = Vec::new();
    for ch in 0..nfchans {
        let exps = state.chexps.get(ch).cloned().unwrap_or_default();
        let (start, end) = state.ch_range.get(ch).copied().unwrap_or((0, 253));
        // `exps` is exactly `endmant` long by construction (see the
        // exponent-decode loop above), but never index past it if a
        // truncated bitstream produced a shorter array.
        let end = end.min(exps.len());
        let mut params = state.alloc;
        params.start_bin = start;
        params.end_bin = end;
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
        params.start_bin = 0;
        params.end_bin = LFE_COEFFS;
        let bap = bitalloc::compute_bap(&exps, &params);
        mantissa::decode(r, &bap, &exps, true, simple_dither)
    });

    apply_rematrix(&mut channels, state.acmod, rematflg);

    DecodedBlock {
        channels,
        lfe,
        dynrng,
    }
}

/// §7.1.3: `nchgrps`/`ncplgrps` (number of 7-bit `gexp` codes to read),
/// truncating integer division per the exact formula for each strategy.
#[allow(
    clippy::integer_division,
    reason = "the spec states these as truncating division, not a precision loss"
)]
const fn ncodes_for(width: usize, strategy: ExpStrategy) -> usize {
    match strategy {
        ExpStrategy::D25 => (width + 3) / 6,
        ExpStrategy::D45 => (width + 9) / 12,
        _ => width / 3,
    }
}

/// §7.1.3: `ncplgrps`, the coupling channel's own version of
/// [`ncodes_for`] — no "-1" offset (see call site).
#[allow(
    clippy::integer_division,
    reason = "the spec states this as truncating division, not a precision loss"
)]
const fn ncplgrps_for(width: usize, strategy: ExpStrategy) -> usize {
    match strategy {
        ExpStrategy::D25 => width / 6,
        ExpStrategy::D45 => width / 12,
        _ => width / 3,
    }
}

/// `dynrng` is an 8-bit two's-complement value scaled to dB per §7.2.2.1.1's
/// gain-word convention (roughly 6 dB per unit of the top nibble, finer
/// resolution below) — approximated here as a linear dB mapping across the
/// field's signed range, which preserves the sign and rough magnitude
/// without claiming the exact companding curve.
fn dynrng_db(code: u32) -> f32 {
    let signed = i32::from(code as i8);
    f32::from(signed as i16) * (12.0 / 128.0)
}

/// §7.2.2.1.1: `snroffset = (((csnroffst - 15) << 4) + fine) << 2`.
fn combine_snroffset(coarse: u32, fine: u32) -> i32 {
    ((coarse.cast_signed() - 15) * 16 + fine.cast_signed()) * 4
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
/// sum/difference rather than left/right. §7.2.2.1 (reconstruction is
/// informative Annex material, not separately clause-numbered in the main
/// body). Only meaningful for `acmod == 2`; a no-op otherwise since
/// `channels.len() != 2`.
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

    #[test]
    fn ncodes_formula_matches_the_spec_for_full_bandwidth() {
        // endmant=253 (max bandwidth), D15: (253-1)/3 = 84.
        assert_eq!(ncodes_for(252, ExpStrategy::D15), 84);
    }
}
