//! Golomb-power-of-2 (GPO2) codes with JPEG-LS's limited-length escape
//! (LOCO-I paper §3.3.1/§3.3.3; `Vaco-Spec-Ref: locoi-hpl98-193`).
//!
//! A GPO2 code with parameter `k` writes `y >> k` in unary followed by the
//! low `k` bits of `y` in fixed binary. JPEG-LS caps the unary run at
//! `qmax = Lmax - beta - 1` so one pathological sample cannot expand to
//! hundreds of bits: past that point the code switches to `qmax` zeros, a
//! terminator, and `beta` bits carrying `y - 1` directly.

use crate::bits::{BitReader, BitWriter};

/// Length parameters for an 8-bit-per-sample alphabet (`alpha = 256`), the
/// only depth this crate decodes today. `beta = ceil(log2(alpha))`,
/// `Lmax = 2*(beta_max + max(8, beta_max))`, `qmax = Lmax - beta - 1` — all
/// three formulas are the paper's own (§3.3.3 and its footnote).
pub(crate) const BETA_8BIT: u32 = 8;
pub(crate) const LMAX_8BIT: u32 = 32;
pub(crate) const QMAX_8BIT: u32 = LMAX_8BIT - BETA_8BIT - 1;

/// `k = min{k' | (N << k') >= A}` (LOCO-I eq. 8), the "for (k=0;
/// (N<<k)<A; k++)" one-liner the paper gives directly.
#[must_use]
pub(crate) fn select_k(n: u32, a: u32) -> u32 {
    let mut k = 0u32;
    while k < 31 {
        if n.checked_shl(k).is_some_and(|v| v >= a) {
            break;
        }
        k += 1;
    }
    k
}

/// Encode one mapped, non-negative residual with Golomb parameter `k`,
/// escaping at `qmax` (regular samples always pass [`QMAX_8BIT`]; a
/// run-interruption sample passes a smaller value — see
/// [`ri_qmax`]).
pub(crate) fn encode_limited(w: &mut BitWriter, y: u32, k: u32, qmax: u32) {
    let q = y >> k;
    if q < qmax {
        w.put_run(0, q);
        w.put_bits(1, 1);
        if k > 0 {
            w.put_bits(y & ((1u32 << k) - 1), k);
        }
    } else {
        w.put_run(0, qmax);
        w.put_bits(1, 1);
        w.put_bits(y.wrapping_sub(1), BETA_8BIT);
    }
}

/// [`encode_limited`] at the regular (non-run-interruption) escape length.
pub(crate) fn encode(w: &mut BitWriter, y: u32, k: u32) {
    encode_limited(w, y, k, QMAX_8BIT);
}

/// Decode one mapped, non-negative residual with Golomb parameter `k`,
/// escaping at `qmax`. See [`encode_limited`].
///
/// # Errors
/// Whatever [`BitReader`] returns on a truncated segment.
pub(crate) fn decode_limited(r: &mut BitReader<'_>, k: u32, qmax: u32) -> vaco_core::Result<u32> {
    let q = r.get_unary(qmax)?;
    if q < qmax {
        let low = if k > 0 { r.get_bits(k)? } else { 0 };
        Ok((q << k) | low)
    } else {
        let v = r.get_bits(BETA_8BIT)?;
        Ok(v.wrapping_add(1))
    }
}

/// [`decode_limited`] at the regular (non-run-interruption) escape length.
///
/// # Errors
/// As [`decode_limited`].
pub(crate) fn decode(r: &mut BitReader<'_>, k: u32) -> vaco_core::Result<u32> {
    decode_limited(r, k, QMAX_8BIT)
}

/// A run-interruption sample's own escape length (§3.5, final paragraph):
/// "the length limitation for the Golomb code takes into account the `g+1`
/// bits of the last coded run segment, thus limiting every code word length
/// to `Lmax - g - 1` bits" — one `g+1`-bit shorter budget than a regular
/// sample gets, worked out as a smaller `qmax`: `Lmax - g - 1` total bits,
/// minus the `1` terminator and `beta` explicit bits an escape still needs,
/// is `QMAX_8BIT - 1 - g`.
#[must_use]
pub(crate) fn ri_qmax(g: u32) -> u32 {
    QMAX_8BIT.saturating_sub(1).saturating_sub(g)
}

/// `M(eps) = 2|eps| - u(eps)` (LOCO-I eq. 5): the regular mapping, ordering
/// the interleaved sequence `0, -1, 1, -2, 2, ...` by decreasing probability
/// for a distribution centred at (or just above) zero.
#[must_use]
pub(crate) const fn map_regular(eps: i32) -> u32 {
    if eps >= 0 {
        (eps as u32) * 2
    } else {
        (-(eps + 1)) as u32 * 2 + 1
    }
}

#[must_use]
pub(crate) fn unmap_regular(y: u32) -> i32 {
    if y.is_multiple_of(2) {
        (y >> 1).cast_signed()
    } else {
        -((y + 1) >> 1).cast_signed()
    }
}

/// `M'(eps) = M(-eps - 1)`: the alternate mapping used when `k == 0` and the
/// context's bias estimate says negative residuals are (slightly) more
/// likely than non-negative ones.
#[must_use]
pub(crate) const fn map_alternate(eps: i32) -> u32 {
    if eps >= 0 {
        (eps as u32) * 2 + 1
    } else {
        (-(eps + 1)) as u32 * 2
    }
}

#[must_use]
pub(crate) fn unmap_alternate(y: u32) -> i32 {
    if y.is_multiple_of(2) {
        -(y >> 1).cast_signed() - 1
    } else {
        (y >> 1).cast_signed()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn regular_mapping_orders_zero_minus_one_one_minus_two() {
        assert_eq!(map_regular(0), 0);
        assert_eq!(map_regular(-1), 1);
        assert_eq!(map_regular(1), 2);
        assert_eq!(map_regular(-2), 3);
        assert_eq!(map_regular(2), 4);
    }

    #[test]
    fn regular_mapping_round_trips() {
        for eps in -300i32..=300 {
            assert_eq!(unmap_regular(map_regular(eps)), eps);
        }
    }

    #[test]
    fn alternate_mapping_round_trips() {
        for eps in -300i32..=300 {
            assert_eq!(unmap_alternate(map_alternate(eps)), eps);
        }
    }

    #[test]
    fn golomb_round_trips_every_value_at_every_k() {
        for k in 0..8u32 {
            for y in 0..=255u32 {
                let mut w = BitWriter::new();
                encode(&mut w, y, k);
                let out = w.finish();
                let mut r = BitReader::new(&out);
                assert_eq!(decode(&mut r, k).unwrap(), y, "k={k} y={y}");
            }
        }
    }

    #[test]
    fn select_k_matches_the_defining_inequality() {
        for a in 0u32..2000 {
            for n in 1u32..200 {
                let k = select_k(n, a);
                assert!(u64::from(n) << k >= u64::from(a));
                if k > 0 {
                    assert!(u64::from(n) << (k - 1) < u64::from(a));
                }
            }
        }
    }
}
