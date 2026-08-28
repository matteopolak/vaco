//! `tns_data()` — ISO/IEC 14496-3 subpart 4 Table 4.54, field widths from
//! Table 4.155.
//!
//! # Parsed, not applied
//!
//! TNS *application* (`tns_decode_coef`/`tns_ar_filter`, §4.6.9.3 — inverse
//! quantisation of the filter coefficients into LPC coefficients, then an
//! all-pole filter over the spectral coefficients) is reconstruction, #445's
//! "TNS" — not this crate's. This module reads the syntax exactly (every
//! field's bit width depends on `window_sequence` and on `coef_res`/
//! `coef_compress`, so there is no way to *skip* `tns_data()` without
//! parsing it) and keeps every field, so #445 has exactly what it needs
//! without re-parsing.

use vaco_bitstream::BitReader;
use vaco_core::Result;

use crate::ics::WindowSequence;

/// One noise-shaping filter, `filt` of `n_filt[w]`, for one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TnsFilter {
    pub(crate) length: u8,
    pub(crate) order: u8,
    /// `false` = upward, `true` = downward (only meaningful when `order > 0`).
    pub(crate) direction: bool,
    /// Only meaningful when `order > 0`.
    pub(crate) coef_compress: bool,
    /// `coef_res[w]`, the *window's* resolution flag (`tns_decode_coef`'s
    /// `coef_res_bits = coef_res + 3`) — carried per filter, redundantly
    /// with its siblings in the same window, so `tns_apply` can inverse-
    /// quantise a filter without threading a second array alongside
    /// [`TnsData::per_window`].
    pub(crate) coef_res: bool,
    /// `order` raw (not yet inverse-quantised) coefficients.
    pub(crate) coef: Vec<u8>,
}

/// A parsed `tns_data()`: one list of filters per window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TnsData {
    pub(crate) per_window: Vec<Vec<TnsFilter>>,
}

impl TnsData {
    /// Read `tns_data()` for a block with `num_windows` windows of the given
    /// sequence (only `is_short` actually affects field widths, per Table
    /// 4.155, but `num_windows` is what drives the outer loop).
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::UnexpectedEof`] on truncation.
    pub(crate) fn read(
        r: &mut BitReader<'_>,
        window_sequence: WindowSequence,
        num_windows: usize,
    ) -> Result<Self> {
        let is_short = window_sequence.is_short();
        let (n_filt_bits, length_bits, order_bits) = if is_short { (1, 4, 3) } else { (2, 6, 5) };

        let mut per_window = Vec::new();
        for _ in 0..num_windows {
            let n_filt = r.get(n_filt_bits);
            let coef_res = if n_filt != 0 { r.get_bit() != 0 } else { false };
            let mut filters = Vec::new();
            for _ in 0..n_filt {
                let length = r.get(length_bits) as u8;
                let order = r.get(order_bits) as u8;
                let (direction, coef_compress, coef) = if order != 0 {
                    let direction = r.get_bit() != 0;
                    let coef_compress = r.get_bit() != 0;
                    let base_bits = if coef_res { 4u32 } else { 3 };
                    let coef_bits = base_bits.saturating_sub(u32::from(coef_compress));
                    let mut coef = Vec::new();
                    for _ in 0..order {
                        coef.push(r.get(coef_bits) as u8);
                    }
                    (direction, coef_compress, coef)
                } else {
                    (false, false, Vec::new())
                };
                filters.push(TnsFilter {
                    length,
                    order,
                    direction,
                    coef_compress,
                    coef_res,
                    coef,
                });
            }
            per_window.push(filters);
        }
        r.check()?;
        Ok(Self { per_window })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
    use super::TnsData;
    use crate::ics::WindowSequence;
    use vaco_bitstream::{BitReader, BitWriter};

    #[test]
    fn no_filters_consumes_exactly_n_filt_bits() {
        let mut w = BitWriter::new();
        w.put(2, 0); // n_filt[0] = 0, long window
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let tns = TnsData::read(&mut r, WindowSequence::OnlyLong, 1).unwrap();
        assert_eq!(tns.per_window.len(), 1);
        assert!(tns.per_window[0].is_empty());
    }

    #[test]
    fn one_long_filter_round_trips_every_field() {
        let mut w = BitWriter::new();
        w.put(2, 1); // n_filt[0] = 1
        w.put(1, 1); // coef_res = 1 (4-bit base)
        w.put(6, 20); // length
        w.put(5, 3); // order
        w.put(1, 1); // direction = downward
        w.put(1, 1); // coef_compress = 1 -> 3-bit coefficients
        w.put(3, 5);
        w.put(3, 2);
        w.put(3, 7);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let tns = TnsData::read(&mut r, WindowSequence::OnlyLong, 1).unwrap();
        let filt = &tns.per_window[0][0];
        assert_eq!(filt.length, 20);
        assert_eq!(filt.order, 3);
        assert!(filt.direction);
        assert!(filt.coef_compress);
        assert_eq!(filt.coef, vec![5, 2, 7]);
    }

    #[test]
    fn eight_short_uses_the_narrower_field_widths() {
        let mut w = BitWriter::new();
        for _ in 0..8 {
            w.put(1, 0); // n_filt[w] = 0 for every one of the 8 windows
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let tns = TnsData::read(&mut r, WindowSequence::EightShort, 8).unwrap();
        assert_eq!(tns.per_window.len(), 8);
        assert!(tns.per_window.iter().all(Vec::is_empty));
        // Exactly 8 bits consumed: 1 bit each, no filters.
        assert_eq!(r.bit_pos(), 8);
    }
}
