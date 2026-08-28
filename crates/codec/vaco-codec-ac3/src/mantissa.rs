//! Mantissa VLC read and dequantisation, driven by the `bap` array
//! [`crate::bitalloc`] computed. ATSC A/52:2012 §7.3.

use vaco_bitstream::BitReader;

use crate::tables::{Quant, quant_for_bap};

/// §7.3.2: true two's-complement fractional quantization. "The decimal
/// point is considered to be to the left of the MSB" — a `bits`-wide signed
/// integer divided by `2^(bits-1)`.
fn dequant_asymmetric(r: &mut BitReader<'_>, bits: u8) -> f32 {
    let signed = r.get_signed(u32::from(bits));
    let half = f64::from(1u32 << (bits - 1));
    (f64::from(signed) / half) as f32
}

/// §7.3.5: base-`levels` decomposition of a grouped code into `count`
/// digits, most significant first — the inverse of how the encoder packs
/// `group_code = digit[0]*levels^(count-1) + ... + digit[count-1]`.
fn decompose_group(mut code: u32, levels: u16, count: u8) -> Vec<u32> {
    let levels = u32::from(levels);
    let mut digits = Vec::new();
    for _ in 0..count {
        digits.push(code % levels);
        code /= levels;
    }
    digits.reverse();
    digits
}

/// §7.3.5 Tables 7.19/7.20/7.22: bap 1/2/4's grouped values are evenly
/// spaced across `(-1, 1)` — verified directly against the specification's
/// own tables, not merely a plausible-looking approximation.
fn dequant_grouped(level: u32, levels: u16) -> f32 {
    let levels = f32::from(levels);
    (2.0 * level as f32 - (levels - 1.0)) / levels
}

/// Carries a straddling group's not-yet-consumed mantissas across calls to
/// [`decode`]. §7.3.5: "If the number of mantissas in an exponent set does
/// not fill an integral number of groups, the groups are shared across
/// exponent sets. The next exponent set in the block continues filling the
/// partial groups" — grouping for bap 1/2/4 is a property of the *block's*
/// linear mantissa stream, not of any one channel's mantissa count, so a
/// channel whose bap-1/2/4 bin count is not a multiple of 3 (or 2, for
/// bap=4) hands its last group's unused slots to whichever channel is
/// decoded next in the same block. One instance must be created per block
/// and threaded through every [`decode`] call in that block's own
/// processing order (fbw channels, then LFE — coupling-channel mantissas
/// are not read at all yet, a separate, disclosed gap); dropping it between
/// calls, or creating a fresh one per channel, desyncs every mantissa read
/// after the first channel whose count does not land on a group boundary.
///
/// Holds dequantised-but-unscaled values (each bin still applies its own
/// exponent's scale on consumption), in the order they will be assigned to
/// bins, so callers ready to receive the very next value are always at the
/// front — `next()` pops from there.
#[derive(Debug, Default)]
pub struct PendingGroup(std::collections::VecDeque<f32>);

impl PendingGroup {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn next(&mut self) -> Option<f32> {
        self.0.pop_front()
    }

    fn extend(&mut self, values: impl IntoIterator<Item = f32>) {
        self.0.extend(values);
    }
}

/// Read and dequantise `bap.len()` mantissas in order, applying each one's
/// exponent to produce a coefficient. `dither` supplies a caller-chosen
/// pseudo-random value in `(-1, 1)` for `bap == 0` positions when `dithflag`
/// is set (§7.3.4); pass a function returning `0.0` to disable dither.
/// `pending` is this block's shared straddling-group state — see
/// [`PendingGroup`]'s docs for why it must outlive a single call.
#[must_use]
pub fn decode(
    r: &mut BitReader<'_>,
    bap: &[u8],
    exps: &[u8],
    dither: bool,
    mut rng: impl FnMut() -> f32,
    pending: &mut PendingGroup,
) -> Vec<f32> {
    let mut out = Vec::new();
    for (i, &b) in bap.iter().enumerate() {
        let exp = exps.get(i).copied().unwrap_or(24);
        let scale = 2f32.powi(-i32::from(exp));

        if let Some(v) = pending.next() {
            out.push(v * scale);
            continue;
        }

        let value = match quant_for_bap(b) {
            Quant::Zero => {
                if dither {
                    rng()
                } else {
                    0.0
                }
            }
            Quant::Asymmetric { bits } => dequant_asymmetric(r, bits),
            Quant::SymmetricTable { bits, values } => {
                let code = r.get(u32::from(bits));
                values.get(code as usize).copied().unwrap_or(0.0)
            }
            Quant::Grouped {
                levels,
                per_group,
                bits,
            } => {
                let code = r.get(u32::from(bits));
                // §7.3.5's decoder equations give the *first* (earliest,
                // most-significant) mantissa in the group — `a` for bap 1/2,
                // `a` for bap 4 — as this bin's own value; `b` (and `c`)
                // queue for whichever bins are decoded next, in that order.
                let mut digits = decompose_group(code, levels, per_group).into_iter();
                let first = digits.next().unwrap_or(0);
                pending.extend(digits.map(|lvl| dequant_grouped(lvl, levels)));
                dequant_grouped(first, levels)
            }
        };
        out.push(value * scale);
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_bap_with_no_dither_decodes_to_silence() {
        let mut r = BitReader::new(&[0u8; 4]);
        let out = decode(
            &mut r,
            &[0, 0, 0],
            &[10, 10, 10],
            false,
            || 1.0,
            &mut PendingGroup::new(),
        );
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn grouped_mantissas_recover_the_right_count() {
        let mut r = BitReader::new(&[0u8; 8]);
        let bap = [1u8, 1, 1, 2, 2, 2];
        let exps = [0u8; 6];
        let out = decode(&mut r, &bap, &exps, false, || 0.0, &mut PendingGroup::new());
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn asymmetric_dequant_is_two_s_complement_not_offset_binary() {
        // bap=6 -> 5 bits. All-zero code is exactly zero (two's complement),
        // not the most-negative value an offset-binary reading would give.
        let mut r = BitReader::new(&[0u8; 4]);
        let out = decode(&mut r, &[6], &[0], false, || 0.0, &mut PendingGroup::new());
        assert_eq!(out[0], 0.0, "got {}", out[0]);

        // The MSB set, rest zero ("10000") is two's complement's most
        // negative value, -1.0 at this bit width — the case an
        // offset-binary `(code-half)/half` reading would instead map to 0.
        let mut buf = [0u8; 4];
        buf[0] = 0b1000_0000;
        let mut r2 = BitReader::new(&buf);
        let out2 = decode(&mut r2, &[6], &[0], false, || 0.0, &mut PendingGroup::new());
        assert!((out2[0] - (-1.0)).abs() < 1e-6, "got {}", out2[0]);
    }

    #[test]
    fn bap3_table_values_are_not_evenly_spaced_like_a_uniform_quantizer() {
        // code=3 (middle of 0..=6) must dequantise to exactly 0, per Table
        // 7.21 — a two's-complement reading of a 3-bit field would instead
        // treat code 3 as slightly positive (0.5 with a 4-level half).
        let mut buf = [0u8; 4];
        buf[0] = 0b011_00000; // code 3 in the first 3 bits
        let mut r = BitReader::new(&buf);
        let out = decode(&mut r, &[3], &[0], false, || 0.0, &mut PendingGroup::new());
        assert_eq!(out[0], 0.0);
    }

    #[test]
    fn never_panics_on_a_truncated_buffer() {
        let mut r = BitReader::new(&[]);
        let bap = [15u8; 20];
        let exps = [0u8; 20];
        let out = decode(&mut r, &bap, &exps, true, || 0.5, &mut PendingGroup::new());
        assert_eq!(out.len(), 20);
    }
}
