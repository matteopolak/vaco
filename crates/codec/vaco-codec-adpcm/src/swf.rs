//! Adobe SWF ADPCM (`adpcm_swf`).
//!
//! `Vaco-Spec-Ref: adobe-swf-19` — the *SWF File Format Specification*'s
//! ADPCM sound-data format: a 2-bit code-width selector (2/3/4/5-bit codes,
//! `crate::tables::SWF_INDEX_TABLE_*`), then per channel a 16-bit initial
//! sample and a 6-bit initial step-table index, then the codes themselves —
//! all packed MSB-first into a single bitstream (SWF's own bit-packing
//! convention throughout the format). The step-table and per-code adaptation
//! rule are the same IMA/DVI shape [`crate::ima::ImaState`] uses, generalised
//! to codes narrower than 4 bits — see [`Code`] for the generalisation.

use vaco_core::{Error, Result};

use crate::tables::{
    IMA_STEP_TABLE, SWF_INDEX_TABLE_2BIT, SWF_INDEX_TABLE_3BIT, SWF_INDEX_TABLE_4BIT,
    SWF_INDEX_TABLE_5BIT,
};

/// Samples represented by one SWF `ADPCMPACKET`: the initial sample plus its
/// 4095 fixed-width ADPCM codes (SWF File Format Specification v19,
/// `ADPCMMONOPACKET`/`ADPCMSTEREOPACKET`).
pub(crate) const SAMPLES_PER_PACKET: u32 = 4096;

pub(crate) fn validate_channels(channels: u32) -> Result<u32> {
    if (1..=2).contains(&channels) {
        Ok(channels)
    } else {
        Err(Error::InvalidData(
            "adpcm_swf: channel count must be mono or stereo",
        ))
    }
}

/// An MSB-first bit reader over a byte slice.
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    #[allow(
        clippy::integer_division,
        reason = "converting a bit offset to a byte index is exact floor division by 8, \
                  not a rounding shortcut"
    )]
    fn read(&mut self, bits: u32) -> Result<u32> {
        let mut out = 0u32;
        for _ in 0..bits {
            let byte_idx = self.bit_pos / 8;
            let bit_idx = 7 - (self.bit_pos % 8);
            let byte = *self.data.get(byte_idx).ok_or(Error::UnexpectedEof)?;
            let bit = (byte >> bit_idx) & 1;
            out = (out << 1) | u32::from(bit);
            self.bit_pos += 1;
        }
        Ok(out)
    }
}

/// An MSB-first bit writer, growing a byte buffer.
#[derive(Default)]
struct BitWriter {
    data: Vec<u8>,
    bit_pos: usize,
}

impl BitWriter {
    #[allow(
        clippy::integer_division,
        reason = "converting a bit offset to a byte index is exact floor division by 8, \
                  not a rounding shortcut"
    )]
    fn write(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            let bit = (value >> i) & 1;
            let byte_idx = self.bit_pos / 8;
            if byte_idx >= self.data.len() {
                self.data.push(0);
            }
            if bit != 0
                && let Some(slot) = self.data.get_mut(byte_idx)
            {
                *slot |= 1 << (7 - (self.bit_pos % 8));
            }
            self.bit_pos += 1;
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

fn index_table(bits: u32) -> &'static [i32] {
    match bits {
        2 => &SWF_INDEX_TABLE_2BIT,
        3 => &SWF_INDEX_TABLE_3BIT,
        5 => &SWF_INDEX_TABLE_5BIT,
        _ => &SWF_INDEX_TABLE_4BIT,
    }
}

fn clamp_index(index: i32) -> i32 {
    index.clamp(0, 88)
}

/// One channel's running state, generalised to `bits`-wide codes (2..=5).
struct SwfState {
    predictor: i32,
    index: i32,
    bits: u32,
}

impl SwfState {
    fn decode_code(&mut self, code: u32) -> i16 {
        let step = *IMA_STEP_TABLE.get(self.index as usize).unwrap_or(&7);
        let mag_bits = self.bits - 1;
        let sign_bit = 1u32 << mag_bits;
        let magnitude = code & (sign_bit - 1);
        let mut diff = step >> mag_bits;
        for i in 0..mag_bits {
            if magnitude & (1 << i) != 0 {
                diff += step >> (mag_bits - 1 - i);
            }
        }
        if code & sign_bit != 0 {
            diff = -diff;
        }
        self.predictor = (self.predictor + diff).clamp(-32768, 32767);
        let delta = *index_table(self.bits).get(code as usize).unwrap_or(&0);
        self.index = clamp_index(self.index + delta);
        self.predictor as i16
    }

    fn encode_sample(&mut self, sample: i16) -> u32 {
        let step = *IMA_STEP_TABLE.get(self.index as usize).unwrap_or(&7);
        let mag_bits = self.bits - 1;
        let diff = i32::from(sample) - self.predictor;
        let (sign, mut mag) = if diff < 0 {
            (1u32, -diff)
        } else {
            (0u32, diff)
        };
        let mut code = 0u32;
        let mut tmp = step;
        for i in (0..mag_bits).rev() {
            if mag >= tmp {
                code |= 1 << i;
                mag -= tmp;
            }
            tmp >>= 1;
        }
        code |= sign << mag_bits;
        self.decode_code(code);
        code
    }
}

/// Decode one SWF ADPCM block: `[nBits: 2 bits]` then, per channel,
/// `[initial sample: 16 bits][initial index: 6 bits]`, then codes.
///
/// `sample_count` is how many samples per channel to decode (SWF's own
/// framing states this at the container level, in the `SoundStreamHead`/
/// `DefineSound` tag, which this codec-level function does not see — the
/// caller supplies it, the same way it supplies `channels`).
///
/// # Errors
/// [`Error::InvalidData`] when `channels` is not mono or stereo;
/// [`Error::UnexpectedEof`] for a block shorter than `sample_count` implies.
pub(crate) fn decode_block(data: &[u8], channels: u32, sample_count: u32) -> Result<Vec<i16>> {
    let channels = validate_channels(channels)?;
    let mut r = BitReader::new(data);
    let bits = r.read(2)? + 2;
    let mut states = Vec::new();
    let mut per_channel: Vec<Vec<i16>> = Vec::new();
    for _ in 0..channels {
        let initial = r.read(16)? as i16;
        let index = clamp_index(r.read(6)?.cast_signed());
        states.push(SwfState {
            predictor: i32::from(initial),
            index,
            bits,
        });
        per_channel.push(vec![initial]);
    }
    for _ in 1..sample_count {
        for (c, state) in states.iter_mut().enumerate() {
            let code = r.read(bits)?;
            let sample = state.decode_code(code);
            if let Some(out) = per_channel.get_mut(c) {
                out.push(sample);
            }
        }
    }
    Ok(crate::ima::interleave(&per_channel))
}

/// Encode `samples` (interleaved, channel-minor) as one SWF ADPCM block,
/// always at 4-bit codes (the IMA-equivalent width, a reasonable default —
/// a real encoder chooses per-block width to trade quality for size).
///
/// SWF fixes every packet at [`SAMPLES_PER_PACKET`] samples per channel.
/// Short input is padded by repeating each channel's final sample; callers
/// retain the original count in the packet duration. A single packet cannot
/// represent more than that fixed count.
///
/// # Errors
/// [`Error::InvalidData`] if there are no samples or if the input exceeds one
/// fixed-size packet.
pub(crate) fn encode_block(samples: &[i16], channels: u32) -> Result<Vec<u8>> {
    let channels = validate_channels(channels)?;
    let mut per_channel = crate::ima::deinterleave(samples, channels as usize)?;
    let Some(input_len) = per_channel.first().map(Vec::len) else {
        return Err(Error::InvalidData("adpcm_swf: no samples"));
    };
    if input_len == 0 {
        return Err(Error::InvalidData("adpcm_swf: no samples"));
    }
    let packet_len = usize::try_from(SAMPLES_PER_PACKET).unwrap_or(usize::MAX);
    if input_len > packet_len {
        return Err(Error::InvalidData(
            "adpcm_swf: more samples than one packet can represent",
        ));
    }
    for channel in &mut per_channel {
        let last = channel.last().copied().unwrap_or(0);
        channel.resize(packet_len, last);
    }
    let len = packet_len;
    let bits = 4u32;
    let mut w = BitWriter::default();
    w.write(bits - 2, 2);
    let mut states = Vec::new();
    for ch in &per_channel {
        let first = ch.first().copied().unwrap_or(0);
        // The header's index field is only 6 bits (0..=63) — narrower than
        // the shared 89-entry step table IMA/QT can address with their own
        // 7/8-bit fields, so clamp here rather than reusing `clamp_index`.
        let index = crate::ima::estimate_initial_index(ch).clamp(0, 63);
        w.write(u32::from(first.cast_unsigned()), 16);
        w.write(index as u32, 6);
        states.push(SwfState {
            predictor: i32::from(first),
            index,
            bits,
        });
    }
    for n in 1..len {
        for (c, ch) in per_channel.iter().enumerate() {
            let Some(state) = states.get_mut(c) else {
                continue;
            };
            let sample = ch.get(n).copied().unwrap_or(0);
            let code = state.encode_sample(sample);
            w.write(code, bits);
        }
    }
    Ok(w.into_bytes())
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
            .map(|i| ((i as f64 * 0.25).sin() * 5000.0) as i16)
            .collect()
    }

    #[test]
    fn bit_reader_writer_round_trips() {
        let mut w = BitWriter::default();
        w.write(0b10, 2);
        w.write(0b10110, 5);
        w.write(0xFFFF, 16);
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read(2).unwrap(), 0b10);
        assert_eq!(r.read(5).unwrap(), 0b10110);
        assert_eq!(r.read(16).unwrap(), 0xFFFF);
    }

    #[test]
    fn mono_round_trips_approximately() {
        let samples = tone(40);
        let block = encode_block(&samples, 1).unwrap();
        let decoded = decode_block(&block, 1, samples.len() as u32).unwrap();
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((i32::from(*a) - i32::from(*b)).abs() < 2500, "{a} vs {b}");
        }
    }

    #[test]
    fn stereo_preserves_channel_identity() {
        let n = 20;
        let mut samples = Vec::new();
        for _ in 0..n {
            samples.push(3000i16);
            samples.push(-3000i16);
        }
        let block = encode_block(&samples, 2).unwrap();
        let decoded = decode_block(&block, 2, n as u32).unwrap();
        for pair in decoded.chunks(2) {
            assert!(pair[0] > 0);
            assert!(pair[1] < 0);
        }
    }

    #[test]
    fn truncated_block_is_an_eof_error() {
        assert!(matches!(
            decode_block(&[0], 1, 100),
            Err(Error::UnexpectedEof)
        ));
    }

    #[test]
    fn oversized_block_is_rejected_instead_of_emitting_invalid_wire() {
        let samples = vec![0i16; SAMPLES_PER_PACKET as usize + 1];
        assert!(matches!(
            encode_block(&samples, 1),
            Err(Error::InvalidData(
                "adpcm_swf: more samples than one packet can represent"
            ))
        ));
    }

    #[test]
    fn unsupported_channel_counts_are_rejected() {
        for channels in [0, 3, u32::MAX] {
            assert!(matches!(
                decode_block(&[0; 3], channels, 1),
                Err(Error::InvalidData(
                    "adpcm_swf: channel count must be mono or stereo"
                ))
            ));
            assert!(matches!(
                encode_block(&[0], channels),
                Err(Error::InvalidData(
                    "adpcm_swf: channel count must be mono or stereo"
                ))
            ));
        }
    }
}
