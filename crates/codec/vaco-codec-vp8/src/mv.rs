//! Motion vector decoding, RFC 6386 §16.3-§16.4 and §17.
//!
//! Units: a decoded component is in **quarter-pel** for luma (§17.1's
//! documented range -1023..1023); [`crate::decode`] doubles luma vectors to
//! eighth-pel before motion compensation, per §18.1.

use vaco_codec_msac::Vp8BoolDecoder as Bd;
use vaco_codec_msac::tree::write_tree;

use crate::encode::BoolWriter;
use crate::tables::{
    MV_PARTITION_COUNTS, MV_PARTITIONS, MVP_BITS, MVP_IS_SHORT, MVP_SHORT, MVP_SIGN,
    MVPARTITION_PROB, MVPARTITION_TREE, SMALL_MVTREE, SUB_MV_REF_PROB, SUB_MV_REF_TREE,
    VP8_MODE_CONTEXTS,
};

/// A motion vector in quarter-pel units, `(row, col)`.
pub type Mv = (i32, i32);

/// RFC 6386 §17.1's `read_mvcomponent`.
fn read_component(bd: &mut Bd<'_>, p: &[u8; 19]) -> i32 {
    let a: i32 = if bd.read_bool(p[MVP_IS_SHORT]) {
        let mut a = 0i32;
        for i in 0..3 {
            let prob = p.get(MVP_BITS + i).copied().unwrap_or(128);
            a += i32::from(bd.read_bool(prob)) << i;
        }
        for i in (4..=9).rev() {
            let prob = p.get(MVP_BITS + i).copied().unwrap_or(128);
            a += i32::from(bd.read_bool(prob)) << i;
        }
        // Bit 3 is implied set when bits 4..9 are all zero (a long-coded
        // value is always >= 8); otherwise it is coded explicitly.
        let bit3 = (a & 0xfff0) == 0 || bd.read_bool(p[MVP_BITS + 3]);
        if bit3 { a + 8 } else { a }
    } else {
        bd.read_tree(&SMALL_MVTREE, &p[MVP_SHORT..])
    };
    if a != 0 && bd.read_bool(p[MVP_SIGN]) {
        -a
    } else {
        a
    }
}

/// Decode one `(row, col)` motion vector delta, RFC 6386 §17.1.
#[must_use]
pub fn read_mv(bd: &mut Bd<'_>, probs: &[[u8; 19]; 2]) -> Mv {
    let row = read_component(bd, &probs[0]);
    let col = read_component(bd, &probs[1]);
    (row, col)
}

/// The encode-side inverse of [`read_component`]: write `a`'s magnitude
/// (short tree or long form, matching whichever [`read_component`] would
/// have chosen for the same value) then its sign, if non-zero.
fn write_component(bw: &mut BoolWriter, p: &[u8; 19], a: i32) {
    let mag = a.abs();
    let is_short = mag < 8;
    // `read_component`'s `if bd.read_bool(p[MVP_IS_SHORT]) { <long form> }
    // else { <short tree> }` -- despite the field's name, a `true` bit
    // selects the *long* branch, so the short-form bit is written inverted.
    bw.write_bool(p[MVP_IS_SHORT], !is_short);
    if is_short {
        write_tree(&SMALL_MVTREE, mag, |node, bit| {
            let prob = p.get(MVP_SHORT + node).copied().unwrap_or(128);
            bw.write_bool(prob, bit);
        });
    } else {
        for i in 0..3 {
            let prob = p.get(MVP_BITS + i).copied().unwrap_or(128);
            bw.write_bool(prob, (mag >> i) & 1 != 0);
        }
        for i in (4..=9).rev() {
            let prob = p.get(MVP_BITS + i).copied().unwrap_or(128);
            bw.write_bool(prob, (mag >> i) & 1 != 0);
        }
        // Bit 3 is only written when it is not already implied by bits
        // 4..9 all being zero -- the mirror of `read_component`'s
        // `(a & 0xfff0) == 0 || bd.read_bool(...)` short-circuit.
        if mag & 0xfff0 != 0 {
            let prob = p.get(MVP_BITS + 3).copied().unwrap_or(128);
            bw.write_bool(prob, (mag >> 3) & 1 != 0);
        }
    }
    if mag != 0 {
        bw.write_bool(p[MVP_SIGN], a < 0);
    }
}

/// Encode one `(row, col)` motion vector delta, the inverse of [`read_mv`].
pub fn write_mv(bw: &mut BoolWriter, probs: &[[u8; 19]; 2], mv: Mv) {
    write_component(bw, &probs[0], mv.0);
    write_component(bw, &probs[1], mv.1);
}

/// Clamp a motion vector (quarter-pel here; the caller passes eighth-pel
/// bounds already doubled, or calls this at quarter-pel consistently — see
/// [`crate::decode`] for which unit is in play at each call site) to RFC
/// 6386 §16.3/§18.1's one-macroblock border.
#[must_use]
pub fn clamp_mv(mv: Mv, to_left: i32, to_right: i32, to_top: i32, to_bottom: i32) -> Mv {
    (mv.0.clamp(to_top, to_bottom), mv.1.clamp(to_left, to_right))
}

/// One neighbour macroblock's motion-vector/reference-frame state, as
/// `vp8_find_near_mvs` (§16.3) needs it. `ref_frame == 0` means intra (no
/// contribution).
#[derive(Debug, Clone, Copy, Default)]
pub struct NeighborMv {
    pub ref_frame: u8,
    pub mv: Mv,
    pub is_splitmv: bool,
}

/// Output of `vp8_find_near_mvs`: the three candidate vectors and the
/// 4-entry weighted count array used to derive `mv_ref`'s probabilities.
#[derive(Debug, Clone, Copy, Default)]
pub struct NearMvs {
    pub best: Mv,
    pub nearest: Mv,
    pub near: Mv,
    pub cnt: [i32; 4],
}

fn bias(mv: Mv, same_sign: bool) -> Mv {
    if same_sign { mv } else { (-mv.0, -mv.1) }
}

/// RFC 6386 §16.3's `vp8_find_near_mvs`. `sign_bias_matches` tells whether a
/// given neighbour's reference frame shares the current macroblock's sign
/// bias (so its motion vector is used as-is rather than negated).
#[must_use]
pub fn find_near_mvs(
    above: Option<NeighborMv>,
    left: Option<NeighborMv>,
    above_left: Option<NeighborMv>,
    sign_bias_matches: impl Fn(u8) -> bool,
) -> NearMvs {
    let mut near_mvs = [(0i32, 0i32); 4];
    let mut cnt = [0i32; 4];
    let mut idx = 0usize;

    if let Some(n) = above
        && n.ref_frame != 0
    {
        let mv = bias(n.mv, sign_bias_matches(n.ref_frame));
        if mv != (0, 0) {
            idx += 1;
            if let Some(slot) = near_mvs.get_mut(idx) {
                *slot = mv;
            }
        }
        if let Some(c) = cnt.get_mut(idx) {
            *c += 2;
        }
    }

    if let Some(n) = left
        && n.ref_frame != 0
    {
        let mv = bias(n.mv, sign_bias_matches(n.ref_frame));
        if mv != (0, 0) {
            if mv != near_mvs.get(idx).copied().unwrap_or((0, 0)) || idx == 0 {
                idx += 1;
                if let Some(slot) = near_mvs.get_mut(idx) {
                    *slot = mv;
                }
            }
            if let Some(c) = cnt.get_mut(idx) {
                *c += 2;
            }
        } else if let Some(c) = cnt.first_mut() {
            *c += 2;
        }
    }

    let mut third_idx: Option<usize> = None;
    if let Some(n) = above_left
        && n.ref_frame != 0
    {
        let mv = bias(n.mv, sign_bias_matches(n.ref_frame));
        if mv != (0, 0) {
            if mv != near_mvs.get(idx).copied().unwrap_or((0, 0)) || idx == 0 {
                idx += 1;
                if let Some(slot) = near_mvs.get_mut(idx) {
                    *slot = mv;
                }
                third_idx = Some(idx);
            }
            if let Some(c) = cnt.get_mut(idx) {
                *c += 1;
            }
        } else if let Some(c) = cnt.first_mut() {
            *c += 1;
        }
    }

    // Merge a genuinely-distinct 3rd candidate into "nearest" if they match.
    if let Some(ti) = third_idx
        && ti == 3
        && near_mvs.get(3).copied().unwrap_or((0, 0)) == near_mvs.get(1).copied().unwrap_or((0, 0))
    {
        let c3 = cnt.get(3).copied().unwrap_or(0);
        if let Some(c1) = cnt.get_mut(1) {
            *c1 += c3;
        }
    }

    cnt[3] = i32::from(above.is_some_and(|n| n.is_splitmv)) * 2
        + i32::from(left.is_some_and(|n| n.is_splitmv)) * 2
        + i32::from(above_left.is_some_and(|n| n.is_splitmv));

    if cnt.get(2).copied().unwrap_or(0) > cnt.get(1).copied().unwrap_or(0) {
        cnt.swap(1, 2);
        near_mvs.swap(1, 2);
    }

    let best = if cnt.get(1).copied().unwrap_or(0) >= cnt.first().copied().unwrap_or(0) {
        near_mvs.get(1).copied().unwrap_or((0, 0))
    } else {
        (0, 0)
    };

    NearMvs {
        best,
        nearest: near_mvs.get(1).copied().unwrap_or((0, 0)),
        near: near_mvs.get(2).copied().unwrap_or((0, 0)),
        cnt,
    }
}

/// RFC 6386 §16.3's `vp8_mv_ref_probs`.
#[must_use]
pub fn mv_ref_probs(cnt: [i32; 4]) -> [u8; 4] {
    let row = |c: i32, col: usize| -> u8 {
        let idx = usize::try_from(c).unwrap_or(0).min(5);
        VP8_MODE_CONTEXTS
            .get(idx)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(128)
    };
    [
        row(cnt[0], 0),
        row(cnt[1], 1),
        row(cnt[2], 2),
        row(cnt[3], 3),
    ]
}

/// RFC 6386 §16.4's `vp8_mvCont`: context index for [`SUB_MV_REF_PROB`].
#[must_use]
#[allow(
    clippy::bool_to_int_with_if,
    reason = "mirrors RFC 6386 §16.4's own SUBMVREF_* if/else-if ladder"
)]
pub fn sub_mv_context(left: Mv, above: Mv) -> usize {
    let l_zero = left == (0, 0);
    let a_zero = above == (0, 0);
    let same = left == above;
    if same && l_zero {
        4
    } else if same {
        3
    } else if a_zero {
        2
    } else if l_zero {
        1
    } else {
        0
    }
}

/// Decode which of the four split-partition layouts is used, and — walking
/// each partition in order — its `sub_mv_ref` mode plus (for `NEW4x4`) an
/// MV delta. Returns one motion vector per of the 16 4x4 luma subblocks
/// (raster order).
///
/// `above_boundary(col)`/`left_boundary(row)` supply the *neighbouring
/// macroblock's* matching-edge subblock MV (row 3 / column 3) for a
/// partition whose first subblock sits on this macroblock's top/left edge;
/// a lookup that stays inside this macroblock is resolved from the
/// partitions already decided earlier in this same call, since RFC 6386
/// §16.4's `LEFT4x4`/`ABOVE4x4` must see this macroblock's own
/// already-decoded subblocks, not a stale value from before this call.
pub fn decode_split(
    bd: &mut Bd<'_>,
    mv_probs: &[[u8; 19]; 2],
    best_mv: Mv,
    above_boundary: impl Fn(usize) -> Mv,
    left_boundary: impl Fn(usize) -> Mv,
) -> [Mv; 16] {
    let partition_type = bd.read_tree(&MVPARTITION_TREE, &MVPARTITION_PROB) as usize;
    let layout = MV_PARTITIONS
        .get(partition_type)
        .copied()
        .unwrap_or(MV_PARTITIONS[3]);
    let count = MV_PARTITION_COUNTS
        .get(partition_type)
        .copied()
        .unwrap_or(16);

    let mut out = [(0i32, 0i32); 16];
    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..16 subblock index into its 4x4 grid position"
    )]
    for p in 0..count {
        let first_sub = (0..16).find(|&k| layout.get(k).copied().unwrap_or(0) as usize == p);
        let Some(first_sub) = first_sub else { continue };

        let above_mv = if first_sub < 4 {
            above_boundary(first_sub % 4)
        } else {
            out.get(first_sub - 4).copied().unwrap_or((0, 0))
        };
        let left_mv = if first_sub % 4 == 0 {
            left_boundary(first_sub / 4)
        } else {
            out.get(first_sub - 1).copied().unwrap_or((0, 0))
        };
        let ctx = sub_mv_context(left_mv, above_mv);
        let probs = SUB_MV_REF_PROB.get(ctx).copied().unwrap_or([128; 3]);
        let mode = bd.read_tree(&SUB_MV_REF_TREE, &probs);

        let mv = match mode {
            0 => left_mv,
            1 => above_mv,
            2 => (0, 0),
            _ => {
                // read_mv returns a quarter-pel delta (RFC 6386 §17.1);
                // best_mv is kept in eighth-pel throughout this crate
                // (§18.1's "stored luma motion vectors are all doubled"),
                // so the delta is doubled before combining.
                let (dr, dc) = read_mv(bd, mv_probs);
                (best_mv.0 + dr * 2, best_mv.1 + dc * 2)
            }
        };

        for (k, &part) in layout.iter().enumerate() {
            if part as usize == p
                && let Some(slot) = out.get_mut(k)
            {
                *slot = mv;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_neighbors_yield_zero_everything() {
        let r = find_near_mvs(None, None, None, |_| true);
        assert_eq!(r.best, (0, 0));
        assert_eq!(r.nearest, (0, 0));
        assert_eq!(r.near, (0, 0));
    }

    #[test]
    fn a_single_nonzero_above_neighbor_becomes_nearest() {
        let above = NeighborMv {
            ref_frame: 1,
            mv: (4, -2),
            is_splitmv: false,
        };
        let r = find_near_mvs(Some(above), None, None, |_| true);
        assert_eq!(r.nearest, (4, -2));
        assert_eq!(r.best, (4, -2));
    }

    #[test]
    fn sub_mv_context_matches_the_five_cases() {
        assert_eq!(sub_mv_context((0, 0), (0, 0)), 4);
        assert_eq!(sub_mv_context((1, 1), (1, 1)), 3);
        assert_eq!(sub_mv_context((1, 1), (0, 0)), 2);
        assert_eq!(sub_mv_context((0, 0), (1, 1)), 1);
        assert_eq!(sub_mv_context((1, 1), (2, 2)), 0);
    }

    proptest::proptest! {
        #[test]
        fn split_decode_never_panics(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64)) {
            let mut bd = Bd::new(&data);
            let probs = crate::tables::DEFAULT_MV_CONTEXT;
            let _ = decode_split(&mut bd, &probs, (0, 0), |_| (0, 0), |_| (0, 0));
        }
    }

    #[test]
    fn write_mv_then_read_mv_round_trips_short_and_long_values() {
        let probs = crate::tables::DEFAULT_MV_CONTEXT;
        for mv in [(0, 0), (3, -5), (7, 7), (8, -8), (100, -400), (1023, -1023)] {
            let mut bw = BoolWriter::new();
            write_mv(&mut bw, &probs, mv);
            let bytes = bw.finish();
            let mut bd = Bd::new(&bytes);
            assert_eq!(read_mv(&mut bd, &probs), mv, "round trip of {mv:?}");
        }
    }

    proptest::proptest! {
        #[test]
        fn write_mv_then_read_mv_round_trips_arbitrary_components(
            row in -1023i32..=1023, col in -1023i32..=1023,
        ) {
            let probs = crate::tables::DEFAULT_MV_CONTEXT;
            let mut bw = BoolWriter::new();
            write_mv(&mut bw, &probs, (row, col));
            let bytes = bw.finish();
            let mut bd = Bd::new(&bytes);
            assert_eq!(read_mv(&mut bd, &probs), (row, col));
        }
    }
}
