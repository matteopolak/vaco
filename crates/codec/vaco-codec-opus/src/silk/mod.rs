//! SILK decode: the speech-oriented half of Opus. RFC 6716 §4.2, from
//! `silk/dec_API.c`'s `silk_decode` (the multi-subframe-per-packet and
//! stereo orchestration) and `silk/decode_frame.c` (one regular frame).
//!
//! # Known gaps
//!
//! - **PLC/FEC is not the reference's LPC-extrapolation concealment.**
//!   [`SilkDecoder::decode`] always takes the "normal" path; a lost packet
//!   is handled by the caller (see `crate::lib`'s packet-loss handling)
//!   rather than by re-deriving a lost frame's spectral envelope here.
//! - **LBRR (in-band FEC redundancy) frames are decoded and discarded**,
//!   not stored for later loss recovery — correct for keeping the entropy
//!   coder in sync (RFC 6716 4.2.3's redundancy data is still real
//!   range-coded content that has to be consumed), but this crate never
//!   uses the recovered audio the way `-fec` decoding would.
//! - **Resampling from SILK's internal rate (8/12/16 kHz) to 48 kHz** uses
//!   an original windowed-sinc polyphase upsampler (`crate::silk::resample`),
//!   not the reference's own filter design — RFC 6716 does not mandate a
//!   specific one (silk/resampler.c's tables are an implementation choice,
//!   not part of the bitstream contract).

pub mod decode;
pub mod nlsf;
pub mod tables;

use crate::range::RangeDecoder;
use decode::{CondCoding, MonoState};

/// Bandwidth determines SILK's internal sample rate: 8/12/16 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalRate {
    Narrowband,
    Mediumband,
    Wideband,
}

impl InternalRate {
    #[must_use]
    pub const fn khz(self) -> i32 {
        match self {
            Self::Narrowband => 8,
            Self::Mediumband => 12,
            Self::Wideband => 16,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct StereoState {
    pred_prev_q13: [f32; 2],
    s_mid: [f32; 2],
    prev_decode_only_middle: bool,
}

/// The SILK decoder for one Opus stream (mono or stereo).
#[derive(Debug)]
pub struct SilkDecoder {
    channels: usize,
    rate: InternalRate,
    nb_subfr: usize,
    mono: Vec<MonoState>,
    stereo: StereoState,
}

impl SilkDecoder {
    #[must_use]
    pub fn new(channels: usize, rate: InternalRate, frame_ms: u32) -> Self {
        let channels = channels.clamp(1, 2);
        let nb_subfr = if frame_ms <= 10 { 2 } else { 4 };
        Self {
            channels,
            rate,
            nb_subfr,
            mono: (0..channels).map(|_| MonoState::new(rate.khz(), nb_subfr)).collect(),
            stereo: StereoState::default(),
        }
    }

    /// Reconfigure for a new bandwidth/channel count (e.g. mono/stereo or
    /// NB/MB/WB switches mid-stream); resets all persistent state.
    pub fn reconfigure(&mut self, channels: usize, rate: InternalRate, frame_ms: u32) {
        *self = Self::new(channels, rate, frame_ms);
    }

    #[must_use]
    pub const fn internal_khz(&self) -> i32 {
        self.rate.khz()
    }

    /// Decode one Opus SILK frame's payload (which may itself carry 1-3
    /// consecutive 10/20 ms SILK sub-frames for a 20/40/60 ms Opus frame).
    /// Returns per-channel PCM in this crate's PCM-adjacent scale (not yet
    /// normalized or resampled) at [`Self::internal_khz`].
    pub fn decode(&mut self, dec: &mut RangeDecoder<'_>, frame_ms: u32) -> Vec<Vec<f32>> {
        let n_frames_per_packet = match frame_ms {
            0..=20 => 1,
            40 => 2,
            _ => 3,
        };
        let channels = self.channels;

        // Each regular SILK frame this packet decodes is 10 ms (nb_subfr=2)
        // only when the whole packet is itself a single native 10 ms
        // frame; every other case (20 ms standalone, or each 20 ms unit of
        // a 40/60 ms packet) is nb_subfr=4. `ensure_silk` only reconfigures
        // on an internal-rate change, so a mid-stream frame-duration change
        // at a fixed rate would otherwise leave every `MonoState` decoding
        // with a stale subframe count.
        let nb_subfr = if frame_ms <= 10 { 2 } else { 4 };
        if nb_subfr != self.nb_subfr {
            self.nb_subfr = nb_subfr;
            for st in &mut self.mono {
                st.set_nb_subfr(nb_subfr);
            }
        }

        // VAD flags + LBRR flag, per channel.
        let mut vad_flags = vec![vec![false; n_frames_per_packet]; channels];
        let mut lbrr_flag = vec![false; channels];
        for c in 0..channels {
            for f in &mut vad_flags[c] {
                *f = dec.bit_logp(1);
            }
            lbrr_flag[c] = dec.bit_logp(1);
        }
        let mut lbrr_flags = vec![vec![false; n_frames_per_packet]; channels];
        for c in 0..channels {
            if !lbrr_flag[c] {
                continue;
            }
            if n_frames_per_packet == 1 {
                lbrr_flags[c][0] = true;
            } else {
                let table: &[u8] =
                    if n_frames_per_packet == 2 { &tables::LBRR_FLAGS_2_ICDF } else { &tables::LBRR_FLAGS_3_ICDF };
                let sym = dec.icdf(table, 8).unwrap_or(0) + 1;
                for (f, flag) in lbrr_flags[c].iter_mut().enumerate() {
                    *flag = (sym >> f) & 1 != 0;
                }
            }
        }

        // Skip over any LBRR redundancy content, to stay in sync.
        for i in 0..n_frames_per_packet {
            for c in 0..channels {
                if !lbrr_flags[c][i] {
                    continue;
                }
                if channels == 2 && c == 0 {
                    let _ = decode_stereo_pred(dec);
                    if !lbrr_flags[1][i] {
                        let _ = dec.icdf(&tables::STEREO_ONLY_CODE_MID_ICDF, 8);
                    }
                }
                let cond = if i > 0 && lbrr_flags[c][i - 1] { CondCoding::Conditional } else { CondCoding::Independent };
                let vad = vad_flags[c][i];
                if let Some(st) = self.mono.get_mut(c) {
                    let _ = decode::decode_frame(dec, st, cond, vad);
                }
            }
        }

        // Stereo mid/side predictors and mid-only flag for the regular data.
        let mut ms_pred = self.stereo.pred_prev_q13;
        let mut decode_only_middle = false;
        if channels == 2 {
            ms_pred = decode_stereo_pred(dec);
            if vad_flags.get(1).and_then(|v| v.first()).copied() == Some(false) {
                decode_only_middle = dec.icdf(&tables::STEREO_ONLY_CODE_MID_ICDF, 8).unwrap_or(0) != 0;
            }
        }

        if channels == 2 && !decode_only_middle && self.stereo.prev_decode_only_middle
            && let Some(side) = self.mono.get_mut(1) {
                *side = MonoState::new(self.rate.khz(), self.nb_subfr);
            }
        let has_side = !decode_only_middle;

        let mut mid_pcm = Vec::new();
        let mut side_pcm: Option<Vec<f32>> = None;
        for i in 0..n_frames_per_packet {
            for c in 0..channels {
                if c > 0 && !has_side {
                    continue;
                }
                let frame_index = i as i32 - c as i32;
                let cond = if frame_index <= 0 {
                    CondCoding::Independent
                } else if c > 0 && self.stereo.prev_decode_only_middle {
                    CondCoding::IndependentNoLtpScaling
                } else {
                    CondCoding::Conditional
                };
                let vad = vad_flags.get(c).and_then(|v| v.get(i)).copied().unwrap_or(false);
                if let Some(st) = self.mono.get_mut(c) {
                    let out = decode::decode_frame(dec, st, cond, vad);
                    // Advance the persistent PCM history.
                    let keep = st.ltp_mem_length.saturating_sub(out.pcm.len());
                    let mut new_buf = Vec::new();
                    new_buf.extend_from_slice(&st.out_buf[st.out_buf.len().saturating_sub(keep)..]);
                    new_buf.extend_from_slice(&out.pcm);
                    if new_buf.len() > st.ltp_mem_length {
                        let drop = new_buf.len() - st.ltp_mem_length;
                        new_buf.drain(0..drop);
                    }
                    st.out_buf = new_buf;
                    if c == 0 {
                        mid_pcm.extend_from_slice(&out.pcm);
                    } else {
                        side_pcm.get_or_insert_with(Vec::new).extend_from_slice(&out.pcm);
                    }
                }
            }
        }

        self.stereo.prev_decode_only_middle = decode_only_middle;
        self.stereo.pred_prev_q13 = ms_pred;

        if channels == 2 {
            let side = side_pcm.unwrap_or_else(|| vec![0.0; mid_pcm.len()]);
            let (l, r) = stereo_ms_to_lr(&mut self.stereo, &mid_pcm, &side, ms_pred, self.rate.khz());
            vec![l, r]
        } else {
            vec![mid_pcm]
        }
    }
}

fn decode_stereo_pred(dec: &mut RangeDecoder<'_>) -> [f32; 2] {
    let n = dec.icdf(&tables::STEREO_PRED_JOINT_ICDF, 8).unwrap_or(0);
    let i2 = [n / 5, n - 5 * (n / 5)];
    let mut ix = [[0i32; 3]; 2];
    for c in 0..2 {
        ix[c][2] = i2[c];
        ix[c][0] = dec.icdf(&tables::UNIFORM3_ICDF, 8).unwrap_or(0);
        ix[c][1] = dec.icdf(&tables::UNIFORM5_ICDF, 8).unwrap_or(0);
    }
    let mut pred = [0.0f32; 2];
    for c in 0..2 {
        let idx0 = (ix[c][0] + 3 * ix[c][2]).clamp(0, 15) as usize;
        let low = tables::STEREO_PRED_QUANT_Q13.get(idx0).copied().unwrap_or(0.0);
        let high = tables::STEREO_PRED_QUANT_Q13.get(idx0 + 1).copied().unwrap_or(low);
        let step = (high - low) * (0.5 / tables::STEREO_QUANT_SUB_STEPS);
        pred[c] = low + step * (2.0 * ix[c][1] as f32 + 1.0);
    }
    pred[0] -= pred[1];
    pred
}

/// `stereo_MS_to_LR.c`'s `silk_stereo_MS_to_LR`, on this crate's
/// PCM-adjacent-scale `f32` samples.
fn stereo_ms_to_lr(state: &mut StereoState, mid_in: &[f32], side_in: &[f32], pred: [f32; 2], fs_khz: i32) -> (Vec<f32>, Vec<f32>) {
    let n = mid_in.len();
    // `x1`/`x2` carry a 2-sample lookahead/lookbehind buffer, matching the
    // reference's `x1[n+1]`-centred indexing.
    let mut x1 = vec![0.0f32; n + 2];
    let mut x2 = vec![0.0f32; n + 2];
    if let Some(s) = x1.get_mut(0..2) {
        s.copy_from_slice(&state.s_mid);
    }
    if let Some(s) = x1.get_mut(2..2 + n) {
        s.copy_from_slice(mid_in);
    }
    if let Some(s) = x2.get_mut(2..2 + n) {
        let take = s.len().min(side_in.len());
        if let (Some(dst), Some(src)) = (s.get_mut(..take), side_in.get(..take)) {
            dst.copy_from_slice(src);
        }
    }
    if n >= 2 {
        state.s_mid = [mid_in[n - 2], mid_in[n - 1]];
    }

    let interp_len = (tables::STEREO_INTERP_LEN_MS * fs_khz).max(1) as usize;
    let denom = 1.0 / interp_len as f32;
    let delta0 = (pred[0] - state.pred_prev_q13[0]) * denom;
    let delta1 = (pred[1] - state.pred_prev_q13[1]) * denom;
    let mut p0 = state.pred_prev_q13[0];
    let mut p1 = state.pred_prev_q13[1];

    for i in 0..n {
        let (pp0, pp1) = if i < interp_len {
            p0 += delta0;
            p1 += delta1;
            (p0, p1)
        } else {
            (pred[0], pred[1])
        };
        let a = x1.get(i).copied().unwrap_or(0.0);
        let b = x1.get(i + 1).copied().unwrap_or(0.0);
        let c = x1.get(i + 2).copied().unwrap_or(0.0);
        let smoothed_mid = (a + 2.0 * b + c) * 0.25; // [1,2,1] smoothing, normalized.
        let side = x2.get(i + 1).copied().unwrap_or(0.0);
        let sum = side + smoothed_mid * pp0 + b * pp1;
        if let Some(slot) = x2.get_mut(i + 1) {
            *slot = sum.clamp(-32768.0, 32767.0);
        }
    }
    state.pred_prev_q13 = pred;

    let mut left = vec![0.0f32; n];
    let mut right = vec![0.0f32; n];
    for i in 0..n {
        let m = x1.get(i + 1).copied().unwrap_or(0.0);
        let s = x2.get(i + 1).copied().unwrap_or(0.0);
        left[i] = (m + s).clamp(-32768.0, 32767.0);
        right[i] = (m - s).clamp(-32768.0, 32767.0);
    }
    (left, right)
}

pub use decode::SignalType as SilkSignalType;
pub mod resample;
