//! `scale_factor_data()` — ISO/IEC 14496-3 subpart 4 Table 4.53 (the
//! non-error-resilient branch; `aacScalefactorDataResilienceFlag` is never
//! set for plain AAC-LC/ADTS streams).
//!
//! # Three independent DPCM chains, one shared codebook
//!
//! Every active (non-`ZERO_HCB`) scalefactor band carries one of three
//! different quantities, each its own differentially-coded (DPCM) chain
//! with its own running predecessor, all Huffman-coded through the *same*
//! 121-entry scalefactor codebook (`spectral_tables::SCALEFACTOR_HUFFMAN`,
//! decoding to an index 0..=120 translated to a signed delta by subtracting
//! 60, per Table 4.150's `index_offset = -60`):
//!
//! - **Regular scalefactors** (`sfb_cb` 1..=11): seeded from `global_gain`
//!   (read earlier, in `individual_channel_stream()`, 8-bit absolute PCM —
//!   "the first active scalefactor is differentially coded relative to the
//!   global gain", §4.6.2's "Recovering `scale_factor_data()`" prose).
//! - **Intensity stereo positions** (`sfb_cb` 14/15, `INTENSITY_HCB2`/
//!   `INTENSITY_HCB`): seeded at **0** — "there is no first value sent as
//!   PCM" (§4.6.8.2.3) — and never touches the regular-scalefactor
//!   predecessor: "the scalefactor decoder ignores interposed intensity
//!   stereo position values and vice versa."
//! - **Noise (PNS) energies** (`sfb_cb` 13, `NOISE_HCB`): the *first*
//!   occurrence in the whole channel is a raw 9-bit **PCM** value (not
//!   Huffman-coded at all — `noise_pcm_flag`, Table 4.53), added to a
//!   `global_gain - NOISE_OFFSET(90) - 256` seed (§4.6.13.3); every
//!   subsequent noise band is a normal Huffman DPCM delta against the
//!   running noise energy.
//!
//! This module produces the **integer** per-band values (the running-sum
//! result of each DPCM chain) — not the linear-domain gain each represents.
//! Turning a scalefactor into `2^(0.25*sf)`, an intensity position into
//! `0.5^(0.25*is_position)`, or a noise energy into an actual generated and
//! scaled random vector is reconstruction (`4.6.2`'s own "Once the
//! scalefactors are decoded, the actual values are found via a power
//! function" — #445's "inverse quantisation"/"joint stereo", not this
//! crate's "scalefactor decode").

use vaco_bitstream::BitReader;
use vaco_core::Result;

use crate::section::{INTENSITY_HCB, INTENSITY_HCB2, NOISE_HCB, ZERO_HCB};
use crate::spectral_tables::SCALEFACTOR_HUFFMAN;
use vaco_codec_vlc::VlcTable;

/// ISO/IEC 14496-3 subpart 4 §4.6.13.3.
const NOISE_OFFSET: i32 = 90;

/// What kind of value an active band's DPCM chain produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BandValue {
    /// A regular scalefactor (still in log/index domain — not yet a gain).
    Scalefactor(i32),
    /// An intensity stereo position.
    IntensityPosition(i32),
    /// A noise (PNS) energy.
    NoiseEnergy(i32),
    /// `ZERO_HCB`: no data transmitted, value is definitionally zero.
    Zero,
}

/// Decode a Huffman-coded scalefactor-codebook delta: the raw index (0..120)
/// minus the 60 offset from Table 4.150.
fn read_delta(r: &mut BitReader<'_>) -> Result<i32> {
    let table = VlcTable::new(&SCALEFACTOR_HUFFMAN);
    let index = table.decode(r).ok_or(vaco_core::Error::InvalidData(
        "vaco-codec-aac: scalefactor Huffman codeword matches no entry",
    ))?;
    Ok(index.cast_signed() - 60)
}

/// Read `scale_factor_data()` for every window group, given each group's
/// per-band codebook assignment (from [`crate::section::read_all_groups`])
/// and the frame's `global_gain`.
///
/// # Errors
///
/// [`vaco_core::Error::UnexpectedEof`]/[`vaco_core::Error::InvalidData`] on
/// truncation or an unmatched Huffman codeword.
pub(crate) fn read_all_groups(
    r: &mut BitReader<'_>,
    sfb_cb: &[Vec<u8>],
    global_gain: u8,
) -> Result<Vec<Vec<BandValue>>> {
    let mut sf_pred = i32::from(global_gain);
    let mut is_pred = 0i32;
    let mut noise_pred = i32::from(global_gain) - NOISE_OFFSET - 256;
    let mut noise_pcm_flag = true;

    let mut groups = Vec::new();
    for group in sfb_cb {
        let mut bands = Vec::new();
        for &cb in group {
            let value = match cb {
                ZERO_HCB => BandValue::Zero,
                INTENSITY_HCB | INTENSITY_HCB2 => {
                    is_pred += read_delta(r)?;
                    BandValue::IntensityPosition(is_pred)
                }
                NOISE_HCB if noise_pcm_flag => {
                    noise_pcm_flag = false;
                    let raw = r.get(9).cast_signed();
                    noise_pred += raw;
                    BandValue::NoiseEnergy(noise_pred)
                }
                NOISE_HCB => {
                    noise_pred += read_delta(r)?;
                    BandValue::NoiseEnergy(noise_pred)
                }
                _ => {
                    sf_pred += read_delta(r)?;
                    BandValue::Scalefactor(sf_pred)
                }
            };
            bands.push(value);
        }
        groups.push(bands);
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::cast_possible_wrap,
        reason = "test code"
    )]
    use super::{BandValue, read_all_groups};
    use crate::spectral_tables::SCALEFACTOR_HUFFMAN;
    use vaco_bitstream::{BitReader, BitWriter};
    use vaco_codec_vlc::VlcTable;

    fn write_delta(w: &mut BitWriter, delta: i32) {
        let idx = (delta + 60) as u32;
        let entry = SCALEFACTOR_HUFFMAN
            .iter()
            .find(|e| e.symbol == idx)
            .unwrap();
        w.put(u32::from(entry.len), entry.code);
    }

    #[test]
    fn zero_hcb_band_reads_nothing() {
        let sfb_cb = vec![vec![0u8, 0, 0]];
        let bytes: Vec<u8> = vec![];
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, &sfb_cb, 100).unwrap();
        assert_eq!(groups, vec![vec![BandValue::Zero; 3]]);
    }

    #[test]
    fn first_active_scalefactor_is_relative_to_global_gain() {
        let sfb_cb = vec![vec![5u8]]; // a regular codebook
        let mut w = BitWriter::new();
        write_delta(&mut w, 0); // zero delta -> equals global_gain
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, &sfb_cb, 100).unwrap();
        assert_eq!(groups, vec![vec![BandValue::Scalefactor(100)]]);
    }

    #[test]
    fn scalefactor_chain_accumulates_and_ignores_intensity_bands() {
        let sfb_cb = vec![vec![5u8, 15, 5]]; // regular, intensity, regular
        let mut w = BitWriter::new();
        write_delta(&mut w, 3); // regular: 100 + 3 = 103
        write_delta(&mut w, -7); // intensity: 0 + -7 = -7 (own chain)
        write_delta(&mut w, 2); // regular: 103 + 2 = 105 (unaffected by intensity)
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, &sfb_cb, 100).unwrap();
        assert_eq!(
            groups,
            vec![vec![
                BandValue::Scalefactor(103),
                BandValue::IntensityPosition(-7),
                BandValue::Scalefactor(105),
            ]]
        );
    }

    #[test]
    fn first_noise_band_is_raw_9_bit_pcm_not_huffman() {
        let sfb_cb = vec![vec![13u8, 13]]; // two noise bands
        let mut w = BitWriter::new();
        w.put(9, 5); // raw PCM for the first noise band
        write_delta(&mut w, 2); // Huffman delta for the second
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, &sfb_cb, 100).unwrap();
        let seed = 100 - 90 - 256;
        assert_eq!(
            groups,
            vec![vec![
                BandValue::NoiseEnergy(seed + 5),
                BandValue::NoiseEnergy(seed + 5 + 2),
            ]]
        );
    }

    #[test]
    fn noise_pcm_flag_is_shared_across_groups_not_reset_per_group() {
        // First noise band of the whole channel is PCM even if it's in the
        // second window group, not the first.
        let sfb_cb = vec![vec![0u8], vec![13u8]];
        let mut w = BitWriter::new();
        w.put(9, 10);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, &sfb_cb, 50).unwrap();
        let seed = 50 - 90 - 256;
        assert_eq!(groups[1][0], BandValue::NoiseEnergy(seed + 10));
    }

    #[test]
    fn every_decoded_delta_round_trips_through_the_real_huffman_table() {
        // Sanity: `write_delta`'s reverse lookup and `VlcTable::decode`
        // agree for the full range Table 4.150 allows.
        let table = VlcTable::new(&SCALEFACTOR_HUFFMAN);
        for delta in -60..=60 {
            let mut w = BitWriter::new();
            write_delta(&mut w, delta);
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            let sym = table.decode(&mut r).unwrap();
            assert_eq!(sym as i32 - 60, delta);
        }
    }
}
