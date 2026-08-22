//! `st_ref_pic_set()`, ITU-T H.265 §7.3.7, and the derivation of §7.4.8.
//!
//! # Why a header parser has to do this much work
//!
//! A short-term reference picture set can be coded **by reference to another
//! one**: `inter_ref_pic_set_prediction_flag` says "like set *k*, but shifted",
//! and the number of bits that follow is `NumDeltaPocs[k] + 1`. So a parser
//! that wants to read set *n* has to know how many pictures set *k* ended up
//! naming — and that is the output of §7.4.8's derivation, not of the syntax.
//!
//! Skipping the derivation is not an option: the very next set in the SPS, and
//! the one the slice segment header may carry inline, both become unreadable.
//! This is the one place in the crate where a *derivation* is load-bearing for
//! *parsing*.
//!
//! It is still not decoding. What comes out is a list of picture-order-count
//! deltas and a used/unused flag each — an ordering statement about pictures,
//! with no picture anywhere in it.

use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};

use crate::util::MAX_DELTA_POCS;

/// One short-term reference picture set, after §7.4.8's derivation.
///
/// The syntax elements are not kept: `delta_poc_s0_minus1` and its friends only
/// exist to build these two lists, and a set that was coded by prediction has
/// no such elements at all. Storing the derived form is the only representation
/// both spellings share.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShortTermRps {
    /// `DeltaPocS0[i]`, the negative deltas, in decreasing order — that is,
    /// nearest first. Always negative for a conforming set.
    pub delta_poc_s0: Vec<i32>,
    /// `UsedByCurrPicS0[i]`.
    pub used_by_curr_pic_s0: Vec<bool>,
    /// `DeltaPocS1[i]`, the positive deltas, nearest first.
    pub delta_poc_s1: Vec<i32>,
    /// `UsedByCurrPicS1[i]`.
    pub used_by_curr_pic_s1: Vec<bool>,
    /// Whether this set was coded as a delta against another one — kept because
    /// a writer has to know, and because it is the field a `hevc_metadata`
    /// filter would have to preserve.
    pub inter_predicted: bool,
}

impl ShortTermRps {
    /// `NumNegativePics`.
    #[must_use]
    pub fn num_negative_pics(&self) -> u32 {
        self.delta_poc_s0.len() as u32
    }

    /// `NumPositivePics`.
    #[must_use]
    pub fn num_positive_pics(&self) -> u32 {
        self.delta_poc_s1.len() as u32
    }

    /// `NumDeltaPocs`, §7.4.8 — the total, and the thing every *other* set's
    /// parse depends on.
    #[must_use]
    pub fn num_delta_pocs(&self) -> u32 {
        self.num_negative_pics() + self.num_positive_pics()
    }

    /// How many of this set's pictures are used by the current picture — the
    /// short-term half of `NumPicTotalCurr`, §7.4.7.2.
    #[must_use]
    pub fn num_used_by_curr_pic(&self) -> u32 {
        let a = self.used_by_curr_pic_s0.iter().filter(|&&u| u).count();
        let b = self.used_by_curr_pic_s1.iter().filter(|&&u| u).count();
        (a + b) as u32
    }
}

/// Read one `st_ref_pic_set( st_rps_idx )`, §7.3.7.
///
/// `previous` is every set already read, in order; `st_rps_idx` is this set's
/// index; `num_short_term_ref_pic_sets` is the SPS's declared count.
///
/// # Why the count is a separate argument
///
/// §7.3.7 reads `delta_idx_minus1` only when
/// `stRpsIdx == num_short_term_ref_pic_sets`, which is true for exactly one
/// set: the one a **slice segment header** codes inline, beyond the SPS's list.
/// Inferring the count from `previous.len()` looks equivalent and is not —
/// inside the SPS, set *i* is always read with *i* sets already parsed, so the
/// condition would fire on every one of them and consume an `ue(v)` that is not
/// there. Every SPS with two or more reference picture sets would desynchronise
/// from that point on. Caught by a unit test that fed a predicted set with no
/// `delta_idx_minus1` in front of it.
///
/// # Errors
///
/// [`Error::InvalidData`] when a predicted set names a source that does not
/// exist or a count exceeds §7.4.8's bound, [`Error::UnexpectedEof`] on
/// truncation, or a budget error.
pub fn parse_st_ref_pic_set(
    g: &mut BoundedGolomb<'_, '_, '_>,
    st_rps_idx: u32,
    previous: &[ShortTermRps],
    num_short_term_ref_pic_sets: u32,
) -> Result<ShortTermRps> {
    let inter_predicted = if st_rps_idx != 0 { g.u(1)? != 0 } else { false };
    if !inter_predicted {
        return read_explicit(g);
    }

    // Present only for the set a slice segment header codes inline.
    let delta_idx_minus1 = if st_rps_idx == num_short_term_ref_pic_sets {
        g.ue_v(st_rps_idx.saturating_sub(1))?
    } else {
        0
    };
    let ref_idx = st_rps_idx
        .checked_sub(delta_idx_minus1 + 1)
        .ok_or(Error::InvalidData(
            "st_ref_pic_set refers before the first set",
        ))?;
    let source = previous
        .get(ref_idx as usize)
        .ok_or(Error::InvalidData("st_ref_pic_set refers to an unread set"))?;

    let delta_rps_sign = g.u(1)?;
    // §7.4.8 bounds `abs_delta_rps_minus1` at 2^15 - 1, so the conversion and
    // the increment both fit; the fallbacks cannot be reached.
    let abs_delta_rps_minus1 = g.ue_v(32_767)?;
    let magnitude = i32::try_from(abs_delta_rps_minus1)
        .unwrap_or(i32::MAX - 1)
        .saturating_add(1);
    let delta_rps = if delta_rps_sign == 0 {
        magnitude
    } else {
        -magnitude
    };

    // One flag pair per picture of the source set, plus one for the source
    // picture itself: `NumDeltaPocs[RefRpsIdx] + 1` iterations.
    let n = source.num_delta_pocs() as usize;
    g.budget().consume_fuel(n as u64 + 1)?;
    // At most `MAX_DELTA_POCS + 1` entries, so no budgeted allocation is
    // needed; the fuel charge above is what bounds the loop.
    let mut used = Vec::new();
    let mut use_delta = Vec::new();
    for _ in 0..=n {
        let u = g.u(1)? != 0;
        // §7.3.7: `use_delta_flag[j]` is inferred to be 1 when absent.
        let d = if u { true } else { g.u(1)? != 0 };
        used.push(u);
        use_delta.push(d);
    }

    Ok(derive_predicted(source, delta_rps, &used, &use_delta))
}

/// The explicitly-coded spelling of §7.3.7.
fn read_explicit(g: &mut BoundedGolomb<'_, '_, '_>) -> Result<ShortTermRps> {
    // §7.4.8 bounds both counts by `sps_max_dec_pic_buffering_minus1`, which
    // Annex A caps at 15 — so 16 is the loosest bound the specification allows
    // and no conforming stream reaches it.
    let num_negative = g.ue_v(MAX_DELTA_POCS)?;
    let num_positive = g.ue_v(MAX_DELTA_POCS)?;
    g.budget()
        .consume_fuel(u64::from(num_negative) + u64::from(num_positive))?;

    // Both counts are capped at `MAX_DELTA_POCS`, and the fuel charge above
    // is what stops an implausible one before any read happens.
    let mut out = ShortTermRps::default();

    let mut prev = 0i32;
    for _ in 0..num_negative {
        // §7.4.8 bounds `delta_poc_s0_minus1` at 2^15 - 1.
        let delta = i32::try_from(g.ue_v(32_767)?).unwrap_or(i32::MAX - 1) + 1;
        prev = prev.saturating_sub(delta);
        out.delta_poc_s0.push(prev);
        out.used_by_curr_pic_s0.push(g.u(1)? != 0);
    }
    let mut prev = 0i32;
    for _ in 0..num_positive {
        let delta = i32::try_from(g.ue_v(32_767)?).unwrap_or(i32::MAX - 1) + 1;
        prev = prev.saturating_add(delta);
        out.delta_poc_s1.push(prev);
        out.used_by_curr_pic_s1.push(g.u(1)? != 0);
    }
    Ok(out)
}

/// §7.4.8's equations (7-59) and (7-60): rebuild the two lists from the source
/// set shifted by `delta_rps`.
///
/// The traversal order is the interesting part and it is not symmetric. To
/// build the *negative* list, the source's positive deltas are walked
/// **backwards** (so the shifted values come out in decreasing order), then the
/// source picture itself, then the source's negative deltas forwards. The
/// positive list mirrors it. Walking either in the obvious order produces a
/// correctly-sized but wrongly-ordered set, which then mis-sizes the next
/// prediction.
fn derive_predicted(
    source: &ShortTermRps,
    delta_rps: i32,
    used: &[bool],
    use_delta: &[bool],
) -> ShortTermRps {
    let n_neg = source.delta_poc_s0.len();
    let n_delta = source.num_delta_pocs() as usize;
    let mut out = ShortTermRps {
        inter_predicted: true,
        ..ShortTermRps::default()
    };

    // (7-59): the negative list.
    for j in (0..source.delta_poc_s1.len()).rev() {
        let d = source
            .delta_poc_s1
            .get(j)
            .copied()
            .unwrap_or(0)
            .saturating_add(delta_rps);
        let k = n_neg + j;
        if d < 0 && use_delta.get(k).copied().unwrap_or(true) {
            out.delta_poc_s0.push(d);
            out.used_by_curr_pic_s0
                .push(used.get(k).copied().unwrap_or(false));
        }
    }
    if delta_rps < 0 && use_delta.get(n_delta).copied().unwrap_or(true) {
        out.delta_poc_s0.push(delta_rps);
        out.used_by_curr_pic_s0
            .push(used.get(n_delta).copied().unwrap_or(false));
    }
    for j in 0..n_neg {
        let d = source
            .delta_poc_s0
            .get(j)
            .copied()
            .unwrap_or(0)
            .saturating_add(delta_rps);
        if d < 0 && use_delta.get(j).copied().unwrap_or(true) {
            out.delta_poc_s0.push(d);
            out.used_by_curr_pic_s0
                .push(used.get(j).copied().unwrap_or(false));
        }
    }

    // (7-60): the positive list, mirrored.
    for j in (0..n_neg).rev() {
        let d = source
            .delta_poc_s0
            .get(j)
            .copied()
            .unwrap_or(0)
            .saturating_add(delta_rps);
        if d > 0 && use_delta.get(j).copied().unwrap_or(true) {
            out.delta_poc_s1.push(d);
            out.used_by_curr_pic_s1
                .push(used.get(j).copied().unwrap_or(false));
        }
    }
    if delta_rps > 0 && use_delta.get(n_delta).copied().unwrap_or(true) {
        out.delta_poc_s1.push(delta_rps);
        out.used_by_curr_pic_s1
            .push(used.get(n_delta).copied().unwrap_or(false));
    }
    for j in 0..source.delta_poc_s1.len() {
        let d = source
            .delta_poc_s1
            .get(j)
            .copied()
            .unwrap_or(0)
            .saturating_add(delta_rps);
        let k = n_neg + j;
        if d > 0 && use_delta.get(k).copied().unwrap_or(true) {
            out.delta_poc_s1.push(d);
            out.used_by_curr_pic_s1
                .push(used.get(k).copied().unwrap_or(false));
        }
    }

    out
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_bitstream::{BitReader, BitWriter};
    use vaco_limits::{Budget, Limits};

    /// Read a set as the SPS would: `st_rps_idx` below the declared count, so
    /// no `delta_idx_minus1`.
    fn read(bytes: &[u8], idx: u32, prev: &[ShortTermRps]) -> Result<ShortTermRps> {
        let mut r = BitReader::new(bytes);
        let mut b = Budget::new(Limits::strict());
        let mut g = BoundedGolomb::new(&mut r, &mut b);
        parse_st_ref_pic_set(&mut g, idx, prev, 64)
    }

    /// Read a set as a slice segment header would: `st_rps_idx` equal to the
    /// declared count, so `delta_idx_minus1` IS present.
    fn read_slice(bytes: &[u8], count: u32, prev: &[ShortTermRps]) -> Result<ShortTermRps> {
        let mut r = BitReader::new(bytes);
        let mut b = Budget::new(Limits::strict());
        let mut g = BoundedGolomb::new(&mut r, &mut b);
        parse_st_ref_pic_set(&mut g, count, prev, count)
    }

    /// Two negative and one positive picture, written by hand from §7.3.7.
    #[test]
    fn an_explicit_set_reads_its_deltas_in_order() {
        let mut w = BitWriter::new();
        w.ue(2); // num_negative_pics
        w.ue(1); // num_positive_pics
        w.ue(0); // delta_poc_s0_minus1[0] -> delta -1
        w.put(1, 1); // used
        w.ue(1); // delta_poc_s0_minus1[1] -> delta -3
        w.put(1, 0); // not used
        w.ue(3); // delta_poc_s1_minus1[0] -> delta +4
        w.put(1, 1);
        w.rbsp_trailing();
        let set = read(&w.finish(), 0, &[]).expect("parses");
        assert_eq!(set.delta_poc_s0, [-1, -3]);
        assert_eq!(set.used_by_curr_pic_s0, [true, false]);
        assert_eq!(set.delta_poc_s1, [4]);
        assert_eq!(set.num_delta_pocs(), 3);
        assert_eq!(set.num_used_by_curr_pic(), 2);
        assert!(!set.inter_predicted);
    }

    /// The predicted spelling, checked against the derivation done by hand.
    ///
    /// Source set: S0 = [-1, -3], S1 = [+4]. Shift by `delta_rps = -2`, keeping
    /// every picture. Then:
    ///
    /// * negative list: `+4 - 2 = +2` is not negative and is dropped; the
    ///   source picture at `-2` is; `-1 - 2 = -3` and `-3 - 2 = -5` are.
    ///   So S0 = [-2, -3, -5].
    /// * positive list: `-1 - 2` and `-3 - 2` are negative; `-2` is not
    ///   positive; `+4 - 2 = +2` is. So S1 = [+2].
    #[test]
    fn a_predicted_set_walks_the_source_in_the_specified_order() {
        let source = ShortTermRps {
            delta_poc_s0: vec![-1, -3],
            used_by_curr_pic_s0: vec![true, true],
            delta_poc_s1: vec![4],
            used_by_curr_pic_s1: vec![true],
            inter_predicted: false,
        };
        let mut w = BitWriter::new();
        w.put(1, 1); // inter_ref_pic_set_prediction_flag
        w.put(1, 1); // delta_rps_sign = 1 -> negative
        w.ue(1); // abs_delta_rps_minus1 = 1 -> |delta| = 2
        for _ in 0..4 {
            w.put(1, 1); // used_by_curr_pic_flag: use_delta_flag inferred 1
        }
        w.rbsp_trailing();
        let set = read(&w.finish(), 1, &[source]).expect("parses");
        assert_eq!(set.delta_poc_s0, [-2, -3, -5]);
        assert_eq!(set.delta_poc_s1, [2]);
        assert_eq!(set.num_delta_pocs(), 4);
        assert!(set.inter_predicted);
    }

    /// `use_delta_flag` is read only when `used_by_curr_pic_flag` is 0, and a
    /// cleared one drops the picture from the derived set entirely.
    #[test]
    fn a_cleared_use_delta_flag_drops_the_picture() {
        let source = ShortTermRps {
            delta_poc_s0: vec![-1, -3],
            used_by_curr_pic_s0: vec![true, true],
            delta_poc_s1: Vec::new(),
            used_by_curr_pic_s1: Vec::new(),
            inter_predicted: false,
        };
        let mut w = BitWriter::new();
        w.put(1, 1); // predicted
        w.put(1, 1); // sign: negative
        w.ue(0); // |delta| = 1
        w.put(1, 0); // used[0] = 0
        w.put(1, 0); // use_delta[0] = 0 -> -1 - 1 = -2 dropped
        w.put(1, 1); // used[1] = 1     -> -3 - 1 = -4 kept
        w.put(1, 1); // used[2] = 1     -> the source picture at -1 kept
        w.rbsp_trailing();
        let set = read(&w.finish(), 1, &[source]).expect("parses");
        assert_eq!(set.delta_poc_s0, [-1, -4]);
        assert_eq!(set.used_by_curr_pic_s0, [true, true]);
    }

    /// The slice-header spelling reads a `delta_idx_minus1` the SPS spelling
    /// does not — the distinction the `num_short_term_ref_pic_sets` argument
    /// exists for.
    #[test]
    fn a_slice_header_set_carries_a_delta_idx() {
        let source = ShortTermRps {
            delta_poc_s0: vec![-1],
            used_by_curr_pic_s0: vec![true],
            delta_poc_s1: Vec::new(),
            used_by_curr_pic_s1: Vec::new(),
            inter_predicted: false,
        };
        let mut w = BitWriter::new();
        w.put(1, 1); // inter_ref_pic_set_prediction_flag
        w.ue(0); // delta_idx_minus1 -> RefRpsIdx = 0
        w.put(1, 1); // delta_rps_sign -> negative
        w.ue(0); // |delta| = 1
        w.put(1, 1); // used[0]
        w.put(1, 1); // used[1] (the source picture)
        w.rbsp_trailing();
        let bytes = w.finish();
        let set = read_slice(&bytes, 1, &[source]).expect("parses");
        assert_eq!(set.delta_poc_s0, [-1, -2]);
    }

    /// A prediction that names a set nobody read is refused rather than read
    /// against zeros — which is what would silently desynchronise the SPS.
    #[test]
    fn a_prediction_with_no_source_is_refused() {
        let mut w = BitWriter::new();
        w.put(1, 1);
        w.put(1, 0);
        w.ue(0);
        w.rbsp_trailing();
        // st_rps_idx 1 but nothing in `previous`.
        assert!(matches!(
            read_slice(&w.finish(), 1, &[]),
            Err(Error::InvalidData(_))
        ));
    }

    /// An implausible count is refused before it can allocate.
    #[test]
    fn an_absurd_picture_count_is_refused() {
        let mut w = BitWriter::new();
        w.ue(100_000); // num_negative_pics
        w.ue(0);
        w.rbsp_trailing();
        assert!(read(&w.finish(), 0, &[]).is_err());
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let mut w = BitWriter::new();
        w.ue(2);
        w.ue(1);
        w.ue(0);
        w.put(1, 1);
        w.ue(1);
        w.put(1, 0);
        w.ue(3);
        w.put(1, 1);
        w.rbsp_trailing();
        let bytes = w.finish();
        for n in 0..bytes.len() {
            let _ = read(&bytes[..n], 0, &[]);
            let _ = read(&bytes[..n], 1, &[ShortTermRps::default()]);
            let _ = read_slice(&bytes[..n], 1, &[ShortTermRps::default()]);
        }
    }
}
