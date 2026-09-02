//! `pulse_data()` (ISO/IEC 14496-3 subpart 4 Table 4.7) and its application
//! to `x_quant` (§4.6.3.3's pseudo-C, reproduced in [`apply`]'s doc).
//!
//! Pulses adjust already-Huffman-decoded quantized coefficients directly —
//! "restore coefficients" the encoder replaced with smaller-magnitude ones —
//! before inverse quantisation, so this belongs with spectral decode
//! (#444's own issue title names "pulse data"), not with reconstruction.
//!
//! "the pulse escape method is illegal for a block whose `window_sequence` is
//! `EIGHT_SHORT_SEQUENCE`" (§4.6.3.3) — this crate does not enforce that as
//! a hard error (a stream that violates it is otherwise well-formed to
//! parse; the pseudocode's own `g=0, win=0` already assumes a single window
//! group of one window, which is exactly what every non-`EIGHT_SHORT`
//! sequence has), it simply is never invoked for one.

use vaco_bitstream::BitReader;
use vaco_core::Result;

/// A parsed `pulse_data()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PulseData {
    pub(crate) start_sfb: u8,
    /// `pulse_offset[i]`, one per pulse (`number_pulse + 1` of them).
    pub(crate) offsets: Vec<u8>,
    /// `pulse_amp[i]`, parallel to `offsets`.
    pub(crate) amplitudes: Vec<u8>,
}

impl PulseData {
    /// # Errors
    ///
    /// [`vaco_core::Error::UnexpectedEof`] on truncation.
    pub(crate) fn read(r: &mut BitReader<'_>) -> Result<Self> {
        let number_pulse = r.get(2);
        let start_sfb = r.get(6) as u8;
        let mut offsets = Vec::new();
        let mut amplitudes = Vec::new();
        for _ in 0..=number_pulse {
            offsets.push(r.get(5) as u8);
            amplitudes.push(r.get(4) as u8);
        }
        r.check()?;
        Ok(Self {
            start_sfb,
            offsets,
            amplitudes,
        })
    }
}

/// Apply a parsed `pulse_data()` to a long window's full `x_quant` array
/// (as [`crate::spectral::read_one_group`] returns it — length matching
/// `swb_offset`'s top boundary, 1024 for a long window), following
/// §4.6.3.3's pseudo-C exactly:
///
/// ```text
/// k = swb_offset[pulse_start_sfb];
/// for j in 0..=number_pulse:
///     k += pulse_offset[j];
///     find sfb (>= pulse_start_sfb) with k < swb_offset[sfb+1];
///     bin = k - swb_offset[sfb];
///     x_quant[bin] += pulse_amp[j] if x_quant[bin] > 0 else -= pulse_amp[j];
/// ```
///
/// A `k` that runs past the end of `swb_offset` (a malformed or adversarial
/// `pulse_offset` sum) stops applying further pulses rather than panicking
/// or indexing out of bounds — this crate's usual "gate rather than guess"
/// stance applied to a syntax element with no natural bound of its own.
pub(crate) fn apply(x_quant: &mut [i32], swb_offset: &[u16], pulse: &PulseData) {
    let Some(&start_offset) = swb_offset.get(usize::from(pulse.start_sfb)) else {
        return;
    };
    let mut k = u32::from(start_offset);
    for (&offset, &amp) in pulse.offsets.iter().zip(pulse.amplitudes.iter()) {
        k = k.saturating_add(u32::from(offset));
        let Some(sfb) =
            (usize::from(pulse.start_sfb)..swb_offset.len().saturating_sub(1)).find(|&sfb| {
                swb_offset
                    .get(sfb + 1)
                    .is_some_and(|&top| k < u32::from(top))
            })
        else {
            return;
        };
        let Some(slot) = x_quant.get_mut(k as usize) else {
            return;
        };
        if *slot > 0 {
            *slot = slot.saturating_add(i32::from(amp));
        } else {
            *slot = slot.saturating_sub(i32::from(amp));
        }
        let _ = sfb; // computed for fidelity to the spec pseudocode; `k` is what indexes x_quant directly.
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::{PulseData, apply};
    use vaco_bitstream::{BitReader, BitWriter};

    #[test]
    fn reads_number_pulse_plus_one_entries() {
        let mut w = BitWriter::new();
        w.put(2, 1); // number_pulse = 1 -> 2 pulses
        w.put(6, 3); // start_sfb
        w.put(5, 2);
        w.put(4, 5);
        w.put(5, 4);
        w.put(4, 3);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let pulse = PulseData::read(&mut r).unwrap();
        assert_eq!(pulse.start_sfb, 3);
        assert_eq!(pulse.offsets, vec![2, 4]);
        assert_eq!(pulse.amplitudes, vec![5, 3]);
    }

    #[test]
    fn a_positive_coefficient_is_increased_and_a_non_positive_one_is_decreased() {
        let swb_offset = [0u16, 4, 8, 12];
        let mut x = vec![5i32, -3, 0, 2];
        let pulse = PulseData {
            start_sfb: 0,
            offsets: vec![0, 1, 1],
            amplitudes: vec![10, 10, 10],
        };
        // k starts at swb_offset[0]=0; +0 -> k=0 (positive: 5+10=15);
        // +1 -> k=1 (negative: -3-10=-13); +1 -> k=2 (zero, goes the "else"
        // branch per the spec's literal `if (>0) add else subtract`: 0-10=-10).
        apply(&mut x, &swb_offset, &pulse);
        assert_eq!(x, vec![15, -13, -10, 2]);
    }

    #[test]
    fn a_pulse_running_past_the_end_stops_rather_than_panicking() {
        let swb_offset = [0u16, 4];
        let mut x = vec![1i32, 2, 3, 4];
        let pulse = PulseData {
            start_sfb: 0,
            offsets: vec![100],
            amplitudes: vec![5],
        };
        apply(&mut x, &swb_offset, &pulse); // must not panic
        assert_eq!(x, vec![1, 2, 3, 4]); // out of range: no change applied
    }
}
