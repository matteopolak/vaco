//! The in-loop deblocking filter, RFC 6386 §15.
//!
//! Every function takes and returns plain pixel values rather than a plane
//! reference, mirroring [`crate::predict`]'s split: [`crate::decode`] is
//! responsible for locating the eight (or four, or two) pixels straddling a
//! given edge and writing the results back, in the exact order §15.1
//! prescribes (left macroblock edge, then internal vertical edges, then top
//! macroblock edge, then internal horizontal edges).

fn c(v: i32) -> i32 {
    v.clamp(-128, 127)
}

fn u2s(p: u8) -> i32 {
    i32::from(p) - 128
}

fn s2u(v: i32) -> u8 {
    (c(v) + 128) as u8
}

/// RFC 6386 §15.2's `common_adjust`. Returns `(new_p0, new_q0, a)`; `a` is
/// reused by [`subblock_filter`] for the secondary P1/Q1 adjustment.
fn common_adjust(use_outer_taps: bool, p1: u8, p0: u8, q0: u8, q1: u8) -> (u8, u8, i32) {
    let (p1, p0, q0, q1) = (u2s(p1), u2s(p0), u2s(q0), u2s(q1));
    let outer = if use_outer_taps { c(p1 - q1) } else { 0 };
    let a = c(outer + 3 * (q0 - p0));
    let b = c(a + 3) >> 3;
    let a = c(a + 4) >> 3;
    (s2u(p0 + b), s2u(q0 - a), a)
}

/// RFC 6386 §15.2's `simple_segment`. Applies unconditionally when the
/// caller has already checked the edge-limit mask; only luma edges ever
/// use the simple filter.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "RFC 6386 §15.2's mask is exactly abs_diff(p0,q0)*2 + abs_diff(p1,q1)/2"
)]
pub fn simple_filter(edge_limit: i32, p1: u8, p0: u8, q0: u8, q1: u8) -> (u8, u8) {
    let d0 = i32::from(p0).abs_diff(i32::from(q0)).cast_signed();
    let d1 = i32::from(p1).abs_diff(i32::from(q1)).cast_signed();
    let mask = d0 * 2 + d1 / 2;
    if mask > edge_limit {
        return (p0, q0);
    }
    let (np0, nq0, _) = common_adjust(true, p1, p0, q0, q1);
    (np0, nq0)
}

/// RFC 6386 §15.3's `filter_mask` (the normal filter's mask, shared by the
/// subblock and macroblock-edge filters). `p`/`q` are both ordered
/// inner-to-outer (nearest edge pixel first): `p = [p0,p1,p2,p3]`,
/// `q = [q0,q1,q2,q3]`.
#[allow(
    clippy::integer_division,
    reason = "RFC 6386 §15.3's filter_yes edge test is exactly abs_diff(p0,q0)*2 + abs_diff(p1,q1)/2"
)]
fn filter_mask(interior_limit: i32, edge_limit: i32, p: [u8; 4], q: [u8; 4]) -> bool {
    let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b)).cast_signed();
    d(p[0], q[0]) * 2 + d(p[1], q[1]) / 2 <= edge_limit
        && d(p[3], p[2]) <= interior_limit
        && d(p[2], p[1]) <= interior_limit
        && d(p[1], p[0]) <= interior_limit
        && d(q[3], q[2]) <= interior_limit
        && d(q[2], q[1]) <= interior_limit
        && d(q[1], q[0]) <= interior_limit
}

/// RFC 6386 §15.3's `hev`.
fn high_edge_variance(threshold: i32, p1: u8, p0: u8, q0: u8, q1: u8) -> bool {
    let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b)).cast_signed();
    d(p1, p0) > threshold || d(q1, q0) > threshold
}

/// RFC 6386 §15.3's `subblock_filter` (internal 4x4-grid edges). `p`/`q` are
/// both ordered inner-to-outer (nearest edge pixel first): `p = [p0,p1,p2,p3]`,
/// `q = [q0,q1,q2,q3]`. The output arrays use the same convention.
#[must_use]
pub fn subblock_filter(
    hev_threshold: i32,
    interior_limit: i32,
    edge_limit: i32,
    p: [u8; 4],
    q: [u8; 4],
) -> ([u8; 4], [u8; 4]) {
    if !filter_mask(interior_limit, edge_limit, p, q) {
        return (p, q);
    }
    let hv = high_edge_variance(hev_threshold, p[1], p[0], q[0], q[1]);
    let (new_p0, new_q0, a) = common_adjust(hv, p[1], p[0], q[0], q[1]);
    let mut out_p = p;
    let mut out_q = q;
    out_p[0] = new_p0;
    out_q[0] = new_q0;
    if !hv {
        let adj = (a + 1) >> 1;
        out_p[1] = s2u(u2s(p[1]) + adj);
        out_q[1] = s2u(u2s(q[1]) - adj);
    }
    (out_p, out_q)
}

/// RFC 6386 §15.3's `MBfilter` (inter-macroblock edges). Same argument
/// and output order as [`subblock_filter`] (`p`/`q` inner-to-outer); can
/// modify up to P2/Q2 (P3/Q3 pass through unmodified).
#[must_use]
pub fn mb_filter(
    hev_threshold: i32,
    interior_limit: i32,
    edge_limit: i32,
    p: [u8; 4],
    q: [u8; 4],
) -> ([u8; 4], [u8; 4]) {
    if !filter_mask(interior_limit, edge_limit, p, q) {
        return (p, q);
    }
    let (p2, p1, p0) = (u2s(p[2]), u2s(p[1]), u2s(p[0]));
    let (q0, q1, q2) = (u2s(q[0]), u2s(q[1]), u2s(q[2]));

    if high_edge_variance(hev_threshold, p[1], p[0], q[0], q[1]) {
        let (new_p0, new_q0, _) = common_adjust(true, p[1], p[0], q[0], q[1]);
        let mut out_p = p;
        let mut out_q = q;
        out_p[0] = new_p0;
        out_q[0] = new_q0;
        return (out_p, out_q);
    }

    let w = c(c(p1 - q1) + 3 * (q0 - p0));
    let a0 = c((27 * w + 63) >> 7);
    let a1 = c((18 * w + 63) >> 7);
    let a2 = c((9 * w + 63) >> 7);

    (
        [s2u(p0 + a0), s2u(p1 + a1), s2u(p2 + a2), p[3]],
        [s2u(q0 - a0), s2u(q1 - a1), s2u(q2 - a2), q[3]],
    )
}

/// RFC 6386 §15.4: `interior_limit` from `filter_level`/`sharpness_level`.
#[must_use]
pub fn interior_limit(filter_level: i32, sharpness_level: i32) -> i32 {
    let mut limit = filter_level;
    if sharpness_level > 0 {
        limit >>= if sharpness_level > 4 { 2 } else { 1 };
        limit = limit.min(9 - sharpness_level);
    }
    limit.max(1)
}

/// RFC 6386 §15.4: `hev_threshold` from `filter_level` and frame type.
#[must_use]
#[allow(
    clippy::bool_to_int_with_if,
    reason = "mirrors RFC 6386 §15.4's own if/else-if ladder for easy comparison against the spec"
)]
pub fn hev_threshold(filter_level: i32, key_frame: bool) -> i32 {
    if key_frame {
        if filter_level >= 40 {
            2
        } else if filter_level >= 15 {
            1
        } else {
            0
        }
    } else if filter_level >= 40 {
        3
    } else if filter_level >= 20 {
        2
    } else if filter_level >= 15 {
        1
    } else {
        0
    }
}

/// RFC 6386 §15.4: the two edge limits, shared by the simple and normal
/// filters.
#[must_use]
pub fn edge_limits(filter_level: i32, interior_limit: i32) -> (i32, i32) {
    (
        (filter_level + 2) * 2 + interior_limit, // mb edge
        filter_level * 2 + interior_limit,       // subblock edge
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_edge_is_left_untouched() {
        let p = [100u8; 4];
        let q = [100u8; 4];
        let (np, nq) = subblock_filter(2, 10, 20, p, q);
        assert_eq!(np, p);
        assert_eq!(nq, q);
    }

    #[test]
    fn interior_limit_is_never_zero() {
        for level in 0..=63 {
            for sharpness in 0..=7 {
                assert!(interior_limit(level, sharpness) >= 1);
            }
        }
    }

    #[test]
    fn simple_filter_never_touches_p1_q1() {
        let (p0, q0) = simple_filter(1000, 10, 20, 200, 210);
        assert_ne!((p0, q0), (20, 200)); // filter did run (mask passed with huge limit)
    }

    proptest::proptest! {
        #[test]
        fn filters_never_panic(
            p in proptest::array::uniform4(proptest::prelude::any::<u8>()),
            q in proptest::array::uniform4(proptest::prelude::any::<u8>()),
            level in 0i32..64, sharpness in 0i32..8,
        ) {
            let il = interior_limit(level, sharpness);
            let (mbe, sbe) = edge_limits(level, il);
            let hev = hev_threshold(level, true);
            let _ = subblock_filter(hev, il, sbe, p, q);
            let _ = mb_filter(hev, il, mbe, p, q);
            let _ = simple_filter(mbe, p[1], p[0], q[0], q[1]);
        }
    }
}
