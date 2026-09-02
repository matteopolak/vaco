//! Batched, vectorised siblings of [`crate::filter_luma_line`]/
//! [`crate::filter_chroma_line`] -- the masked-lane-select kernel this
//! crate's own module doc named as deferred, unblocked by `#619`
//! (`vaco-simd`'s `ops::select_i16`).
//!
//! # Why a batch, not a per-line kernel
//!
//! [`crate::filter_luma_line`]/[`crate::filter_chroma_line`] each filter
//! **one** line -- 4 or 2 samples per side. That is too narrow to fill a
//! vector register: even NEON's native `i16` width (8 lanes) needs 8 lines'
//! worth of the same sample position to fill one vector. A real edge has
//! exactly that many lines available (16 for a luma macroblock/internal
//! edge, 8 for 4:2:0 chroma), and every line along one edge shares the same
//! [`EdgeThresholds`] (alpha/beta/tC0 depend only on the two sides' QP, not
//! on sample values) -- so the batch this module wants is "one edge, every
//! line", which is exactly the shape a caller already has in hand once it
//! stops calling the per-line primitive in a loop.
//!
//! # The technique: compute both candidates, select per lane
//!
//! Every per-sample branch in clause 8.7.2.3/8.7.2.4 -- `bS == 4` vs `bS <
//! 4`, `|p1-p0| < beta`, `filterSamplesFlag` itself -- becomes: compute the
//! result both branches would produce, unconditionally, then pick per lane
//! from a comparison-derived mask via [`vaco_simd::ops::simd::select_i16`].
//! No branch survives into the vector body; the mask tree *is* the control
//! flow. `bS == 0` folds into the same tree as one more mask term (line
//! unmodified) rather than a separate skip path, which is what lets a batch
//! include every line along an edge rather than only the ones some other
//! stage already knows have positive strength.
//!
//! All of clause 8.7.2.3/8.7.2.4's arithmetic stays within `i16`: every
//! sample is `0..=255`, every accumulated sum in the filter equations is
//! bounded well under `i16::MAX` (the largest, `2*p3+3*p2+p1+p0+q0+4`, tops
//! out at `255*8+4`), and `alpha`/`beta`/`tC0` are all small non-negative
//! table entries -- so this batch needs only [`vaco_simd::ops::select_i16`],
//! matching `#619`'s own scope (it did not add `select_i32` for this crate's
//! sake; `vaco-codec-dsp-deblock` never needed it).
//!
//! # Memory shape: `SoA` slices, gathered by the caller
//!
//! Each side's sample position (`p0`, `p1`, ...) is one `&mut [u8]` slice,
//! one element per line -- struct-of-arrays, not the [`crate::LumaLine`]/
//! [`crate::ChromaLine`] array-of-structs the per-line primitives use. A
//! caller walking a picture buffer already reads/writes one sample at a
//! time per line (see `vaco-codec-h264::deblock`'s own `get`/`set`
//! closures); gathering into eight small stack arrays before the call and
//! scattering back after is the "minimal call-site change" this module's
//! own crate doc anticipates, and it is exactly as many individual sample
//! reads/writes as the per-line loop already performed -- the win this
//! module is measured for is in the arithmetic, not the memory traffic
//! (round 1 of the H.264 profiling loop already found that batching
//! deblocking's *memory* access alone moves nothing).
//!
//! `bs[i] == 0` means "line `i` is unmodified", the batched equivalent of
//! [`crate::filter_luma_line`]'s `NonZeroU8` contract -- expressed as a
//! plain `u8` here so a caller can build the whole array (including lines a
//! scalar caller would have skipped outright) without a `NonZeroU8` per
//! lane.

use core::num::NonZeroU8;

use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel, ops};

use crate::{ChromaLine, EdgeThresholds, LumaLine, filter_chroma_line, filter_luma_line};

/// Batched form of [`crate::filter_luma_line`]: filters every line `i`
/// described by `p0[i]..p3[i]`/`q0[i]..q3[i]` against one shared `edge`,
/// `bs[i] == 0` meaning "leave line `i` unmodified".
///
/// `p3`/`q3` are read-only -- clause 8.7.2.3/8.7.2.4 never write them, the
/// same asymmetry [`crate::LumaLine`] already reflects. All nine slices
/// (`p0..p3`, `q0..q3`, `bs`) must be the same length; a length mismatch
/// processes only the shared prefix rather than panicking, matching this
/// crate's no-panic contract on caller-controlled slices.
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per clause-8.7 sample/threshold name"
)]
pub fn filter_luma_edge(
    caps: Caps,
    p0: &mut [u8],
    p1: &mut [u8],
    p2: &mut [u8],
    p3: &[u8],
    q0: &mut [u8],
    q1: &mut [u8],
    q2: &mut [u8],
    q3: &[u8],
    bs: &[u8],
    edge: EdgeThresholds,
) {
    let len = [
        p0.len(),
        p1.len(),
        p2.len(),
        p3.len(),
        q0.len(),
        q1.len(),
        q2.len(),
        q3.len(),
        bs.len(),
    ]
    .into_iter()
    .min()
    .unwrap_or(0);
    let (Some(p0), Some(p1), Some(p2), Some(p3), Some(q0), Some(q1), Some(q2), Some(q3), Some(bs)) = (
        p0.get_mut(..len),
        p1.get_mut(..len),
        p2.get_mut(..len),
        p3.get(..len),
        q0.get_mut(..len),
        q1.get_mut(..len),
        q2.get_mut(..len),
        q3.get(..len),
        bs.get(..len),
    ) else {
        return;
    };
    dispatch_kernel!(caps, s => filter_luma_edge_body(s, p0, p1, p2, p3, q0, q1, q2, q3, bs, edge));
}

/// Batched form of [`crate::filter_chroma_line`]. Same contract as
/// [`filter_luma_edge`], at chroma's narrower `p0`/`p1`/`q0`/`q1` window --
/// `p1`/`q1` are read-only, since clause 8.7.2.4's chroma case never writes
/// them (`crate::filter_chroma_line`'s own asymmetry).
pub fn filter_chroma_edge(
    caps: Caps,
    p0: &mut [u8],
    p1: &[u8],
    q0: &mut [u8],
    q1: &[u8],
    bs: &[u8],
    edge: EdgeThresholds,
) {
    let len = [p0.len(), p1.len(), q0.len(), q1.len(), bs.len()]
        .into_iter()
        .min()
        .unwrap_or(0);
    let (Some(p0), Some(p1), Some(q0), Some(q1), Some(bs)) = (
        p0.get_mut(..len),
        p1.get(..len),
        q0.get_mut(..len),
        q1.get(..len),
        bs.get(..len),
    ) else {
        return;
    };
    dispatch_kernel!(caps, s => filter_chroma_edge_body(s, p0, p1, q0, q1, bs, edge));
}

/// `Clip1_Y`/`Clip1_C` on a vector of already-widened `i16` samples.
#[inline(always)]
fn clip1_vec<S: Lanes>(simd: S, v: S::i16s) -> S::i16s {
    let zero = <S::i16s as SimdBase<S>>::splat(simd, 0);
    let max = <S::i16s as SimdBase<S>>::splat(simd, 255);
    v.max(zero).min(max)
}

/// `Clip3(-c, c, v)` on vectors: `c` is itself per-lane (it is `tC`/`tC0`,
/// which varies with `bS`), unlike [`clip1_vec`]'s fixed bounds.
#[inline(always)]
fn clip3_sym_vec<S, V>(c: V, v: V) -> V
where
    S: Lanes,
    V: SimdInt<S, Element = i16> + core::ops::Neg<Output = V>,
{
    v.max(-c).min(c)
}

/// Boolean-as-`0`/`1` widening of a mask into `i16` lanes, for the two
/// `+1`-per-condition terms clause 8.7.2.3's own `tC` derivation adds.
#[inline(always)]
fn mask_to_0_1<S: Lanes>(simd: S, m: S::mask16s) -> S::i16s {
    let one = <S::i16s as SimdBase<S>>::splat(simd, 1);
    let zero = <S::i16s as SimdBase<S>>::splat(simd, 0);
    ops::simd::select_i16::<S>(m, one, zero)
}

/// Largest native `u8`/`i16` vector width this crate builds for: 64 covers
/// AVX-512's 64-lane `u8s` (and therefore its 32-lane `i16s`), the widest
/// real backend `fearless_simd` 0.7 ships.
const MAX_NATIVE_WIDTH: usize = 64;

/// Load exactly one native-`i16`-width group from a slice shorter than the
/// native `u8` width, by staging it into a zero-padded native-`u8`-width
/// buffer before widening.
///
/// This is what lets a batch smaller than the native `u8` width (chroma's
/// 8 lines, against NEON's 16-lane `u8s`) still reach the vector path at
/// all: [`filter_luma_edge_body`]/[`filter_chroma_edge_body`]'s main loop
/// needs a *full* `u8`-width chunk to widen (one load covers two `i16`
/// groups, low half and high half), so anything shorter than that would
/// otherwise fall straight to the scalar tail -- which is exactly what an
/// earlier version of this module did, and it measured as a **regression**
/// on chroma (`docs/core/simd-adoption-measurements.md` Group 8 records the
/// number). The padding lanes are never read back (only the low `i16` half,
/// which holds the real data, is returned), so their content does not
/// matter.
#[inline(always)]
fn load_i16_group_padded<S: Lanes>(simd: S, chunk: &[u8]) -> S::i16s {
    let n_u8 = <S::u8s as SimdBase<S>>::N;
    let mut buf = [0u8; MAX_NATIVE_WIDTH];
    let Some(buf_n) = buf.get_mut(..n_u8) else {
        return <S::i16s as SimdBase<S>>::splat(simd, 0);
    };
    let take = chunk.len().min(buf_n.len());
    if let (Some(d), Some(s)) = (buf_n.get_mut(..take), chunk.get(..take)) {
        d.copy_from_slice(s);
    }
    let v = <S::u8s as SimdBase<S>>::from_slice(simd, buf_n);
    ops::simd::widen_u8_i16::<S>(v).0
}

/// The store counterpart of [`load_i16_group_padded`]: writes only the real
/// `out.len()` samples of one native-`i16`-width result vector back to
/// `out`, via a small on-stack array rather than [`ops::simd::pack_u8_from_i16`]
/// (which would need a second, unused `i16` half to pack against). Every
/// lane of `v` is already `Clip1`-ed to `0..=255` by the caller, so the
/// narrowing cast is exact, never a wraparound.
#[inline(always)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "v's lanes are always Clip1(...)-ed to 0..=255 by filter_luma_lanes/filter_chroma_lanes \
              before this is called -- exact by construction, never a bitstream-derived value"
)]
fn store_i16_group_padded<S: Lanes>(v: S::i16s, out: &mut [u8]) {
    let n_i16 = <S::i16s as SimdBase<S>>::N;
    let mut buf = [0i16; MAX_NATIVE_WIDTH];
    let Some(buf_n) = buf.get_mut(..n_i16) else {
        return;
    };
    v.store_slice(buf_n);
    let take = out.len().min(buf_n.len());
    if let (Some(o), Some(s)) = (out.get_mut(..take), buf_n.get(..take)) {
        for (ob, &sv) in o.iter_mut().zip(s.iter()) {
            *ob = sv as u8;
        }
    }
}

/// The level-generic body behind [`filter_luma_edge`].
///
/// Structure: reload the native `u8` width per side, widen once
/// ([`ops::simd::widen_u8_i16`]) into a low/high `i16` pair, run the
/// masked-select filter tree on each half independently (both candidates
/// computed unconditionally, `bS==4` selected against `bS<4`, then gated by
/// `filterSamplesFlag && bS!=0`), and pack the two `i16` halves back into
/// one `u8` store ([`ops::simd::pack_u8_from_i16`]). One `u8`-width load
/// covers two `i16`-width lane groups, so this is Rule A's "batch until you
/// spill" already satisfied by the widen itself -- no extra unrolling
/// added on top.
#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    clippy::integer_division,
    reason = "SoA sample slices, one per clause-8.7 name (p0..p3/q0..q3) -- see this module's own \
              doc for why AoS was not chosen; the division computes the native-width prefix length"
)]
fn filter_luma_edge_body<S: Lanes>(
    simd: S,
    p0: &mut [u8],
    p1: &mut [u8],
    p2: &mut [u8],
    p3: &[u8],
    q0: &mut [u8],
    q1: &mut [u8],
    q2: &mut [u8],
    q3: &[u8],
    bs: &[u8],
    edge: EdgeThresholds,
) {
    let n = <S::u8s as SimdBase<S>>::N;
    let len = bs.len();
    let full = (len / n) * n;

    for base in (0..full).step_by(n.max(1)) {
        let (
            Some(p0c),
            Some(p1c),
            Some(p2c),
            Some(p3c),
            Some(q0c),
            Some(q1c),
            Some(q2c),
            Some(q3c),
            Some(bsc),
        ) = (
            p0.get(base..base + n),
            p1.get(base..base + n),
            p2.get(base..base + n),
            p3.get(base..base + n),
            q0.get(base..base + n),
            q1.get(base..base + n),
            q2.get(base..base + n),
            q3.get(base..base + n),
            bs.get(base..base + n),
        )
        else {
            break;
        };

        let p0v = <S::u8s as SimdBase<S>>::from_slice(simd, p0c);
        let p1v = <S::u8s as SimdBase<S>>::from_slice(simd, p1c);
        let p2v = <S::u8s as SimdBase<S>>::from_slice(simd, p2c);
        let p3v = <S::u8s as SimdBase<S>>::from_slice(simd, p3c);
        let q0v = <S::u8s as SimdBase<S>>::from_slice(simd, q0c);
        let q1v = <S::u8s as SimdBase<S>>::from_slice(simd, q1c);
        let q2v = <S::u8s as SimdBase<S>>::from_slice(simd, q2c);
        let q3v = <S::u8s as SimdBase<S>>::from_slice(simd, q3c);
        let bsv = <S::u8s as SimdBase<S>>::from_slice(simd, bsc);

        let (p0lo, p0hi) = ops::simd::widen_u8_i16::<S>(p0v);
        let (p1lo, p1hi) = ops::simd::widen_u8_i16::<S>(p1v);
        let (p2lo, p2hi) = ops::simd::widen_u8_i16::<S>(p2v);
        let (p3lo, p3hi) = ops::simd::widen_u8_i16::<S>(p3v);
        let (q0lo, q0hi) = ops::simd::widen_u8_i16::<S>(q0v);
        let (q1lo, q1hi) = ops::simd::widen_u8_i16::<S>(q1v);
        let (q2lo, q2hi) = ops::simd::widen_u8_i16::<S>(q2v);
        let (q3lo, q3hi) = ops::simd::widen_u8_i16::<S>(q3v);
        let (bslo, bshi) = ops::simd::widen_u8_i16::<S>(bsv);

        let (p0nlo, p1nlo, p2nlo, q0nlo, q1nlo, q2nlo) = filter_luma_lanes::<S>(
            simd, p0lo, p1lo, p2lo, p3lo, q0lo, q1lo, q2lo, q3lo, bslo, edge,
        );
        let (p0nhi, p1nhi, p2nhi, q0nhi, q1nhi, q2nhi) = filter_luma_lanes::<S>(
            simd, p0hi, p1hi, p2hi, p3hi, q0hi, q1hi, q2hi, q3hi, bshi, edge,
        );

        let Some(p0o) = p0.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(p0nlo, p0nhi).store_slice(p0o);
        let Some(p1o) = p1.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(p1nlo, p1nhi).store_slice(p1o);
        let Some(p2o) = p2.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(p2nlo, p2nhi).store_slice(p2o);
        let Some(q0o) = q0.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(q0nlo, q0nhi).store_slice(q0o);
        let Some(q1o) = q1.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(q1nlo, q1nhi).store_slice(q1o);
        let Some(q2o) = q2.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(q2nlo, q2nhi).store_slice(q2o);
    }

    // The remainder is shorter than one native `u8`-width chunk. If it is
    // still at least one native `i16`-width group, take that group through
    // the padded-load vector path (see [`load_i16_group_padded`]'s own doc
    // for why this stage exists at all -- chroma's whole 8-line batch lives
    // here on a target whose native `u8` width is 16, and skipping this
    // stage measured as a regression on exactly that case).
    let n_i16 = <S::i16s as SimdBase<S>>::N;
    let mut full = full;
    while len - full >= n_i16 {
        let base = full;
        let (
            Some(p0c),
            Some(p1c),
            Some(p2c),
            Some(p3c),
            Some(q0c),
            Some(q1c),
            Some(q2c),
            Some(q3c),
            Some(bsc),
        ) = (
            p0.get(base..base + n_i16),
            p1.get(base..base + n_i16),
            p2.get(base..base + n_i16),
            p3.get(base..base + n_i16),
            q0.get(base..base + n_i16),
            q1.get(base..base + n_i16),
            q2.get(base..base + n_i16),
            q3.get(base..base + n_i16),
            bs.get(base..base + n_i16),
        )
        else {
            break;
        };

        let p0v = load_i16_group_padded::<S>(simd, p0c);
        let p1v = load_i16_group_padded::<S>(simd, p1c);
        let p2v = load_i16_group_padded::<S>(simd, p2c);
        let p3v = load_i16_group_padded::<S>(simd, p3c);
        let q0v = load_i16_group_padded::<S>(simd, q0c);
        let q1v = load_i16_group_padded::<S>(simd, q1c);
        let q2v = load_i16_group_padded::<S>(simd, q2c);
        let q3v = load_i16_group_padded::<S>(simd, q3c);
        let bsv = load_i16_group_padded::<S>(simd, bsc);

        let (p0n, p1n, p2n, q0n, q1n, q2n) =
            filter_luma_lanes::<S>(simd, p0v, p1v, p2v, p3v, q0v, q1v, q2v, q3v, bsv, edge);

        if let Some(p0o) = p0.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(p0n, p0o);
        }
        if let Some(p1o) = p1.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(p1n, p1o);
        }
        if let Some(p2o) = p2.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(p2n, p2o);
        }
        if let Some(q0o) = q0.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(q0n, q0o);
        }
        if let Some(q1o) = q1.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(q1n, q1o);
        }
        if let Some(q2o) = q2.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(q2n, q2o);
        }
        full += n_i16;
    }

    for i in full..len {
        let mut line = LumaLine {
            p: [
                p0.get(i).copied().unwrap_or(0),
                p1.get(i).copied().unwrap_or(0),
                p2.get(i).copied().unwrap_or(0),
                p3.get(i).copied().unwrap_or(0),
            ],
            q: [
                q0.get(i).copied().unwrap_or(0),
                q1.get(i).copied().unwrap_or(0),
                q2.get(i).copied().unwrap_or(0),
                q3.get(i).copied().unwrap_or(0),
            ],
        };
        if let Some(bsi) = bs.get(i).copied().and_then(NonZeroU8::new) {
            filter_luma_line(&mut line, bsi, edge);
        }
        if let Some(slot) = p0.get_mut(i) {
            *slot = line.p[0];
        }
        if let Some(slot) = p1.get_mut(i) {
            *slot = line.p[1];
        }
        if let Some(slot) = p2.get_mut(i) {
            *slot = line.p[2];
        }
        if let Some(slot) = q0.get_mut(i) {
            *slot = line.q[0];
        }
        if let Some(slot) = q1.get_mut(i) {
            *slot = line.q[1];
        }
        if let Some(slot) = q2.get_mut(i) {
            *slot = line.q[2];
        }
    }
}

/// One `i16`-native-width group's worth of clause 8.7.2.3/8.7.2.4's luma
/// filter, both `bS==4` and `bS<4` candidates computed unconditionally and
/// selected per lane -- the masked-lane-select tree this whole module
/// exists for. Returns `(p0n, p1n, p2n, q0n, q1n, q2n)`; `p3`/`q3` are
/// read-only inputs, matching [`crate::filter_luma_line`].
#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per clause-8.7 sample/threshold name"
)]
fn filter_luma_lanes<S: Lanes>(
    simd: S,
    p0: S::i16s,
    p1: S::i16s,
    p2: S::i16s,
    p3: S::i16s,
    q0: S::i16s,
    q1: S::i16s,
    q2: S::i16s,
    q3: S::i16s,
    bs: S::i16s,
    edge: EdgeThresholds,
) -> (S::i16s, S::i16s, S::i16s, S::i16s, S::i16s, S::i16s) {
    let splat = |v: i16| <S::i16s as SimdBase<S>>::splat(simd, v);
    let zero = splat(0);
    let one = splat(1);
    let two = splat(2);
    let three = splat(3);
    let four = splat(4);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "alpha/beta/tC0 are Table 8-16/8-17 entries, always small (alpha <= 255, beta <= \
                  18, tC0 <= 25) -- always representable in i16"
    )]
    let (alpha_v, beta_v, tc0_1, tc0_2, tc0_3, strong_thresh) = (
        splat(edge.alpha as i16),
        splat(edge.beta as i16),
        splat(edge.tc0[0] as i16),
        splat(edge.tc0[1] as i16),
        splat(edge.tc0[2] as i16),
        splat(((edge.alpha >> 2) + 2) as i16),
    );

    // Clause 8.7.2.1's `filterSamplesFlag`, and the `ap`/`aq` "is the far
    // sample close to the near one" tests both filter shapes need.
    let d_p0q0 = ops::simd::abs_i16::<S, S::i16s>(p0 - q0);
    let d_p1p0 = ops::simd::abs_i16::<S, S::i16s>(p1 - p0);
    let d_q1q0 = ops::simd::abs_i16::<S, S::i16s>(q1 - q0);
    let samples_pass = d_p0q0.simd_lt(alpha_v) & d_p1p0.simd_lt(beta_v) & d_q1q0.simd_lt(beta_v);

    let ap = ops::simd::abs_i16::<S, S::i16s>(p2 - p0);
    let aq = ops::simd::abs_i16::<S, S::i16s>(q2 - q0);
    let ap_lt_beta = ap.simd_lt(beta_v);
    let aq_lt_beta = aq.simd_lt(beta_v);

    // -------- bS < 4 candidate (clause 8.7.2.3) --------
    let tc0 = ops::simd::select_i16::<S>(
        bs.simd_eq(one),
        tc0_1,
        ops::simd::select_i16::<S>(bs.simd_eq(two), tc0_2, tc0_3),
    );
    let tc = tc0 + mask_to_0_1::<S>(simd, ap_lt_beta) + mask_to_0_1::<S>(simd, aq_lt_beta);
    let delta = clip3_sym_vec::<S, S::i16s>(tc, (((q0 - p0) << 2u32) + (p1 - q1) + four) >> 3u32);
    let p0n_normal = clip1_vec::<S>(simd, p0 + delta);
    let q0n_normal = clip1_vec::<S>(simd, q0 - delta);
    let p1n_normal = ops::simd::select_i16::<S>(
        ap_lt_beta,
        clip1_vec::<S>(
            simd,
            p1 + clip3_sym_vec::<S, S::i16s>(
                tc0,
                (p2 + ((p0 + q0 + one) >> 1u32) - (p1 << 1u32)) >> 1u32,
            ),
        ),
        p1,
    );
    let q1n_normal = ops::simd::select_i16::<S>(
        aq_lt_beta,
        clip1_vec::<S>(
            simd,
            q1 + clip3_sym_vec::<S, S::i16s>(
                tc0,
                (q2 + ((p0 + q0 + one) >> 1u32) - (q1 << 1u32)) >> 1u32,
            ),
        ),
        q1,
    );
    // Clause 8.7.2.3's `bS < 4` case never touches p2/q2 at all.
    let p2n_normal = p2;
    let q2n_normal = q2;

    // -------- bS == 4 candidate (clause 8.7.2.4) --------
    let strong = d_p0q0.simd_lt(strong_thresh);
    let strong_p = ap_lt_beta & strong;
    let strong_q = aq_lt_beta & strong;

    let p0n_strong = ops::simd::select_i16::<S>(
        strong_p,
        clip1_vec::<S>(
            simd,
            (p2 + two * p1 + two * p0 + two * q0 + q1 + four) >> 3u32,
        ),
        clip1_vec::<S>(simd, (two * p1 + p0 + q1 + two) >> 2u32),
    );
    let p1n_strong = ops::simd::select_i16::<S>(
        strong_p,
        clip1_vec::<S>(simd, (p2 + p1 + p0 + q0 + two) >> 2u32),
        p1,
    );
    let p2n_strong = ops::simd::select_i16::<S>(
        strong_p,
        clip1_vec::<S>(simd, (two * p3 + three * p2 + p1 + p0 + q0 + four) >> 3u32),
        p2,
    );

    let q0n_strong = ops::simd::select_i16::<S>(
        strong_q,
        clip1_vec::<S>(
            simd,
            (q2 + two * q1 + two * q0 + two * p0 + p1 + four) >> 3u32,
        ),
        clip1_vec::<S>(simd, (two * q1 + q0 + p1 + two) >> 2u32),
    );
    let q1n_strong = ops::simd::select_i16::<S>(
        strong_q,
        clip1_vec::<S>(simd, (q2 + q1 + q0 + p0 + two) >> 2u32),
        q1,
    );
    let q2n_strong = ops::simd::select_i16::<S>(
        strong_q,
        clip1_vec::<S>(simd, (two * q3 + three * q2 + q1 + q0 + p0 + four) >> 3u32),
        q2,
    );

    // -------- bS==4 vs bS<4, then filterSamplesFlag && bS!=0 --------
    let is_bs4 = bs.simd_eq(four);
    let p0n = ops::simd::select_i16::<S>(is_bs4, p0n_strong, p0n_normal);
    let p1n = ops::simd::select_i16::<S>(is_bs4, p1n_strong, p1n_normal);
    let p2n = ops::simd::select_i16::<S>(is_bs4, p2n_strong, p2n_normal);
    let q0n = ops::simd::select_i16::<S>(is_bs4, q0n_strong, q0n_normal);
    let q1n = ops::simd::select_i16::<S>(is_bs4, q1n_strong, q1n_normal);
    let q2n = ops::simd::select_i16::<S>(is_bs4, q2n_strong, q2n_normal);

    let apply = samples_pass & !bs.simd_eq(zero);
    (
        ops::simd::select_i16::<S>(apply, p0n, p0),
        ops::simd::select_i16::<S>(apply, p1n, p1),
        ops::simd::select_i16::<S>(apply, p2n, p2),
        ops::simd::select_i16::<S>(apply, q0n, q0),
        ops::simd::select_i16::<S>(apply, q1n, q1),
        ops::simd::select_i16::<S>(apply, q2n, q2),
    )
}

/// The level-generic body behind [`filter_chroma_edge`]. Same shape as
/// [`filter_luma_edge_body`] at chroma's narrower window.
#[inline(always)]
#[allow(
    clippy::integer_division,
    reason = "computing the native-width prefix length"
)]
fn filter_chroma_edge_body<S: Lanes>(
    simd: S,
    p0: &mut [u8],
    p1: &[u8],
    q0: &mut [u8],
    q1: &[u8],
    bs: &[u8],
    edge: EdgeThresholds,
) {
    let n = <S::u8s as SimdBase<S>>::N;
    let len = bs.len();
    let full = (len / n) * n;

    for base in (0..full).step_by(n.max(1)) {
        let (Some(p0c), Some(p1c), Some(q0c), Some(q1c), Some(bsc)) = (
            p0.get(base..base + n),
            p1.get(base..base + n),
            q0.get(base..base + n),
            q1.get(base..base + n),
            bs.get(base..base + n),
        ) else {
            break;
        };

        let p0v = <S::u8s as SimdBase<S>>::from_slice(simd, p0c);
        let p1v = <S::u8s as SimdBase<S>>::from_slice(simd, p1c);
        let q0v = <S::u8s as SimdBase<S>>::from_slice(simd, q0c);
        let q1v = <S::u8s as SimdBase<S>>::from_slice(simd, q1c);
        let bsv = <S::u8s as SimdBase<S>>::from_slice(simd, bsc);

        let (p0lo, p0hi) = ops::simd::widen_u8_i16::<S>(p0v);
        let (p1lo, p1hi) = ops::simd::widen_u8_i16::<S>(p1v);
        let (q0lo, q0hi) = ops::simd::widen_u8_i16::<S>(q0v);
        let (q1lo, q1hi) = ops::simd::widen_u8_i16::<S>(q1v);
        let (bslo, bshi) = ops::simd::widen_u8_i16::<S>(bsv);

        let (p0nlo, q0nlo) = filter_chroma_lanes::<S>(simd, p0lo, p1lo, q0lo, q1lo, bslo, edge);
        let (p0nhi, q0nhi) = filter_chroma_lanes::<S>(simd, p0hi, p1hi, q0hi, q1hi, bshi, edge);

        let Some(p0o) = p0.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(p0nlo, p0nhi).store_slice(p0o);
        let Some(q0o) = q0.get_mut(base..base + n) else {
            break;
        };
        ops::simd::pack_u8_from_i16::<S>(q0nlo, q0nhi).store_slice(q0o);
    }

    // See `filter_luma_edge_body`'s identical stage for why this exists:
    // chroma's whole 8-line batch lives here on a target whose native `u8`
    // width is 16 (NEON), and skipping it measured as a regression.
    let n_i16 = <S::i16s as SimdBase<S>>::N;
    let mut full = full;
    while len - full >= n_i16 {
        let base = full;
        let (Some(p0c), Some(p1c), Some(q0c), Some(q1c), Some(bsc)) = (
            p0.get(base..base + n_i16),
            p1.get(base..base + n_i16),
            q0.get(base..base + n_i16),
            q1.get(base..base + n_i16),
            bs.get(base..base + n_i16),
        ) else {
            break;
        };

        let p0v = load_i16_group_padded::<S>(simd, p0c);
        let p1v = load_i16_group_padded::<S>(simd, p1c);
        let q0v = load_i16_group_padded::<S>(simd, q0c);
        let q1v = load_i16_group_padded::<S>(simd, q1c);
        let bsv = load_i16_group_padded::<S>(simd, bsc);

        let (p0n, q0n) = filter_chroma_lanes::<S>(simd, p0v, p1v, q0v, q1v, bsv, edge);

        if let Some(p0o) = p0.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(p0n, p0o);
        }
        if let Some(q0o) = q0.get_mut(base..base + n_i16) {
            store_i16_group_padded::<S>(q0n, q0o);
        }
        full += n_i16;
    }

    for i in full..len {
        let mut line = ChromaLine {
            p: [
                p0.get(i).copied().unwrap_or(0),
                p1.get(i).copied().unwrap_or(0),
            ],
            q: [
                q0.get(i).copied().unwrap_or(0),
                q1.get(i).copied().unwrap_or(0),
            ],
        };
        if let Some(bsi) = bs.get(i).copied().and_then(NonZeroU8::new) {
            filter_chroma_line(&mut line, bsi, edge);
        }
        if let Some(slot) = p0.get_mut(i) {
            *slot = line.p[0];
        }
        if let Some(slot) = q0.get_mut(i) {
            *slot = line.q[0];
        }
    }
}

/// One `i16`-native-width group's worth of clause 8.7.2.4's chroma filter.
/// Unlike luma there is no `ap`/`aq` gate at all -- chroma's `bS<4` case
/// always touches `p0`/`q0` once `filterSamplesFlag` holds, and `tC =
/// tC0 + 1` unconditionally (`crate::filter_chroma_line`'s own comment on
/// this asymmetry). Returns `(p0n, q0n)`.
#[inline(always)]
fn filter_chroma_lanes<S: Lanes>(
    simd: S,
    p0: S::i16s,
    p1: S::i16s,
    q0: S::i16s,
    q1: S::i16s,
    bs: S::i16s,
    edge: EdgeThresholds,
) -> (S::i16s, S::i16s) {
    let splat = |v: i16| <S::i16s as SimdBase<S>>::splat(simd, v);
    let zero = splat(0);
    let one = splat(1);
    let two = splat(2);
    let four = splat(4);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "alpha/beta/tC0 are Table 8-16/8-17 entries, always small -- always representable \
                  in i16"
    )]
    let (alpha_v, beta_v, tc0_1, tc0_2, tc0_3) = (
        splat(edge.alpha as i16),
        splat(edge.beta as i16),
        splat(edge.tc0[0] as i16),
        splat(edge.tc0[1] as i16),
        splat(edge.tc0[2] as i16),
    );

    let d_p0q0 = ops::simd::abs_i16::<S, S::i16s>(p0 - q0);
    let d_p1p0 = ops::simd::abs_i16::<S, S::i16s>(p1 - p0);
    let d_q1q0 = ops::simd::abs_i16::<S, S::i16s>(q1 - q0);
    let samples_pass = d_p0q0.simd_lt(alpha_v) & d_p1p0.simd_lt(beta_v) & d_q1q0.simd_lt(beta_v);

    let p0n_strong = clip1_vec::<S>(simd, (two * p1 + p0 + q1 + two) >> 2u32);
    let q0n_strong = clip1_vec::<S>(simd, (two * q1 + q0 + p1 + two) >> 2u32);

    let tc0 = ops::simd::select_i16::<S>(
        bs.simd_eq(one),
        tc0_1,
        ops::simd::select_i16::<S>(bs.simd_eq(two), tc0_2, tc0_3),
    );
    let tc = tc0 + one;
    let delta = clip3_sym_vec::<S, S::i16s>(tc, (((q0 - p0) << 2u32) + (p1 - q1) + four) >> 3u32);
    let p0n_normal = clip1_vec::<S>(simd, p0 + delta);
    let q0n_normal = clip1_vec::<S>(simd, q0 - delta);

    let is_bs4 = bs.simd_eq(four);
    let p0n = ops::simd::select_i16::<S>(is_bs4, p0n_strong, p0n_normal);
    let q0n = ops::simd::select_i16::<S>(is_bs4, q0n_strong, q0n_normal);

    let apply = samples_pass & !bs.simd_eq(zero);
    (
        ops::simd::select_i16::<S>(apply, p0n, p0),
        ops::simd::select_i16::<S>(apply, q0n, q0),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Runs the scalar per-line reference over the same `SoA` arrays, for
    /// comparison against the batched kernel -- the differential oracle
    /// every kernel in `vaco-simd`'s own authoring model requires.
    fn scalar_luma(
        p0: &[u8],
        p1: &[u8],
        p2: &[u8],
        p3: &[u8],
        q0: &[u8],
        q1: &[u8],
        q2: &[u8],
        q3: &[u8],
        bs: &[u8],
        edge: EdgeThresholds,
    ) -> Vec<LumaLine> {
        (0..bs.len())
            .map(|i| {
                let mut line = LumaLine {
                    p: [p0[i], p1[i], p2[i], p3[i]],
                    q: [q0[i], q1[i], q2[i], q3[i]],
                };
                if let Some(bsi) = NonZeroU8::new(bs[i]) {
                    filter_luma_line(&mut line, bsi, edge);
                }
                line
            })
            .collect()
    }

    fn scalar_chroma(
        p0: &[u8],
        p1: &[u8],
        q0: &[u8],
        q1: &[u8],
        bs: &[u8],
        edge: EdgeThresholds,
    ) -> Vec<ChromaLine> {
        (0..bs.len())
            .map(|i| {
                let mut line = ChromaLine {
                    p: [p0[i], p1[i]],
                    q: [q0[i], q1[i]],
                };
                if let Some(bsi) = NonZeroU8::new(bs[i]) {
                    filter_chroma_line(&mut line, bsi, edge);
                }
                line
            })
            .collect()
    }

    fn edges() -> impl Strategy<Value = EdgeThresholds> {
        (0u8..=51, 0u8..=51, -12i32..=12, -12i32..=12).prop_map(|(qp_p, qp_q, off_a, off_b)| {
            EdgeThresholds::derive(qp_p, qp_q, off_a * 2, off_b * 2)
        })
    }

    proptest! {
        #[test]
        fn batched_luma_matches_scalar_reference(
            edge in edges(),
            p0 in prop::collection::vec(any::<u8>(), 17),
            p1 in prop::collection::vec(any::<u8>(), 17),
            p2 in prop::collection::vec(any::<u8>(), 17),
            p3 in prop::collection::vec(any::<u8>(), 17),
            q0 in prop::collection::vec(any::<u8>(), 17),
            q1 in prop::collection::vec(any::<u8>(), 17),
            q2 in prop::collection::vec(any::<u8>(), 17),
            q3 in prop::collection::vec(any::<u8>(), 17),
            bs in prop::collection::vec(0u8..=4, 17),
        ) {
            let want = scalar_luma(&p0, &p1, &p2, &p3, &q0, &q1, &q2, &q3, &bs, edge);
            let mut gp0 = p0.clone();
            let mut gp1 = p1.clone();
            let mut gp2 = p2.clone();
            let mut gq0 = q0.clone();
            let mut gq1 = q1.clone();
            let mut gq2 = q2.clone();
            filter_luma_edge(
                Caps::detect(),
                &mut gp0, &mut gp1, &mut gp2, &p3,
                &mut gq0, &mut gq1, &mut gq2, &q3,
                &bs, edge,
            );
            for i in 0..bs.len() {
                prop_assert_eq!(gp0[i], want[i].p[0], "p0[{i}]", i = i);
                prop_assert_eq!(gp1[i], want[i].p[1], "p1[{i}]", i = i);
                prop_assert_eq!(gp2[i], want[i].p[2], "p2[{i}]", i = i);
                prop_assert_eq!(gq0[i], want[i].q[0], "q0[{i}]", i = i);
                prop_assert_eq!(gq1[i], want[i].q[1], "q1[{i}]", i = i);
                prop_assert_eq!(gq2[i], want[i].q[2], "q2[{i}]", i = i);
            }
        }

        #[test]
        fn batched_chroma_matches_scalar_reference(
            edge in edges(),
            p0 in prop::collection::vec(any::<u8>(), 17),
            p1 in prop::collection::vec(any::<u8>(), 17),
            q0 in prop::collection::vec(any::<u8>(), 17),
            q1 in prop::collection::vec(any::<u8>(), 17),
            bs in prop::collection::vec(0u8..=4, 17),
        ) {
            let want = scalar_chroma(&p0, &p1, &q0, &q1, &bs, edge);
            let mut gp0 = p0.clone();
            let mut gq0 = q0.clone();
            filter_chroma_edge(Caps::detect(), &mut gp0, &p1, &mut gq0, &q1, &bs, edge);
            for i in 0..bs.len() {
                prop_assert_eq!(gp0[i], want[i].p[0], "p0[{i}]", i = i);
                prop_assert_eq!(gq0[i], want[i].q[0], "q0[{i}]", i = i);
            }
        }
    }

    #[test]
    fn empty_batch_is_a_no_op() {
        let edge = EdgeThresholds::derive(30, 30, 0, 0);
        filter_luma_edge(
            Caps::detect(),
            &mut [],
            &mut [],
            &mut [],
            &[],
            &mut [],
            &mut [],
            &mut [],
            &[],
            &[],
            edge,
        );
        filter_chroma_edge(Caps::detect(), &mut [], &[], &mut [], &[], &[], edge);
    }

    #[test]
    fn a_flat_batch_at_every_bs_is_never_modified() {
        // Mirrors this crate's own `a_flat_line_is_never_modified_regardless_of_bs`,
        // batched: sixteen flat lines at every bS value from 0 to 4.
        let edge = EdgeThresholds::derive(28, 28, 0, 0);
        let mut p0 = [128u8; 16];
        let mut p1 = [128u8; 16];
        let mut p2 = [128u8; 16];
        let p3 = [128u8; 16];
        let mut q0 = [128u8; 16];
        let mut q1 = [128u8; 16];
        let mut q2 = [128u8; 16];
        let q3 = [128u8; 16];
        let bs: Vec<u8> = (0..16u8).map(|i| i % 5).collect();
        filter_luma_edge(
            Caps::detect(),
            &mut p0,
            &mut p1,
            &mut p2,
            &p3,
            &mut q0,
            &mut q1,
            &mut q2,
            &q3,
            &bs,
            edge,
        );
        assert_eq!(p0, [128u8; 16]);
        assert_eq!(p1, [128u8; 16]);
        assert_eq!(p2, [128u8; 16]);
        assert_eq!(q0, [128u8; 16]);
        assert_eq!(q1, [128u8; 16]);
        assert_eq!(q2, [128u8; 16]);
    }
}
