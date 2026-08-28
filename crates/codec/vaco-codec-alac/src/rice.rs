//! Adaptive Golomb-Rice coding of the predictor's residual stream.
//!
//! # Provenance
//!
//! Own design (see `predictor.rs`'s doc comment for why: no independent
//! bitstream specification exists for ALAC's actual "dyn" adaptive code, and
//! this crate does not read the reference source to recover it). The
//! adaptation rule below — grow `k` when a value overflows what the current
//! `k` addresses well, shrink it when a value undershoots — is the standard
//! shape used by several adaptive Rice/Golomb schemes (e.g. Shorten, and
//! JPEG-LS's Golomb stage); the specific thresholds and the escape mechanism
//! are this crate's own choice, not measured from anywhere.
//!
//! # The unary prefix must be bounded
//!
//! `BitReader::get` pads with zeros past the logical end of a truncated
//! packet (its "sticky overrun" model). A unary prefix read as "count zero
//! bits until a one" would therefore spin for the rest of the buffer's
//! padding on any truncated or malformed input — the exact `try_get`-in-a-
//! loop hazard `planning/AGENT-CONSTRAINTS.md` documents for AAC's
//! `raw_data_block`. [`ESCAPE_Q`] bounds the unary read to a fixed number of
//! iterations; reaching it switches to a fixed-width escape code instead of
//! extending the prefix further.

use vaco_bitstream::{BitReader, BitWriter};

/// Unary quotient values at or above this switch to the escape code. Chosen
/// so the common case (residuals within a few multiples of 2^k) never pays
/// for it, while a malformed stream can never make the reader loop more than
/// this many times per symbol.
const ESCAPE_Q: u32 = 24;
/// Width of the escape code's raw value field. 64 bits comfortably covers
/// this crate's residual range (at most `bit_depth + 2` bits, and
/// `bit_depth <= 32`).
const ESCAPE_BITS: u32 = 64;
/// Largest Rice parameter this crate ever selects or accepts.
const MAX_K: u32 = 30;

/// Per-channel adaptive Rice coder state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RiceState {
    k: u32,
}

impl RiceState {
    pub(crate) fn new() -> Self {
        Self { k: 4 }
    }

    /// Map a signed residual to a non-negative code word: `0, -1, 1, -2, 2,
    /// ...` becomes `0, 1, 2, 3, 4, ...`, so small magnitudes of either sign
    /// stay small after mapping.
    fn zigzag(v: i64) -> u64 {
        if v >= 0 {
            (v as u64).wrapping_shl(1)
        } else {
            (v.wrapping_neg() as u64).wrapping_shl(1).wrapping_sub(1)
        }
    }

    fn unzigzag(z: u64) -> i64 {
        if z & 1 == 0 {
            (z >> 1).cast_signed()
        } else {
            -((z >> 1).cast_signed().wrapping_add(1))
        }
    }

    fn adapt(&mut self, value: u64) {
        let k = self.k;
        if value >= (1u64 << k) {
            self.k = (k + 1).min(MAX_K);
        } else if k > 0 && value < (1u64 << (k - 1)) {
            self.k = k - 1;
        }
    }

    /// Write one residual.
    pub(crate) fn write(&mut self, w: &mut BitWriter, residual: i64) {
        let value = Self::zigzag(residual);
        let k = self.k;
        let q = value >> k;
        if q >= u64::from(ESCAPE_Q) {
            w.put_zeros(ESCAPE_Q);
            w.put(1, 1);
            w.put_long(ESCAPE_BITS, value);
        } else {
            w.put_zeros(q as u32);
            w.put(1, 1);
            if k > 0 {
                w.put_long(k, value & ((1u64 << k) - 1));
            }
        }
        self.adapt(value);
    }

    /// Read one residual. Bounded to at most `ESCAPE_Q + 1` unary bits
    /// regardless of the input, so a truncated packet's zero-padding tail
    /// cannot turn this into an unbounded loop.
    pub(crate) fn read(&mut self, r: &mut BitReader<'_>) -> i64 {
        let mut q = 0u32;
        let mut stopped = false;
        while q < ESCAPE_Q {
            if r.get_bit() == 1 {
                stopped = true;
                break;
            }
            q += 1;
        }
        if !stopped {
            // Either a genuine escape, or a truncated stream whose implicit
            // zero padding never produced a stop bit — the caller checks
            // `r.finish()`/`r.overrun()` afterwards either way, so treating
            // this branch as "read the escape shape" is safe: it consumes a
            // bounded number of bits and never blocks.
            let _ = r.get_bit(); // the stop bit written after the ESCAPE_Q zeros
            let value = r.get_long(ESCAPE_BITS);
            self.adapt(value);
            return Self::unzigzag(value);
        }
        let k = self.k;
        let low = if k > 0 { r.get_long(k) } else { 0 };
        let value = (u64::from(q) << k) | low;
        self.adapt(value);
        Self::unzigzag(value)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn zigzag_round_trips() {
        for v in [
            0i64,
            1,
            -1,
            2,
            -2,
            12345,
            -12345,
            i64::from(i32::MAX),
            i64::from(i32::MIN),
        ] {
            assert_eq!(RiceState::unzigzag(RiceState::zigzag(v)), v);
        }
    }

    #[test]
    fn write_then_read_round_trips_a_mixed_stream() {
        let values: Vec<i64> = (0..2000)
            .map(|i: i64| {
                let base = ((i * 97) % 41) - 20;
                if i % 233 == 0 { base * 5_000_000 } else { base }
            })
            .collect();

        let mut w = BitWriter::new();
        let mut enc_state = RiceState::new();
        for &v in &values {
            enc_state.write(&mut w, v);
        }
        let bytes = w.finish();

        let mut r = BitReader::new(&bytes);
        let mut dec_state = RiceState::new();
        for &v in &values {
            assert_eq!(dec_state.read(&mut r), v);
        }
    }

    #[test]
    fn truncated_stream_terminates_and_flags_overrun() {
        // All-zero bytes: an all-zero unary prefix forever, if unbounded.
        let bytes = [0u8; 4];
        let mut r = BitReader::new(&bytes);
        let mut state = RiceState::new();
        // Must return promptly (bounded reads only) rather than hang.
        let _ = state.read(&mut r);
        // Reading well past a 4-byte buffer must be flagged, not silently
        // accepted as valid data.
        for _ in 0..8 {
            let _ = state.read(&mut r);
        }
        assert!(r.overrun());
    }
}
