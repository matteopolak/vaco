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
//! this mode, so a decoder that only understood the range-coder mode could
//! never pass the real-`ffmpeg`-stream cross-check the crate's brief asks
//! for. This crate's own encoder emits `coder_type = 1` (range coder)
//! exclusively — simpler, and it reuses the range-coder machinery the
//! Configuration Record needs regardless — so there is no round-trip need for
//! a Golomb-Rice *encoder* here. See the crate's top-level docs.

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

/// One plane's run-mode bookkeeping (RFC 9043 §3.8.2.2.1): `run_index` is
/// "reset to zero for each Plane and Slice", and `run_mode`/`run_count` track
/// progress through the current run within one line.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RunState {
    run_index: u32,
    /// `1` while consuming a counted run of zero differences; `2` right after
    /// the run's length has been read and the terminating nonzero difference
    /// still needs decoding.
    mode: u8,
    count: u32,
}

impl RunState {
    /// Fresh state for a new plane (or slice), matching "`run_index` is reset
    /// to zero for each Plane and Slice"; `mode` starts at 1 (ready to read a
    /// new run's length the first time context 0 is seen).
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            run_index: 0,
            mode: 1,
            count: 0,
        }
    }

    /// Called when `context == 0` and no run is currently being consumed
    /// (`count == 0`): reads the run-length prefix (RFC 9043 §3.8.2.2.1) and
    /// switches to mode 2 (a terminating nonzero difference follows) once the
    /// counted run is exhausted immediately (`get_bits(1) == 0` case) or
    /// leaves `count` set so the caller keeps emitting zero differences.
    ///
    /// `x_after_run` is `x + (1 << log2_run[run_index])`, i.e. the sample
    /// position the run would reach if fully consumed — the RFC's
    /// `x + run_count <= w` guard on whether `run_index` advances.
    fn read_run_prefix(&mut self, r: &mut BitReader<'_>, x: usize, w: usize) {
        if r.try_get(1).unwrap_or(0) == 1 {
            let shift = log2_run(self.run_index as usize);
            self.count = 1u32 << shift;
            if x.saturating_add(self.count as usize) <= w {
                self.run_index += 1;
            }
            self.mode = 1;
        } else {
            let shift = log2_run(self.run_index as usize);
            self.count = if shift == 0 {
                0
            } else {
                r.try_get(shift).unwrap_or(0)
            };
            if self.run_index > 0 {
                self.run_index -= 1;
            }
            self.mode = 2;
        }
    }

    /// Decode one sample's difference in Golomb-Rice mode at context 0
    /// (the "run mode" context). Returns the difference (`0` while a counted
    /// run is in progress, otherwise the decoded terminating value).
    pub(crate) fn next_zero_context_diff(
        &mut self,
        r: &mut BitReader<'_>,
        state: &mut RiceState,
        bits: u32,
        x: usize,
        w: usize,
    ) -> i32 {
        if self.count == 0 && self.mode == 1 {
            self.read_run_prefix(r, x, w);
        }
        if self.count > 0 {
            self.count -= 1;
            0
        } else {
            self.mode = 1;
            decode_level(state, r, bits)
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
