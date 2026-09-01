//! Quantization Table Sets (RFC 9043 §3.4, §4.1) and the per-sample context
//! computation built on them (§3.5).
//!
//! `Vaco-Spec-Ref: rfc9043 RFC 9043 §3.4 (Quantization Table Sets), §3.5
//! (Context), §4.1 (QuantizationTableSet/QuantizationTable pseudocode)`.
//!
//! There is no fixed "default quantization table" to transcribe from the
//! RFC — every table is signalled in the bitstream itself (`QuantizationTable`
//! is a run-length description of up to 128 values, always present in
//! `Parameters`). [`QuantTableSet::small_default`] is this crate's own
//! encoder picking *some* valid table (any valid table produces a lossless
//! round trip; only compression ratio depends on the choice), not a value
//! RFC 9043 specifies.

use vaco_core::{Error, Result};

use crate::rangecoder::{RangeDecoder, RangeEncoder, StateTransition, SymbolStates, fresh_states};

/// `MAX_CONTEXT_INPUTS`, RFC 9043 §4.1: one quantization table per gradient
/// input (`l-tl`, `tl-t`, `t-tr`, `L-l`, `T-t`).
pub(crate) const MAX_CONTEXT_INPUTS: usize = 5;

/// One of the five 256-entry tables inside a Quantization Table Set,
/// `quant_tables[i][j]` in the RFC's notation. Index `k` is `sample_difference
/// & 255`.
#[derive(Debug, Clone)]
pub(crate) struct QuantTable {
    values: [i16; 256],
}

impl QuantTable {
    /// Look up `Q_j[diff]` for a raw (unwrapped) sample difference.
    #[inline]
    #[must_use]
    pub(crate) fn get(&self, diff: i32) -> i32 {
        let k = (diff & 0xFF) as usize;
        i32::from(self.values.get(k).copied().unwrap_or(0))
    }
}

/// A Quantization Table Set: five tables plus the context count they imply
/// (RFC 9043 §4.1.2).
#[derive(Debug, Clone)]
pub(crate) struct QuantTableSet {
    tables: [QuantTable; MAX_CONTEXT_INPUTS],
    /// `context_count[i]`, RFC 9043 §4.1.2: at most 32768.
    pub context_count: usize,
}

impl QuantTableSet {
    /// Parse one `QuantizationTableSet()` (RFC 9043 §4.1) from the range
    /// coder. Uses a fresh, self-contained state array — "`QuantizationTableSet`
    /// has its own initial states, all set to 128" (§4.1).
    ///
    /// # Errors
    /// [`Error::InvalidData`] if the run-length description implies more than
    /// 128 total entries for a table (malformed len-1 values), or if the
    /// resulting `context_count` exceeds the spec's cap of 32768.
    pub(crate) fn parse(dec: &mut RangeDecoder<'_>, table: &StateTransition) -> Result<Self> {
        let mut tables: Vec<QuantTable> = Vec::new();
        let mut scale: i64 = 1;
        for _ in 0..MAX_CONTEXT_INPUTS {
            // Each of the five QuantizationTable(i,j) calls gets its own
            // fresh state array — measured against a real ffmpeg
            // Configuration Record: sharing one array across all five (the
            // more obvious reading of "QuantizationTableSet has its own
            // initial states") decoded implausibly large level counts whose
            // product blew straight through the spec's 32768 context_count
            // cap; resetting per table gave three identical 6-level tables
            // and two trivial 1-level tables, product 1331, context_count
            // 666 — sane, and exactly the kind of repeated-table shape a
            // real encoder's default (reusing one quantizer for symmetric
            // gradient directions) would produce.
            let mut states = fresh_states();
            let (qt, len_count) = parse_one_table(dec, table, &mut states, scale)?;
            tables.push(qt);
            scale = scale
                .checked_mul(2 * i64::from(len_count) - 1)
                .ok_or(Error::InvalidData("ffv1: quant table scale overflow"))?;
        }
        #[allow(
            clippy::integer_division,
            reason = "context_count = ceil(scale/2) per RFC 9043 §4.1's own pseudocode; scale is always odd (a product of odd factors), so this floor-division is exact, not lossy"
        )]
        let half = (scale + 1) / 2;
        let context_count = usize::try_from(half).unwrap_or(usize::MAX);
        if context_count == 0 || context_count > 32768 {
            return Err(Error::InvalidData(
                "ffv1: quant table set context_count out of range",
            ));
        }
        let tables: [QuantTable; MAX_CONTEXT_INPUTS] = tables
            .try_into()
            .map_err(|_| Error::InvalidData("ffv1: wrong quant table count"))?;
        Ok(Self {
            tables,
            context_count,
        })
    }

    /// Write one `QuantizationTableSet()`.
    ///
    /// # Errors
    /// Never fails; `Result` kept for symmetry with [`QuantTableSet::parse`].
    pub(crate) fn write(&self, enc: &mut RangeEncoder, table: &StateTransition) -> Result<()> {
        // Mirrors `parse`: a fresh state array per table, not one shared
        // across all five — see `parse`'s docs for the measurement.
        for qt in &self.tables {
            let mut states = fresh_states();
            write_one_table(enc, table, &mut states, qt)?;
        }
        Ok(())
    }

    /// The five quantization tables, indexed `0..MAX_CONTEXT_INPUTS`.
    #[must_use]
    pub(crate) fn table(&self, j: usize) -> Option<&QuantTable> {
        self.tables.get(j)
    }

    /// This crate's own encoder default: a small, valid table set (RFC 9043
    /// puts no constraint on the *values* beyond the run-length encoding and
    /// the 32768 context cap — see the module docs for why any valid choice
    /// is lossless).
    ///
    /// Each of the 5 gradient inputs is quantized to exactly 3 levels
    /// (`{-1, 0, 1}`: "no local gradient" vs "some gradient, sign only"),
    /// giving `context_count = ceil(3^5 / 2) = 122`. Coarse, but correct —
    /// context selection cannot change what value round-trips, only how well
    /// it compresses.
    #[must_use]
    pub(crate) fn small_default() -> Self {
        let mut scale: i64 = 1;
        let tables: Vec<QuantTable> = (0..MAX_CONTEXT_INPUTS)
            .map(|_| {
                let qt = build_table_two_runs(1, 127, scale as i32);
                scale *= 3; // 2*len_count(2) - 1 = 3
                qt
            })
            .collect();
        let tables: [QuantTable; MAX_CONTEXT_INPUTS] = tables
            .try_into()
            .unwrap_or_else(|_| std::array::from_fn(|_| build_table_two_runs(1, 127, 1)));
        #[allow(
            clippy::integer_division,
            reason = "context_count = ceil(scale/2) per RFC 9043 §4.1's own pseudocode; scale is always odd (a product of odd factors), so this floor-division is exact, not lossy"
        )]
        let half = (scale + 1) / 2;
        let context_count = usize::try_from(half).unwrap_or(1);
        Self {
            tables,
            context_count,
        }
    }
}

/// Build a table with exactly two runs in the `k = 0..128` half: `run0_len`
/// entries at level 0, then the rest (`128 - run0_len`) at level `scale`
/// (mirrored negative for `k = 129..256`, per `QuantizationTable`'s
/// pseudocode).
fn build_table_two_runs(run0_len: u32, _run1_len: u32, scale: i32) -> QuantTable {
    let mut values = [0i16; 256];
    for (k, slot) in values.iter_mut().enumerate().take(128) {
        let v = if (k as u32) < run0_len { 0 } else { scale };
        *slot = v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    }
    for k in 1..128usize {
        if let (Some(pos), Some(_)) = (values.get(k).copied(), values.get(256 - k))
            && let Some(slot) = values.get_mut(256 - k)
        {
            *slot = -pos;
        }
    }
    if let (Some(v127), Some(slot128)) = (values.get(127).copied(), values.get_mut(128)) {
        *slot128 = -v127;
    }
    QuantTable { values }
}

/// `QuantizationTable(i, j, scale)`, RFC 9043 §4.1 — parse the run-length
/// description for one table, returning it plus `len_count[i][j]`.
fn parse_one_table(
    dec: &mut RangeDecoder<'_>,
    table: &StateTransition,
    states: &mut SymbolStates,
    scale: i64,
) -> Result<(QuantTable, u32)> {
    let mut values = [0i16; 256];
    let mut k: usize = 0;
    let mut v: i64 = 0;
    // Bounded by construction: k only ever increases, and the loop cannot
    // run more than 128 times regardless of what `len` decodes to (each
    // iteration consumes at least one `k`), so a malformed len of 0 cannot
    // spin forever — `len.max(1)` below guarantees forward progress.
    while k < 128 {
        let len_minus_1 = dec.get_symbol(states, table, false);
        let len = u32::try_from(len_minus_1)
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        for _ in 0..len {
            if k >= 128 {
                break;
            }
            let value = scale
                .checked_mul(v)
                .ok_or(Error::InvalidData("ffv1: quant table value overflow"))?;
            if let Some(slot) = values.get_mut(k) {
                *slot = value.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
            }
            k += 1;
        }
        v += 1;
        if v > 256 {
            return Err(Error::InvalidData("ffv1: quant table run-length overrun"));
        }
    }
    let len_count = u32::try_from(v).unwrap_or(u32::MAX);
    for k in 1..128usize {
        let pos = values.get(k).copied().unwrap_or(0);
        if let Some(slot) = values.get_mut(256 - k) {
            *slot = -pos;
        }
    }
    if let Some(v127) = values.get(127).copied()
        && let Some(slot) = values.get_mut(128)
    {
        *slot = -v127;
    }
    Ok((QuantTable { values }, len_count))
}

/// Write side of [`parse_one_table`]: describe `qt`'s first 128 entries
/// (`k = 0..128`, values `scale*0, scale*1, ...`) as consecutive runs.
///
/// # Errors
/// Never fails; `Result` kept for symmetry with [`parse_one_table`] and so a
/// caller composing this with fallible steps needs no special case.
#[allow(
    clippy::unnecessary_wraps,
    reason = "symmetry with parse_one_table's Result, which genuinely can fail"
)]
fn write_one_table(
    enc: &mut RangeEncoder,
    table: &StateTransition,
    states: &mut SymbolStates,
    qt: &QuantTable,
) -> Result<()> {
    let mut k = 0usize;
    while k < 128 {
        let start = qt.values.get(k).copied().unwrap_or(0);
        let mut len: i32 = 1;
        // Extend the run while the value stays constant.
        while k + (len as usize) < 128
            && qt.values.get(k + len as usize).copied().unwrap_or(0) == start
        {
            len += 1;
        }
        enc.put_symbol(states, table, len - 1, false);
        k += len as usize;
    }
    Ok(())
}

/// The median predictor (RFC 9043 §3.3): `median(l, t, l + t - tl)`.
///
/// `#[inline]` alone is a hint the optimiser is free to ignore (D21); this
/// one is measured elsewhere in this session's own profiling (per-sample,
/// called once per pixel in every decode/encode loop in `slice.rs`) to have
/// stayed out-of-line despite carrying it.
#[allow(
    clippy::inline_always,
    reason = "measured hot per-sample function -- see the doc comment above"
)]
#[inline(always)]
#[must_use]
pub(crate) const fn median_predictor(l: i32, t: i32, tl: i32) -> i32 {
    let grad = l + t - tl;
    // median of three without sorting: min(max(l,t),max(min(l,t),grad)).
    let (lo, hi) = if l < t { (l, t) } else { (t, l) };
    if grad < lo {
        lo
    } else if grad > hi {
        hi
    } else {
        grad
    }
}

/// The per-sample context (RFC 9043 §3.5):
/// `Q_0[l-tl] + Q_1[tl-t] + Q_2[t-tr] + Q_3[L-l] + Q_4[T-t]`.
///
/// Returns `(context, sign_flip)`: `context` is always `>= 0` (the RFC's
/// "if context >= 0 use it, else use -context and flip the coded sign" is
/// folded in here), and `sign_flip` tells the caller whether to negate the
/// sample difference before/after coding it.
#[inline]
#[must_use]
pub(crate) fn compute_context(
    qts: &QuantTableSet,
    l: i32,
    t: i32,
    tl: i32,
    tr: i32,
    ll: i32,
    tt: i32,
) -> (usize, bool) {
    let raw = qts.table(0).map_or(0, |q| q.get(l - tl))
        + qts.table(1).map_or(0, |q| q.get(tl - t))
        + qts.table(2).map_or(0, |q| q.get(t - tr))
        + qts.table(3).map_or(0, |q| q.get(ll - l))
        + qts.table(4).map_or(0, |q| q.get(tt - t));
    if raw >= 0 {
        (raw as usize, false)
    } else {
        ((-raw) as usize, true)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code exercising the module, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;

    #[test]
    fn small_default_context_count_is_bounded() {
        let qts = QuantTableSet::small_default();
        assert!(qts.context_count > 0);
        assert!(qts.context_count <= 32768);
        assert_eq!(qts.context_count, 122); // ceil(3^5/2)
    }

    #[test]
    fn quant_table_set_round_trips_through_range_coder() {
        let st = StateTransition::default_table();
        let qts = QuantTableSet::small_default();

        let mut enc = RangeEncoder::new();
        qts.write(&mut enc, &st).expect("write");
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let parsed = QuantTableSet::parse(&mut dec, &st).expect("parse");
        assert_eq!(parsed.context_count, qts.context_count);
        for j in 0..MAX_CONTEXT_INPUTS {
            let a = qts.table(j).expect("table");
            let b = parsed.table(j).expect("table");
            for k in -300..300 {
                assert_eq!(a.get(k), b.get(k), "table {j} k {k}");
            }
        }
    }

    #[test]
    fn median_predictor_is_a_true_median() {
        for &(l, t, tl) in &[
            (0, 0, 0),
            (5, 5, 5),
            (10, 2, 6),
            (2, 10, 6),
            (100, 0, 0),
            (0, 100, 0),
        ] {
            let grad = l + t - tl;
            let mut v = [l, t, grad];
            v.sort_unstable();
            assert_eq!(median_predictor(l, t, tl), v[1]);
        }
    }

    #[test]
    fn context_is_zero_for_flat_region() {
        let qts = QuantTableSet::small_default();
        let (ctx, flip) = compute_context(&qts, 5, 5, 5, 5, 5, 5);
        assert_eq!(ctx, 0);
        assert!(!flip);
    }
}
