//! Bluetooth Low Complexity Subband Codec (SBC) decoding.
//!
//! The frame parser, CRC-8, bit allocation, APCM reconstruction, joint
//! processing, and polyphase synthesis filter follow Bluetooth A2DP 1.3.2,
//! Appendix B, sections 12.4-12.8. One packet is one complete `0x9c` SBC
//! frame. The synthesis history persists across packets and is reset when the
//! channel count, sample rate, or subband count changes.

use std::f64::consts::PI;

use vaco_core::{Error, Result};
use vaco_limits::Budget;

const SAMPLE_RATES: [u32; 4] = [16_000, 32_000, 44_100, 48_000];
const OFFSET_4: [[i8; 4]; 4] = [[-1, 0, 0, 0], [-2, 0, 0, 1], [-2, 0, 0, 1], [-2, 0, 0, 1]];
const OFFSET_8: [[i8; 8]; 4] = [
    [-2, 0, 0, 0, 0, 0, 0, 1],
    [-3, 0, 0, 0, 0, 0, 1, 2],
    [-4, 0, 0, 0, 0, 0, 1, 2],
    [-4, 0, 0, 0, 0, 0, 1, 2],
];

// A2DP 1.3.2 Tables 12.23 and 12.24, extracted mechanically from the PDF.
#[allow(
    clippy::unreadable_literal,
    reason = "preserve the specification table's exact decimal notation"
)]
const PROTO_4: [f64; 40] = [
    0.00000000E+00,
    5.36548976E-04,
    1.49188357E-03,
    2.73370904E-03,
    3.83720193E-03,
    3.89205149E-03,
    1.86581691E-03,
    -3.06012286E-03,
    1.09137620E-02,
    2.04385087E-02,
    2.88757392E-02,
    3.21939290E-02,
    2.58767811E-02,
    6.13245186E-03,
    -2.88217274E-02,
    -7.76463494E-02,
    1.35593274E-01,
    1.94987841E-01,
    2.46636662E-01,
    2.81828203E-01,
    2.94315332E-01,
    2.81828203E-01,
    2.46636662E-01,
    1.94987841E-01,
    -1.35593274E-01,
    -7.76463494E-02,
    -2.88217274E-02,
    6.13245186E-03,
    2.58767811E-02,
    3.21939290E-02,
    2.88757392E-02,
    2.04385087E-02,
    -1.09137620E-02,
    -3.06012286E-03,
    1.86581691E-03,
    3.89205149E-03,
    3.83720193E-03,
    2.73370904E-03,
    1.49188357E-03,
    5.36548976E-04,
];

#[allow(
    clippy::unreadable_literal,
    reason = "preserve the specification table's exact decimal notation"
)]
const PROTO_8: [f64; 80] = [
    0.00000000E+00,
    1.56575398E-04,
    3.43256425E-04,
    5.54620202E-04,
    8.23919506E-04,
    1.13992507E-03,
    1.47640169E-03,
    1.78371725E-03,
    2.01182542E-03,
    2.10371989E-03,
    1.99454554E-03,
    1.61656283E-03,
    9.02154502E-04,
    -1.78805361E-04,
    -1.64973098E-03,
    -3.49717454E-03,
    5.65949473E-03,
    8.02941163E-03,
    1.04584443E-02,
    1.27472335E-02,
    1.46525263E-02,
    1.59045603E-02,
    1.62208471E-02,
    1.53184106E-02,
    1.29371806E-02,
    8.85757540E-03,
    2.92408442E-03,
    -4.91578024E-03,
    -1.46404076E-02,
    -2.61098752E-02,
    -3.90751381E-02,
    -5.31873032E-02,
    6.79989431E-02,
    8.29847578E-02,
    9.75753918E-02,
    1.11196689E-01,
    1.23264548E-01,
    1.33264415E-01,
    1.40753505E-01,
    1.45389847E-01,
    1.46955068E-01,
    1.45389847E-01,
    1.40753505E-01,
    1.33264415E-01,
    1.23264548E-01,
    1.11196689E-01,
    9.75753918E-02,
    8.29847578E-02,
    -6.79989431E-02,
    -5.31873032E-02,
    -3.90751381E-02,
    -2.61098752E-02,
    -1.46404076E-02,
    -4.91578024E-03,
    2.92408442E-03,
    8.85757540E-03,
    1.29371806E-02,
    1.53184106E-02,
    1.62208471E-02,
    1.59045603E-02,
    1.46525263E-02,
    1.27472335E-02,
    1.04584443E-02,
    8.02941163E-03,
    -5.65949473E-03,
    -3.49717454E-03,
    -1.64973098E-03,
    -1.78805361E-04,
    9.02154502E-04,
    1.61656283E-03,
    1.99454554E-03,
    2.10371989E-03,
    2.01182542E-03,
    1.78371725E-03,
    1.47640169E-03,
    1.13992507E-03,
    8.23919506E-04,
    5.54620202E-04,
    3.43256425E-04,
    1.56575398E-04,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelMode {
    Mono,
    Dual,
    Stereo,
    JointStereo,
}

#[derive(Debug, Clone, Copy)]
struct Header {
    frequency_index: usize,
    sample_rate: u32,
    blocks: usize,
    mode: ChannelMode,
    channels: usize,
    loudness: bool,
    subbands: usize,
    bitpool: usize,
    crc: u8,
}

#[allow(
    clippy::indexing_slicing,
    reason = "the two-bit frequency index is exactly the four-entry table's domain"
)]
fn parse_header(data: &[u8]) -> Result<Header> {
    let [sync, parameters, bitpool, crc, ..] = data else {
        return Err(Error::UnexpectedEof);
    };
    if *sync != 0x9c {
        return Err(Error::InvalidData("sbc: invalid sync word"));
    }
    let frequency_index = usize::from(parameters >> 6);
    let blocks = 4 * (usize::from((parameters >> 4) & 3) + 1);
    let mode = match (parameters >> 2) & 3 {
        0 => ChannelMode::Mono,
        1 => ChannelMode::Dual,
        2 => ChannelMode::Stereo,
        _ => ChannelMode::JointStereo,
    };
    let channels = if mode == ChannelMode::Mono { 1 } else { 2 };
    let subbands = if parameters & 1 == 0 { 4 } else { 8 };
    let max_bitpool = if matches!(mode, ChannelMode::Mono | ChannelMode::Dual) {
        16 * subbands
    } else {
        32 * subbands
    };
    if !(2..=max_bitpool).contains(&usize::from(*bitpool)) {
        return Err(Error::InvalidData("sbc: bitpool outside profile limits"));
    }
    Ok(Header {
        frequency_index,
        sample_rate: SAMPLE_RATES[frequency_index],
        blocks,
        mode,
        channels,
        loudness: parameters & 2 == 0,
        subbands,
        bitpool: usize::from(*bitpool),
        crc: *crc,
    })
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "the checked end bit proves every indexed byte exists; division maps a bit offset to its byte"
)]
impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], bit: usize) -> Self {
        Self { data, bit }
    }

    fn read(&mut self, count: usize) -> Result<u32> {
        let end = self.bit.checked_add(count).ok_or(Error::UnexpectedEof)?;
        if end > self.data.len().saturating_mul(8) {
            return Err(Error::UnexpectedEof);
        }
        let mut value = 0u32;
        for _ in 0..count {
            let byte = self.data[self.bit / 8];
            value = (value << 1) | u32::from((byte >> (7 - self.bit % 8)) & 1);
            self.bit += 1;
        }
        Ok(value)
    }
}

fn crc_feed_bit(crc: &mut u8, bit: u8) {
    let feedback = (*crc >> 7) ^ bit;
    *crc <<= 1;
    if feedback & 1 != 0 {
        *crc ^= 0x1d;
    }
}

fn checked_crc(data: &[u8], protected_payload_bits: usize) -> Result<u8> {
    let mut crc = 0x0f;
    for &byte in data.get(1..3).ok_or(Error::UnexpectedEof)? {
        for shift in (0..8).rev() {
            crc_feed_bit(&mut crc, (byte >> shift) & 1);
        }
    }
    let mut reader = BitReader::new(data, 32);
    for _ in 0..protected_payload_bits {
        let bit = u8::try_from(reader.read(1)?).unwrap_or(0);
        crc_feed_bit(&mut crc, bit);
    }
    Ok(crc)
}

#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "header enums bound channels to 1..=2 and subbands to 4 or 8; positive loudness div 2 is the specification's integer operation"
)]
fn bitneed(header: Header, scale_factors: &[[u8; 8]; 2]) -> [[i32; 8]; 2] {
    let mut need = [[0i32; 8]; 2];
    for ch in 0..header.channels {
        for sb in 0..header.subbands {
            let scale = i32::from(scale_factors[ch][sb]);
            need[ch][sb] = if !header.loudness {
                scale
            } else if scale == 0 {
                -5
            } else {
                let offset = if header.subbands == 4 {
                    i32::from(OFFSET_4[header.frequency_index][sb])
                } else {
                    i32::from(OFFSET_8[header.frequency_index][sb])
                };
                let loudness = scale - offset;
                if loudness > 0 { loudness / 2 } else { loudness }
            };
        }
    }
    need
}

#[allow(
    clippy::indexing_slicing,
    reason = "subbands is parsed as 4 or 8 and every allocation table has eight entries"
)]
fn allocate_one(need: &[i32; 8], subbands: usize, bitpool: usize) -> [u8; 8] {
    let mut bits = [0u8; 8];
    let max_bitneed = need[..subbands].iter().copied().max().unwrap_or(0).max(0);
    let mut bitcount = 0usize;
    let mut slicecount = 0usize;
    let mut bitslice = max_bitneed + 1;
    loop {
        bitslice -= 1;
        bitcount += slicecount;
        slicecount = 0;
        for &value in &need[..subbands] {
            if value > bitslice + 1 && value < bitslice + 16 {
                slicecount += 1;
            } else if value == bitslice + 1 {
                slicecount += 2;
            }
        }
        if bitcount + slicecount >= bitpool {
            break;
        }
    }
    if bitcount + slicecount == bitpool {
        bitcount += slicecount;
        bitslice -= 1;
    }
    for sb in 0..subbands {
        if need[sb] >= bitslice + 2 {
            bits[sb] = u8::try_from((need[sb] - bitslice).min(16)).unwrap_or(16);
        }
    }
    for sb in 0..subbands {
        if bitcount >= bitpool {
            break;
        }
        if (2..16).contains(&bits[sb]) {
            bits[sb] += 1;
            bitcount += 1;
        } else if need[sb] == bitslice + 1 && bitpool > bitcount + 1 {
            bits[sb] = 2;
            bitcount += 2;
        }
    }
    for value in &mut bits[..subbands] {
        if bitcount >= bitpool {
            break;
        }
        if *value < 16 {
            *value += 1;
            bitcount += 1;
        }
    }
    bits
}

#[allow(
    clippy::indexing_slicing,
    reason = "stereo fixes channels at two and the parsed subband count is 4 or 8"
)]
fn allocate_stereo(need: &[[i32; 8]; 2], subbands: usize, bitpool: usize) -> [[u8; 8]; 2] {
    let mut bits = [[0u8; 8]; 2];
    let max_bitneed = need
        .iter()
        .flat_map(|channel| channel[..subbands].iter().copied())
        .max()
        .unwrap_or(0)
        .max(0);
    let mut bitcount = 0usize;
    let mut slicecount = 0usize;
    let mut bitslice = max_bitneed + 1;
    loop {
        bitslice -= 1;
        bitcount += slicecount;
        slicecount = 0;
        for channel in need {
            for &value in &channel[..subbands] {
                if value > bitslice + 1 && value < bitslice + 16 {
                    slicecount += 1;
                } else if value == bitslice + 1 {
                    slicecount += 2;
                }
            }
        }
        if bitcount + slicecount >= bitpool {
            break;
        }
    }
    if bitcount + slicecount == bitpool {
        bitcount += slicecount;
        bitslice -= 1;
    }
    for ch in 0..2 {
        for sb in 0..subbands {
            if need[ch][sb] >= bitslice + 2 {
                bits[ch][sb] = u8::try_from((need[ch][sb] - bitslice).min(16)).unwrap_or(16);
            }
        }
    }
    for sb in 0..subbands {
        for ch in 0..2 {
            if bitcount >= bitpool {
                break;
            }
            if (2..16).contains(&bits[ch][sb]) {
                bits[ch][sb] += 1;
                bitcount += 1;
            } else if need[ch][sb] == bitslice + 1 && bitpool > bitcount + 1 {
                bits[ch][sb] = 2;
                bitcount += 2;
            }
        }
    }
    for sb in 0..subbands {
        for ch in 0..2 {
            if bitcount >= bitpool {
                break;
            }
            if bits[ch][sb] < 16 {
                bits[ch][sb] += 1;
                bitcount += 1;
            }
        }
    }
    bits
}

#[allow(
    clippy::indexing_slicing,
    reason = "header enums bound channels to 1..=2 before this fixed-array dispatch"
)]
fn allocate_bits(header: Header, scale_factors: &[[u8; 8]; 2]) -> [[u8; 8]; 2] {
    let need = bitneed(header, scale_factors);
    if matches!(header.mode, ChannelMode::Mono | ChannelMode::Dual) {
        let mut bits = [[0u8; 8]; 2];
        for ch in 0..header.channels {
            bits[ch] = allocate_one(&need[ch], header.subbands, header.bitpool);
        }
        bits
    } else {
        allocate_stereo(&need, header.subbands, header.bitpool)
    }
}

/// Persistent polyphase synthesis history for one SBC stream.
#[derive(Debug, Clone)]
pub struct DecoderState {
    synthesis: [[f64; 160]; 2],
    subbands: usize,
    channels: usize,
    sample_rate: u32,
}

impl Default for DecoderState {
    fn default() -> Self {
        Self {
            synthesis: [[0.0; 160]; 2],
            subbands: 0,
            channels: 0,
            sample_rate: 0,
        }
    }
}

impl DecoderState {
    fn prepare(&mut self, channels: usize, subbands: usize, sample_rate: u32) {
        if self.channels != channels || self.subbands != subbands || self.sample_rate != sample_rate
        {
            *self = Self {
                subbands,
                channels,
                sample_rate,
                ..Self::default()
            };
        }
    }
}

#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "subbands is 4 or 8; every derived index is bounded by the fixed 160-sample history and 80-coefficient window"
)]
fn synthesize(state: &mut [f64; 160], subband: &[f64; 8], subbands: usize) -> [i16; 8] {
    let matrix_width = 2 * subbands;
    for i in (matrix_width..20 * subbands).rev() {
        state[i] = state[i - matrix_width];
    }
    for (k, slot) in state[..matrix_width].iter_mut().enumerate() {
        *slot = subband[..subbands]
            .iter()
            .enumerate()
            .map(|(i, sample)| {
                let phase =
                    (i as f64 + 0.5) * (k as f64 + subbands as f64 / 2.0) * PI / subbands as f64;
                phase.cos() * sample
            })
            .sum();
    }

    let mut windowed = [0.0f64; 80];
    let prototype: &[f64] = if subbands == 4 { &PROTO_4 } else { &PROTO_8 };
    for group in 0..5 {
        for j in 0..subbands {
            let first = group * 2 * subbands + j;
            let second = first + subbands;
            windowed[first] =
                state[group * 4 * subbands + j] * prototype[first] * -(subbands as f64);
            windowed[second] = state[group * 4 * subbands + 3 * subbands + j]
                * prototype[second]
                * -(subbands as f64);
        }
    }

    let mut output = [0i16; 8];
    for j in 0..subbands {
        let sample: f64 = (0..10).map(|i| windowed[j + subbands * i]).sum();
        output[j] = sample
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    }
    output
}

/// One decoded SBC frame.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub sample_rate: u32,
    pub channels: u32,
    pub samples_per_channel: u32,
    pub interleaved: Vec<i16>,
}

/// Decode one complete SBC frame according to A2DP 1.3.2 Appendix B.
///
/// # Errors
/// Returns [`Error::InvalidData`] for an invalid sync, profile-illegal
/// bitpool, or CRC mismatch, and [`Error::UnexpectedEof`] for truncation.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "the specification defines a floating-point synthesis filter; all integer ranges are bounded by the SBC header"
)]
pub fn decode(budget: &mut Budget, state: &mut DecoderState, data: &[u8]) -> Result<DecodedFrame> {
    let header = parse_header(data)?;
    budget.check_channels(u64::try_from(header.channels).unwrap_or(2))?;
    let mut reader = BitReader::new(data, 32);
    let mut join = [false; 8];
    let join_bits = if header.mode == ChannelMode::JointStereo {
        for value in &mut join[..header.subbands - 1] {
            *value = reader.read(1)? != 0;
        }
        let _rfa = reader.read(1)?;
        header.subbands
    } else {
        0
    };
    let mut scale_factors = [[0u8; 8]; 2];
    for channel in &mut scale_factors[..header.channels] {
        for value in &mut channel[..header.subbands] {
            *value = u8::try_from(reader.read(4)?).unwrap_or(0);
        }
    }
    let protected_bits = join_bits + 4 * header.channels * header.subbands;
    if checked_crc(data, protected_bits)? != header.crc {
        return Err(Error::InvalidData("sbc: CRC mismatch"));
    }

    let bits = allocate_bits(header, &scale_factors);
    let mut subband_samples = [[[0.0f64; 8]; 2]; 16];
    for block in &mut subband_samples[..header.blocks] {
        for ch in 0..header.channels {
            for sb in 0..header.subbands {
                let width = usize::from(bits[ch][sb]);
                if width != 0 {
                    let quantized = reader.read(width)?;
                    let levels = (1u32 << width) - 1;
                    let scale = f64::from(1u32 << (u32::from(scale_factors[ch][sb]) + 1));
                    block[ch][sb] =
                        scale * ((f64::from(quantized) * 2.0 + 1.0) / f64::from(levels) - 1.0);
                }
            }
        }
        if header.mode == ChannelMode::JointStereo {
            for sb in 0..header.subbands {
                if join[sb] {
                    let sum = block[0][sb] + block[1][sb];
                    let difference = block[0][sb] - block[1][sb];
                    block[0][sb] = sum;
                    block[1][sb] = difference;
                }
            }
        }
    }

    state.prepare(header.channels, header.subbands, header.sample_rate);
    let samples_per_channel = header.blocks * header.subbands;
    let mut interleaved = budget.alloc::<i16>(samples_per_channel * header.channels)?;
    let mut output_offset = 0usize;
    for block in &subband_samples[..header.blocks] {
        let mut channel_output = [[0i16; 8]; 2];
        for ch in 0..header.channels {
            channel_output[ch] = synthesize(&mut state.synthesis[ch], &block[ch], header.subbands);
        }
        for sample in 0..header.subbands {
            for channel in &channel_output[..header.channels] {
                interleaved[output_offset] = channel[sample];
                output_offset += 1;
            }
        }
    }

    Ok(DecodedFrame {
        sample_rate: header.sample_rate,
        channels: u32::try_from(header.channels).unwrap_or(2),
        samples_per_channel: u32::try_from(samples_per_channel).unwrap_or(128),
        interleaved,
    })
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code over fixed specification tables"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn prototype_filters_have_the_specified_symmetry() {
        for i in 1..20 {
            let expected = if i % 8 == 0 { -PROTO_4[i] } else { PROTO_4[i] };
            let mirrored = PROTO_4[40 - i];
            assert!((expected - mirrored).abs() < f64::EPSILON);
        }
        for i in 1..40 {
            let expected = if i % 16 == 0 { -PROTO_8[i] } else { PROTO_8[i] };
            let mirrored = PROTO_8[80 - i];
            assert!((expected - mirrored).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn stream_configuration_change_resets_synthesis_history() {
        let mut state = DecoderState::default();
        state.prepare(1, 8, 44_100);
        state.synthesis[0][0] = 1.0;
        state.prepare(1, 8, 48_000);
        assert!(state.synthesis[0][0].abs() < f64::EPSILON);
    }

    proptest! {
        #[test]
        fn decode_never_panics(data in prop::collection::vec(any::<u8>(), 0..1024)) {
            let mut budget = Budget::new(vaco_limits::Limits::permissive());
            let mut state = DecoderState::default();
            let _ = decode(&mut budget, &mut state, &data);
        }
    }
}
