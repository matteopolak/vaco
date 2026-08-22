//! Context variables and their initialisation.
//!
//! A CABAC context is two numbers: `pStateIdx`, an index into the probability
//! ladder, and `valMPS`, which of the two bin values is currently the more
//! probable. [`ContextModel`] stores them packed into one byte as
//! `(pStateIdx << 1) | valMPS`, which is exactly the form clause 9.3.1.1's
//! initialisation naturally produces and exactly the form the transition tables
//! want. See `tables` for why the packing removes a branch from the hot loop.
//!
//! # Initialisation is per-codec, but the *arithmetic* is not
//!
//! H.264 and H.265 derive a context's starting state from the slice QP by the
//! same two-step formula, differing only in how the pair `(m, n)` is spelled:
//!
//! | | `(m, n)` source |
//! |---|---|
//! | H.264, clause 9.3.1.1 | given directly, as Tables 9-12 … 9-33 |
//! | H.265, clause 9.3.2.2 | packed into one `initValue` byte and unpacked |
//!
//! Both formulas live here. **The per-syntax-element `(m, n)` values do not.**
//! They are dozens of tables of several hundred entries each, they are indexed
//! by `ctxIdx` assignments that only a specific codec's slice syntax defines,
//! and putting them here would make this crate know what a macroblock is —
//! which `10-architecture.md` §1.5 says a shared layer must not. They belong to
//! `vaco-codec-h264` and `vaco-codec-hevc`, which pass them to
//! [`init_contexts`] or [`init_contexts_hevc`].

/// One adaptive context variable: `pStateIdx` and `valMPS` packed into a byte.
///
/// `Copy` and one byte wide, so a codec's whole context set is a plain array
/// that clones for free — which matters, because H.264 CABAC-based slice
/// decoding wants a snapshot of the entire set at the start of each macroblock
/// row for wavefront threading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextModel(pub(crate) u8);

impl ContextModel {
    /// `pStateIdx` 0, `valMPS` 0 — the least-skewed state, and what a context
    /// that has never been initialised holds.
    pub const UNINITIALISED: Self = Self(0);

    /// The state clause 9.3.3.2.4 requires for `end_of_slice_flag`.
    ///
    /// `pStateIdx` 63 is the one state `transIdxMPS` maps to itself, so it never
    /// adapts. `DecodeTerminate` does not use a context at all in this
    /// implementation — the constant is here because the specification describes
    /// the terminating bin *as* a context at `pStateIdx` 63, and a reader
    /// checking this crate against clause 9.3 should be able to find it.
    pub const TERMINATE: Self = Self(63 << 1);

    /// Build from the two spec variables.
    ///
    /// `state_idx` above 63 saturates: a context index is derived from a table,
    /// never from the bitstream, so this can only be a caller bug, and clamping
    /// keeps the type's invariant (`0..=127`) true by construction.
    #[must_use]
    pub const fn new(state_idx: u8, mps: bool) -> Self {
        let p = if state_idx > 63 { 63 } else { state_idx };
        Self((p << 1) | (mps as u8))
    }

    /// `pStateIdx`, 0–63.
    #[must_use]
    #[inline]
    pub const fn state_idx(self) -> u8 {
        self.0 >> 1
    }

    /// `valMPS`.
    #[must_use]
    #[inline]
    pub const fn mps(self) -> bool {
        self.0 & 1 == 1
    }

    /// The packed byte, for a codec that wants to checkpoint a context set.
    #[must_use]
    #[inline]
    pub const fn packed(self) -> u8 {
        self.0
    }

    /// Rebuild from a byte produced by [`packed`](ContextModel::packed).
    ///
    /// Total: every `u8` names a valid state, because 128–255 mirror 0–127 in
    /// the transition tables. A checkpoint round-trips exactly.
    #[must_use]
    #[inline]
    pub const fn from_packed(byte: u8) -> Self {
        Self(byte & 0x7F)
    }

    /// H.264 clause 9.3.1.1 — derive the initial state from `(m, n)` and the
    /// slice QP.
    ///
    /// ```text
    /// preCtxState = Clip3(1, 126, ((m * Clip3(0, 51, SliceQPY)) >> 4) + n)
    /// if preCtxState <= 63 { pStateIdx = 63 - preCtxState; valMPS = 0 }
    /// else                 { pStateIdx = preCtxState - 64; valMPS = 1 }
    /// ```
    ///
    /// The packed representation makes the second half free: `preCtxState` is
    /// already `(pStateIdx << 1) | valMPS` reflected about 64, so the two
    /// branches are one conditional negate. That is not a shortcut — it is why
    /// the packing was chosen.
    #[must_use]
    pub const fn init_h264(m: i16, n: i16, slice_qp: i8) -> Self {
        let qp = clip3_i32(0, 51, slice_qp as i32);
        let pre = clip3_i32(1, 126, ((m as i32 * qp) >> 4) + n as i32);
        if pre <= 63 {
            // pStateIdx = 63 - pre, valMPS = 0
            Self(((63 - pre) as u8) << 1)
        } else {
            // pStateIdx = pre - 64, valMPS = 1
            Self((((pre - 64) as u8) << 1) | 1)
        }
    }

    /// H.265 clause 9.3.2.2 — the same derivation, with `(m, n)` unpacked from
    /// a single `initValue` byte.
    ///
    /// ```text
    /// slopeIdx  = initValue >> 4
    /// offsetIdx = initValue & 15
    /// m = slopeIdx * 5 - 45
    /// n = (offsetIdx << 3) - 16
    /// ```
    #[must_use]
    pub const fn init_hevc(init_value: u8, slice_qp: i8) -> Self {
        let slope_idx = (init_value >> 4) as i16;
        let offset_idx = (init_value & 15) as i16;
        let m = slope_idx * 5 - 45;
        let n = (offset_idx << 3) - 16;
        Self::init_h264(m, n, slice_qp)
    }
}

/// `Clip3(a, b, x)` as the specifications define it.
const fn clip3_i32(lo: i32, hi: i32, x: i32) -> i32 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

/// One row of an H.264 context initialisation table: the `(m, n)` pair.
///
/// The codec crate owns the tables; this is only the shape they have, so the
/// initialisation loop can be written once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextInit {
    /// The slope, multiplied by the clipped slice QP.
    pub m: i16,
    /// The offset.
    pub n: i16,
}

impl ContextInit {
    /// Construct a pair.
    #[must_use]
    pub const fn new(m: i16, n: i16) -> Self {
        Self { m, n }
    }
}

/// Initialise a context set from H.264 `(m, n)` pairs, clause 9.3.1.1.
///
/// Writes `min(dst.len(), inits.len())` contexts and returns that count, so a
/// mismatched pair of lengths truncates rather than panicking — the caller's
/// tables and its `ctxIdx` range should agree, but a decoder must not die if a
/// future table is added with the wrong length.
pub fn init_contexts(dst: &mut [ContextModel], inits: &[ContextInit], slice_qp: i8) -> usize {
    let mut n = 0usize;
    for (d, init) in dst.iter_mut().zip(inits.iter()) {
        *d = ContextModel::init_h264(init.m, init.n, slice_qp);
        n += 1;
    }
    n
}

/// Initialise a context set from H.265 `initValue` bytes, clause 9.3.2.2.
///
/// As [`init_contexts`], with the `(m, n)` pair unpacked from each byte.
pub fn init_contexts_hevc(dst: &mut [ContextModel], init_values: &[u8], slice_qp: i8) -> usize {
    let mut n = 0usize;
    for (d, &iv) in dst.iter_mut().zip(init_values.iter()) {
        *d = ContextModel::init_hevc(iv, slice_qp);
        n += 1;
    }
    n
}

/// Whether `state_idx` is the non-adapting state, `pStateIdx == 63`.
///
/// `transIdxMPS[63] == 63` and `transIdxLPS[63] == 63`, so a context that
/// reaches this state never leaves it. Exposed because a codec verifying its own
/// context handling wants to assert it; `tests/spec.rs` checks the tables really
/// do have that fixed point.
#[must_use]
#[inline]
pub const fn is_terminal_state(state_idx: u8) -> bool {
    state_idx == 63
}
