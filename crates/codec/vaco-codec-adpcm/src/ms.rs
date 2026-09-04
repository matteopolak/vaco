//! Microsoft ADPCM (`adpcm_ms`), the `WAVE_FORMAT_ADPCM` block format.
//!
//! No `provenance/sources.toml` entry names Microsoft's own ADPCM
//! documentation today, so no `Vaco-Spec-Ref` is attached — the coefficient
//! table in [`crate::tables`] and the block layout below are the widely
//! published, decades-old public description of this format (the same seven
//! coefficient pairs and block header shape every independent MS-ADPCM
//! implementation uses), derived from that description rather than from any
//! one codebase's expression of it.
//!
//! # Block layout
//!
//! For `channels` channels: one byte per channel (a predictor-coefficient
//! index, `0..=6`, selecting a row of [`crate::tables::MS_ADAPT_COEFF1`]/
//! [`crate::tables::MS_ADAPT_COEFF2`]), then one `i16` LE "delta" (initial
//! step) per channel, then one `i16` LE "sample 1" (newer) per channel, then
//! one `i16` LE "sample 2" (older) per channel -- the real
//! `ADPCMBLOCKHEADER` field order is `bPredictor, iDelta, iSamp1, iSamp2`.
//! The two seed samples are themselves the first two decoded output samples,
//! oldest (`sample2`) first. After the header, 4-bit codes follow packed two
//! per byte (high nibble first),
//! consumed in round-robin channel order one output sample at a time — which
//! is why a stereo block's nibble stream is naturally byte-aligned (2
//! channels x 1 nibble each = 1 byte per output-sample step).

use vaco_core::{Error, Result};

use crate::tables::{MS_ADAPT_COEFF1, MS_ADAPT_COEFF2, MS_ADAPT_TABLE};

#[derive(Debug, Clone, Copy)]
struct MsState {
    coeff1: i32,
    coeff2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

impl MsState {
    fn predict(&self) -> i32 {
        (self.sample1 * self.coeff1 + self.sample2 * self.coeff2) >> 8
    }

    fn decode_nibble(&mut self, nibble: u8) -> i16 {
        let signed = signed_nibble(nibble);
        let predicted = self.predict();
        let new_sample = (predicted + signed * self.delta).clamp(-32768, 32767);
        let mult = *MS_ADAPT_TABLE.get(nibble as usize & 0x0F).unwrap_or(&256);
        self.delta = ((self.delta * mult) >> 8).max(16);
        self.sample2 = self.sample1;
        self.sample1 = new_sample;
        new_sample as i16
    }

    #[allow(
        clippy::integer_division,
        reason = "quantizing the prediction error by the current step is the MS-ADPCM \
                  encode rule itself, not an incidental rounding shortcut"
    )]
    fn encode_sample(&mut self, sample: i16) -> u8 {
        let predicted = self.predict();
        let err = i32::from(sample) - predicted;
        let raw = if self.delta == 0 { 0 } else { err / self.delta };
        let nibble = (raw.clamp(-8, 7) & 0x0F) as u8;
        self.decode_nibble(nibble);
        nibble
    }
}

const fn signed_nibble(n: u8) -> i32 {
    let n = (n & 0x0F) as i32;
    if n >= 8 { n - 16 } else { n }
}

fn header_bytes(channels: usize) -> usize {
    channels * (1 + 2 + 2 + 2)
}

/// Decode one `adpcm_ms` block into interleaved samples (channel-minor),
/// starting with each channel's two seed samples.
///
/// # Errors
/// [`Error::InvalidData`] for a block shorter than its own header, or a
/// predictor-coefficient index outside `0..=6`.
pub(crate) fn decode_block(data: &[u8], channels: u32) -> Result<Vec<i16>> {
    let channels = channels.max(1) as usize;
    let hdr = header_bytes(channels);
    if data.len() < hdr {
        return Err(Error::InvalidData(
            "adpcm_ms: block shorter than its own header",
        ));
    }
    let mut states = Vec::new();
    let mut cursor = 0usize;
    let mut coeff_idx = Vec::new();
    for _ in 0..channels {
        let idx = *data
            .get(cursor)
            .ok_or(Error::InvalidData("adpcm_ms: truncated header"))?;
        coeff_idx.push(idx);
        cursor += 1;
    }
    let mut deltas = Vec::new();
    for _ in 0..channels {
        let b = data
            .get(cursor..cursor + 2)
            .ok_or(Error::InvalidData("adpcm_ms: truncated header"))?;
        let &[lo, hi] = b else {
            return Err(Error::InvalidData("adpcm_ms: truncated header"));
        };
        deltas.push(i32::from(i16::from_le_bytes([lo, hi])));
        cursor += 2;
    }
    // The on-disk `ADPCMBLOCKHEADER` field order is
    // `bPredictor, iDelta, iSamp1, iSamp2` -- `iSamp1` (the newer of the two
    // seed samples) comes first on the wire, `iSamp2` (older) second. An
    // earlier version of this function read them in the opposite order
    // (`sample2s` first, `sample1s` second), which swapped the crate's own
    // "sample1 = newer" convention throughout: the predictor's `sample1`/
    // `sample2` fields ended up holding the wrong sample, and the first two
    // *output* samples of every block came out reversed relative to a real
    // decoder. Measured against a real `ffmpeg -c:a adpcm_ms` mono fixture:
    // `ours[0..2] == ffmpeg_ref[1], ffmpeg_ref[0]` exactly -- i.e. the first
    // two decoded samples were transposed, with everything after them
    // (produced by `decode_nibble`, not by this header at all) already
    // correct. See `tests/oracle_ffmpeg.rs`'s
    // `ms_adpcm_decodes_a_real_ffmpeg_stream_bit_exact`.
    let mut sample1s = Vec::new();
    for _ in 0..channels {
        let b = data
            .get(cursor..cursor + 2)
            .ok_or(Error::InvalidData("adpcm_ms: truncated header"))?;
        let &[lo, hi] = b else {
            return Err(Error::InvalidData("adpcm_ms: truncated header"));
        };
        sample1s.push(i32::from(i16::from_le_bytes([lo, hi])));
        cursor += 2;
    }
    let mut sample2s = Vec::new();
    for _ in 0..channels {
        let b = data
            .get(cursor..cursor + 2)
            .ok_or(Error::InvalidData("adpcm_ms: truncated header"))?;
        let &[lo, hi] = b else {
            return Err(Error::InvalidData("adpcm_ms: truncated header"));
        };
        sample2s.push(i32::from(i16::from_le_bytes([lo, hi])));
        cursor += 2;
    }
    let mut per_channel: Vec<Vec<i16>> = Vec::new();
    for c in 0..channels {
        let idx = *coeff_idx.get(c).unwrap_or(&0) as usize;
        if idx >= MS_ADAPT_COEFF1.len() {
            return Err(Error::InvalidData(
                "adpcm_ms: predictor coefficient index out of range",
            ));
        }
        states.push(MsState {
            coeff1: *MS_ADAPT_COEFF1.get(idx).unwrap_or(&256),
            coeff2: *MS_ADAPT_COEFF2.get(idx).unwrap_or(&0),
            delta: *deltas.get(c).unwrap_or(&16),
            sample1: *sample1s.get(c).unwrap_or(&0),
            sample2: *sample2s.get(c).unwrap_or(&0),
        });
        per_channel.push(vec![
            *sample2s.get(c).unwrap_or(&0) as i16,
            *sample1s.get(c).unwrap_or(&0) as i16,
        ]);
    }

    let body = data.get(cursor..).unwrap_or(&[]);
    let mut chan = 0usize;
    for &byte in body {
        for nibble in [byte >> 4, byte & 0x0F] {
            let state = states
                .get_mut(chan)
                .ok_or(Error::InvalidData("adpcm_ms: channel index"))?;
            let out = per_channel
                .get_mut(chan)
                .ok_or(Error::InvalidData("adpcm_ms: channel index"))?;
            out.push(state.decode_nibble(nibble));
            chan = (chan + 1) % channels;
        }
    }
    Ok(crate::ima::interleave(&per_channel))
}

/// Encode `samples` (interleaved, channel-minor) into one `adpcm_ms` block,
/// always using predictor-coefficient index 0 (`coeff1=256, coeff2=0`, plain
/// first-order differencing) — a real encoder searches all seven per block
/// for the best fit; this always-decodable choice keeps the encoder simple
/// while staying fully compatible with any conformant decoder, including the
/// one above.
///
/// # Errors
/// [`Error::InvalidData`] if there are fewer than two samples per channel.
pub(crate) fn encode_block(samples: &[i16], channels: u32) -> Result<Vec<u8>> {
    let channels = channels.max(1) as usize;
    let per_channel = crate::ima::deinterleave(samples, channels)?;
    let Some(len) = per_channel.first().map(Vec::len) else {
        return Err(Error::InvalidData("adpcm_ms: no samples"));
    };
    if len < 2 {
        return Err(Error::InvalidData(
            "adpcm_ms: need at least 2 samples per channel",
        ));
    }

    let mut out = Vec::new();
    out.extend(std::iter::repeat_n(0u8, channels)); // coefficient index 0, every channel
    for _ in 0..channels {
        out.extend_from_slice(&16i16.to_le_bytes()); // initial delta
    }
    // Each channel's two seed samples, oldest (`sample2`) first.
    let seeds: Vec<(i16, i16)> = per_channel
        .iter()
        .map(|ch| {
            let s2 = ch.first().copied().unwrap_or(0);
            let s1 = ch.get(1).copied().unwrap_or(0);
            (s2, s1)
        })
        .collect();
    // Wire order is `iSamp1` (newer) then `iSamp2` (older) -- see
    // `decode_block`'s comment on the real `ADPCMBLOCKHEADER` field order.
    for &(_, s1) in &seeds {
        out.extend_from_slice(&s1.to_le_bytes());
    }
    for &(s2, _) in &seeds {
        out.extend_from_slice(&s2.to_le_bytes());
    }

    let mut states: Vec<MsState> = seeds
        .iter()
        .map(|&(s2, s1)| MsState {
            coeff1: 256,
            coeff2: 0,
            delta: 16,
            sample1: i32::from(s1),
            sample2: i32::from(s2),
        })
        .collect();

    let mut nibble_buf: Vec<u8> = Vec::new();
    for n in 2..len {
        for (c, ch) in per_channel.iter().enumerate() {
            let state = states
                .get_mut(c)
                .ok_or(Error::InvalidData("adpcm_ms: channel"))?;
            let sample = ch.get(n).copied().unwrap_or(0);
            nibble_buf.push(state.encode_sample(sample));
        }
    }
    for pair in nibble_buf.chunks(2) {
        let hi = pair.first().copied().unwrap_or(0);
        let lo = pair.get(1).copied().unwrap_or(0);
        out.push((hi << 4) | lo);
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
            .map(|i| ((i as f64 * 0.15).sin() * 6000.0) as i16)
            .collect()
    }

    #[test]
    fn mono_round_trips_approximately() {
        let samples = tone(50);
        let block = encode_block(&samples, 1).unwrap();
        let decoded = decode_block(&block, 1).unwrap();
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((i32::from(*a) - i32::from(*b)).abs() < 2500, "{a} vs {b}");
        }
    }

    #[test]
    fn stereo_round_trips_and_preserves_channel_identity() {
        let n = 30;
        let mut samples = Vec::new();
        for i in 0..n {
            samples.push(2000i16);
            samples.push(-2000i16);
            let _ = i;
        }
        let block = encode_block(&samples, 2).unwrap();
        let decoded = decode_block(&block, 2).unwrap();
        for pair in decoded.chunks(2) {
            assert!(pair[0] > 0);
            assert!(pair[1] < 0);
        }
    }

    #[test]
    fn short_block_is_rejected() {
        assert!(decode_block(&[0, 1, 2], 1).is_err());
    }

    #[test]
    fn bad_coefficient_index_is_rejected() {
        let mut block = vec![99u8]; // out-of-range coeff index for mono
        block.extend_from_slice(&16i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        block.extend_from_slice(&0i16.to_le_bytes());
        assert!(decode_block(&block, 1).is_err());
    }
}
