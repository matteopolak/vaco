//! Layer I decode: header sync (via `vaco-format-mpegaudio`), 4-bit-index
//! bit allocation, linear dequantisation, and the shared synthesis
//! filterbank.
//!
//! `Vaco-Spec-Ref: iso-11172-3` §2.4.2.3 (bit allocation is a direct 4-bit
//! index, `nb = allocation + 1`), §2.4.1.7 (32 subbands × 12 samples = 384
//! samples per frame per channel).

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_format_mpegaudio::MpegAudioHeader;
use vaco_frame::Frame;
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::bitalloc::layer1_dequant;
use crate::synthesis::Synthesis;
use crate::tables::LAYER12_SCALEFACTORS;

const SUBBANDS: usize = 32;
const GRANULES: usize = 12;

/// Decode one Layer I frame's audio payload (the header and any CRC already
/// consumed by the caller) into 384 samples per channel.
///
/// `synth` holds one [`Synthesis`] filterbank per channel, indexed the same
/// way the header's channel order is: left/mono first, right second.
pub(crate) fn decode(
    header: MpegAudioHeader,
    body: &[u8],
    synth: &mut [Synthesis],
    budget: &mut Budget,
) -> Result<Frame> {
    let channels = usize::from(header.channels());
    if synth.len() < channels {
        return Err(Error::Unsupported(
            "mpegaudio: missing per-channel synthesis state",
        ));
    }
    let mut r = BitReader::new(body);

    // 1. Bit allocation: one 4-bit index per subband per channel.
    let mut allocation = [[0u8; SUBBANDS]; 2];
    for sb in 0..SUBBANDS {
        for ch in allocation.iter_mut().take(channels) {
            if let Some(slot) = ch.get_mut(sb) {
                *slot = r.get(4) as u8;
            }
        }
    }

    // 2. Scalefactors: one 6-bit index per subband per channel that got a
    // nonzero allocation.
    let mut scalefactor = [[1.0f32; SUBBANDS]; 2];
    for sb in 0..SUBBANDS {
        for ch in 0..channels {
            let bal = allocation
                .get(ch)
                .and_then(|c| c.get(sb))
                .copied()
                .unwrap_or(0);
            if bal != 0 {
                let idx = usize::from(r.get(6) as u8);
                let value = LAYER12_SCALEFACTORS.get(idx).copied().unwrap_or(0.0);
                if let Some(slot) = scalefactor.get_mut(ch).and_then(|c| c.get_mut(sb)) {
                    *slot = value;
                }
            }
        }
    }

    // 3. 12 granules of one sample per subband per channel, dequantised and
    // fed straight through the synthesis filterbank.
    let samples_per_channel = SUBBANDS * GRANULES;
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); channels];
    for _granule in 0..GRANULES {
        let mut subband_sample = [[0.0f32; SUBBANDS]; 2];
        for sb in 0..SUBBANDS {
            for ch in 0..channels {
                let bal = allocation
                    .get(ch)
                    .and_then(|c| c.get(sb))
                    .copied()
                    .unwrap_or(0);
                if bal == 0 {
                    continue;
                }
                let nb = u32::from(bal) + 1;
                let code = r.get(nb);
                let factor = scalefactor
                    .get(ch)
                    .and_then(|c| c.get(sb))
                    .copied()
                    .unwrap_or(0.0);
                let value = layer1_dequant(code, nb) * factor;
                if let Some(slot) = subband_sample.get_mut(ch).and_then(|c| c.get_mut(sb)) {
                    *slot = value;
                }
            }
        }
        for (ch, synth_ch) in synth.iter_mut().enumerate().take(channels) {
            let Some(sample) = subband_sample.get(ch) else {
                continue;
            };
            let block = synth_ch.synth_block(sample);
            if let Some(dst) = out.get_mut(ch) {
                dst.extend_from_slice(&block);
            }
        }
    }

    let layout = ChannelLayout::default_for(channels as u32)
        .ok_or(Error::Unsupported("mpegaudio: unsupported channel count"))?;
    let mut frame = Frame::alloc_audio(
        budget,
        SampleFmt::F32P,
        layout,
        samples_per_channel as u32,
        header.sample_rate_hz(),
    )?;
    for (ch, samples) in out.iter().enumerate() {
        let mut plane = frame
            .plane_mut(ch)
            .ok_or(Error::Unsupported("mpegaudio: missing output plane"))?;
        let row = plane
            .row_mut(0)
            .ok_or(Error::Unsupported("mpegaudio: output plane too short"))?;
        for (dst, &sample) in row.chunks_exact_mut(4).zip(samples.iter()) {
            dst.copy_from_slice(&sample.to_le_bytes());
        }
    }
    Ok(frame)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn all_zero_allocation_table_decodes_to_silence() {
        // Every subband's 4-bit allocation index is 0 (an all-zero body):
        // no scalefactors or samples get read, and the filterbank's own
        // "silence in, silence out" property should hold.
        let header_word = header_word(0b11, 0b11, 5, 0, 0b11); // MPEG-1 Layer I, mono
        let header = MpegAudioHeader::parse(header_word).unwrap();
        let body = vec![0u8; 200];
        let mut synth = vec![Synthesis::new()];
        let mut budget = Budget::new(Limits::strict());
        let frame = decode(header, &body, &mut synth, &mut budget).unwrap();
        assert!(frame.is_audio());
        let plane = frame.plane(0).unwrap();
        let row = plane.row(0).unwrap();
        assert!(row.chunks_exact(4).all(|b| {
            let v = f32::from_le_bytes(b.try_into().unwrap());
            v == 0.0
        }));
    }

    fn header_word(version: u32, layer: u32, bitrate: u32, rate: u32, mode: u32) -> u32 {
        (0x7FFu32 << 21)
            | (version << 19)
            | (layer << 17)
            | (1 << 16)
            | (bitrate << 12)
            | (rate << 10)
            | (mode << 6)
    }
}
