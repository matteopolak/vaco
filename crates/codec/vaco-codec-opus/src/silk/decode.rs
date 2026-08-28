//! One 10/20 ms SILK "regular frame": side-info indices, excitation, and
//! the LTP+LPC core synthesis filter. RFC 6716 §4.2.7-§4.2.8, from
//! `silk/decode_indices.c`, `decode_parameters.c`, `decode_pitch.c`,
//! `decode_core.c`, `decode_pulses.c`, `shell_coder.c`, `code_signs.c` and
//! `gain_quant.c`.
//!
//! # The scale convention this module (and [`crate::silk`]) uses
//!
//! Every sample here — excitation, LTP/LPC filter state, and the final
//! `xq` output — is carried in the reference's *un-normalized* PCM-adjacent
//! scale (roughly the range of a 16-bit sample), matching what
//! `decode_core.c` computes before the caller ever touches it. Dividing by
//! `32768` to reach this crate's normalized `f32` convention happens
//! exactly once, in [`crate::silk::SilkDecoder`]'s final output stage —
//! the same shape CELT's own `SCALEOUT` uses (see `crate::celt`'s module
//! doc). Working in this scale throughout, rather than normalizing every
//! intermediate value, is what lets the gain-change rescaling of the LPC
//! and LTP history below match the reference's `gain_adj_Q16` step
//! directly instead of re-deriving it through an extra unit conversion.

use crate::range::RangeDecoder;
use crate::silk::nlsf;
use crate::silk::tables::{self, NlsfCodebook};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    NoVoice,
    Unvoiced,
    Voiced,
}

impl SignalType {
    const fn from_raw(v: i32) -> Self {
        match v {
            2 => Self::Voiced,
            1 => Self::Unvoiced,
            _ => Self::NoVoice,
        }
    }
}

/// Whether a frame's gain/LSF are delta-coded from the previous SILK frame.
/// `silk_decode_indices`' `condCoding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondCoding {
    Independent,
    IndependentNoLtpScaling,
    Conditional,
}

/// Persistent per-(mono-)channel SILK state, carried across Opus packets.
#[derive(Debug, Clone)]
pub struct MonoState {
    pub fs_khz: i32,
    pub lpc_order: usize,
    pub nb_subfr: usize,
    pub subfr_length: usize,
    pub ltp_mem_length: usize,
    /// Decoded PCM history (this crate's PCM-adjacent scale), length
    /// `ltp_mem_length`. Shifted left by `frame_length` each frame.
    pub out_buf: Vec<f32>,
    /// `sLPC_Q14_buf`: the persistent `MAX_LPC_ORDER`-sample AR filter tail.
    pub lpc_history: [f32; 16],
    pub prev_nlsf_q15: Vec<i32>,
    pub prev_gain: f32,
    pub last_gain_index: i32,
    pub lag_prev: i32,
    pub ec_prev_lag_index: i32,
    pub ec_prev_signal_type: SignalType,
    pub first_frame_after_reset: bool,
}

impl MonoState {
    #[must_use]
    pub fn new(fs_khz: i32, nb_subfr: usize) -> Self {
        let lpc_order = if fs_khz == 16 { 16 } else { 10 };
        let subfr_length = 5 * fs_khz as usize;
        Self {
            fs_khz,
            lpc_order,
            nb_subfr,
            subfr_length,
            ltp_mem_length: 20 * fs_khz as usize,
            out_buf: vec![0.0; 20 * fs_khz as usize],
            lpc_history: [0.0; 16],
            prev_nlsf_q15: vec![0; lpc_order],
            prev_gain: 1.0,
            last_gain_index: 10,
            lag_prev: 100,
            ec_prev_lag_index: 0,
            ec_prev_signal_type: SignalType::NoVoice,
            first_frame_after_reset: true,
        }
    }

    fn codebook(&self) -> &'static NlsfCodebook {
        if self.lpc_order == 16 { &tables::NLSF_CB_WB } else { &tables::NLSF_CB_NB_MB }
    }

    /// Update the subframe count for a new packet's regular-SILK-frame
    /// duration (10 ms -> 2, 20/40/60 ms's own 20 ms units -> 4). This is
    /// *not* a reset: `out_buf`/`lpc_history`/`prev_gain`/`prev_nlsf_q15`/
    /// `last_gain_index` all stay valid across a frame-duration change --
    /// `subfr_length` (`5 * fs_khz`) and `ltp_mem_length` depend only on
    /// the sample rate, never on `nb_subfr`. Called every
    /// [`crate::silk::SilkDecoder::decode`] rather than cached from
    /// construction, since the SILK internal rate (the only thing
    /// `ensure_silk` watches for) can stay fixed while the frame duration
    /// changes mid-stream.
    pub fn set_nb_subfr(&mut self, nb_subfr: usize) {
        self.nb_subfr = nb_subfr;
    }
}

/// The decoded side-info for one regular frame.
struct Indices {
    signal_type: SignalType,
    quant_offset_type: usize,
    gains_raw: Vec<i32>,
    nlsf_cb1: usize,
    nlsf_residual: Vec<i32>,
    nlsf_interp_q2: i32,
    lag_index: i32,
    contour_index: i32,
    per_index: usize,
    ltp_index: Vec<usize>,
    ltp_scale_index: usize,
    seed: u32,
}

fn decode_indices(dec: &mut RangeDecoder<'_>, st: &mut MonoState, cond: CondCoding, decode_lbrr: bool, vad_active: bool) -> Indices {
    let ix = if decode_lbrr || vad_active {
        dec.icdf(&tables::TYPE_OFFSET_VAD_ICDF, 8).unwrap_or(0) + 2
    } else {
        dec.icdf(&tables::TYPE_OFFSET_NO_VAD_ICDF, 8).unwrap_or(0)
    };
    let signal_type = SignalType::from_raw(ix >> 1);
    let quant_offset_type = (ix & 1) as usize;

    let mut gains_raw = vec![0i32; st.nb_subfr];
    if cond == CondCoding::Conditional {
        gains_raw[0] = dec.icdf(&tables::DELTA_GAIN_ICDF, 8).unwrap_or(0);
    } else {
        let signal_idx = (signal_type as i32).clamp(0, 2) as usize;
        let msb = dec.icdf(&tables::GAIN_ICDF[signal_idx], 8).unwrap_or(0);
        let lsb = dec.icdf(&tables::UNIFORM8_ICDF, 8).unwrap_or(0);
        gains_raw[0] = (msb << 3) + lsb;
    }
    for g in gains_raw.iter_mut().skip(1) {
        *g = dec.icdf(&tables::DELTA_GAIN_ICDF, 8).unwrap_or(0);
    }

    let cb = st.codebook();
    let (nlsf_cb1, nlsf_residual) = nlsf::decode_nlsf_indices(dec, cb, signal_type == SignalType::Voiced);

    let nlsf_interp_q2 =
        if st.nb_subfr == 4 { dec.icdf(&tables::NLSF_INTERPOLATION_FACTOR_ICDF, 8).unwrap_or(4) } else { 4 };

    let mut lag_index = 0i32;
    let mut contour_index = 0i32;
    let mut per_index = 0usize;
    let mut ltp_index = vec![0usize; st.nb_subfr];
    let mut ltp_scale_index = 0usize;
    let mut ec_prev_lag_index = st.ec_prev_lag_index;
    if signal_type == SignalType::Voiced {
        let mut decode_absolute = true;
        if cond == CondCoding::Conditional && st.ec_prev_signal_type == SignalType::Voiced {
            let delta = dec.icdf(&tables::PITCH_DELTA_ICDF, 8).unwrap_or(0);
            if delta > 0 {
                lag_index = ec_prev_lag_index + (delta - 9);
                decode_absolute = false;
            }
        }
        if decode_absolute {
            let base = dec.icdf(&tables::PITCH_LAG_ICDF, 8).unwrap_or(0) * (st.fs_khz >> 1);
            let low_bits_icdf: &[u8] = match st.fs_khz {
                16 => &tables::UNIFORM8_ICDF,
                12 => &tables::UNIFORM6_ICDF,
                _ => &tables::UNIFORM4_ICDF,
            };
            let low = dec.icdf(low_bits_icdf, 8).unwrap_or(0);
            lag_index = base + low;
        }
        ec_prev_lag_index = lag_index;

        let contour_icdf: &[u8] = if st.fs_khz == 8 {
            if st.nb_subfr == 4 { &tables::PITCH_CONTOUR_NB_ICDF } else { &tables::PITCH_CONTOUR_10MS_NB_ICDF }
        } else if st.nb_subfr == 4 {
            &tables::PITCH_CONTOUR_ICDF
        } else {
            &tables::PITCH_CONTOUR_10MS_ICDF
        };
        contour_index = dec.icdf(contour_icdf, 8).unwrap_or(0);

        per_index = dec.icdf(&tables::LTP_PER_INDEX_ICDF, 8).unwrap_or(0).clamp(0, 2) as usize;
        let gain_icdf: &[u8] = match per_index {
            0 => &tables::LTP_GAIN_ICDF_0,
            1 => &tables::LTP_GAIN_ICDF_1,
            _ => &tables::LTP_GAIN_ICDF_2,
        };
        for v in &mut ltp_index {
            *v = dec.icdf(gain_icdf, 8).unwrap_or(0).max(0) as usize;
        }

        ltp_scale_index = if cond == CondCoding::Independent { dec.icdf(&tables::LTPSCALE_ICDF, 8).unwrap_or(0).max(0) as usize } else { 0 };
    }

    let seed = dec.icdf(&tables::UNIFORM4_ICDF, 8).unwrap_or(0).max(0) as u32;

    st.ec_prev_lag_index = ec_prev_lag_index;
    st.ec_prev_signal_type = signal_type;

    Indices {
        signal_type,
        quant_offset_type,
        gains_raw,
        nlsf_cb1,
        nlsf_residual,
        nlsf_interp_q2,
        lag_index,
        contour_index,
        per_index,
        ltp_index,
        ltp_scale_index,
        seed,
    }
}

/// `gain_quant.c`'s `silk_gains_dequant`.
fn gains_dequant(gains_raw: &[i32], prev_ind: &mut i32, conditional: bool) -> Vec<f32> {
    let mut out = Vec::new();
    for (k, &raw) in gains_raw.iter().enumerate() {
        if k == 0 && !conditional {
            *prev_ind = raw.max(*prev_ind - 16);
        } else {
            let ind_tmp = raw + tables::MIN_DELTA_GAIN_QUANT;
            let threshold = 2 * tables::MAX_DELTA_GAIN_QUANT - tables::N_LEVELS_QGAIN + *prev_ind;
            if ind_tmp > threshold {
                *prev_ind += 2 * ind_tmp - threshold;
            } else {
                *prev_ind += ind_tmp;
            }
        }
        *prev_ind = (*prev_ind).clamp(0, tables::N_LEVELS_QGAIN - 1);
        let log_db_q7 = (tables::MIN_QGAIN_DB * 128.0 / 6.0 + 16.0 * 128.0)
            + (*prev_ind as f32) * ((tables::MAX_QGAIN_DB - tables::MIN_QGAIN_DB) * 128.0 / 6.0) / (tables::N_LEVELS_QGAIN as f32 - 1.0);
        let log_db_q7 = log_db_q7.min(3967.0);
        // `silk_log2lin`'s result is `Gains_Q16` (real gain * 65536); this
        // module works in real units throughout (see the module doc), so
        // divide the Q16 scale straight back out.
        let g = 2f32.powf(log_db_q7 / 128.0) / 65536.0;
        out.push(g);
    }
    out
}

/// `decode_pitch.c`'s `silk_decode_pitch`.
fn decode_pitch(lag_index: i32, contour_index: i32, fs_khz: i32, nb_subfr: usize) -> Vec<i32> {
    let min_lag = tables::PE_MIN_LAG_MS * fs_khz;
    let max_lag = tables::PE_MAX_LAG_MS * fs_khz;
    let lag = min_lag + lag_index;
    let ci = contour_index.max(0) as usize;
    (0..nb_subfr)
        .map(|k| {
            let offset = if fs_khz == 8 {
                if nb_subfr == 4 {
                    i32::from(*tables::CB_LAGS_STAGE2.get(k).and_then(|r| r.get(ci)).unwrap_or(&0))
                } else {
                    i32::from(*tables::CB_LAGS_STAGE2_10MS.get(k).and_then(|r| r.get(ci)).unwrap_or(&0))
                }
            } else if nb_subfr == 4 {
                i32::from(*tables::CB_LAGS_STAGE3.get(k).and_then(|r| r.get(ci)).unwrap_or(&0))
            } else {
                i32::from(*tables::CB_LAGS_STAGE3_10MS.get(k).and_then(|r| r.get(ci)).unwrap_or(&0))
            };
            (lag + offset).clamp(min_lag, max_lag)
        })
        .collect()
}

/// `decode_pulses.c` + `shell_coder.c` + `code_signs.c`: the excitation
/// pulse signal for a whole SILK frame.
fn decode_excitation(dec: &mut RangeDecoder<'_>, signal_type: SignalType, quant_offset_type: usize, frame_length: usize, seed0: u32) -> Vec<i32> {
    let rate_row = usize::from(signal_type == SignalType::Voiced);
    let rate_level = dec.icdf(&tables::RATE_LEVELS_ICDF[rate_row], 8).unwrap_or(0).clamp(0, 9) as usize;

    let iter = frame_length.div_ceil(16);
    let mut sum_pulses = vec![0i32; iter];
    let mut n_lshifts = vec![0i32; iter];
    for i in 0..iter {
        let mut cdf: &[u8] = &tables::PULSES_PER_BLOCK_ICDF[rate_level.min(9)];
        let mut sp = dec.icdf(cdf, 8).unwrap_or(0);
        while sp == 17 {
            n_lshifts[i] += 1;
            let extra_row = &tables::PULSES_PER_BLOCK_ICDF[9];
            cdf = if n_lshifts[i] == 10 { extra_row.get(1..).unwrap_or(extra_row) } else { extra_row };
            sp = dec.icdf(cdf, 8).unwrap_or(0);
        }
        sum_pulses[i] = sp;
    }

    let mut pulses = vec![0i32; iter * 16];
    for (i, &sp) in sum_pulses.iter().enumerate() {
        if sp > 0 {
            let mut block = [0i32; 16];
            shell_decode(dec, &mut block, sp);
            if let Some(slot) = pulses.get_mut(i * 16..i * 16 + 16) {
                slot.copy_from_slice(&block);
            }
        }
    }

    for (i, &n_ls) in n_lshifts.iter().enumerate() {
        if n_ls > 0 {
            for k in 0..16 {
                let idx = i * 16 + k;
                let mut abs_q = pulses.get(idx).copied().unwrap_or(0);
                for _ in 0..n_ls {
                    abs_q <<= 1;
                    abs_q += dec.icdf(&tables::LSB_ICDF, 8).unwrap_or(0);
                }
                if let Some(slot) = pulses.get_mut(idx) {
                    *slot = abs_q;
                }
            }
        }
    }

    // Signs. `code_signs.c`'s `silk_decode_signs`: the row selector is
    // `quantOffsetType + 2*signalType` (not the other way around), and the
    // per-block index into that 7-entry row is `min(p & 0x1F, 6)` -- pulse
    // counts above 6 (up to `SILK_MAX_PULSES` before the escape path) would
    // otherwise read past the row into the next one.
    let sign_base = 7 * ((quant_offset_type + 2 * signal_type as usize).min(5));
    let table = tables::SIGN_ICDF.get(sign_base..sign_base + 7).unwrap_or(&tables::SIGN_ICDF[0..7]);
    for i in 0..iter {
        let p = sum_pulses[i] | (n_lshifts[i] << 5);
        if p <= 0 {
            continue;
        }
        let row = table.get((p & 0x1f).min(6) as usize).copied().unwrap_or(0);
        let icdf = [row, 0u8];
        for k in 0..16 {
            let idx = i * 16 + k;
            if pulses.get(idx).copied().unwrap_or(0) != 0 {
                let sign = dec.icdf(&icdf, 8).unwrap_or(0);
                if sign == 0
                    && let Some(slot) = pulses.get_mut(idx) {
                        *slot = -*slot;
                    }
            }
        }
    }

    pulses.truncate(frame_length);
    let _ = seed0;
    pulses
}

fn shell_decode(dec: &mut RangeDecoder<'_>, out: &mut [i32; 16], total: i32) {
    // `shell_coder.c`'s `silk_shell_decoder`: a depth-first traversal of the
    // 16-pulse binary tree, not a breadth-first one -- each `decode_split`
    // reads from the range coder in exactly this order, so the sequence
    // below must match the reference's call order verbatim rather than
    // grouping by tree level.
    let mut p3 = [0i32; 2];
    decode_split(&mut p3, dec, total, &tables::SHELL_CODE_TABLE3);

    let mut p2 = [0i32; 4];
    decode_split(&mut p2[0..2], dec, p3[0], &tables::SHELL_CODE_TABLE2);

    let mut p1 = [0i32; 8];
    decode_split(&mut p1[0..2], dec, p2[0], &tables::SHELL_CODE_TABLE1);
    decode_split(&mut out[0..2], dec, p1[0], &tables::SHELL_CODE_TABLE0);
    decode_split(&mut out[2..4], dec, p1[1], &tables::SHELL_CODE_TABLE0);

    decode_split(&mut p1[2..4], dec, p2[1], &tables::SHELL_CODE_TABLE1);
    decode_split(&mut out[4..6], dec, p1[2], &tables::SHELL_CODE_TABLE0);
    decode_split(&mut out[6..8], dec, p1[3], &tables::SHELL_CODE_TABLE0);

    decode_split(&mut p2[2..4], dec, p3[1], &tables::SHELL_CODE_TABLE2);

    decode_split(&mut p1[4..6], dec, p2[2], &tables::SHELL_CODE_TABLE1);
    decode_split(&mut out[8..10], dec, p1[4], &tables::SHELL_CODE_TABLE0);
    decode_split(&mut out[10..12], dec, p1[5], &tables::SHELL_CODE_TABLE0);

    decode_split(&mut p1[6..8], dec, p2[3], &tables::SHELL_CODE_TABLE1);
    decode_split(&mut out[12..14], dec, p1[6], &tables::SHELL_CODE_TABLE0);
    decode_split(&mut out[14..16], dec, p1[7], &tables::SHELL_CODE_TABLE0);
}

fn decode_split(out: &mut [i32], dec: &mut RangeDecoder<'_>, p: i32, table: &[u8]) {
    let (c1, c2);
    if p > 0 {
        let offset = usize::from(tables::SHELL_CODE_TABLE_OFFSETS.get(p as usize).copied().unwrap_or(0));
        let row = table.get(offset..).unwrap_or(&[0]);
        c1 = dec.icdf(row, 8).unwrap_or(0);
        c2 = p - c1;
    } else {
        c1 = 0;
        c2 = 0;
    }
    if let Some(a) = out.first_mut() {
        *a = c1;
    }
    if let Some(b) = out.get_mut(1) {
        *b = c2;
    }
}

/// The result of decoding one regular SILK frame: PCM in this module's
/// scale (see the module doc) at `st.fs_khz` kHz.
#[derive(Debug)]
pub struct FrameOutput {
    pub pcm: Vec<f32>,
    pub signal_type: SignalType,
}

/// `decode_frame.c`'s normal (non-PLC) path: indices, excitation,
/// parameters and the core synthesis filter, for one regular frame.
pub fn decode_frame(dec: &mut RangeDecoder<'_>, st: &mut MonoState, cond: CondCoding, vad_active: bool) -> FrameOutput {
    let ind = decode_indices(dec, st, cond, false, vad_active);
    let pulses = decode_excitation(dec, ind.signal_type, ind.quant_offset_type, st.nb_subfr * st.subfr_length, ind.seed);

    let conditional = cond != CondCoding::Independent;
    let mut prev_ind = st.last_gain_index;
    let gains = gains_dequant(&ind.gains_raw, &mut prev_ind, conditional);
    st.last_gain_index = prev_ind;

    let cb = st.codebook();
    let nlsf_curr = nlsf::nlsf_decode(cb, ind.nlsf_cb1, &ind.nlsf_residual);
    let interp = if st.first_frame_after_reset { 4 } else { ind.nlsf_interp_q2 };
    let nlsf0 = if interp < 4 { nlsf::interpolate_nlsf(&st.prev_nlsf_q15, &nlsf_curr, interp) } else { nlsf_curr.clone() };
    let a1 = nlsf::nlsf_to_lpc(&nlsf_curr, st.lpc_order);
    let a0 = if interp < 4 { nlsf::nlsf_to_lpc(&nlsf0, st.lpc_order) } else { a1.clone() };
    st.prev_nlsf_q15 = nlsf_curr;
    // This was the first frame after construction/reset (which forces
    // `interp=4`, matching `decode_parameters.c`'s own
    // `first_frame_after_reset` guard against interpolating from an
    // undefined previous NLSF) -- every later frame decodes its own
    // `nlsf_interp_q2` for real.
    st.first_frame_after_reset = false;

    let pitch_lags = if ind.signal_type == SignalType::Voiced {
        decode_pitch(ind.lag_index, ind.contour_index, st.fs_khz, st.nb_subfr)
    } else {
        vec![0; st.nb_subfr]
    };
    let ltp_cb: &[[i8; 5]] = match ind.per_index {
        0 => &tables::LTP_GAIN_VQ_0,
        1 => &tables::LTP_GAIN_VQ_1,
        _ => &tables::LTP_GAIN_VQ_2,
    };
    let ltp_taps: Vec<[f32; 5]> = ind
        .ltp_index
        .iter()
        .map(|&i| {
            let row = ltp_cb.get(i).copied().unwrap_or([0; 5]);
            let mut r = [0.0f32; 5];
            for (dst, &v) in r.iter_mut().zip(row.iter()) {
                *dst = f32::from(v) / 128.0;
            }
            r
        })
        .collect();
    let ltp_scale = if ind.signal_type == SignalType::Voiced {
        tables::LTP_SCALES_TABLE_Q14[ind.ltp_scale_index.min(2)]
    } else {
        0.0
    };

    let quant_offset = tables::QUANTIZATION_OFFSETS_Q10[usize::from(ind.signal_type == SignalType::Voiced)][ind.quant_offset_type];

    let pcm = synthesize(
        st,
        &pulses,
        ind.signal_type,
        quant_offset,
        ind.seed,
        &gains,
        &[a0, a1],
        interp < 4,
        &pitch_lags,
        &ltp_taps,
        ltp_scale,
    );

    FrameOutput { pcm, signal_type: ind.signal_type }
}

/// `decode_core.c`'s `silk_decode_core`.
///
/// The LPC recursion runs on a per-subframe buffer `sLPC` of
/// `order + subfr_length` entries — `sLPC[..order]` is the persistent tail
/// carried from the previous subframe (`st.lpc_history`, rescaled by
/// `gain_adj` exactly as the reference does when the gain changes between
/// subframes — see the module doc for why that rescaling is faithfully
/// reproduced even though this decoder does not chase bit-exactness
/// elsewhere), and `sLPC[order + i]` is this subframe's `i`-th pre-gain
/// synthesis value, indexed directly for the feedback recursion rather
/// than reconstructed from the post-gain `xq` output.
///
/// The LTP delay line (`ltp_old`/`ltp_new` below) mirrors the reference's
/// `sLTP_Q15`: a buffer re-whitened from the persistent `out_buf` history
/// only at subframe 0, or at subframe 2 when NLSF interpolation is active
/// (`nlsf_interpolating` -- the point where the LPC coefficients actually
/// change), and merely rescaled by the gain ratio on every other subframe
/// whose gain changed. Re-deriving it from `out_buf` on *every* subframe
/// (an earlier version of this function did) reads the wrong history for
/// subframes that share the previous subframe's LPC coefficients.
#[expect(clippy::too_many_arguments, reason = "mirrors silk_decode_core's parameter set")]
fn synthesize(
    st: &mut MonoState,
    pulses: &[i32],
    signal_type: SignalType,
    quant_offset_q10: f32,
    seed0: u32,
    gains: &[f32],
    ab: &[Vec<f32>; 2],
    nlsf_interpolating: bool,
    pitch_lags: &[i32],
    ltp_taps: &[[f32; 5]],
    ltp_scale: f32,
) -> Vec<f32> {
    // `LTP_ORDER / 2` = 2 (integer division of the reference's 5-tap
    // filter order): the reference's re-whitening depth around the lag.
    const LTP_ORDER_HALF: usize = 2;

    let frame_length = st.nb_subfr * st.subfr_length;
    let order = st.lpc_order;

    // Excitation, `decode_core.c`'s first loop.
    let mut exc = vec![0.0f32; frame_length];
    let mut seed = seed0 as i32;
    for (i, &p) in pulses.iter().enumerate().take(frame_length) {
        seed = seed.wrapping_mul(196_314_165).wrapping_add(907_633_515);
        let mut e = p as f32;
        if e > 0.0 {
            e -= tables::QUANT_LEVEL_ADJUST;
        } else if e < 0.0 {
            e += tables::QUANT_LEVEL_ADJUST;
        }
        e += quant_offset_q10;
        if seed < 0 {
            e = -e;
        }
        if let Some(slot) = exc.get_mut(i) {
            *slot = e;
        }
        seed = seed.wrapping_add(p);
    }

    let mut xq = vec![0.0f32; frame_length];
    let mut lpc_hist = st.lpc_history;
    let mut prev_gain = st.prev_gain;

    let max_lag = pitch_lags.iter().copied().max().unwrap_or(0).max(0) as usize;
    let old_len = max_lag + LTP_ORDER_HALF;
    // `ltp_old`/`ltp_new` together mirror `sLTP_Q15`: `ltp_old` is the
    // "old" region re-whitened from `out_buf` at the appropriate subframe
    // boundaries (see the function doc), `ltp_new` is the growing region
    // of this call's own LTP-predicted residual, indexed contiguously
    // after `ltp_old` (`ltp_new[0]` is `sLTP_Q15[old_len]`).
    let mut ltp_old = vec![0.0f32; old_len];
    let mut ltp_new: Vec<f32> = Vec::new();
    let read_ltp = |pos: i64, old: &[f32], new: &[f32]| -> f32 {
        if pos < 0 {
            return 0.0;
        }
        let pos = pos as usize;
        if pos < old.len() { old.get(pos).copied().unwrap_or(0.0) } else { new.get(pos - old.len()).copied().unwrap_or(0.0) }
    };

    for k in 0..st.nb_subfr {
        let a = ab.get(usize::from(k >= st.nb_subfr / 2)).unwrap_or(&ab[1]);
        let gain = gains.get(k).copied().unwrap_or(1.0).max(1e-6);
        let gain_changed = (gain - prev_gain).abs() > f32::EPSILON;
        let gain_adj = prev_gain / gain;
        if gain_changed {
            for v in &mut lpc_hist {
                *v *= gain_adj;
            }
        }

        let sub_start = k * st.subfr_length;
        let lag = pitch_lags.get(k).copied().unwrap_or(0).max(0) as usize;
        let taps = ltp_taps.get(k).copied().unwrap_or([0.0; 5]);

        if signal_type == SignalType::Voiced && lag > 0 {
            let need = (lag + LTP_ORDER_HALF).min(old_len);
            // Re-whiten only where the LPC coefficients actually change:
            // subframe 0 always, and subframe 2 only when this frame
            // interpolates NLSFs (`decode_core.c`'s
            // `k == 0 || (k == 2 && NLSF_interpolation_flag)`). Every other
            // subframe reuses the last re-whitened tail, rescaled for any
            // gain change since then -- it is never re-derived from
            // `out_buf`, which would use the wrong (already-superseded)
            // history for a subframe whose LPC coefficients didn't change.
            if k == 0 || (k == 2 && nlsf_interpolating) {
                let inv_gain = if k == 0 { ltp_scale / gain } else { 1.0 / gain };
                let mut hist = Vec::new();
                hist.extend_from_slice(&st.out_buf);
                hist.extend_from_slice(&xq[..sub_start]);
                let hist_len = hist.len();
                for i in 0..need {
                    let pos = hist_len - need + i;
                    let mut pred = 0.0f32;
                    for (j, &aj) in a.iter().enumerate() {
                        // `a` (from `nlsf::nlsf_to_lpc`) is already a
                        // real-valued coefficient in this module's
                        // PCM-adjacent scale, not a raw Q12 integer --
                        // unlike the reference's `A_Q12`, it must not be
                        // descaled by 4096 again here.
                        pred += aj * hist.get(pos.wrapping_sub(1 + j)).copied().unwrap_or(0.0);
                    }
                    let residual = hist.get(pos).copied().unwrap_or(0.0) - pred;
                    if let Some(slot) = ltp_old.get_mut(old_len - need + i) {
                        *slot = residual * inv_gain;
                    }
                }
            } else if gain_changed {
                let start = old_len.saturating_sub(need);
                if let Some(slice) = ltp_old.get_mut(start..) {
                    for v in slice {
                        *v *= gain_adj;
                    }
                }
            }
        }

        // `sLPC`: `order` samples of persistent tail, then this
        // subframe's `subfr_length` pre-gain synthesis values.
        let mut s_lpc = vec![0.0f32; order + st.subfr_length];
        if let Some(slot) = s_lpc.get_mut(..order) {
            slot.copy_from_slice(&lpc_hist[..order]);
        }

        for i in 0..st.subfr_length {
            let e = exc.get(sub_start + i).copied().unwrap_or(0.0);
            let res = if signal_type == SignalType::Voiced && lag > 0 {
                // `base` is `sLTP_buf_idx` at this sample, before this
                // sample's own residual is appended; taps read offsets
                // `{+2,+1,0,-1,-2}` around `base - lag`.
                let base = (old_len + ltp_new.len()) as i64;
                let mut ltp_pred = 0.0f32;
                for (t, &b) in taps.iter().enumerate() {
                    let pos = base - lag as i64 + LTP_ORDER_HALF as i64 - t as i64;
                    ltp_pred += b * read_ltp(pos, &ltp_old, &ltp_new);
                }
                let r = e + ltp_pred;
                ltp_new.push(r);
                r
            } else {
                e
            };

            let mut pred = 0.0f32;
            for (j, &aj) in a.iter().enumerate() {
                // Same real-scale `a` as above -- no `/4096.0` here either.
                pred += aj * s_lpc.get(order + i - 1 - j).copied().unwrap_or(0.0);
            }
            let sample = res + pred;
            if let Some(slot) = s_lpc.get_mut(order + i) {
                *slot = sample;
            }
            let out = (sample * gain).round().clamp(-32768.0, 32767.0);
            if let Some(slot) = xq.get_mut(sub_start + i) {
                *slot = out;
            }
        }

        // The new persistent tail: this subframe's last `order` pre-gain
        // values.
        let tail_start = s_lpc.len().saturating_sub(order);
        if let (Some(dst), Some(src)) = (lpc_hist.get_mut(..order), s_lpc.get(tail_start..)) {
            let n = dst.len().min(src.len());
            dst[..n].copy_from_slice(&src[..n]);
        }
        prev_gain = gain;
    }

    st.lpc_history = lpc_hist;
    st.prev_gain = prev_gain;
    xq
}
