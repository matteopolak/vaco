//! The Golomb-Rice coding mode (RFC 9043 §3.8.2), decode-only.
//!
//! `Vaco-Spec-Ref: rfc9043 RFC 9043 §3.8.2.1 (get_ur_golomb/get_sr_golomb),
//! §3.8.2.2.1 (Figure "log2_run" run-length coding), §3.8.2.3 (sign_extend),
//! §3.8.2.4 (get_vlc_symbol, the per-context adaptive state)`.
//!
//! # Why decode-only
//!
//! `ffmpeg -c:v ffv1`'s own default (`-coder` defaults to `rice`, measured via
//! `ffmpeg -h encoder=ffv1`, recorded as a `blackbox` provenance entry) uses
//! this mode, so a decoder that only understood the range-coder mode could not
//! read most FFV1 in the wild. This crate's own encoder emits `coder_type = 1`
//! (range coder) exclusively — simpler, and it reuses the range-coder
//! machinery the Configuration Record needs regardless — so there is no need
//! for a Golomb-Rice *encoder* here. See the crate's top-level docs.

use vaco_bitstream::BitReader;

/// `log2_run[41]`, RFC 9043 §3.8.2.2.1: also used by JPEG-LS
/// ([ISO.14495-1.1999], per the RFC's own note), which is why the shape looks
/// familiar.
///
/// `Vaco-Spec-Ref: rfc9043 RFC 9043 §3.8.2.2.1 (log2_run, 41 entries)`
#[rustfmt::skip]
pub(crate) const LOG2_RUN: [u8; 41] = [
    0, 0, 0, 0, 1, 1, 1, 1,
    2, 2, 2, 2, 3, 3, 3, 3,
    4, 4, 5, 5, 6, 6, 7, 7,
    8, 9, 10, 11, 12, 13, 14, 15,
    16, 17, 18, 19, 20, 21, 22, 23,
    24,
];

#[inline]
fn log2_run(index: usize) -> u32 {
    u32::from(LOG2_RUN.get(index).copied().unwrap_or(24))
}

/// `get_ur_golomb` (RFC 9043 Figure 26). `esc_bits` is the ESC-case follow-up
/// field's width (`get_bits(bits)` in the pseudocode, which leaves `bits`
/// itself undefined in prose) — RFC 9043 Table 3's last row pins it down
/// empirically: the worked example `000000000000 10000000 -> 139` consumes
/// exactly 8 more bits after the 12-bit all-zero prefix (`128 + 11 = 139`),
/// i.e. `esc_bits` is the sample bit-depth parameter (`bits_per_raw_sample`,
/// or its RCT-adjusted `+1`), the same value callers already thread through
/// to [`sign_extend`] — not a fixed constant. `try_get` bounds every read so
/// a stream that never legitimately reaches ESC still cannot over-read.
///
/// `Vaco-Spec-Ref: rfc9043 RFC 9043 Table 3 (last row: the ESC width, read
/// off the worked example rather than the free variable in Figure 26's text)`
#[must_use]
pub(crate) fn get_ur_golomb(r: &mut BitReader<'_>, k: u32, esc_bits: u32) -> u32 {
    for prefix in 0..12u32 {
        if r.try_get(1).unwrap_or(0) == 1 {
            let suffix = r.try_get(k).unwrap_or(0);
            return suffix.wrapping_add(prefix << k);
        }
    }
    r.try_get(esc_bits).unwrap_or(0).wrapping_add(11)
}

/// `get_sr_golomb` (RFC 9043 Figure 27).
#[must_use]
pub(crate) fn get_sr_golomb(r: &mut BitReader<'_>, k: u32, esc_bits: u32) -> i32 {
    let v = get_ur_golomb(r, k, esc_bits);
    if v & 1 == 1 {
        -(v >> 1).cast_signed() - 1
    } else {
        (v >> 1).cast_signed()
    }
}

/// `sign_extend` (RFC 9043 §3.8.2.3).
#[must_use]
pub(crate) fn sign_extend(input: i32, bits: u32) -> i32 {
    if bits == 0 || bits >= 32 {
        return input;
    }
    let negative_bias: i32 = 1 << (bits - 1);
    let mask = negative_bias - 1;
    let mut out = input & mask;
    if input & negative_bias != 0 {
        out -= negative_bias;
    }
    out
}

/// The per-context adaptive state for Golomb-Rice sample coding (RFC 9043
/// §3.8.2.4/§3.8.2.5).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RiceState {
    drift: i32,
    error_sum: i32,
    bias: i32,
    count: i32,
}

impl RiceState {
    /// Initial values on a keyframe (RFC 9043 §3.8.2.5).
    #[must_use]
    pub(crate) const fn fresh() -> Self {
        Self {
            drift: 0,
            error_sum: 4,
            bias: 0,
            count: 1,
        }
    }

    /// `get_vlc_symbol` (RFC 9043 §3.8.2.4). `bits` is `bits_per_raw_sample`
    /// (or `+1` for JPEG 2000 RCT, per Figure 10's note — this crate's own
    /// Golomb-Rice decode path only exercises YCbCr, so callers pass
    /// `bits_per_raw_sample` directly; the RCT adjustment is the caller's to
    /// make if it ever decodes RCT content in this mode).
    pub(crate) fn get_vlc_symbol(&mut self, r: &mut BitReader<'_>, bits: u32) -> i32 {
        let mut i = self.count;
        let mut k: u32 = 0;
        while i < self.error_sum {
            k += 1;
            i += i;
        }

        let mut v = get_sr_golomb(r, k, bits);
        if 2 * self.drift < -self.count {
            v = -1 - v;
        }

        let ret = sign_extend(v.wrapping_add(self.bias), bits);

        self.error_sum += v.abs();
        self.drift += v;

        if self.count == 128 {
            self.count >>= 1;
            self.drift >>= 1;
            self.error_sum >>= 1;
        }
        self.count += 1;

        if self.drift <= -self.count {
            self.bias = (self.bias - 1).max(-128);
            self.drift = (self.drift + self.count).max(-self.count + 1);
        } else if self.drift > 0 {
            self.bias = (self.bias + 1).min(127);
            self.drift = (self.drift - self.count).min(0);
        }

        ret
    }
}

impl Default for RiceState {
    fn default() -> Self {
        Self::fresh()
    }
}

/// Golomb-Rice level coding for a sample difference (RFC 9043 §3.8.2.4.1):
/// like [`RiceState::get_vlc_symbol`], but `0` is impossible (it is what run
/// mode already accounted for) so the decoded value is shifted past it.
pub(crate) fn decode_level(state: &mut RiceState, r: &mut BitReader<'_>, bits: u32) -> i32 {
    let diff = state.get_vlc_symbol(r, bits);
    if diff >= 0 { diff + 1 } else { diff }
}

/// One plane's run-mode bookkeeping (RFC 9043 §3.8.2.2.1).
///
/// `run_index` is "reset to zero for each Plane and Slice" and so lives for
/// the whole plane; `mode`/`count` are scoped to a single **Line** and are
/// cleared by [`RunState::begin_line`]. RFC 9043 never states that scoping
/// outright, but its own run-length pseudocode only makes sense that way: the
/// `x + run_count <= w` guard measures the run against the line width, and a
/// run carried across a line boundary would let `run_count` outlive the `x` it
/// is compared against. Confirmed by black-box measurement — a real
/// `ffmpeg -coder rice` 160x120 encode decodes byte-exact with per-line
/// scoping and diverges partway through the first plane without it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RunState {
    run_index: u32,
    /// `0` = not in run mode; `1` = in a run whose next length prefix is still
    /// to be read; `2` = in the final, explicitly-counted stretch of a run.
    mode: u8,
    /// Samples left in the current run. Signed because the RFC's own control
    /// flow decrements first and treats the resulting `-1` as "the run ends
    /// here, decode the terminating level".
    count: i32,
}

impl RunState {
    /// Fresh state for a new plane (or slice), matching "`run_index` is reset
    /// to zero for each Plane and Slice".
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            run_index: 0,
            mode: 0,
            count: 0,
        }
    }

    /// Leave run mode at the start of a Line, keeping `run_index` — see the
    /// struct's docs.
    pub(crate) const fn begin_line(&mut self) {
        self.mode = 0;
        self.count = 0;
    }

    /// Whether run mode is currently active. Once entered, a run continues
    /// until a nonzero difference regardless of the *context* of the samples
    /// it covers (RFC 9043 §3.8.2.2: "entered when the context is 0 and left
    /// as soon as a nonzero difference is found"), so the caller must consult
    /// this before it consults the context.
    #[must_use]
    pub(crate) const fn in_run(&self) -> bool {
        self.mode != 0
    }

    /// Enter run mode if this sample's context is 0 and a run is not already
    /// in progress.
    pub(crate) const fn enter_if_zero_context(&mut self, ctx: usize) {
        if ctx == 0 && self.mode == 0 {
            self.mode = 1;
        }
    }

    /// Reads the run-length prefix (RFC 9043 §3.8.2.2.1) when one is due.
    ///
    /// `x` is the sample position and `w` the Line width, together forming the
    /// RFC's `x + run_count <= w` guard on whether `run_index` advances.
    fn read_run_prefix(&mut self, r: &mut BitReader<'_>, x: usize, w: usize) {
        let idx = (self.run_index as usize).min(LOG2_RUN.len() - 1);
        if r.try_get(1).unwrap_or(0) == 1 {
            self.count = 1i32.checked_shl(log2_run(idx)).unwrap_or(i32::MAX);
            if x.saturating_add(self.count.cast_unsigned() as usize) <= w {
                self.run_index = self.run_index.saturating_add(1);
            }
        } else {
            let shift = log2_run(idx);
            self.count = if shift == 0 {
                0
            } else {
                r.try_get(shift).unwrap_or(0).cast_signed()
            };
            if self.run_index > 0 {
                self.run_index -= 1;
            }
            self.mode = 2;
        }
    }

    /// Advance one sample inside run mode. Returns `true` if this sample's
    /// difference is `0` (the run continues), `false` if the run ends here and
    /// the caller must decode a terminating level with [`decode_level`].
    ///
    /// Only call this while [`RunState::in_run`] is true.
    pub(crate) fn next_sample(&mut self, r: &mut BitReader<'_>, x: usize, w: usize) -> bool {
        if self.count == 0 && self.mode == 1 {
            self.read_run_prefix(r, x, w);
        }
        self.count -= 1;
        if self.count < 0 {
            self.mode = 0;
            self.count = 0;
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code exercising the module, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;
    use vaco_bitstream::BitWriter;

    /// RFC 9043 Table 3's worked examples, decoded straight off hand-built
    /// bit patterns — independent of this crate's own encoder (there is none
    /// for this mode), so this is a check against the spec's own text, not a
    /// self-consistency round trip.
    ///
    /// Table 3 is titled "signed Golomb Rice codes" but every non-ambiguous
    /// row (rows where the unsigned and signed readings differ) matches
    /// `get_ur_golomb` directly, not `get_sr_golomb` — e.g. `k=2, bits="0101"
    /// -> 5"`: `get_ur_golomb` gives 5 exactly; `get_sr_golomb` would halve
    /// and negate an odd `5` to `-3`. Read as "the value `get_ur_golomb`
    /// produces before any caller applies its own sign convention", which is
    /// what these cases check.
    #[test]
    fn unsigned_golomb_matches_rfc_table_3() {
        let cases: &[(u32, &[u8], u32)] = &[
            (0, &[1], 0),
            (0, &[0, 0, 1], 2),
            (2, &[1, 0, 0], 0),
            (2, &[1, 1, 0], 2),
            (2, &[0, 1, 0, 1], 5),
        ];
        for &(k, bits, expected) in cases {
            let mut w = BitWriter::new();
            for &b in bits {
                w.put(1, u32::from(b));
            }
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            assert_eq!(get_ur_golomb(&mut r, k, 8), expected, "k={k} bits={bits:?}");
        }
    }

    /// The table's last row: the ESC case, any `k`, escape width 8 bits.
    #[test]
    fn unsigned_golomb_esc_case_matches_rfc_table_3() {
        let mut w = BitWriter::new();
        for _ in 0..12 {
            w.put(1, 0);
        }
        w.put(8, 0b1000_0000);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(get_ur_golomb(&mut r, 3, 8), 139);
    }

    #[test]
    fn sign_extend_round_trips_two_complement() {
        assert_eq!(sign_extend(0b0111, 4), 7);
        assert_eq!(sign_extend(0b1111, 4), -1);
        assert_eq!(sign_extend(0b1000, 4), -8);
        assert_eq!(sign_extend(0, 8), 0);
    }

    #[test]
    fn rice_state_starts_at_documented_initial_values() {
        let s = RiceState::fresh();
        assert_eq!(s.count, 1);
        assert_eq!(s.error_sum, 4);
        assert_eq!(s.bias, 0);
        assert_eq!(s.drift, 0);
    }
}
