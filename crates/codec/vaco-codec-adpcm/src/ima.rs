//! IMA ADPCM: the shared nibble codec, and the two container framings this
//! crate covers (`adpcm_ima_wav`, `adpcm_ima_qt`).
//!
//! `Vaco-Spec-Ref: apple-qtff` for the `QuickTime` `ima4` framing (§ "IMA 4:1");
//! the WAV framing follows the RIFF WAVE `WAVE_FORMAT_IMA_ADPCM` registry
//! entry (no `provenance/sources.toml` id registered for it, so no
//! `Vaco-Spec-Ref` for that half). The nibble-to-sample step algorithm itself
//! is the IMA/DVI reference table in [`crate::tables`] — see that module's docs.

use vaco_core::{Error, Result};

use crate::tables::{IMA_INDEX_TABLE, IMA_STEP_TABLE};

/// One channel's running predictor/step-index state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImaState {
    pub predictor: i32,
    pub index: i32,
}

impl ImaState {
    #[must_use]
    pub(crate) const fn new(predictor: i32, index: i32) -> Self {
        Self {
            predictor,
            index: clamp_index(index),
        }
    }

    /// Decode one 4-bit code, advancing state, returning the new sample.
    pub(crate) fn decode_nibble(&mut self, nibble: u8) -> i16 {
        let step = *IMA_STEP_TABLE.get(self.index as usize).unwrap_or(&7);
        let nibble = nibble & 0x0F;
        let mut diff = step >> 3;
        if nibble & 4 != 0 {
            diff += step;
        }
        if nibble & 2 != 0 {
            diff += step >> 1;
        }
        if nibble & 1 != 0 {
            diff += step >> 2;
        }
        if nibble & 8 != 0 {
            diff = -diff;
        }
        self.predictor = (self.predictor + diff).clamp(-32768, 32767);
        let delta = *IMA_INDEX_TABLE.get(nibble as usize).unwrap_or(&0);
        self.index = clamp_index(self.index + delta);
        self.predictor as i16
    }

    /// Encode `sample`, advancing state identically to [`ImaState::decode_nibble`]
    /// on the code it returns (so encoder and decoder never diverge), and
    /// return the 4-bit code.
    pub(crate) fn encode_sample(&mut self, sample: i16) -> u8 {
        let step = *IMA_STEP_TABLE.get(self.index as usize).unwrap_or(&7);
        let diff = i32::from(sample) - self.predictor;
        let (sign, mut mag) = if diff < 0 { (8u8, -diff) } else { (0u8, diff) };
        let mut nibble = 0u8;
        let mut tmp_step = step;
        if mag >= tmp_step {
            nibble |= 4;
            mag -= tmp_step;
        }
        tmp_step >>= 1;
        if mag >= tmp_step {
            nibble |= 2;
            mag -= tmp_step;
        }
        tmp_step >>= 1;
        if mag >= tmp_step {
            nibble |= 1;
        }
        nibble |= sign;
        // Re-run the decode side on our own code so `self` ends up in exactly
        // the state a decoder fed this nibble would reach.
        self.decode_nibble(nibble);
        nibble
    }
}

/// Pick a starting step-table index whose step roughly matches `samples`'
/// own peak magnitude, instead of always cold-starting at index 0 (step 7).
///
/// A fixed cold start badly "slope-overloads" on any block whose amplitude
/// is much larger than a step-7 quantizer can track within a few samples —
/// every real encoder either transmits a well-chosen initial index (which
/// both container framings here have a field for) or ramps up over many
/// samples. Estimating it from the block's own peak is the simplest of the
/// two that does not need per-container-specific tuning.
#[allow(
    clippy::integer_division,
    reason = "an eighth-of-peak target step is a deliberate floor division, not a rounding bug"
)]
pub(crate) fn estimate_initial_index(samples: &[i16]) -> i32 {
    let peak = samples.iter().map(|s| i32::from(*s).unsigned_abs()).max().unwrap_or(0);
    // Aim for a step roughly a sixteenth of the peak: comfortably inside the
    // 4-bit code's dynamic range even at the peak sample.
    let target = i32::try_from((peak / 16).max(1)).unwrap_or(i32::MAX);
    IMA_STEP_TABLE
        .iter()
        .position(|&step| step >= target)
        .map_or(88, |i| i32::try_from(i).unwrap_or(88))
}

const fn clamp_index(index: i32) -> i32 {
    if index < 0 {
        0
    } else if index > 88 {
        88
    } else {
        index
    }
}

// -------------------------------------------------------------- WAV framing

/// One channel header: 4 bytes, `predictor: i16 LE`, `index: u8`, reserved.
const WAV_HEADER_BYTES: usize = 4;

/// Decode one `adpcm_ima_wav` block. `channels` headers open the block, then
/// 4-byte (8-nibble) groups round-robin across channels until the block ends.
///
/// Returns interleaved `i16` samples (channel-minor, i.e. `[c0s0, c1s0, c0s1,
/// c1s1, ...]`).
///
/// # Errors
/// [`Error::InvalidData`] if the block is shorter than one header per channel.
pub(crate) fn decode_wav_block(data: &[u8], channels: u32) -> Result<Vec<i16>> {
    let channels = channels.max(1) as usize;
    let header_bytes = WAV_HEADER_BYTES.saturating_mul(channels);
    if data.len() < header_bytes {
        return Err(Error::InvalidData("adpcm_ima_wav: block shorter than its own header"));
    }
    let mut states = Vec::new();
    // Per-channel decoded sample streams, later interleaved.
    let mut per_channel: Vec<Vec<i16>> = Vec::new();
    for c in 0..channels {
        let Some(h) = data.get(c * WAV_HEADER_BYTES..c * WAV_HEADER_BYTES + WAV_HEADER_BYTES)
        else {
            return Err(Error::InvalidData("adpcm_ima_wav: truncated header"));
        };
        let &[lo, hi, idx, _reserved] = h else {
            return Err(Error::InvalidData("adpcm_ima_wav: truncated header"));
        };
        let predictor = i16::from_le_bytes([lo, hi]);
        states.push(ImaState::new(i32::from(predictor), i32::from(idx)));
        per_channel.push(vec![predictor]);
    }

    let body = data.get(header_bytes..).unwrap_or(&[]);
    let mut chan = 0usize;
    for group in body.chunks(4) {
        if group.len() < 4 {
            break; // a short trailing group carries no complete nibble set
        }
        let state = states.get_mut(chan).ok_or(Error::InvalidData("adpcm_ima_wav: channel index"))?;
        let out = per_channel
            .get_mut(chan)
            .ok_or(Error::InvalidData("adpcm_ima_wav: channel index"))?;
        for &byte in group {
            out.push(state.decode_nibble(byte & 0x0F));
            out.push(state.decode_nibble(byte >> 4));
        }
        chan = (chan + 1) % channels;
    }

    Ok(interleave(&per_channel))
}

/// Encode `samples` (interleaved, channel-minor, as [`decode_wav_block`]
/// returns) into one `adpcm_ima_wav` block. `samples_per_channel` must be
/// consistent for every channel (callers pad the final short block).
///
/// # Errors
/// [`Error::InvalidData`] if `samples.len()` is not a multiple of `channels`,
/// or fewer than one sample per channel.
#[allow(
    clippy::integer_division,
    reason = "packing two 4-bit codes per byte at position k/2 is exact floor division, not a rounding bug"
)]
pub(crate) fn encode_wav_block(samples: &[i16], channels: u32) -> Result<Vec<u8>> {
    let channels = channels.max(1) as usize;
    let per_channel = deinterleave(samples, channels)?;
    let Some(first_len) = per_channel.first().map(Vec::len) else {
        return Err(Error::InvalidData("adpcm_ima_wav: no samples"));
    };
    if first_len == 0 {
        return Err(Error::InvalidData("adpcm_ima_wav: no samples"));
    }

    let mut out = Vec::new();
    let mut states = Vec::new();
    for ch in &per_channel {
        let Some(&first) = ch.first() else {
            return Err(Error::InvalidData("adpcm_ima_wav: empty channel"));
        };
        let index = estimate_initial_index(ch);
        out.extend_from_slice(&first.to_le_bytes());
        out.push(index as u8);
        out.push(0);
        states.push(ImaState::new(i32::from(first), index));
    }

    // Body: 8-sample (4-byte) groups per channel, round-robin, starting from
    // sample index 1 (index 0 was consumed by the header).
    let remaining = first_len.saturating_sub(1);
    let groups = remaining.div_ceil(8);
    for g in 0..groups {
        for (c, ch) in per_channel.iter().enumerate() {
            let state = states.get_mut(c).ok_or(Error::InvalidData("adpcm_ima_wav: channel"))?;
            let base = 1 + g * 8;
            let mut byte_group = [0u8; 4];
            for k in 0..8usize {
                let idx = base + k;
                // Padding samples (past the real end) repeat the last real
                // sample, which encodes to a zero-magnitude nibble and keeps
                // the block a whole number of 4-byte groups.
                let sample = ch.get(idx).copied().unwrap_or_else(|| ch.last().copied().unwrap_or(0));
                let code = state.encode_sample(sample);
                if let Some(slot) = byte_group.get_mut(k / 2) {
                    if k % 2 == 0 {
                        *slot = code & 0x0F;
                    } else {
                        *slot |= code << 4;
                    }
                }
            }
            out.extend_from_slice(&byte_group);
        }
    }
    Ok(out)
}

// --------------------------------------------------------------- QT framing

/// One `ima4` chunk: 2-byte big-endian header (top 9 bits predictor, low 7
/// bits step index) + 32 bytes (64 nibbles) of data, one chunk per channel
/// per 64-sample frame.
pub(crate) const QT_CHUNK_BYTES: usize = 34;
pub(crate) const QT_SAMPLES_PER_CHUNK: usize = 64;

/// Decode one or more `ima4` chunk-sets (`channels` chunks per set) into
/// interleaved samples.
///
/// # Errors
/// [`Error::InvalidData`] if `data`'s length is not a whole number of
/// `channels`-chunk sets.
#[allow(
    clippy::integer_division,
    reason = "chunk-set count is an exact division once the is_multiple_of check above holds"
)]
pub(crate) fn decode_qt_block(data: &[u8], channels: u32) -> Result<Vec<i16>> {
    let channels = channels.max(1) as usize;
    let set_bytes = QT_CHUNK_BYTES.saturating_mul(channels);
    if set_bytes == 0 || !data.len().is_multiple_of(set_bytes) {
        return Err(Error::InvalidData("adpcm_ima_qt: not a whole number of chunk sets"));
    }
    let sets = data.len() / set_bytes;
    let mut per_channel: Vec<Vec<i16>> = vec![Vec::new(); channels];
    for s in 0..sets {
        for c in 0..channels {
            let start = s * set_bytes + c * QT_CHUNK_BYTES;
            let chunk = data
                .get(start..start + QT_CHUNK_BYTES)
                .ok_or(Error::InvalidData("adpcm_ima_qt: truncated chunk"))?;
            let &[hi, lo, ref body @ ..] = chunk else {
                return Err(Error::InvalidData("adpcm_ima_qt: truncated chunk"));
            };
            let header = u16::from_be_bytes([hi, lo]);
            let predictor = (header & 0xFF80).cast_signed();
            let index = i32::from(header & 0x007F);
            let mut state = ImaState::new(i32::from(predictor), index);
            let out = per_channel
                .get_mut(c)
                .ok_or(Error::InvalidData("adpcm_ima_qt: channel"))?;
            for &byte in body {
                out.push(state.decode_nibble(byte & 0x0F));
                out.push(state.decode_nibble(byte >> 4));
            }
        }
    }
    Ok(interleave(&per_channel))
}

/// Encode interleaved `samples` into `ima4` chunk sets, `QT_SAMPLES_PER_CHUNK`
/// samples per channel per set (the final set is padded by repeating the
/// last real sample, same as [`encode_wav_block`]).
///
/// # Errors
/// [`Error::InvalidData`] if `samples.len()` is not a multiple of `channels`.
#[allow(
    clippy::integer_division,
    reason = "packing two 4-bit codes per byte at position k/2 is exact floor division, not a rounding bug"
)]
pub(crate) fn encode_qt_block(samples: &[i16], channels: u32) -> Result<Vec<u8>> {
    let channels = channels.max(1) as usize;
    let per_channel = deinterleave(samples, channels)?;
    let Some(len) = per_channel.first().map(Vec::len) else {
        return Ok(Vec::new());
    };
    let sets = len.div_ceil(QT_SAMPLES_PER_CHUNK).max(1);
    let mut out = Vec::new();
    for s in 0..sets {
        for ch in &per_channel {
            let Some(&first) = ch
                .get(s * QT_SAMPLES_PER_CHUNK)
                .or_else(|| ch.last())
            else {
                continue;
            };
            let window_start = s * QT_SAMPLES_PER_CHUNK;
            let window = ch.get(window_start..).unwrap_or(&[]);
            let index = estimate_initial_index(window) & 0x7F;
            let header_predictor = (first.cast_unsigned() & 0xFF80).cast_signed();
            let mut state = ImaState::new(i32::from(header_predictor), index);
            let index_bits = u16::try_from(index).unwrap_or(0) & 0x7F;
            let header = (header_predictor.cast_unsigned() & 0xFF80) | index_bits;
            out.extend_from_slice(&header.to_be_bytes());
            let mut body = [0u8; 32];
            for k in 0..QT_SAMPLES_PER_CHUNK {
                let idx = s * QT_SAMPLES_PER_CHUNK + k;
                let sample = ch.get(idx).copied().unwrap_or_else(|| ch.last().copied().unwrap_or(0));
                let code = state.encode_sample(sample);
                if let Some(slot) = body.get_mut(k / 2) {
                    if k % 2 == 0 {
                        *slot = code & 0x0F;
                    } else {
                        *slot |= code << 4;
                    }
                }
            }
            out.extend_from_slice(&body);
        }
    }
    Ok(out)
}

/// Channel-minor interleave: `per_channel[c][n]` -> `out[n*channels+c]`.
/// Channels of unequal length are truncated to the shortest.
pub(crate) fn interleave(per_channel: &[Vec<i16>]) -> Vec<i16> {
    let Some(len) = per_channel.iter().map(Vec::len).min() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in 0..len {
        for ch in per_channel {
            out.push(ch.get(n).copied().unwrap_or(0));
        }
    }
    out
}

pub(crate) fn deinterleave(samples: &[i16], channels: usize) -> Result<Vec<Vec<i16>>> {
    if channels == 0 || !samples.len().is_multiple_of(channels) {
        return Err(Error::InvalidData("adpcm: sample count not a multiple of channel count"));
    }
    let mut out = vec![Vec::new(); channels];
    for (n, &s) in samples.iter().enumerate() {
        if let Some(ch) = out.get_mut(n % channels) {
            ch.push(s);
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code exercising the codec, not the untrusted-input surface"
)]
mod tests {
    use super::*;

    fn tone(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| ((i as f64 * 0.2).sin() * 8000.0) as i16)
            .collect()
    }

    #[test]
    fn wav_mono_round_trips_approximately() {
        // 41 = 1 (header sample) + 5*8 (whole 4-byte/8-nibble groups), so the
        // block needs no padding and decodes back to exactly this many
        // samples.
        let samples = tone(41);
        let block = encode_wav_block(&samples, 1).unwrap();
        let decoded = decode_wav_block(&block, 1).unwrap();
        assert_eq!(decoded.len(), samples.len());
        // Lossy codec: check the decoder reproduces its own encoder closely,
        // not bit-exactly (ADPCM's whole point is a lossy adaptive delta).
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((i32::from(*a) - i32::from(*b)).abs() < 2000, "{a} vs {b}");
        }
    }

    #[test]
    fn wav_stereo_round_trips_and_preserves_channel_identity() {
        // 33 = 1 + 4*8 per channel, again a whole number of groups.
        let n = 33;
        let mut samples = Vec::new();
        for i in 0..n {
            samples.push(1000 + i as i16); // left: rising
            samples.push(-1000 - i as i16); // right: falling
        }
        let block = encode_wav_block(&samples, 2).unwrap();
        let decoded = decode_wav_block(&block, 2).unwrap();
        assert_eq!(decoded.len(), samples.len());
        // Left channel stays positive-ish, right stays negative-ish.
        for pair in decoded.chunks(2) {
            assert!(pair[0] > -500);
            assert!(pair[1] < 500);
        }
    }

    #[test]
    fn qt_mono_round_trips_approximately() {
        let samples = tone(QT_SAMPLES_PER_CHUNK * 2 + 5);
        let block = encode_qt_block(&samples, 1).unwrap();
        assert_eq!(block.len() % QT_CHUNK_BYTES, 0);
        let decoded = decode_qt_block(&block, 1).unwrap();
        assert_eq!(decoded.len() % QT_SAMPLES_PER_CHUNK, 0);
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((i32::from(*a) - i32::from(*b)).abs() < 2000, "{a} vs {b}");
        }
    }

    #[test]
    fn short_wav_block_is_rejected() {
        assert!(decode_wav_block(&[0, 1], 1).is_err());
    }
}
