//! CELT decode: the transform-coded half of Opus. RFC 6716 §4.3, from
//! `celt/celt.c`'s `celt_decode_with_ec`.
//!
//! # What is implemented
//!
//! The full per-frame syntax (silence flag, post-filter fields, transient
//! flag, coarse/fine/final energy, `tf_select`, spreading, dynalloc, trim,
//! anti-collapse reservation, the band allocator and PVQ shape decode) and
//! the IMDCT/overlap-add synthesis with de-emphasis.
//!
//! # Known gap: the post-filter (comb filter) is read but not applied
//!
//! The post-filter's `octave`/`period`/`gain`/`tapset` fields are decoded
//! so the entropy coder stays in sync — skipping the *read* would desync
//! every symbol after it — but the filter itself (a 5-tap comb applied to
//! the reconstructed signal, RFC 6716 4.3.7.2) is not applied. It only
//! sharpens low-bitrate voiced content; leaving it off yields a fully
//! decodable, correctly-timed signal that is measurably less crisp exactly
//! where the reference would apply it. Noted rather than chased, per this
//! batch's scope.

pub mod bands;
pub mod energy;
pub mod pvq;
pub mod rate;
pub mod tables;

use std::collections::HashMap;
use std::sync::Arc;

use tables::{EBANDS, NB_EBANDS, WINDOW120};
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind};

use crate::range::{BITRES, RangeDecoder};

const OVERLAP: usize = 120;
/// De-emphasis (and pre-emphasis) filter coefficients for 48 kHz. `celt.c`'s
/// `mode->preemph`, the `Fs>=44100` row.
const PREEMPH: [f32; 4] = [0.850_006_1, 0.0, 1.0, 1.0];
/// `celt.c`'s `SCALEOUT`: the float build's signal-domain-to-normalized-PCM
/// scale, `1/CELT_SIG_SCALE`.
const CELT_OUT_SCALE: f32 = 1.0 / 32768.0;

/// Per-channel decode state that must persist across frames: MDCT overlap
/// memory, band-energy history (for the coarse-energy predictor and
/// anti-collapse) and the de-emphasis filter memory.
#[derive(Debug, Clone)]
struct ChannelState {
    overlap_mem: Vec<f32>,
    preemph_mem: f32,
}

impl ChannelState {
    fn new() -> Self {
        Self { overlap_mem: vec![0.0; OVERLAP], preemph_mem: 0.0 }
    }
}

/// The CELT decoder. One instance per Opus decoded stream (mono or stereo);
/// a multistream Opus file drives one per embedded Opus stream.
#[derive(Debug)]
pub struct CeltDecoder {
    channels: Vec<ChannelState>,
    old_band_e: Vec<f32>,
    old_log_e: Vec<f32>,
    old_log_e2: Vec<f32>,
    rng: u32,
    plans: HashMap<usize, Arc<Plan<f32>>>,
}

impl CeltDecoder {
    /// A fresh decoder for `channels` (1 or 2) coded channels.
    #[must_use]
    pub fn new(channels: usize) -> Self {
        let channels = channels.clamp(1, 2);
        Self {
            channels: (0..channels).map(|_| ChannelState::new()).collect(),
            old_band_e: vec![0.0; 2 * NB_EBANDS],
            old_log_e: vec![-28.0; 2 * NB_EBANDS],
            old_log_e2: vec![-28.0; 2 * NB_EBANDS],
            rng: 0,
            plans: HashMap::new(),
        }
    }

    /// Discard history after a seek or a gap; configuration (channel count)
    /// is unaffected.
    pub fn reset(&mut self) {
        for c in &mut self.channels {
            *c = ChannelState::new();
        }
        self.old_band_e.fill(0.0);
        self.old_log_e.fill(-28.0);
        self.old_log_e2.fill(-28.0);
        self.rng = 0;
    }

    /// The cached IMDCT plan for a `full_len`-sample transform (`240`,
    /// `480`, `960` or `1920` — one per CELT frame size this crate ever
    /// builds). `None` only if `vaco-tx` rejects one of those four fixed
    /// lengths, which would be a `vaco-tx` defect rather than anything
    /// bitstream-dependent; the caller falls back to silence for that
    /// channel rather than panicking.
    fn imdct_plan(&mut self, full_len: usize) -> Option<Arc<Plan<f32>>> {
        if let Some(p) = self.plans.get(&full_len) {
            return Some(Arc::clone(p));
        }
        let plan = Plan::<f32>::new(TxKind::Mdct, Direction::Inverse, full_len, 1.0, TxFlags::FULL_IMDCT).ok()?;
        self.plans.insert(full_len, Arc::clone(&plan));
        Some(plan)
    }

    /// Decode one CELT frame's payload into `channels` (1 or 2) planes of
    /// `frame_size` samples each. `start_band`/`end_band` restrict the
    /// coded spectrum (hybrid mode starts CELT above the SILK/CELT
    /// crossover). `dec` is shared with SILK inside a hybrid frame — CELT
    /// picks up wherever SILK's fields left the range decoder.
    pub fn decode(
        &mut self,
        dec: &mut RangeDecoder<'_>,
        len_bytes: usize,
        frame_size: usize,
        channels: usize,
        start_band: usize,
        end_band: usize,
    ) -> Vec<Vec<f32>> {
        let channels = channels.clamp(1, 2);
        if self.channels.len() != channels {
            self.channels = (0..channels).map(|_| ChannelState::new()).collect();
        }
        let end_band = end_band.min(NB_EBANDS).max(start_band + 1);

        // Frame size must be one of {120,240,480,960} << 0; pick LM.
        let mut lm = 0i32;
        while lm <= 3 && 120usize << lm != frame_size {
            lm += 1;
        }
        let lm = lm.min(3);
        let m = 1usize << lm;
        let n = m * 120;

        let total_bits_bytes = len_bytes as i32 * 8;
        let mut tell = dec.tell();

        let silence = if tell >= total_bits_bytes {
            true
        } else if tell == 1 {
            dec.bit_logp(15)
        } else {
            false
        };
        if silence {
            tell = len_bytes as i32 * 8;
        }

        // Post-filter fields: read to stay in sync, not applied (see module doc).
        if start_band == 0 && tell + 16 <= total_bits_bytes {
            if dec.bit_logp(1) {
                let octave = dec.dec_uint(6).unwrap_or(0);
                let _period = (16u32 << octave) + dec.dec_bits(4 + octave) - 1;
                let _qg = dec.dec_bits(3);
                if dec.tell() + 2 <= total_bits_bytes {
                    let _tapset = dec.icdf(&tables::TAPSET_ICDF, 2).unwrap_or(0);
                }
            }
            tell = dec.tell();
        }

        let is_transient = if lm > 0 && tell + 3 <= total_bits_bytes {
            let v = dec.bit_logp(3);
            tell = dec.tell();
            v
        } else {
            false
        };
        let short_blocks = if is_transient { m } else { 0 };

        let intra_ener = if tell + 3 <= total_bits_bytes { dec.bit_logp(3) } else { false };
        energy::unquant_coarse_energy(dec, &mut self.old_band_e, NB_EBANDS, start_band, end_band, intra_ener, channels, lm as usize);

        let mut tf_res = vec![0i32; NB_EBANDS];
        tf_decode(dec, start_band, end_band, is_transient, &mut tf_res, lm, total_bits_bytes);

        tell = dec.tell();
        let spread = if tell + 4 <= total_bits_bytes { dec.icdf(&tables::SPREAD_ICDF, 5).unwrap_or(2) } else { 2 };

        let cap = rate::init_caps(&EBANDS, lm, channels as i32);
        let mut offsets = vec![0i32; NB_EBANDS];
        let mut dynalloc_logp = 6i32;
        let mut total_bits_q3 = total_bits_bytes << BITRES;
        tell = dec.tell_frac();
        for i in start_band..end_band {
            let width = ((channels as i32) * i32::from(EBANDS[i + 1] - EBANDS[i])) << lm;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut dynalloc_loop_logp = dynalloc_logp;
            let mut boost = 0i32;
            while tell + (dynalloc_loop_logp << BITRES) < total_bits_q3 && boost < cap.get(i).copied().unwrap_or(0) {
                if !dec.bit_logp(dynalloc_loop_logp as u32) {
                    tell = dec.tell_frac();
                    break;
                }
                tell = dec.tell_frac();
                boost += quanta;
                total_bits_q3 -= quanta;
                dynalloc_loop_logp = 1;
            }
            offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = 2.max(dynalloc_logp - 1);
            }
        }

        let alloc_trim = if tell + (6 << BITRES) <= total_bits_q3 { dec.icdf(&tables::TRIM_ICDF, 7).unwrap_or(5) } else { 5 };

        let mut bits = ((len_bytes as i32 * 8) << BITRES) - dec.tell_frac() - 1;
        let anti_collapse_rsv = i32::from(is_transient && lm >= 2 && bits >= (lm + 2) << BITRES) << BITRES;
        bits -= anti_collapse_rsv;

        let alloc = rate::compute_allocation(dec, &EBANDS, start_band, end_band, &offsets, &cap, alloc_trim, bits, channels as i32, lm);

        energy::unquant_fine_energy(dec, &mut self.old_band_e, NB_EBANDS, start_band, end_band, channels, &alloc.fine_energy);

        let mut x = vec![0.0f32; channels * n];
        let mut collapse_masks = vec![0u8; NB_EBANDS * channels];
        {
            let (x0, x1) = x.split_at_mut(n);
            let y_opt = if channels == 2 { Some(x1) } else { None };
            bands::quant_all_bands(
                dec,
                start_band,
                end_band,
                x0,
                y_opt,
                &mut collapse_masks,
                &alloc.pulses,
                short_blocks,
                spread,
                alloc.dual_stereo,
                alloc.intensity,
                &tf_res,
                len_bytes as i32 * (8 << BITRES) - anti_collapse_rsv,
                alloc.balance,
                lm,
                alloc.coded_bands,
                &mut self.rng,
            );
        }

        let anti_collapse_on = if anti_collapse_rsv > 0 { dec.dec_bits(1) != 0 } else { false };

        let bits_left = len_bytes as i32 * 8 - dec.tell();
        energy::unquant_energy_finalise(
            dec,
            &mut self.old_band_e,
            NB_EBANDS,
            start_band,
            end_band,
            channels,
            &alloc.fine_energy,
            &alloc.fine_priority,
            bits_left,
        );

        if anti_collapse_on {
            anti_collapse(&mut x, &collapse_masks, lm, channels, n, start_band, end_band, &self.old_band_e, &self.old_log_e, &self.old_log_e2, &alloc.pulses, &mut self.rng);
        }

        let mut band_e = vec![0.0f32; channels * NB_EBANDS];
        energy::log2_amp(&self.old_band_e, &mut band_e, NB_EBANDS, start_band, end_band, channels);

        if silence {
            band_e.fill(0.0);
            self.old_band_e.fill(-28.0);
        }

        // Denormalise: freq[c*n+i] = X[c*n+i] * bandE[band(i), c].
        let mut freq = vec![0.0f32; channels * n];
        for c in 0..channels {
            for i in start_band..end_band.min(NB_EBANDS) {
                let s = m * usize::from(EBANDS[i] as u16);
                let e = m * usize::from(EBANDS[i + 1] as u16);
                let gain = band_e.get(i + c * NB_EBANDS).copied().unwrap_or(0.0);
                for j in s..e {
                    if let (Some(&xv), Some(slot)) = (x.get(j + c * n), freq.get_mut(j + c * n)) {
                        *slot = xv * gain;
                    }
                }
            }
        }

        // IMDCT + windowed overlap-add per channel.
        let mut out: Vec<Vec<f32>> = Vec::new();
        let n2 = if short_blocks != 0 { 120 } else { n };
        let b_count = if short_blocks != 0 { short_blocks } else { 1 };
        let full_len = 2 * n2;
        let Some(plan) = self.imdct_plan(full_len) else {
            // See `imdct_plan`'s doc: unreachable for the four fixed sizes
            // this crate ever requests, but silence beats a panic.
            return (0..channels).map(|_| vec![0.0f32; n]).collect();
        };
        let mut tx = Tx::new(plan);
        let n4 = n2 / 2;
        let half_overlap = OVERLAP / 2;
        for c in 0..channels {
            let mut acc = vec![0.0f32; n + OVERLAP];
            let mut coeffs = vec![0.0f32; n2];
            let mut y = vec![0.0f32; full_len];
            for b in 0..b_count {
                for (k, slot) in coeffs.iter_mut().enumerate() {
                    let idx = b + k * b_count;
                    *slot = freq.get(idx + c * n).copied().unwrap_or(0.0);
                }
                tx.execute(&mut y, &coeffs);
                // `y` is the raw (unwindowed) `full_len`-sample IMDCT of this
                // subframe's spectrum, computed via `vaco_tx`'s plain
                // FULL_IMDCT. `celt/mdct.c`'s `clt_mdct_backward` computes an
                // equivalent quantity through a faster N/4-point-FFT path
                // that only ever materializes `n2 + overlap` samples, but
                // the two agree exactly (mod the reference's own small-angle
                // `sine ~= angle` approximation) via the identity
                // `f2[idx] = y[n4 + idx]` for `idx` in `0..n2`, where `f2` is
                // that function's de-shuffled intermediate -- verified
                // numerically against a literal transliteration of its
                // pointer-walk. That identity lets this decoder reuse a
                // plain full IMDCT and still reproduce
                // `clt_mdct_backward`'s exact windowed output (the two
                // "mirror on both sides for TDAC" loops in `celt/mdct.c`).
                let g = |idx: usize| -> f32 { y.get(n4 + idx).copied().unwrap_or(0.0) };
                let base = b * n2;
                // Flat (unwindowed) middle: x[p] = g(p - overlap/2).
                for p in OVERLAP..n2 {
                    if let (Some(slot), gv) = (acc.get_mut(base + p), g(p - half_overlap)) {
                        *slot += gv;
                    }
                }
                // Leading edge: accumulates onto the previous subframe's (or
                // previous Opus frame's, via `overlap_mem`) trailing edge.
                for m in 0..half_overlap {
                    let gv = g(half_overlap - 1 - m);
                    let w_lo = WINDOW120.get(m).copied().unwrap_or(0.0);
                    let w_hi = WINDOW120.get(OVERLAP - 1 - m).copied().unwrap_or(0.0);
                    if let Some(slot) = acc.get_mut(base + m) {
                        *slot += -w_lo * gv;
                    }
                    if let Some(slot) = acc.get_mut(base + OVERLAP - 1 - m) {
                        *slot += w_hi * gv;
                    }
                }
                // Trailing edge: a plain overwrite of this subframe's own
                // tail, to be accumulated into by whatever reads it next.
                for m in 0..half_overlap {
                    let gv = g(n2 - half_overlap + m);
                    let w_lo = WINDOW120.get(m).copied().unwrap_or(0.0);
                    let w_hi = WINDOW120.get(OVERLAP - 1 - m).copied().unwrap_or(0.0);
                    if let Some(slot) = acc.get_mut(base + n2 + m) {
                        *slot = w_hi * gv;
                    }
                    if let Some(slot) = acc.get_mut(base + n2 + OVERLAP - 1 - m) {
                        *slot = w_lo * gv;
                    }
                }
            }
            let chan_idx = c.min(self.channels.len() - 1);
            let state = &mut self.channels[chan_idx];
            let mut pcm = vec![0.0f32; n];
            for j in 0..OVERLAP.min(n) {
                pcm[j] = acc[j] + state.overlap_mem.get(j).copied().unwrap_or(0.0);
            }
            if let (Some(dst), Some(src)) = (pcm.get_mut(OVERLAP.min(n)..n), acc.get(OVERLAP.min(n)..n)) {
                dst.copy_from_slice(src);
            }
            for j in 0..OVERLAP {
                if let Some(slot) = state.overlap_mem.get_mut(j) {
                    *slot = acc.get(n + j).copied().unwrap_or(0.0);
                }
            }
            // De-emphasis, RFC 6716 4.3.7.3 (`celt.c`'s `deemphasis`).
            let mut mem = state.preemph_mem;
            for v in &mut pcm {
                let tmp = *v + mem;
                mem = PREEMPH[0] * tmp - PREEMPH[1] * *v;
                *v = PREEMPH[3] * tmp * 4.0 * CELT_OUT_SCALE;
            }
            state.preemph_mem = mem;
            out.push(pcm);
        }
        out
    }
}

/// `celt.c`'s `tf_decode`.
fn tf_decode(dec: &mut RangeDecoder<'_>, start: usize, end: usize, is_transient: bool, tf_res: &mut [i32], lm: i32, total_bits: i32) {
    let mut tell = dec.tell();
    let mut logp = if is_transient { 2 } else { 4 };
    let tf_select_rsv = i32::from(lm > 0 && tell + logp < total_bits);
    let budget = total_bits - tf_select_rsv;
    let mut curr = false;
    let mut tf_changed = false;
    for slot in tf_res.iter_mut().take(end).skip(start) {
        if tell + logp <= budget {
            let bit = dec.bit_logp(logp as u32);
            curr ^= bit;
            tell = dec.tell();
            tf_changed |= curr;
        }
        *slot = i32::from(curr);
        logp = if is_transient { 4 } else { 5 };
    }
    let mut tf_select = 0usize;
    if tf_select_rsv != 0 {
        let row = &tables::TF_SELECT_TABLE[lm.clamp(0, 3) as usize];
        let a = row[4 * usize::from(is_transient) + usize::from(tf_changed)];
        let b = row[4 * usize::from(is_transient) + 2 + usize::from(tf_changed)];
        if a != b {
            tf_select = usize::from(dec.bit_logp(1));
        }
    }
    let row = &tables::TF_SELECT_TABLE[lm.clamp(0, 3) as usize];
    for slot in tf_res.iter_mut().take(end).skip(start) {
        let idx = 4 * usize::from(is_transient) + 2 * tf_select + (*slot as usize);
        *slot = i32::from(row.get(idx).copied().unwrap_or(0));
    }
}

/// `bands.c`'s `anti_collapse`: refill bands whose PVQ shape collapsed to
/// all-zero under a short transient with low-level noise, so the energy
/// this frame's coarse/fine decode already committed to is not silently
/// lost.
#[expect(clippy::too_many_arguments, reason = "mirrors celt/bands.c's anti_collapse")]
fn anti_collapse(
    x: &mut [f32],
    collapse_masks: &[u8],
    lm: i32,
    channels: usize,
    size: usize,
    start: usize,
    end: usize,
    log_e: &[f32],
    prev1_log_e: &[f32],
    prev2_log_e: &[f32],
    pulses: &[i32],
    seed: &mut u32,
) {
    for i in start..end {
        let n0 = i32::from(EBANDS[i + 1] - EBANDS[i]);
        if n0 <= 0 {
            continue;
        }
        let depth = (1 + pulses.get(i).copied().unwrap_or(0)) / (n0 << lm).max(1);
        let thresh = 0.5 * (-0.125 * depth as f32).exp2();
        let sqrt1 = 1.0 / ((n0 << lm) as f32).sqrt();
        for c in 0..channels {
            let mut prev1 = prev1_log_e.get(c * NB_EBANDS + i).copied().unwrap_or(-28.0);
            let mut prev2 = prev2_log_e.get(c * NB_EBANDS + i).copied().unwrap_or(-28.0);
            if channels == 1 {
                prev1 = prev1.max(prev1_log_e.get(NB_EBANDS + i).copied().unwrap_or(-28.0));
                prev2 = prev2.max(prev2_log_e.get(NB_EBANDS + i).copied().unwrap_or(-28.0));
            }
            let e_diff = (log_e.get(c * NB_EBANDS + i).copied().unwrap_or(-28.0) - prev1.min(prev2)).max(0.0);
            let mut r = 2.0 * (-e_diff).exp2();
            if lm == 3 {
                r *= std::f32::consts::SQRT_2;
            }
            r = r.min(thresh) * sqrt1;
            let base = c * size + (usize::from(EBANDS[i] as u16) << lm);
            let mut renorm = false;
            for k in 0..(1usize << lm) {
                let mask = collapse_masks.get(i * channels + c).copied().unwrap_or(0);
                if mask & (1 << k) == 0 {
                    for j in 0..n0 as usize {
                        *seed = 1_664_525u32.wrapping_mul(*seed).wrapping_add(1_013_904_223);
                        let v = if *seed & 0x8000 != 0 { r } else { -r };
                        if let Some(slot) = x.get_mut(base + (j << lm) + k) {
                            *slot = v;
                        }
                    }
                    renorm = true;
                }
            }
            if renorm {
                let n = (n0 << lm) as usize;
                if let Some(slice) = x.get_mut(base..base + n) {
                    pvq::renormalise_vector(slice, 1.0);
                }
            }
        }
    }
}
