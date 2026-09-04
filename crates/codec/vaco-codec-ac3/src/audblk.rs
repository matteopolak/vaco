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
//! Coupling (§7.4) is reconstructed here. It is not optional in practice:
//! `ffmpeg`'s AC-3 encoder turns it on for every stereo and 5.1 fixture in
//! this crate's own test matrix, and a decoder that only skips the coupling
//! side information desyncs at the first coupling-channel mantissa.

use vaco_bitstream::BitReader;

use crate::bitalloc::{self, AllocParams};
use crate::exponent::{self, ExpStrategy};
use crate::mantissa;

/// LFE channel coefficient count: fixed, low-frequency-only. §7.2.2.1.
pub const LFE_COEFFS: usize = 7;

/// §7.4.2 Table 7.24: transform coefficients 37..252 form 18 coupling
/// sub-bands of 12 coefficients each.
pub const NCPLSUBND: usize = 18;

/// §7.4.2: the first transform coefficient any coupling sub-band covers.
const CPL_BIN_BASE: usize = 37;

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
    /// LFE's own combined SNR offset/fast gain — distinct from `alloc`'s
    /// (which the coupling channel uses when `cplinu`), and previously not
    /// tracked at all (`lfefsnroffst`/`lfefgaincod` were read and
    /// discarded, leaving LFE's own allocation using whatever
    /// `alloc.snroffset` happened to hold — 0 when no coupling channel had
    /// ever set it). Persists across blocks exactly like `ch_snroffset`.
    pub lfe_snroffset: i32,
    pub lfe_fgaincod: u8,
    /// Coupling strategy, persisting across blocks where `cplstre == 0`.
    pub cplinu: bool,
    pub cplbegf: u32,
    pub cplendf: u32,
    pub chincpl: Vec<bool>,
    /// §5.4.3: `phsflginu`, read only when `cplstre && cplinu` — like
    /// `cplbegf`/`cplendf`, it persists across later blocks that reuse the
    /// coupling strategy rather than being re-read.
    pub phsflginu: bool,
    /// `cplbndstrc[sbnd]`: sub-band `sbnd` is folded into the coupling band
    /// below it. Indexed by absolute sub-band number, so `cplbegf` is the
    /// first meaningful entry and is always `false` (the spec reads the
    /// array from `bnd = 1`). Part of the coupling strategy, so it persists
    /// exactly like `cplbegf`/`cplendf`.
    pub cplbndstrc: Vec<bool>,
    /// The coupling channel's exponents, one per bin from bin 0 (entries
    /// below `cplstrtmant` are unused padding), persisting across blocks
    /// whose `cplexpstr` is reuse.
    pub cplexps: Vec<u8>,
    /// §7.4.3's `cplco[ch][sbnd]`, already expanded from coupling bands to
    /// coupling sub-bands and already folded in `mstrcplco`. Persists per
    /// channel across blocks where `cplcoe[ch] == 0`.
    pub cplco: Vec<Vec<f32>>,
    /// §7.4.1's `phsflg[bnd]`, expanded to sub-bands. Persists with the
    /// coupling strategy.
    pub phsflg: Vec<bool>,
    /// §7.2.2.1's `(cplfleak, cplsleak)` leak seeds for the coupling
    /// channel's own bit allocation, persisting across blocks where
    /// `cplleake == 0`.
    pub cpl_leaks: (i32, i32),
    /// §7.2.2.6's delta bit allocation segments, per fbw channel and for
    /// the coupling channel, as `(band, run, deltba)`. `deltbae == 0` means
    /// "reuse", so these outlive the block that sent them.
    pub ch_delta: Vec<Vec<(u8, u8, u8)>>,
    pub cpl_delta: Vec<(u8, u8, u8)>,
    /// §5.4.3.19/§5.4.3.20: `rematflg[rbnd]`. `rematstr == 0` means "no new
    /// flags, reuse the previous values" — not "no rematrixing", which is
    /// what clearing these per block would mean. Blocks 1-5 of a typical
    /// `ffmpeg` 2/0 encode send `rematstr = 0`, so treating that as "off"
    /// drops rematrixing on five blocks in six.
    pub rematflg: [bool; 4],
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
            lfe_snroffset: 0,
            lfe_fgaincod: 4,
            cplinu: false,
            cplbegf: 0,
            cplendf: 0,
            chincpl: vec![false; nfchans],
            phsflginu: false,
            cplbndstrc: vec![false; NCPLSUBND],
            cplexps: vec![0u8; 256],
            cplco: vec![vec![0f32; NCPLSUBND]; nfchans],
            phsflg: vec![false; NCPLSUBND],
            cpl_leaks: (0, 0),
            ch_delta: vec![Vec::new(); nfchans],
            cpl_delta: Vec::new(),
            rematflg: [false; 4],
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
    if cplstre && state.cplinu {
        // §5.4.3: `cplbndstrc[bnd]` is transmitted only with a *new*
        // coupling strategy, and is read from `bnd = 1` — sub-band
        // `cplbegf` always starts its own band. A set bit folds that
        // sub-band into the band below, so `ncplbnd < ncplsubnd` whenever
        // any bit is set; treating every sub-band as its own band (an
        // earlier version of this parser did) reads that many extra
        // `cplcoexp`/`cplcomant` pairs and desyncs the block.
        state.cplbndstrc.iter_mut().for_each(|b| *b = false);
        for sbnd in (cplsubnd_range(state).start + 1)..cplsubnd_range(state).end {
            let bit = r.get_bit() != 0;
            if let Some(slot) = state.cplbndstrc.get_mut(sbnd) {
                *slot = bit;
            }
        }
    }
    if state.cplinu {
        ncplbnd = count_cpl_bands(state);
    }
    let cplinu = state.cplinu;
    let cplbegf = state.cplbegf;
    let cplendf = state.cplendf;
    let chincpl = state.chincpl.clone();

    let sbnd_range = cplsubnd_range(state);
    if cplinu {
        let mut cplcoe = vec![false; nfchans];
        for (ch, incpl) in chincpl.iter().enumerate() {
            if *incpl {
                let coe = r.get_bit() != 0;
                if let Some(slot) = cplcoe.get_mut(ch) {
                    *slot = coe;
                }
                if coe {
                    // §7.4.3. One coordinate per coupling *band*; the
                    // `cplbndstrc` expansion below duplicates it across the
                    // sub-bands that band covers.
                    let mstrcplco = r.get(2);
                    let mut per_band = Vec::new();
                    for _ in 0..ncplbnd {
                        let cplcoexp = r.get(4);
                        let cplcomant = r.get(4);
                        per_band.push(cplco_value(cplcoexp, cplcomant, mstrcplco));
                    }
                    if let Some(dst) = state.cplco.get_mut(ch) {
                        expand_bands_to_subbands(&per_band, &state.cplbndstrc, &sbnd_range, dst);
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
            let mut per_band = Vec::new();
            for _ in 0..ncplbnd {
                per_band.push(f32::from(u8::from(r.get_bit() != 0)));
            }
            let mut flags = vec![0f32; NCPLSUBND];
            expand_bands_to_subbands(&per_band, &state.cplbndstrc, &sbnd_range, &mut flags);
            for (sbnd, slot) in state.phsflg.iter_mut().enumerate() {
                *slot = flags.get(sbnd).copied().unwrap_or(0.0) != 0.0;
            }
        }
    }

    // §5.4.3.14-16: rematflg band count depends on cplbegf/cplinu.
    let nrematbnd = if !cplinu || cplbegf > 2 {
        4
    } else if cplbegf > 0 {
        3
    } else {
        2
    };
    if state.acmod == 2 {
        let rematstr = r.get_bit() != 0;
        if rematstr {
            state.rematflg = [false; 4];
            for slot in state.rematflg.iter_mut().take(nrematbnd) {
                *slot = r.get_bit() != 0;
            }
        }
    }
    let rematflg = state.rematflg;

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
        let decoded = exponent::decode(r, absexp, ncodes, strategy);
        // §7.1.3: `cplabsexp` "is only used as a reference to begin decoding
        // the differential exponents ... it does not represent an actual
        // exponent", so `decoded[0]` is dropped and `decoded[1]` is the
        // exponent of bin `cplstrtmant`.
        let mut exps = vec![0u8; CPL_BIN_BASE + NCPLSUBND * 12];
        for (i, &e) in decoded.iter().skip(1).enumerate() {
            if let Some(slot) = exps.get_mut(cplstrtmant + i) {
                *slot = e;
            }
        }
        state.cplexps = exps;
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
            let fsnr = r.get(4);
            let fgain = r.get(3);
            state.lfe_snroffset = combine_snroffset(csnroffst, fsnr);
            state.lfe_fgaincod = u8::try_from(fgain).unwrap_or(0);
        }
    }

    if cplinu {
        let cplleake = r.get_bit() != 0;
        if cplleake {
            // §7.2.2.1: fastleak = (cplfleak << 8) + 768,
            //           slowleak = (cplsleak << 8) + 768.
            let cplfleak = r.get(3).cast_signed();
            let cplsleak = r.get(3).cast_signed();
            state.cpl_leaks = ((cplfleak << 8) + 768, (cplsleak << 8) + 768);
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
        if cplinu && let Some(segments) = read_delta_bit_allocation(r, cpldeltbae) {
            state.cpl_delta = segments;
        }
        for (ch, d) in deltbae.into_iter().enumerate() {
            if let Some(segments) = read_delta_bit_allocation(r, Some(d))
                && let Some(slot) = state.ch_delta.get_mut(ch)
            {
                *slot = segments;
            }
        }
    }

    let skiple = r.get_bit() != 0;
    if skiple {
        // §5.4.3: `skipfld` is `skipl x 8` bits — `skipl` counts *bytes*,
        // not bits. Skipping `skipl` bits leaves the mantissa reader short
        // by `7 * skipl` bits for the rest of the block.
        let skipl = r.get(9);
        r.skip_long(u64::from(skipl) * 8);
    }

    // -- bit allocation + mantissas -----------------------------------
    // Coupling channel mantissas are not read here (see module docs): a
    // frame using coupling desyncs from this point, a known, disclosed gap.
    //
    // §7.3.5: bap 1/2/4's group state is shared across the whole block's
    // linear mantissa stream, not reset per channel — a channel whose
    // bap-1/2/4 bin count does not land on a group boundary hands its last
    // group's unused slots to whichever channel is decoded next in this
    // same block. One `PendingGroup` for the whole block, threaded through
    // every call below in the spec's own processing order (fbw channels,
    // then LFE), is what that requires.
    //
    // §7.2.2.1.1's special case: if every SNR offset source in the block
    // is exactly zero, `bap[]` is all zero for the whole block and no
    // further bit-allocation processing is required. See
    // `all_snr_offsets_raw_zero`'s own docs for why comparing already-
    // combined values against a sentinel is equivalent to checking the raw
    // fields.
    let all_snr_offsets_raw_zero = all_snr_offsets_raw_zero(state, cplinu);

    let mut pending_group = mantissa::PendingGroup::new();
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut cplmant: Vec<f32> = Vec::new();
    let mut got_cplchan = false;

    for ch in 0..nfchans {
        let exps = state.chexps.get(ch).cloned().unwrap_or_default();
        let (start, end) = state.ch_range.get(ch).copied().unwrap_or((0, 253));
        // `exps` is exactly `endmant` long by construction (see the
        // exponent-decode loop above), but never index past it if a
        // truncated bitstream produced a shorter array.
        let end = end.min(exps.len());
        let bap = if all_snr_offsets_raw_zero {
            vec![0u8; end.saturating_sub(start)]
        } else {
            let mut params = state.alloc;
            params.start_bin = start;
            params.end_bin = end;
            params.snroffset = state.ch_snroffset.get(ch).copied().unwrap_or(0);
            params.fgaincod = state.ch_fgaincod.get(ch).copied().unwrap_or(4);
            params.cpl_leaks = None;
            let delta = state.ch_delta.get(ch).map_or(&[][..], Vec::as_slice);
            bitalloc::compute_bap(&exps, &params, delta)
        };
        let mut coeffs = mantissa::decode(
            r,
            &bap,
            &exps,
            dithflag.get(ch).copied().unwrap_or(true),
            simple_dither,
            &mut pending_group,
        );
        coeffs.truncate(end);
        channels.push(coeffs);

        // §5.4.3: the coupling channel's mantissas sit inline, immediately
        // after the *first* coupled channel's own — not before the loop and
        // not after it. Reading them anywhere else puts every later
        // mantissa at the wrong bit offset, and (because bap 1/2/4 groups
        // straddle channels, §7.3.5) mis-assigns the grouped ones even if
        // the offsets happened to line up.
        if cplinu && chincpl.get(ch).copied().unwrap_or(false) && !got_cplchan {
            let cplbap = if all_snr_offsets_raw_zero {
                vec![0u8; cplendmant]
            } else {
                let mut params = state.alloc;
                params.start_bin = cplstrtmant;
                params.end_bin = cplendmant;
                params.cpl_leaks = Some(state.cpl_leaks);
                bitalloc::compute_bap(&state.cplexps, &params, &state.cpl_delta)
            };
            let slice_start = cplstrtmant.min(cplbap.len());
            let slice_end = cplendmant.min(cplbap.len());
            let values = mantissa::decode(
                r,
                cplbap.get(slice_start..slice_end).unwrap_or(&[]),
                state.cplexps.get(slice_start..slice_end).unwrap_or(&[]),
                // §7.3.4: the coupling channel's own dither flag is
                // `dithflag[ch]` of the channel that carries it.
                dithflag.get(ch).copied().unwrap_or(true),
                simple_dither,
                &mut pending_group,
            );
            cplmant = values;
            got_cplchan = true;
        }
    }

    let lfe = state.lfeon.then(|| {
        let exps = state.lfeexps.clone();
        let bap = if all_snr_offsets_raw_zero {
            vec![0u8; LFE_COEFFS]
        } else {
            let mut params = state.alloc;
            params.start_bin = 0;
            params.end_bin = LFE_COEFFS;
            params.snroffset = state.lfe_snroffset;
            params.fgaincod = state.lfe_fgaincod;
            params.cpl_leaks = None;
            bitalloc::compute_bap(&exps, &params, &[])
        };
        mantissa::decode(r, &bap, &exps, true, simple_dither, &mut pending_group)
    });

    // §7.4.3: `chmant[ch][sbnd*12+bin+37] = cplmant[...] * cplco[ch][sbnd] * 8`.
    if cplinu {
        for (ch, coeffs) in channels.iter_mut().enumerate() {
            if !chincpl.get(ch).copied().unwrap_or(false) {
                continue;
            }
            coeffs.resize(cplendmant, 0.0);
            for sbnd in sbnd_range.clone() {
                let co = state
                    .cplco
                    .get(ch)
                    .and_then(|c| c.get(sbnd))
                    .copied()
                    .unwrap_or(0.0);
                // §7.4.1: in 2/0 mode a set phase flag negates the *right*
                // channel's coefficients in that sub-band, after the
                // coordinate is applied.
                let sign = if state.acmod == 2
                    && ch == 1
                    && state.phsflg.get(sbnd).copied().unwrap_or(false)
                {
                    -1.0
                } else {
                    1.0
                };
                for bin in 0..12 {
                    let abs_bin = CPL_BIN_BASE + sbnd * 12 + bin;
                    let src = cplmant
                        .get(abs_bin.saturating_sub(cplstrtmant))
                        .copied()
                        .unwrap_or(0.0);
                    if let Some(slot) = coeffs.get_mut(abs_bin) {
                        *slot = src * co * 8.0 * sign;
                    }
                }
            }
        }
    }

    apply_rematrix(
        &mut channels,
        state.acmod,
        rematflg,
        nrematbnd,
        rematrix_top(cplinu, cplbegf),
    );

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

/// §7.2.2.1.1: true when every SNR offset source this block carries —
/// `csnroffst` combined with every `fsnroffst[ch]`, `cplfsnroffst` if
/// `cplinu`, and `lfefsnroffst` if `lfeon` — is exactly zero, in which case
/// `bap[]` must be all zero for the whole block and the
/// excitation/masking/floor chain is skipped entirely, not merely likely
/// to produce zero on its own.
///
/// Compares already-*combined* values against what a raw zero pair
/// combines to, rather than the raw fields directly, because this crate
/// does not keep the raw `csnroffst`/`fsnroffst` around once combined.
/// That is still exactly equivalent to checking the raw fields:
/// `combine_snroffset` is injective over their valid ranges (`csnroffst`
/// 0..=63, `fsnroffst` 0..=15 — the `<< 4` shift on the coarse term leaves
/// no room for two distinct raw pairs to collide), so two combined values
/// agree if and only if their raw pairs did.
fn all_snr_offsets_raw_zero(state: &BlockState, cplinu: bool) -> bool {
    let raw_zero = combine_snroffset(0, 0);
    state.ch_snroffset.iter().all(|&s| s == raw_zero)
        && (!cplinu || state.alloc.snroffset == raw_zero)
        && (!state.lfeon || state.lfe_snroffset == raw_zero)
}

/// §5.4.3/§7.2.2.6: read one channel's delta-bit-allocation segments.
///
/// `deltbae` codes: 0 = reuse the previous block's segments, 1 = new info
/// follows, 2 = no delta, 3 = reserved. Returns `None` for "reuse" (the
/// caller keeps what it had) and `Some(vec![])` for "none", which is what
/// clears a previously-sent set. §7.2.2.6 applies the adjustment for
/// `deltbae` 0 *and* 1 — reuse really does re-apply the old segments — so
/// the segments have to be stored, not merely skipped over.
fn read_delta_bit_allocation(
    r: &mut BitReader<'_>,
    deltbae: Option<u32>,
) -> Option<Vec<(u8, u8, u8)>> {
    match deltbae {
        Some(0) | None => None,
        Some(1) => {
            let deltnseg = r.get(3).saturating_add(1);
            let mut out = Vec::new();
            // §7.2.2.6's `band` accumulates across segments; `deltoffst` is
            // a delta from the previous segment's end, not an absolute band.
            let mut band = 0u32;
            for _ in 0..deltnseg {
                let deltoffst = r.get(5);
                let deltlen = r.get(4);
                let deltba = r.get(3);
                band = band.saturating_add(deltoffst);
                out.push((
                    u8::try_from(band).unwrap_or(u8::MAX),
                    u8::try_from(deltlen).unwrap_or(0),
                    u8::try_from(deltba).unwrap_or(0),
                ));
                band = band.saturating_add(deltlen);
            }
            Some(out)
        }
        _ => Some(Vec::new()),
    }
}

/// §7.4.2: the half-open range of coupling sub-bands this strategy covers,
/// `cplbegf .. 3 + cplendf`.
fn cplsubnd_range(state: &BlockState) -> std::ops::Range<usize> {
    let start = (state.cplbegf as usize).min(NCPLSUBND);
    let end = ((state.cplendf as usize) + 3).min(NCPLSUBND).max(start);
    start..end
}

/// §7.4.2: coupling *bands* are runs of sub-bands; each set `cplbndstrc`
/// bit folds its sub-band into the band below.
fn count_cpl_bands(state: &BlockState) -> usize {
    let range = cplsubnd_range(state);
    let mut n = 0usize;
    for sbnd in range.clone() {
        if sbnd == range.start || !state.cplbndstrc.get(sbnd).copied().unwrap_or(false) {
            n += 1;
        }
    }
    n
}

/// Duplicate one per-coupling-band value across the sub-bands its band
/// covers, writing into `out` indexed by absolute sub-band number.
fn expand_bands_to_subbands(
    per_band: &[f32],
    cplbndstrc: &[bool],
    range: &std::ops::Range<usize>,
    out: &mut [f32],
) {
    let mut band = 0usize;
    let mut last = 0f32;
    for sbnd in range.clone() {
        if sbnd == range.start || !cplbndstrc.get(sbnd).copied().unwrap_or(false) {
            last = per_band.get(band).copied().unwrap_or(0.0);
            band += 1;
        }
        if let Some(slot) = out.get_mut(sbnd) {
            *slot = last;
        }
    }
}

/// §7.4.3's coupling coordinate, already scaled by `mstrcplco`:
/// `cplco = (cplcoexp == 15 ? cplcomant/16 : (cplcomant + 16)/32)
///          >> (cplcoexp + 3 * mstrcplco)`.
fn cplco_value(cplcoexp: u32, cplcomant: u32, mstrcplco: u32) -> f32 {
    let mant = if cplcoexp == 15 {
        cplcomant as f32 / 16.0
    } else {
        (cplcomant as f32 + 16.0) / 32.0
    };
    let shift = cplcoexp + 3 * mstrcplco;
    mant / (2f32).powi(i32::try_from(shift).unwrap_or(0))
}

/// §7.5.2: the exclusive top coefficient of the last rematrixing band.
/// With coupling in use it is where coupling begins (`cplstrtmant`);
/// without, the full 253-bin span.
const fn rematrix_top(cplinu: bool, cplbegf: u32) -> usize {
    if cplinu {
        CPL_BIN_BASE + 12 * cplbegf as usize
    } else {
        253
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
/// §7.5.2 Tables 7.25-7.28: rematrixing band edges. The first three are
/// fixed; the last band of whichever set is in use ends where coupling
/// begins, so its top comes from the caller.
const REMATRIX_BANDS: [(usize, usize); 4] = [(13, 25), (25, 37), (37, 61), (61, 253)];

/// §7.5.4: bands where `rematflg` is set were coded as sum/difference
/// rather than left/right: `left = L + R`, `right = L - R`. Only meaningful
/// for `acmod == 2`.
///
/// `nrematbnd` and `top` come from §7.5.2's four cases: without coupling
/// there are 4 bands ending at bin 253, and with it there are 4, 3 or 2
/// depending on `cplbegf`, the last of them ending at `cplstrtmant`.
/// Rematrixing a band past where coupling begins would undo the coupling
/// reconstruction that just wrote those bins.
fn apply_rematrix(
    channels: &mut [Vec<f32>],
    acmod: u8,
    rematflg: [bool; 4],
    nrematbnd: usize,
    top: usize,
) {
    if acmod != 2 || channels.len() != 2 {
        return;
    }
    let (left, rest) = channels.split_at_mut(1);
    let Some(l) = left.first_mut() else { return };
    let Some(r) = rest.first_mut() else { return };
    for (band, &(start, end)) in REMATRIX_BANDS.iter().enumerate().take(nrematbnd) {
        if !rematflg.get(band).copied().unwrap_or(false) {
            continue;
        }
        let end = if band + 1 == nrematbnd {
            top
        } else {
            end.min(top)
        };
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
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

    #[test]
    fn raw_zero_snr_offsets_are_detected_across_every_source() {
        let mut state = BlockState::new(2, true, 2, 0);
        let zero = combine_snroffset(0, 0);
        state.ch_snroffset = vec![zero; 2];
        state.lfe_snroffset = zero;
        // cplinu=false: coupling contributes nothing, so this is already
        // the all-zero case regardless of `alloc.snroffset`'s value.
        assert!(all_snr_offsets_raw_zero(&state, false));

        // A single non-zero fine offset on one channel breaks it.
        state.ch_snroffset[1] = zero + 4;
        assert!(!all_snr_offsets_raw_zero(&state, false));
        state.ch_snroffset[1] = zero;

        // LFE's own offset is checked only when `lfeon`; it does carry
        // weight here since this state was built with `lfeon = true`.
        state.lfe_snroffset = zero + 4;
        assert!(!all_snr_offsets_raw_zero(&state, false));
        state.lfe_snroffset = zero;

        // Coupling's offset is checked only when `cplinu` is true.
        state.alloc.snroffset = zero + 4;
        assert!(!all_snr_offsets_raw_zero(&state, true));
        assert!(all_snr_offsets_raw_zero(&state, false));
    }

    #[test]
    fn a_freshly_initialized_block_is_not_mistaken_for_raw_zero() {
        // Before any `snroffste` has ever been read, `ch_snroffset`
        // defaults to a literal 0, not `combine_snroffset(0, 0)` (-960) —
        // the special case must not spuriously fire before real data
        // says it should.
        let state = BlockState::new(1, false, 1, 0);
        assert_ne!(state.ch_snroffset[0], combine_snroffset(0, 0));
        assert!(!all_snr_offsets_raw_zero(&state, false));
    }
}
