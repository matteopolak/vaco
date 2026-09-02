//! Applying a parsed [`crate::tns::TnsData`] — ISO/IEC 14496-3 subpart 4
//! §4.6.9.3's `tns_decode_frame`/`tns_decode_coef`/`tns_ar_filter`,
//! reproduced directly from that pseudo-C.
//!
//! `tns.rs` parses the syntax (field widths that depend on `window_sequence`
//! and `coef_res`/`coef_compress`) and stops there, since applying the
//! filter is reconstruction, not syntax. This module is that application:
//! turning the raw transmitted coefficients into LPC coefficients via an
//! arcsine-domain inverse quantisation, then sliding an all-pole filter over
//! the target scalefactor-band range of one window's own spectrum.

use crate::tns::TnsFilter;

/// `TNS_MAX_ORDER`, Table 4.156: this crate only ever decodes AAC-LC
/// (`audioObjectType != 1`, AAC Main), so only the "other AOT" row applies.
const TNS_MAX_ORDER_LONG: u8 = 12;
const TNS_MAX_ORDER_SHORT: u8 = 7;

/// `TNS_MAX_BANDS`, Table 4.157, "without PQF filterbank" columns (AAC-LC
/// has no PQF) — indexed by `samplingFrequencyIndex` 0..=11.
const TNS_MAX_BANDS_LONG: [u8; 12] = [31, 31, 34, 40, 42, 51, 46, 46, 42, 42, 42, 39];
const TNS_MAX_BANDS_SHORT: [u8; 12] = [9, 9, 10, 14, 14, 14, 14, 14, 14, 14, 14, 14];

/// `TNS_MAX_BANDS` for this sample rate and window kind, or the largest
/// value in the table if `sfi` is somehow out of range (defensive only —
/// callers only ever pass a valid `sfi`, already checked upstream).
pub(crate) fn tns_max_bands(sfi: u8, is_short: bool) -> u8 {
    let table = if is_short {
        TNS_MAX_BANDS_SHORT
    } else {
        TNS_MAX_BANDS_LONG
    };
    table.get(usize::from(sfi)).copied().unwrap_or(51)
}

/// `tns_decode_coef`: raw transmitted coefficients to LPC coefficients
/// `a[1..=order]` (`a[0]` is always 1, per the spec's own convention, and is
/// not included in the returned vector — callers index `a[i]` as
/// `lpc[i - 1]`).
fn tns_decode_coef(order: u8, coef_res: bool, coef_compress: bool, coef: &[u8]) -> Vec<f64> {
    let coef_res_bits: u32 = if coef_res { 4 } else { 3 };
    let sgn_mask: [i32; 2] = [0x2, 0x4];
    let neg_mask: [i32; 2] = [!0x3, !0x7];
    // `coef_res_bits` is 3 or 4; `coef_compress` is 0 or 1; `coef_res2` is
    // therefore 2, 3 or 4, and `coef_res2 - 2` is 0, 1 or 2 — but the
    // tables only have two rows (indices for widths 3 and 4, per
    // §4.6.9.3's own `sgn_mask[]`/`neg_mask[]`, both length 2), so a
    // `coef_res2` of 4 (coef_res=1, coef_compress=0, giving a real
    // transmitted width of 4) is out of the tables' own domain — clamp to
    // the last row rather than index past it.
    let coef_res2 = coef_res_bits.saturating_sub(u32::from(coef_compress));
    let idx = usize::from(coef_res2 > 3);
    let s_mask = sgn_mask.get(idx).copied().unwrap_or(0x2);
    let n_mask = neg_mask.get(idx).copied().unwrap_or(!0x3);

    let half = 1i32 << (coef_res_bits.saturating_sub(1));
    let iqfac = (f64::from(half) - 0.5) / std::f64::consts::FRAC_PI_2;
    let iqfac_m = (f64::from(half) + 0.5) / std::f64::consts::FRAC_PI_2;

    let mut tmp2 = Vec::new();
    for &raw in coef.iter().take(usize::from(order)) {
        let signed = i32::from(raw);
        let tmp = if signed & s_mask != 0 {
            signed | n_mask
        } else {
            signed
        };
        let angle = if tmp >= 0 {
            f64::from(tmp) / iqfac
        } else {
            f64::from(tmp) / iqfac_m
        };
        tmp2.push(angle.sin());
    }

    // Conversion to LPC coefficients, a[0] = 1 implicit and not returned.
    let ord = tmp2.len();
    let mut a = vec![0.0f64; ord + 1];
    if let Some(slot) = a.get_mut(0) {
        *slot = 1.0;
    }
    for m in 1..=ord {
        let coeff_m = tmp2.get(m - 1).copied().unwrap_or(0.0);
        let mut b = a.clone();
        for i in 1..m {
            let ai = a.get(i).copied().unwrap_or(0.0);
            let ami = a.get(m - i).copied().unwrap_or(0.0);
            if let Some(slot) = b.get_mut(i) {
                *slot = ai + coeff_m * ami;
            }
        }
        for i in 1..m {
            if let (Some(&src), Some(dst)) = (b.get(i), a.get_mut(i)) {
                *dst = src;
            }
        }
        if let Some(slot) = a.get_mut(m) {
            *slot = coeff_m;
        }
    }
    a.into_iter().skip(1).collect()
}

/// `tns_ar_filter`: an in-place all-pole filter,
/// `y(n) = x(n) - lpc[1]*y(n-1) - ... - lpc[order]*y(n-order)`, applied to
/// `size` samples of `spectrum` starting at `start` and stepping by `inc`
/// (`+1` upward, `-1` downward). State is implicit and zero-initialised —
/// "The state variables of the filter are initialized to zero every time."
fn tns_ar_filter(spectrum: &mut [f32], start: usize, size: usize, inc: i32, lpc: &[f64]) {
    let order = lpc.len();
    let mut history = vec![0.0f64; order];
    let mut idx = start.cast_signed();
    for _ in 0..size {
        let Ok(u) = usize::try_from(idx) else { break };
        let Some(&x) = spectrum.get(u) else { break };
        let mut y = f64::from(x);
        for (k, &coef) in lpc.iter().enumerate() {
            y -= coef * history.get(k).copied().unwrap_or(0.0);
        }
        if let Some(slot) = spectrum.get_mut(u) {
            *slot = y as f32;
        }
        history.rotate_right(1);
        if let Some(slot) = history.get_mut(0) {
            *slot = y;
        }
        idx += inc as isize;
    }
}

/// Apply every filter of one window's TNS list to that window's own linear
/// spectrum (`spec[w]`), per `tns_decode_frame`'s own loop: filters are
/// applied from the top of the spectrum downward, each one's target range
/// ending where the previous one's began (`bottom` carries across filters
/// within the same window).
pub(crate) fn apply_to_window(
    spec: &mut [f32],
    filters: &[TnsFilter],
    swb_offset: &[u16],
    num_swb: usize,
    max_sfb: usize,
    max_bands: u8,
    is_short: bool,
) {
    let max_order = if is_short {
        TNS_MAX_ORDER_SHORT
    } else {
        TNS_MAX_ORDER_LONG
    };
    let mut bottom = num_swb;
    for filt in filters {
        let top = bottom;
        bottom = top.saturating_sub(usize::from(filt.length));
        let tns_order = filt.order.min(max_order);
        if tns_order == 0 {
            continue;
        }
        let ceiling = usize::from(max_bands).min(max_sfb);
        let bottom_clamped = bottom.min(ceiling);
        let top_clamped = top.min(ceiling);
        let Some(&start_off) = swb_offset.get(bottom_clamped) else {
            continue;
        };
        let Some(&end_off) = swb_offset.get(top_clamped) else {
            continue;
        };
        let (start_off, end_off) = (usize::from(start_off), usize::from(end_off));
        if end_off <= start_off {
            continue;
        }
        let size = end_off - start_off;
        let lpc = tns_decode_coef(tns_order, filt.coef_res, filt.coef_compress, &filt.coef);
        let (start, inc) = if filt.direction {
            (end_off.saturating_sub(1), -1i32)
        } else {
            (start_off, 1i32)
        };
        tns_ar_filter(spec, start, size, inc, &lpc);
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
    use super::{apply_to_window, tns_max_bands};
    use crate::tns::TnsFilter;

    #[test]
    fn a_zero_order_filter_leaves_the_spectrum_untouched() {
        let mut spec = vec![1.0f32, 2.0, 3.0, 4.0];
        let filters = [TnsFilter {
            length: 2,
            order: 0,
            direction: false,
            coef_compress: false,
            coef_res: false,
            coef: vec![],
        }];
        let swb = [0u16, 1, 2, 3, 4];
        apply_to_window(&mut spec, &filters, &swb, 4, 4, 50, false);
        assert_eq!(spec, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn an_order_one_filter_is_a_simple_iir_stage() {
        // order=1, coef=0 -> the arcsine of 0 is 0 -> lpc = [0.0] -> the
        // filter degenerates to the identity (y(n) = x(n) - 0*y(n-1)).
        let mut spec = vec![1.0f32, 2.0, 3.0, 4.0];
        let filters = [TnsFilter {
            length: 4,
            order: 1,
            direction: false,
            coef_compress: false,
            coef_res: false,
            coef: vec![0],
        }];
        let swb = [0u16, 1, 2, 3, 4];
        apply_to_window(&mut spec, &filters, &swb, 4, 4, 50, false);
        assert_eq!(spec, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn tns_max_bands_is_narrower_for_short_windows_than_long() {
        assert!(tns_max_bands(4, true) < tns_max_bands(4, false));
    }
}
