//! `individual_channel_stream()` (ISO/IEC 14496-3 subpart 4 Table 4.50) —
//! the driver that ties `ics_info()`, `section_data()`, `scale_factor_data()`,
//! `pulse_data()`, `tns_data()` and `spectral_data()` together into one
//! channel's worth of decode, in bitstream order.
//!
//! `scale_flag` (Table 4.50's second parameter) is always `false` here: it
//! is only ever `true` for the AAC Scalable object type, which this crate
//! does not implement (object-type gating in `config.rs` rejects everything
//! but LC before decode ever reaches this module).

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

use crate::ics::IcsInfo;
use crate::pulse::{self, PulseData};
use crate::scalefactor::{self, BandValue};
use crate::section;
use crate::spectral;
use crate::swb_tables::{swb_offset_long, swb_offset_short};
use crate::tns::TnsData;

/// One decoded channel: everything `individual_channel_stream()` produced,
/// per window group.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "every field here is carried for #445's reconstruction (window \
              shape/max_sfb, scalefactor and spectral values, TNS filters); \
              this crate's own decoder driver only checks bit consumption \
              today and does not read them back — see docs/codec/vaco-codec-aac.md"
)]
pub(crate) struct IcsStream {
    pub(crate) ics: IcsInfo,
    pub(crate) global_gain: u8,
    /// Per group: per band, the value its DPCM chain produced.
    pub(crate) band_values: Vec<Vec<BandValue>>,
    /// Per group: the raw (pre-inverse-quantisation) spectral coefficients,
    /// zero-filled where nothing was transmitted, pulse-adjusted where
    /// `pulse_data_present`.
    pub(crate) x_quant: Vec<Vec<i32>>,
    pub(crate) tns: Option<TnsData>,
}

/// Read one `individual_channel_stream(common_window, false)`.
///
/// `shared_ics` is `Some` when `common_window` is set (the second channel
/// of a `channel_pair_element()` reuses the first's `ics_info()` rather
/// than reading its own — Table 4.50's `if (!common_window && !scale_flag)
/// ics_info();`).
///
/// # Errors
///
/// [`Error::Unsupported`] for `gain_control_data_present` (SSR-only, not
/// implemented) or a `max_sfb` that exceeds this sample rate's `num_swb`
/// (a corrupt or non-conformant stream — gated, not guessed at, same as
/// everywhere else in this crate). Otherwise whatever the individual
/// section/scalefactor/pulse/TNS/spectral readers return.
pub(crate) fn read(
    r: &mut BitReader<'_>,
    common_window: bool,
    shared_ics: Option<&IcsInfo>,
    sampling_frequency_index: u8,
) -> Result<IcsStream> {
    let global_gain = r.get(8) as u8;

    let ics = if common_window {
        shared_ics.cloned().ok_or(Error::InvalidData(
            "vaco-codec-aac: common_window is set but no shared ics_info is available",
        ))?
    } else {
        IcsInfo::read(r)?
    };

    let is_short = ics.window_sequence.is_short();
    let swb: &[u16] = if is_short {
        swb_offset_short(sampling_frequency_index)
    } else {
        swb_offset_long(sampling_frequency_index)
    }
    .ok_or(Error::Unsupported(
        "vaco-codec-aac: no scalefactor band table for this sampling rate (7350 Hz)",
    ))?;
    let num_swb = swb.len() - 1;
    if usize::from(ics.max_sfb) > num_swb {
        return Err(Error::InvalidData(
            "vaco-codec-aac: max_sfb exceeds this sample rate's num_swb",
        ));
    }

    let num_groups = ics.num_window_groups();
    let group_lengths = ics.window_group_lengths();

    let sfb_cb = section::read_all_groups(r, num_groups, ics.max_sfb, is_short)?;
    let band_values = scalefactor::read_all_groups(r, &sfb_cb, global_gain)?;

    let pulse_data_present = r.get_bit() != 0;
    let pulse = if pulse_data_present {
        Some(PulseData::read(r)?)
    } else {
        None
    };

    let tns_data_present = r.get_bit() != 0;
    let tns = if tns_data_present {
        Some(TnsData::read(
            r,
            ics.window_sequence,
            ics.window_sequence.num_windows(),
        )?)
    } else {
        None
    };

    let gain_control_data_present = r.get_bit() != 0;
    if gain_control_data_present {
        return Err(Error::Unsupported(
            "vaco-codec-aac: gain_control_data is SSR-only and not implemented",
        ));
    }

    let mut x_quant = Vec::new();
    for (g, group_cb) in sfb_cb.iter().enumerate() {
        let group_len = u32::from(*group_lengths.get(g).unwrap_or(&1));
        let mut widths = Vec::new();
        for sfb in 0..usize::from(ics.max_sfb) {
            let Some((&lo, &hi)) = swb.get(sfb).zip(swb.get(sfb + 1)) else {
                break;
            };
            let w = u32::from(hi) - u32::from(lo);
            widths.push(if is_short { w * group_len } else { w });
        }
        let mut xq = spectral::read_one_group(r, group_cb, &widths)?;
        if let (Some(p), false) = (&pulse, is_short) {
            pulse::apply(&mut xq, swb, p);
        }
        x_quant.push(xq);
    }

    Ok(IcsStream {
        ics,
        global_gain,
        band_values,
        x_quant,
        tns,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::read;
    use crate::spectral_tables::SCALEFACTOR_HUFFMAN;
    use vaco_bitstream::{BitReader, BitWriter};

    /// Build a minimal, valid, single-channel `individual_channel_stream()`
    /// for a `ONLY_LONG_SEQUENCE` at `sfi=4` (44100 Hz, 49 scalefactor
    /// bands): `max_sfb=1`, one section covering it with `ZERO_HCB`, so
    /// nothing further is transmitted.
    fn minimal_ics_bytes() -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put(8, 100); // global_gain
        // ics_info: reserved, ONLY_LONG, sine, max_sfb=1, predictor=0
        w.put(1, 0);
        w.put(2, 0);
        w.put(1, 0);
        w.put(6, 1);
        w.put(1, 0);
        // section_data: one section, ZERO_HCB, length 1
        w.put(4, 0);
        w.put(5, 1);
        // scale_factor_data: nothing (ZERO_HCB band)
        // pulse_data_present, tns_data_present, gain_control_data_present
        w.put(1, 0);
        w.put(1, 0);
        w.put(1, 0);
        // spectral_data: nothing (ZERO_HCB band)
        w.finish()
    }

    #[test]
    fn a_minimal_all_zero_ics_reads_cleanly_and_consumes_no_spectral_bits() {
        let bytes = minimal_ics_bytes();
        let mut r = BitReader::new(&bytes);
        let stream = read(&mut r, false, None, 4).unwrap();
        assert_eq!(stream.global_gain, 100);
        assert_eq!(stream.x_quant.len(), 1); // one window group
        assert!(stream.x_quant[0].iter().all(|&v| v == 0));
        assert!(stream.tns.is_none());
    }

    #[test]
    fn max_sfb_exceeding_num_swb_is_rejected() {
        let mut w = BitWriter::new();
        w.put(8, 100);
        w.put(1, 0);
        w.put(2, 0); // ONLY_LONG
        w.put(1, 0);
        w.put(6, 63); // max_sfb=63 > 49 bands at sfi=4
        w.put(1, 0);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(read(&mut r, false, None, 4).is_err());
    }

    #[test]
    fn common_window_without_a_shared_ics_is_rejected() {
        let bytes: Vec<u8> = vec![0u8; 4];
        let mut r = BitReader::new(&bytes);
        assert!(read(&mut r, true, None, 4).is_err());
    }

    #[test]
    fn a_real_scalefactor_band_advances_global_gain_and_consumes_no_pulse_tns_bits() {
        let mut w = BitWriter::new();
        w.put(8, 100); // global_gain
        w.put(1, 0);
        w.put(2, 0); // ONLY_LONG
        w.put(1, 0);
        w.put(6, 1); // max_sfb=1
        w.put(1, 0);
        // section: codebook 5 (dim 2, signed, lav 4), covers band 0 (width 4
        // at sfi=4: swb_offset[0..1] = 0..4)
        w.put(4, 5);
        w.put(5, 1);
        // scale factor: delta 0 (equals global_gain)
        let entry = SCALEFACTOR_HUFFMAN.iter().find(|e| e.symbol == 60).unwrap();
        w.put(u32::from(entry.len), entry.code);
        w.put(1, 0); // pulse_data_present
        w.put(1, 0); // tns_data_present
        w.put(1, 0); // gain_control_data_present
        // spectral: 2 tuples of dim 2 for 4 lines, codebook 5 (mod=9,off=4):
        // both (0,0) -> idx = 4*9+4 = 40
        let hcb5_zero = crate::spectral_tables::SPECTRUM_HCB_5
            .iter()
            .find(|e| e.symbol == 40)
            .unwrap();
        w.put(u32::from(hcb5_zero.len), hcb5_zero.code);
        w.put(u32::from(hcb5_zero.len), hcb5_zero.code);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let stream = read(&mut r, false, None, 4).unwrap();
        assert_eq!(stream.x_quant[0], vec![0, 0, 0, 0]);
    }
}
