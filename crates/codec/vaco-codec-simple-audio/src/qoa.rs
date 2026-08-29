//! QOA (Quite OK Audio), Dominic Szablewski's sign-sign LMS audio codec.
//!
//! Implemented directly from "The Quite OK Audio Format", Specification
//! Version 1.0, 2023.04.24 (<https://qoaformat.org/qoa-specification.pdf>) —
//! decode is transcribed clause-for-clause from that document's numbered
//! steps `[1]`-`[7]`. The specification only defines decode; the encoder
//! below (choice of `sf_quant` per slice) is this crate's own design,
//! constrained only by "must produce a spec-conformant slice", so it cannot
//! affect interoperability with any spec-conformant decoder.
//!
//! # How it works
//!
//! One QOA *frame* (8-byte header, then each channel's 16-byte LMS state,
//! then up to 256 8-byte slices per channel) is this codec's packet unit.
//! Each slice packs a 4-bit scale factor and twenty 3-bit residual indices
//! into one big-endian 64-bit word; decoding a slice dequantises each
//! residual, adds it to the channel's LMS prediction, and feeds the result
//! back into the LMS filter for the next slice. Because the LMS state is
//! re-transmitted in full at every frame header, a frame decodes
//! independently of every other frame.
//!
//! # What is not covered
//!
//! No `.qoa` file-level framing (the 8-byte file magic/sample-count header)
//! is handled here — that is a container concern for whatever demuxer reads
//! `.qoa` files, which is expected to hand this codec exactly one frame's
//! bytes per packet. The "streaming" convention (`samples == 0` in the file
//! header, samplerate/channel count allowed to vary frame-to-frame) falls
//! out of this for free, since every frame already carries its own
//! samplerate and channel count.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Samples per slice, fixed by the format.
pub const SLICE_SAMPLES: usize = 20;
/// Slices per channel in a full (non-final) frame.
pub const MAX_SLICES_PER_FRAME: usize = 256;
/// Bytes in one frame header.
const FRAME_HEADER_BYTES: usize = 8;
/// Bytes of LMS state per channel.
const LMS_STATE_BYTES: usize = 16;
/// Bytes in one slice.
const SLICE_BYTES: usize = 8;

/// `[2]` — index `qr` is a lookup into this table.
const DEQUANT_TAB: [f64; 8] = [0.75, -0.75, 2.5, -2.5, 4.5, -4.5, 7.0, -7.0];

fn get_u8(b: &[u8], off: usize) -> u8 {
    b.get(off).copied().unwrap_or(0)
}

fn get_be_i16(b: &[u8], off: usize) -> i16 {
    b.get(off..off + 2).and_then(|s| <[u8; 2]>::try_from(s).ok()).map_or(0, i16::from_be_bytes)
}

fn put_be_i16(out: &mut [u8], off: usize, v: i16) {
    if let Some(dst) = out.get_mut(off..off + 2) {
        dst.copy_from_slice(&v.to_be_bytes());
    }
}

/// One channel's LMS predictor/history state, the wire shape of
/// `lms_state[num_channels]` (`history`/`weights`, "most recent last").
#[derive(Debug, Clone, Copy, Default)]
pub struct LmsState {
    pub history: [i32; 4],
    pub weights: [i32; 4],
}

impl LmsState {
    fn from_bytes(b: &[u8]) -> Self {
        let mut history = [0i32; 4];
        let mut weights = [0i32; 4];
        for (n, (h, w)) in history.iter_mut().zip(weights.iter_mut()).enumerate() {
            *h = i32::from(get_be_i16(b, n * 2));
            *w = i32::from(get_be_i16(b, 8 + n * 2));
        }
        Self { history, weights }
    }

    fn write_bytes(&self, out: &mut [u8]) {
        for (n, (h, w)) in self.history.iter().zip(self.weights.iter()).enumerate() {
            let hv = (*h).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            let wv = (*w).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            put_be_i16(out, n * 2, hv);
            put_be_i16(out, 8 + n * 2, wv);
        }
    }

    /// `[4]` — the predicted sample, sum of history*weights right-shifted by
    /// 13 bits. `i64` throughout: four `i32*i32` products can exceed `i32`.
    fn predict(&self) -> i64 {
        let p: i64 = self
            .history
            .iter()
            .zip(self.weights.iter())
            .fold(0i64, |acc, (h, w)| acc.wrapping_add(i64::from(*h) * i64::from(*w)));
        p >> 13
    }

    /// `[6]`/`[7]` — weight and history update from the dequantised,
    /// scaled residual `r` and the final output sample `s`.
    fn update(&mut self, r: i64, s: i32) {
        let delta = i32::try_from(r >> 4).unwrap_or(if r < 0 { i32::MIN } else { i32::MAX });
        for (h, w) in self.history.iter().zip(self.weights.iter_mut()) {
            let step = if *h < 0 { delta.wrapping_neg() } else { delta };
            *w = w.wrapping_add(step);
        }
        self.history.rotate_left(1);
        if let Some(last) = self.history.last_mut() {
            *last = s;
        }
    }
}

/// `[1]` — dequantise a 4-bit `sf_quant` into its scale factor.
fn dequant_scalefactor(sf_quant: u32) -> i64 {
    (f64::from(sf_quant + 1)).powf(2.75).round() as i64
}

/// `[3]` — dequantise and scale one residual, "round to nearest, ties away
/// from zero".
fn dequant_residual(sf: i64, qr: u32) -> i64 {
    let tab = DEQUANT_TAB.get(qr as usize % DEQUANT_TAB.len()).copied().unwrap_or(0.75);
    let r = (sf as f64) * tab;
    if r < 0.0 { (r - 0.5).ceil() as i64 } else { (r + 0.5).floor() as i64 }
}

/// One decoded frame: interleaved `i16` PCM plus the header facts a caller
/// needs to build an audio frame from it.
#[derive(Debug)]
pub struct QoaDecodedFrame {
    pub num_channels: u32,
    pub sample_rate: u32,
    /// Samples per channel actually decoded (`<=` the frame header's own
    /// `fsamples`, since a truncated packet still decodes what it has).
    pub samples_per_channel: u32,
    /// Interleaved, channel-major-per-slice as the wire format is.
    pub interleaved: Vec<i16>,
}

/// Decode one QOA frame's bytes (frame header + LMS state + slices).
///
/// Lenient like every demuxer-adjacent decoder in this tree: a frame
/// truncated mid-slice decodes as many whole slices as the data holds
/// rather than failing outright, since a caller such as `-c copy` piping a
/// partial final packet should still get what audio exists.
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] if the packet is shorter than one frame
/// header, or declares zero channels.
#[allow(
    clippy::integer_division,
    reason = "slice/sample counts derived from byte lengths are exact floor divisions by \
              construction (channel and slice sizes are fixed byte counts), not lossy math"
)]
pub fn decode(budget: &mut Budget, data: &[u8]) -> Result<QoaDecodedFrame> {
    if data.len() < FRAME_HEADER_BYTES {
        return Err(Error::InvalidData("qoa: packet shorter than a frame header"));
    }
    let num_channels = u32::from(get_u8(data, 0));
    if num_channels == 0 {
        return Err(Error::InvalidData("qoa: zero channels"));
    }
    let sample_rate = (u32::from(get_u8(data, 1)) << 16)
        | (u32::from(get_u8(data, 2)) << 8)
        | u32::from(get_u8(data, 3));
    let fsamples = u32::from(u16::from_be_bytes([get_u8(data, 4), get_u8(data, 5)]));
    let channels = num_channels as usize;

    budget.check_channels(num_channels.into())?;

    let lms_bytes = LMS_STATE_BYTES.saturating_mul(channels);
    let after_header = data.get(FRAME_HEADER_BYTES..).unwrap_or(&[]);
    if after_header.len() < lms_bytes {
        // Header-only or truncated-in-the-LMS-state packet: no audio to
        // recover.
        return Ok(QoaDecodedFrame {
            num_channels,
            sample_rate,
            samples_per_channel: 0,
            interleaved: Vec::new(),
        });
    }

    let mut states: Vec<LmsState> = Vec::new();
    for c in 0..channels {
        let off = c * LMS_STATE_BYTES;
        let chunk = after_header.get(off..off + LMS_STATE_BYTES).unwrap_or(&[]);
        states.push(LmsState::from_bytes(chunk));
    }

    let slice_bytes = after_header.get(lms_bytes..).unwrap_or(&[]);
    let full_slice_groups = slice_bytes.len() / (SLICE_BYTES * channels.max(1));
    let declared_slice_groups = (fsamples as usize).div_ceil(SLICE_SAMPLES).min(MAX_SLICES_PER_FRAME);
    // Never decode more than the bytes on hand actually contain, and never
    // more than the header itself declares — whichever is smaller.
    let slice_groups = full_slice_groups.min(declared_slice_groups);
    let total_samples = slice_groups.saturating_mul(SLICE_SAMPLES);

    let mut interleaved = budget.alloc::<i16>(total_samples.saturating_mul(channels))?;
    for group in 0..slice_groups {
        for (c, state) in states.iter_mut().enumerate() {
            let idx = (group * channels + c) * SLICE_BYTES;
            let word = u64::from_be_bytes([
                get_u8(slice_bytes, idx),
                get_u8(slice_bytes, idx + 1),
                get_u8(slice_bytes, idx + 2),
                get_u8(slice_bytes, idx + 3),
                get_u8(slice_bytes, idx + 4),
                get_u8(slice_bytes, idx + 5),
                get_u8(slice_bytes, idx + 6),
                get_u8(slice_bytes, idx + 7),
            ]);
            let sf_quant = ((word >> 60) & 0xF) as u32;
            let sf = dequant_scalefactor(sf_quant);
            for n in 0..SLICE_SAMPLES {
                let shift = 57 - 3 * n;
                let qr = ((word >> shift) & 0x7) as u32;
                let r = dequant_residual(sf, qr);
                let p = state.predict();
                let s = (p + r).clamp(-32768, 32767) as i32;
                state.update(r, s);
                let out_idx = (group * SLICE_SAMPLES + n) * channels + c;
                if let Some(slot) = interleaved.get_mut(out_idx) {
                    *slot = s as i16;
                }
            }
        }
    }

    Ok(QoaDecodedFrame {
        num_channels,
        sample_rate,
        samples_per_channel: total_samples as u32,
        interleaved,
    })
}

/// Find the `qr` (and its dequantised residual) minimising `|r - ideal|`
/// for one scale factor. Eight candidates; a linear scan is simplest and
/// cheap.
fn best_qr(sf: i64, ideal: i64) -> (u32, i64) {
    let mut best_qr = 0u32;
    let mut best_r = dequant_residual(sf, 0);
    let mut best_err = (best_r - ideal).abs();
    for qr in 1..8u32 {
        let r = dequant_residual(sf, qr);
        let err = (r - ideal).abs();
        if err < best_err {
            best_err = err;
            best_r = r;
            best_qr = qr;
        }
    }
    (best_qr, best_r)
}

/// Encode `channels` of interleaved `i16` PCM (`samples.len()` must be a
/// multiple of `channels`, up to `MAX_SLICES_PER_FRAME * SLICE_SAMPLES` per
/// channel) into one QOA frame, carrying `state` (one entry per channel)
/// across calls the way a real encoder's running LMS state does.
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] if `channels` is zero, `samples` is not
/// a whole number of frames, or the caller asks for more samples per
/// channel than one frame can hold.
#[allow(
    clippy::integer_division,
    reason = "sample-per-channel count is an exact floor division, already guarded by the \
              is_multiple_of check directly above it"
)]
pub fn encode(
    budget: &mut Budget,
    states: &mut [LmsState],
    channels: u32,
    sample_rate: u32,
    samples: &[i16],
) -> Result<Vec<u8>> {
    let ch = channels as usize;
    if ch == 0 || states.len() != ch {
        return Err(Error::InvalidData("qoa: channel count does not match LMS state count"));
    }
    if !samples.len().is_multiple_of(ch) {
        return Err(Error::InvalidData("qoa: sample buffer is not a whole number of frames"));
    }
    let samples_per_channel = samples.len() / ch;
    let slice_groups = samples_per_channel.div_ceil(SLICE_SAMPLES);
    if slice_groups > MAX_SLICES_PER_FRAME {
        return Err(Error::InvalidData("qoa: more samples than one frame can hold"));
    }

    let total_bytes = FRAME_HEADER_BYTES + ch * LMS_STATE_BYTES + slice_groups * ch * SLICE_BYTES;
    let mut out = budget.alloc::<u8>(total_bytes)?;

    let channels_u8 =
        u8::try_from(channels).map_err(|_| Error::InvalidData("qoa: too many channels"))?;
    if let Some(slot) = out.get_mut(0) {
        *slot = channels_u8;
    }
    if let Some(slot) = out.get_mut(1) {
        *slot = ((sample_rate >> 16) & 0xFF) as u8;
    }
    if let Some(slot) = out.get_mut(2) {
        *slot = ((sample_rate >> 8) & 0xFF) as u8;
    }
    if let Some(slot) = out.get_mut(3) {
        *slot = (sample_rate & 0xFF) as u8;
    }
    let fsamples = u16::try_from(samples_per_channel).unwrap_or(u16::MAX);
    if let Some(dst) = out.get_mut(4..6) {
        dst.copy_from_slice(&fsamples.to_be_bytes());
    }
    let fsize = u16::try_from(total_bytes).unwrap_or(u16::MAX);
    if let Some(dst) = out.get_mut(6..8) {
        dst.copy_from_slice(&fsize.to_be_bytes());
    }

    // The LMS state header is written from the state *entering* this
    // frame, matching a decoder that reads it before touching any slice.
    for (c, state) in states.iter().enumerate() {
        let off = FRAME_HEADER_BYTES + c * LMS_STATE_BYTES;
        if let Some(dst) = out.get_mut(off..off + LMS_STATE_BYTES) {
            state.write_bytes(dst);
        }
    }

    let slices_off = FRAME_HEADER_BYTES + ch * LMS_STATE_BYTES;
    for group in 0..slice_groups {
        for (c, state) in states.iter_mut().enumerate() {
            let mut best_word = 0u64;
            let mut best_total_err = i64::MAX;
            let mut best_final_state = *state;
            for sf_quant in 0u32..16 {
                let sf = dequant_scalefactor(sf_quant);
                let mut trial = *state;
                let mut word = u64::from(sf_quant) << 60;
                let mut total_err: i64 = 0;
                for n in 0..SLICE_SAMPLES {
                    let sample_idx = group * SLICE_SAMPLES + n;
                    let target = i64::from(samples.get(sample_idx * ch + c).copied().unwrap_or(0));
                    let p = trial.predict();
                    let ideal = target - p;
                    let (qr, r) = best_qr(sf, ideal);
                    let s = (p + r).clamp(-32768, 32767) as i32;
                    trial.update(r, s);
                    let err = target - i64::from(s);
                    total_err = total_err.saturating_add(err.saturating_mul(err));
                    let shift = 57 - 3 * n;
                    word |= u64::from(qr) << shift;
                }
                if total_err < best_total_err {
                    best_total_err = total_err;
                    best_word = word;
                    best_final_state = trial;
                }
            }
            *state = best_final_state;
            let idx = slices_off + (group * ch + c) * SLICE_BYTES;
            if let Some(dst) = out.get_mut(idx..idx + 8) {
                dst.copy_from_slice(&best_word.to_be_bytes());
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::integer_division,
    reason = "test code exercising the codec, not the untrusted-input surface"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn tone(n: usize, ch: usize) -> Vec<i16> {
        let mut v = Vec::new();
        for i in 0..n {
            let base = ((f64::from(i as u32) * 0.05).sin() * 12000.0) as i16;
            for c in 0..ch {
                v.push(base.saturating_add(i16::from(c as u8) * 100));
            }
        }
        v
    }

    #[test]
    fn mono_round_trips_with_good_snr() {
        let mut budget = Budget::new(Limits::permissive());
        let samples = tone(400, 1);
        let mut states = vec![LmsState::default()];
        let wire = encode(&mut budget, &mut states, 1, 44_100, &samples).unwrap();
        let frame = decode(&mut budget, &wire).unwrap();
        assert_eq!(frame.num_channels, 1);
        assert_eq!(frame.sample_rate, 44_100);
        assert_eq!(frame.samples_per_channel as usize, samples.len());

        let mut sum_sq_err = 0f64;
        let mut sum_sq_sig = 0f64;
        for (a, b) in samples.iter().zip(frame.interleaved.iter()) {
            let e = f64::from(*a) - f64::from(*b);
            sum_sq_err += e * e;
            sum_sq_sig += f64::from(*a) * f64::from(*a);
        }
        // QOA is lossy by design; this only checks the encoder is in the
        // right ballpark (no channel swap, no garbage), not byte-exactness.
        let snr = 10.0 * (sum_sq_sig / sum_sq_err.max(1.0)).log10();
        assert!(snr > 20.0, "SNR too low: {snr} dB");
    }

    #[test]
    fn stereo_channels_do_not_bleed_into_each_other() {
        let mut budget = Budget::new(Limits::permissive());
        let n = 300;
        let mut samples = Vec::new();
        for i in 0..n {
            samples.push(((i as f64 * 0.2).sin() * 15000.0) as i16); // L: tone
            samples.push(0i16); // R: silence
        }
        let mut states = vec![LmsState::default(); 2];
        let wire = encode(&mut budget, &mut states, 2, 8000, &samples).unwrap();
        let frame = decode(&mut budget, &wire).unwrap();

        let left: Vec<i16> = frame.interleaved.iter().step_by(2).copied().collect();
        let right: Vec<i16> = frame.interleaved.iter().skip(1).step_by(2).copied().collect();
        let left_energy: i64 = left.iter().map(|&s| i64::from(s) * i64::from(s)).sum();
        let right_energy: i64 = right.iter().map(|&s| i64::from(s) * i64::from(s)).sum();
        assert!(left_energy > 1_000_000, "left channel lost its signal");
        // The right (silent) channel must stay near zero — a swapped or
        // interleaved-wrong decode would leak the tone into it.
        assert!(right_energy < left_energy / 10, "signal leaked into the silent channel");
    }

    #[test]
    fn decode_never_panics_on_arbitrary_bytes() {
        let mut budget = Budget::new(Limits::permissive());
        for len in [0usize, 1, 7, 8, 9, 24, 40, 100] {
            let data: Vec<u8> = (0..len).map(|i| (i * 53 + 7) as u8).collect();
            let _ = decode(&mut budget, &data);
        }
    }
}
