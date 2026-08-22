//! Channel rematrixing: building the mix matrix, and applying it.
//!
//! # Where the numbers come from
//!
//! Every coefficient and every fold rule below was recovered by feeding unit
//! impulses — sample `k` carrying 1.0 on channel `k` and nothing else — through
//! `ffmpeg … -af aresample=ochl=<out>` and reading the output back in `dbl`, so
//! the matrix is read at full `f64` precision rather than inferred. The exact
//! commands are in `docs/signal/vaco-resample.md` §Provenance.
//!
//! Two structural facts came out of that and neither is in plan 17:
//!
//! 1. **A downmix to a centre-only layout is a composition, not a direct fold.**
//!    `5.1 → mono` is exactly `stereo→mono ∘ 5.1→stereo`: its `FC` coefficient
//!    is `0.999999982885729`, which is `f32(1/√2) · f64(1/√2) · 2` and nothing
//!    else. We reproduce the composition rather than a table.
//! 2. **The mix levels are `f32` options and the structural folds are `f64`
//!    constants.** `FC → L` in `5.1 → stereo` measures `0.7071067690849304`
//!    (single-rounded) while `SL → BL` in `7.1 → 5.1` measures
//!    `0.7071067811865476` (double). Storing [`MixLevels`] fields as `f32` and
//!    the structural constant as `f64` reproduces both.
//!
//! # Normalisation is global and format-dependent
//!
//! Measured: with `flt` output, `5.1 → stereo` rows sum to 2.414 and nothing
//! rescales them. With `s16` output the whole matrix is scaled by `1/2.414`.
//! So `rematrix_maxval` defaults to `1.0` for integer output and to "no ceiling"
//! for float output, and the ceiling is applied to the **largest row** across
//! the whole matrix, not per row — `7.1 → 5.1` scales its pass-through rows by
//! `1/1.7071` even though they contain a single 1.0.

#![allow(
    clippy::integer_division,
    reason = "the only divisions here are by a channel count already proven non-zero"
)]

use core::f64::consts::FRAC_1_SQRT_2;

use vaco_chlayout::{Channel, ChannelLayout, ChannelOrder};
use vaco_core::Error;

use crate::convert::Internal;

/// The structural fold constant: `1/√2` at `f64`, as the reference uses it for
/// folds that are **not** controlled by a mix level.
const R: f64 = FRAC_1_SQRT_2;

/// Mix levels, as the option surface exposes them.
///
/// The three level fields are `f32` because the reference's options are `float`
/// and the single-rounding is observable in the resulting matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixLevels {
    /// Gain applied to the centre channel when folding it into `FL`/`FR`.
    pub center: f32,
    /// Gain applied to surround channels when folding them forward.
    pub surround: f32,
    /// Gain applied to LFE when folding it in. Default 0: LFE is discarded
    /// unless asked for. The applied gain is `lfe · 1/√2`, measured.
    pub lfe: f32,
    /// Overall output scale. Negative means "auto".
    pub rematrix_volume: f32,
    /// Clipping ceiling. `0.0` means "derive from the output format".
    pub rematrix_maxval: f32,
}

impl Default for MixLevels {
    fn default() -> Self {
        Self {
            center: core::f32::consts::FRAC_1_SQRT_2,
            surround: core::f32::consts::FRAC_1_SQRT_2,
            lfe: 0.0,
            rematrix_volume: 1.0,
            rematrix_maxval: 0.0,
        }
    }
}

/// Matrix-encoded surround, from the `matrix_encoding` option surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MatrixEncoding {
    #[default]
    None,
    /// The classic Lt/Rt fold.
    Dolby,
    /// Dolby Pro Logic II.
    Dplii,
    DpliiX,
    DpliiZ,
    DolbyEx,
    DolbyHeadphone,
}

/// How sparse the matrix is, which decides the kernel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MatrixShape {
    /// Every output takes exactly one input with gain 1.0.
    Permutation,
    /// Every output takes at most one input, with arbitrary gain.
    Scaled,
    /// Every output takes at most two inputs.
    Sparse2,
    Dense,
}

/// `m[out][in]`, dense, `f64`.
#[derive(Clone, Debug, PartialEq)]
pub struct MixMatrix {
    pub rows: usize,
    pub cols: usize,
    m: Vec<f64>,
    shape: MatrixShape,
}

impl MixMatrix {
    /// A zero matrix of the given size.
    #[must_use]
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            m: vec![0.0; rows.saturating_mul(cols)],
            shape: MatrixShape::Permutation,
        }
    }

    /// Build from a caller-supplied row-major slice.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `m.len() != rows * cols` or any entry is not
    /// finite.
    pub fn from_rows(rows: usize, cols: usize, m: &[f64]) -> Result<Self, Error> {
        if rows.checked_mul(cols) != Some(m.len()) {
            return Err(Error::InvalidData("mix matrix has the wrong size"));
        }
        if m.iter().any(|v| !v.is_finite()) {
            return Err(Error::InvalidData("mix matrix entry is not finite"));
        }
        let mut out = Self {
            rows,
            cols,
            m: m.to_vec(),
            shape: MatrixShape::Permutation,
        };
        out.reclassify();
        Ok(out)
    }

    #[must_use]
    pub fn get(&self, out: usize, inp: usize) -> f64 {
        if inp >= self.cols {
            return 0.0;
        }
        self.m
            .get(out.saturating_mul(self.cols).saturating_add(inp))
            .copied()
            .unwrap_or(0.0)
    }

    fn set(&mut self, out: usize, inp: usize, v: f64) {
        if inp >= self.cols {
            return;
        }
        if let Some(slot) = self
            .m
            .get_mut(out.saturating_mul(self.cols).saturating_add(inp))
        {
            *slot = v;
        }
    }

    fn add(&mut self, out: usize, inp: usize, v: f64) {
        let cur = self.get(out, inp);
        self.set(out, inp, cur + v);
    }

    #[must_use]
    pub const fn shape(&self) -> MatrixShape {
        self.shape
    }

    /// Row-major view, for callers that want to inspect or copy the matrix.
    #[must_use]
    pub fn as_slice(&self) -> &[f64] {
        &self.m
    }

    /// The worst-case output magnitude for a full-scale input: the largest row
    /// absolute sum.
    #[must_use]
    pub fn peak(&self) -> f64 {
        self.m
            .chunks(self.cols.max(1))
            .map(|row| row.iter().map(|v| v.abs()).sum::<f64>())
            .fold(0.0_f64, f64::max)
    }

    fn scale(&mut self, k: f64) {
        for v in &mut self.m {
            *v *= k;
        }
        self.reclassify();
    }

    fn reclassify(&mut self) {
        let mut shape = MatrixShape::Permutation;
        for row in self.m.chunks(self.cols.max(1)) {
            let taps = row.iter().filter(|v| **v != 0.0).count();
            let unit = taps == 1 && row.contains(&1.0);
            let s = match taps {
                0 | 1 if unit || taps == 0 => MatrixShape::Permutation,
                1 => MatrixShape::Scaled,
                2 => MatrixShape::Sparse2,
                _ => MatrixShape::Dense,
            };
            shape = shape.max(s);
        }
        self.shape = shape;
    }

    /// `self · other`, used to compose a two-step downmix.
    fn compose(&self, other: &Self) -> Self {
        let mut out = Self::zeros(self.rows, other.cols);
        for o in 0..self.rows {
            for i in 0..other.cols {
                let mut acc = 0.0;
                for k in 0..other.rows.min(self.cols) {
                    acc += self.get(o, k) * other.get(k, i);
                }
                out.set(o, i, acc);
            }
        }
        out.reclassify();
        out
    }
}

impl MatrixShape {
    const fn rank(self) -> u8 {
        match self {
            Self::Permutation => 0,
            Self::Scaled => 1,
            Self::Sparse2 => 2,
            Self::Dense => 3,
        }
    }
    fn max(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// Build the mix matrix for a layout pair.
///
/// `int_output` selects the default clipping ceiling: `1.0` for integer output
/// formats, none for float. That split is measured, not chosen — see the module
/// docs.
///
/// # Errors
/// [`Error::Unsupported`] for a matrix encoding we do not implement;
/// [`Error::InvalidData`] for a structurally invalid layout.
pub fn build_matrix(
    inp: &ChannelLayout,
    out: &ChannelLayout,
    levels: &MixLevels,
    encoding: MatrixEncoding,
    int_output: bool,
) -> Result<MixMatrix, Error> {
    match encoding {
        MatrixEncoding::None => {}
        MatrixEncoding::Dolby | MatrixEncoding::Dplii => {
            if out.channels != 2 {
                return Err(Error::InvalidData(
                    "matrix_encoding requires a stereo output layout",
                ));
            }
        }
        MatrixEncoding::DpliiX
        | MatrixEncoding::DpliiZ
        | MatrixEncoding::DolbyEx
        | MatrixEncoding::DolbyHeadphone => {
            return Err(Error::Unsupported(
                "matrix_encoding: only none, dolby and dplii are implemented",
            ));
        }
    }

    let mut m = if encoding == MatrixEncoding::None {
        raw_matrix(inp, out, levels)?
    } else {
        encoded_matrix(inp, levels, encoding)?
    };

    // Phase 4 — normalisation.
    let vol = f64::from(levels.rematrix_volume);
    if vol >= 0.0 && (vol - 1.0) != 0.0 {
        m.scale(vol);
    }
    let maxval = if levels.rematrix_maxval > 0.0 {
        f64::from(levels.rematrix_maxval)
    } else if int_output {
        1.0
    } else {
        f64::INFINITY
    };
    if maxval.is_finite() {
        let peak = m.peak();
        if peak > maxval && peak > 0.0 {
            m.scale(maxval / peak);
        }
    }
    m.reclassify();
    Ok(m)
}

/// The un-normalised matrix.
fn raw_matrix(
    inp: &ChannelLayout,
    out: &ChannelLayout,
    levels: &MixLevels,
) -> Result<MixMatrix, Error> {
    let in_ch = channels_of(inp)?;
    let out_ch = channels_of(out)?;

    // Positional fallback: a layout with no positions cannot be matched by
    // name, so the only defensible mapping is index-for-index.
    if in_ch.iter().any(Option::is_none) || out_ch.iter().any(Option::is_none) {
        let mut m = MixMatrix::zeros(out_ch.len(), in_ch.len());
        for o in 0..out_ch.len().min(in_ch.len()) {
            m.set(o, o, 1.0);
        }
        m.reclassify();
        return Ok(m);
    }
    let in_ch: Vec<Channel> = in_ch.into_iter().flatten().collect();
    let out_ch: Vec<Channel> = out_ch.into_iter().flatten().collect();

    // A centre-only output is a two-step downmix (see module docs).
    let centre_only = out_ch.len() == 1
        && out_ch.first().copied() == Some(Channel::FrontCenter)
        && in_ch.len() > 1;
    if centre_only {
        let stereo = ChannelLayout::STEREO;
        let a = raw_matrix(inp, &stereo, levels)?;
        let mut b = MixMatrix::zeros(1, 2);
        b.set(0, 0, R);
        b.set(0, 1, R);
        let mut m = b.compose(&a);
        m.reclassify();
        return Ok(m);
    }

    let mut m = MixMatrix::zeros(out_ch.len(), in_ch.len());
    let index_of = |list: &[Channel], c: Channel| list.iter().position(|x| *x == c);

    // Phase 1 — direct copies.
    let mut consumed = vec![false; in_ch.len()];
    let mut fed = vec![false; out_ch.len()];
    for (o, oc) in out_ch.iter().enumerate() {
        if let Some(i) = index_of(&in_ch, *oc) {
            m.set(o, i, 1.0);
            if let Some(c) = consumed.get_mut(i) {
                *c = true;
            }
            if let Some(f) = fed.get_mut(o) {
                *f = true;
            }
        }
    }

    // Phase 2 — side/back equivalence rename. Measured: 6.1 -> 5.1 maps SL onto
    // BL at gain 1.0 because 5.1 has no SL, while 7.1 -> 5.1 folds SL into an
    // already-fed BL at 1/sqrt(2). One rule covers both.
    for (a, b) in [
        (Channel::BackLeft, Channel::SideLeft),
        (Channel::BackRight, Channel::SideRight),
    ] {
        for (target, source) in [(a, b), (b, a)] {
            let (Some(o), Some(i)) = (index_of(&out_ch, target), index_of(&in_ch, source)) else {
                continue;
            };
            // Only when neither end is already spoken for.
            if fed.get(o).copied().unwrap_or(true) || consumed.get(i).copied().unwrap_or(true) {
                continue;
            }
            m.set(o, i, 1.0);
            if let Some(c) = consumed.get_mut(i) {
                *c = true;
            }
            if let Some(f) = fed.get_mut(o) {
                *f = true;
            }
        }
    }

    // Phase 3 — upmix: mono in, wider out.
    //
    // MEASURED, and it is the `consumed` check that makes it right: mono's one
    // channel *is* `FrontCenter`, so `mono -> 5.1` finds a home by name in
    // phase 1 and comes out as a direct copy into FC with silence elsewhere.
    // The upmix only fires when the output has no FC to copy into, which is
    // what makes `mono -> stereo` feed FL and FR at 1/sqrt(2) and
    // `mono -> quad` do the same while leaving BL and BR silent.
    if in_ch.len() == 1
        && in_ch.first().copied() == Some(Channel::FrontCenter)
        && !consumed.first().copied().unwrap_or(true)
    {
        let l = index_of(&out_ch, Channel::FrontLeft);
        let r = index_of(&out_ch, Channel::FrontRight);
        if let (Some(l), Some(r)) = (l, r)
            && !fed.get(l).copied().unwrap_or(true)
            && !fed.get(r).copied().unwrap_or(true)
        {
            m.set(l, 0, R);
            m.set(r, 0, R);
            if let Some(c) = consumed.first_mut() {
                *c = true;
            }
        }
    }

    // Phase 4 — downmix folds for every input channel with no home.
    let clev = f64::from(levels.center);
    let slev = f64::from(levels.surround);
    let lfe = f64::from(levels.lfe);
    let fl = index_of(&out_ch, Channel::FrontLeft);
    let fr = index_of(&out_ch, Channel::FrontRight);
    let fc = index_of(&out_ch, Channel::FrontCenter);
    let bl = index_of(&out_ch, Channel::BackLeft);
    let br = index_of(&out_ch, Channel::BackRight);
    let sl = index_of(&out_ch, Channel::SideLeft);
    let sr = index_of(&out_ch, Channel::SideRight);

    for (i, ic) in in_ch.iter().enumerate() {
        if consumed.get(i).copied().unwrap_or(true) {
            continue;
        }
        let pair = |a: Option<usize>, b: Option<usize>, g: f64, m: &mut MixMatrix| {
            if let Some(o) = a {
                m.add(o, i, g);
            }
            if let Some(o) = b {
                m.add(o, i, g);
            }
        };
        match *ic {
            Channel::FrontCenter => pair(fl, fr, clev, &mut m),
            Channel::LowFrequency => pair(fl, fr, lfe * R, &mut m),
            Channel::BackLeft => {
                if let Some(o) = sl {
                    m.add(o, i, R);
                } else if let Some(o) = fl {
                    m.add(o, i, slev);
                }
            }
            Channel::BackRight => {
                if let Some(o) = sr {
                    m.add(o, i, R);
                } else if let Some(o) = fr {
                    m.add(o, i, slev);
                }
            }
            Channel::SideLeft => {
                if let Some(o) = bl {
                    m.add(o, i, R);
                } else if let Some(o) = fl {
                    m.add(o, i, slev);
                }
            }
            Channel::SideRight => {
                if let Some(o) = br {
                    m.add(o, i, R);
                } else if let Some(o) = fr {
                    m.add(o, i, slev);
                }
            }
            Channel::BackCenter => {
                if bl.is_some() || br.is_some() {
                    pair(bl, br, R, &mut m);
                } else {
                    pair(fl, fr, slev * R, &mut m);
                }
            }
            Channel::FrontLeftOfCenter => {
                if let Some(o) = fl {
                    m.add(o, i, 1.0);
                }
            }
            Channel::FrontRightOfCenter => {
                if let Some(o) = fr {
                    m.add(o, i, 1.0);
                }
            }
            Channel::TopFrontLeft | Channel::TopSideLeft | Channel::TopSurroundLeft => {
                if let Some(o) = fl {
                    m.add(o, i, R);
                }
            }
            Channel::TopFrontRight | Channel::TopSideRight | Channel::TopSurroundRight => {
                if let Some(o) = fr {
                    m.add(o, i, R);
                }
            }
            Channel::TopFrontCenter => {
                if let Some(o) = fc {
                    m.add(o, i, R);
                }
            }
            // Everything else is dropped. Measured: hexadecagonal -> stereo
            // leaves TBL, TBC, TBR, WL and WR at exactly zero.
            _ => {}
        }
    }
    m.reclassify();
    Ok(m)
}

/// Matrix-encoded stereo (`Lt`/`Rt`).
///
/// The DPLII constants `√(2/3) ≈ 0.8165` and `1/√3 ≈ 0.5774` are the published
/// encoder matrix, which D7 permits as spec-dictated constants.
fn encoded_matrix(
    inp: &ChannelLayout,
    levels: &MixLevels,
    encoding: MatrixEncoding,
) -> Result<MixMatrix, Error> {
    let in_ch = channels_of(inp)?;
    let in_ch: Vec<Channel> = in_ch.into_iter().flatten().collect();
    if in_ch.len() != inp.channels as usize {
        return Err(Error::InvalidData(
            "matrix_encoding needs a positioned input layout",
        ));
    }
    let idx = |c: Channel| in_ch.iter().position(|x| *x == c);
    let mut m = MixMatrix::zeros(2, in_ch.len());
    let clev = f64::from(levels.center);
    let slev = f64::from(levels.surround);
    if let Some(i) = idx(Channel::FrontLeft) {
        m.set(0, i, 1.0);
    }
    if let Some(i) = idx(Channel::FrontRight) {
        m.set(1, i, 1.0);
    }
    if let Some(i) = idx(Channel::FrontCenter) {
        let g = if encoding == MatrixEncoding::Dplii {
            R
        } else {
            clev
        };
        m.set(0, i, g);
        m.set(1, i, g);
    }
    let ls = idx(Channel::BackLeft).or_else(|| idx(Channel::SideLeft));
    let rs = idx(Channel::BackRight).or_else(|| idx(Channel::SideRight));
    match encoding {
        MatrixEncoding::Dolby => {
            // S = surround * (Ls + Rs) / sqrt(2), inverted on the left.
            let g = slev * R;
            if let Some(i) = ls {
                m.add(0, i, -g);
                m.add(1, i, g);
            }
            if let Some(i) = rs {
                m.add(0, i, -g);
                m.add(1, i, g);
            }
        }
        MatrixEncoding::Dplii => {
            const A: f64 = 0.816_496_580_927_726; // sqrt(2/3)
            const B: f64 = 0.577_350_269_189_626; // 1/sqrt(3)
            if let Some(i) = ls {
                m.add(0, i, -A);
                m.add(1, i, B);
            }
            if let Some(i) = rs {
                m.add(0, i, -B);
                m.add(1, i, A);
            }
        }
        _ => {}
    }
    m.reclassify();
    Ok(m)
}

/// The channel at each index, or `None` where the layout does not say.
fn channels_of(l: &ChannelLayout) -> Result<Vec<Option<Channel>>, Error> {
    if l.channels == 0 {
        return Err(Error::InvalidData("layout has zero channels"));
    }
    let n = l.channels as usize;
    if matches!(l.order, ChannelOrder::Unspecified) {
        // An unspecified layout with a standard channel count means the usual
        // thing; without one there is nothing to match by name.
        return Ok(ChannelLayout::default_for(l.channels).map_or_else(
            || vec![None; n],
            |d| (0..l.channels).map(|i| d.channel_at(i)).collect(),
        ));
    }
    Ok((0..l.channels).map(|i| l.channel_at(i)).collect())
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

/// The rematrix stage.
#[derive(Clone, Debug)]
pub struct Rematrix {
    matrix: MixMatrix,
    /// Per output row: the non-zero `(input index, gain)` pairs. Precomputing
    /// this is what makes `Permutation` and `Sparse2` cheap without a separate
    /// kernel per shape.
    rows: Vec<Vec<(usize, f64)>>,
}

impl Rematrix {
    #[must_use]
    pub fn new(matrix: MixMatrix) -> Self {
        let rows = (0..matrix.rows)
            .map(|o| {
                (0..matrix.cols)
                    .filter_map(|i| {
                        let v = matrix.get(o, i);
                        (v != 0.0).then_some((i, v))
                    })
                    .collect()
            })
            .collect();
        Self { matrix, rows }
    }

    #[must_use]
    pub const fn matrix(&self) -> &MixMatrix {
        &self.matrix
    }

    #[must_use]
    pub const fn in_channels(&self) -> usize {
        self.matrix.cols
    }

    #[must_use]
    pub const fn out_channels(&self) -> usize {
        self.matrix.rows
    }

    /// Apply the matrix to `n` samples of planar input, appending to `out`.
    ///
    /// Output-stationary: one output plane is accumulated across every input
    /// plane before moving on, so the accumulator stays in registers and each
    /// output is written once.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if the plane counts do not match the matrix, or a
    /// plane is shorter than `in_off + n`.
    pub fn apply<T: Internal>(
        &self,
        inputs: &[Vec<T>],
        in_off: usize,
        n: usize,
        out: &mut [Vec<T>],
    ) -> Result<(), Error> {
        if out.len() != self.matrix.rows || inputs.len() != self.matrix.cols {
            return Err(Error::InvalidData("rematrix channel count mismatch"));
        }
        for (row, dst) in self.rows.iter().zip(out.iter_mut()) {
            let base = dst.len();
            dst.resize(base + n, T::ZERO);
            let Some(acc) = dst.get_mut(base..base + n) else {
                return Err(Error::InvalidData("output plane too short"));
            };
            let mut first = true;
            for (i, g) in row {
                let Some(src) = inputs.get(*i).and_then(|p| p.get(in_off..in_off + n)) else {
                    return Err(Error::InvalidData("input plane too short"));
                };
                let gain = T::from_f64(*g);
                if first {
                    for (a, s) in acc.iter_mut().zip(src) {
                        *a = s.mul(gain);
                    }
                    first = false;
                } else {
                    for (a, s) in acc.iter_mut().zip(src) {
                        *a = a.add(s.mul(gain));
                    }
                }
            }
            if first {
                for a in acc.iter_mut() {
                    *a = T::ZERO;
                }
            }
        }
        Ok(())
    }
}
