//! The byte-oriented binary range coder (RFC 9043 §3.8.1) and the nonbinary
//! `get_symbol`/`put_symbol` built on top of it (§3.8.1.2).
//!
//! `Vaco-Spec-Ref: rfc9043 range coder, RFC 9043 §3.8.1.1 (Figures 14-20:
//! decode update equations, initial values, refill/get_rac pseudocode) and
//! §3.8.1.2 (Figure 21: get_symbol contexts)`.
//!
//! # The one subtlety
//!
//! The RFC gives *decode* pseudocode only ("Encoding is defined as any
//! process that produces a decodable bytestream", §3.8.1.1.1) — the encoder
//! here is this crate's own construction, not transcribed from anywhere. It
//! is the standard carry-propagating byte-renormalising range encoder (the
//! technique independently described by G. Nigel N. Martin and used by many
//! unrelated range coders since): while [`RangeDecoder`]'s `low` never needs
//! more than 16 bits (an invariant of the *decode* equations: `L_i < R_i <=
//! 0xFF00` always), the *encoder*'s running `low` genuinely can carry past a
//! byte that has already been tentatively emitted, so [`RangeEncoder`] tracks
//! pending bytes in `cache`/`cache_size` and propagates the carry into them
//! when it resolves. This is standard textbook arithmetic-coding technique,
//! not anything specific to a particular encoder's own bit-exact output,
//! which this crate is explicitly not required to reproduce byte-for-byte
//! (see the crate's top-level docs).

/// Number of adaptive states a single [`get_symbol`]/[`put_symbol`] context
/// occupies: 1 (zero flag) + 10 (exponent unary) + 11 (sign) + 10 (mantissa).
/// RFC 9043 §4.2 calls this `CONTEXT_SIZE`.
pub(crate) const CONTEXT_SIZE: usize = 32;

/// The default state transition table for `one_state` (RFC 9043 §3.8.1.5,
/// Figure 24): `zero_state[i] = 256 - one_state[256 - i]` is derived from
/// this at lookup time rather than stored twice.
///
/// `Vaco-Spec-Ref: rfc9043 RFC 9043 §3.8.1.5, Figure 24 (default_state_transition, 256 entries)`
#[rustfmt::skip]
pub(crate) const DEFAULT_STATE_TRANSITION: [u8; 256] = [
      0,  0,  0,  0,  0,  0,  0,  0, 20, 21, 22, 23, 24, 25, 26, 27,
     28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 37, 38, 39, 40, 41, 42,
     43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 56, 57,
     58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
     74, 75, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88,
     89, 90, 91, 92, 93, 94, 94, 95, 96, 97, 98, 99,100,101,102,103,
    104,105,106,107,108,109,110,111,112,113,114,114,115,116,117,118,
    119,120,121,122,123,124,125,126,127,128,129,130,131,132,133,133,
    134,135,136,137,138,139,140,141,142,143,144,145,146,147,148,149,
    150,151,152,152,153,154,155,156,157,158,159,160,161,162,163,164,
    165,166,167,168,169,170,171,171,172,173,174,175,176,177,178,179,
    180,181,182,183,184,185,186,187,188,189,190,190,191,192,194,194,
    195,196,197,198,199,200,201,202,202,204,205,206,207,208,209,209,
    210,211,212,213,215,215,216,217,218,219,220,220,222,223,224,225,
    226,227,227,229,229,230,231,232,234,234,235,236,237,238,239,240,
    241,242,243,244,245,246,247,248,248,  0,  0,  0,  0,  0,  0,  0,
];

/// `one_state[i] = default_state_transition[i] + delta[i]`, RFC 9043 Figure 22.
/// `zero_state[i] = 256 - one_state[256 - i]`, Figure 23.
///
/// `table` is 256 entries; `delta` is 0 unless `coder_type > 1` (custom
/// tables), which this crate's own encoder never emits — decode still applies
/// whatever delta a real bitstream carries.
#[derive(Debug, Clone)]
pub(crate) struct StateTransition {
    one_state: [u8; 256],
}

impl StateTransition {
    /// The default table, no delta applied.
    #[must_use]
    pub(crate) const fn default_table() -> Self {
        Self {
            one_state: DEFAULT_STATE_TRANSITION,
        }
    }

    /// Apply `state_transition_delta` (RFC 9043 §4.2.4) on top of the default
    /// table: `one_state[i] = default[i] + delta[i]`, wrapping mod 256 the way
    /// an 8-bit add naturally does.
    #[must_use]
    pub(crate) fn with_delta(delta: &[i32; 255]) -> Self {
        let mut one_state = DEFAULT_STATE_TRANSITION;
        for (i, d) in delta.iter().enumerate() {
            // delta[i] corresponds to state_transition_delta[i+1] (the loop in
            // Parameters() runs i = 1..256).
            if let Some(slot) = one_state.get_mut(i + 1) {
                *slot = (i32::from(*slot) + *d).rem_euclid(256) as u8;
            }
        }
        Self { one_state }
    }

    #[inline]
    fn one_state(&self, s: u8) -> u8 {
        self.one_state.get(usize::from(s)).copied().unwrap_or(0)
    }

    #[inline]
    fn zero_state(&self, s: u8) -> u8 {
        // zero_state[s] = 256 - one_state[256 - s], computed in u16 so
        // `256 - s` and the outer `256 - x` never underflow a u8.
        let idx = 256u16 - u16::from(s);
        let one = self.one_state(idx as u8);
        (256u16 - u16::from(one)) as u8
    }
}

/// A reusable set of [`CONTEXT_SIZE`] adaptive states, all starting at 128 —
/// what `get_symbol`/`put_symbol` (Figure 21) address as `state + offset`.
pub(crate) type SymbolStates = [u8; CONTEXT_SIZE];

/// Fresh states for one context: every byte 128, per RFC 9043's "all set to
/// 128" initial-state convention (repeated for every named state array in
/// the spec: `QuantizationTableSet`, `Parameters`, `SliceHeader`, the
/// `keyframe` flag, and per-pixel contexts on a keyframe).
#[must_use]
pub(crate) const fn fresh_states() -> SymbolStates {
    [128; CONTEXT_SIZE]
}

/// The fixed context RFC 9043 §3.8.1.1.1 uses for the Sentinel-mode
/// terminator symbol at the end of a range-coded region. Not part of any
/// adaptive per-pixel or per-header state array.
const TERMINATOR_STATE: u8 = 129;

/// The byte-oriented range decoder (RFC 9043 §3.8.1.1, Figures 11-20).
///
/// Tracks `j_i` (bytes consumed into `low`) exactly as the spec's pseudocode
/// does, via `pos`, so a caller that needs the Sentinel-mode handoff point
/// (RFC 9043 §3.8.1.1.1 — the switch from a range-coded `SliceHeader` to
/// Golomb-Rice-coded `SliceContent`) can read [`RangeDecoder::byte_pos`]
/// right after consuming the terminator symbol.
#[derive(Debug, Clone)]
pub(crate) struct RangeDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    low: u32,
    range: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Start decoding `data` from its first byte (Figure 11-13's `R_0`, `L_0`,
    /// `j_0`).
    #[must_use]
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let b0 = data.first().copied().unwrap_or(0);
        let b1 = data.get(1).copied().unwrap_or(0);
        Self {
            data,
            pos: 2,
            low: (u32::from(b0) << 8) | u32::from(b1),
            range: 0xFF00,
        }
    }

    #[inline]
    fn next_byte(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        // Past-the-end reads as zero, matching Closed mode (RFC 9043
        // §3.8.1.1.1): a caller that overruns gets a deterministic, non-
        // panicking decode rather than an index error.
        self.pos = self.pos.saturating_add(1);
        b
    }

    #[inline]
    fn refill(&mut self) {
        if self.range < 0x100 {
            self.range <<= 8;
            self.low = (self.low << 8) | u32::from(self.next_byte());
        }
    }

    /// Decode one binary value in context `state`, updating it in place
    /// (Figure 20's `get_rac`).
    #[inline]
    pub(crate) fn get_rac(&mut self, state: &mut u8, table: &StateTransition) -> bool {
        let range_off = (self.range * u32::from(*state)) >> 8;
        let sub = self.range - range_off;
        if self.low < sub {
            self.range = sub;
            *state = table.zero_state(*state);
            self.refill();
            false
        } else {
            self.low -= sub;
            self.range = range_off;
            *state = table.one_state(*state);
            self.refill();
            true
        }
    }

    /// Read and discard the Sentinel-mode terminator symbol (state 129,
    /// unadapting — RFC 9043 §3.8.1.1.1). Call this exactly once, right after
    /// a range-coded structure that a Golomb-coded region follows.
    pub(crate) fn read_terminator(&mut self, table: &StateTransition) {
        let mut state = TERMINATOR_STATE;
        let _ = self.get_rac(&mut state, table);
    }

    /// How many bytes of `data` have been consumed into `low` so far —
    /// RFC 9043's `j_i`. After [`RangeDecoder::read_terminator`] this is
    /// exactly where a following Golomb-coded region begins (§3.8.1.1.1: "the
    /// decoder will have read one byte beyond the end of the range-coded
    /// bytestream. This way the byte position of the end can be determined").
    #[must_use]
    pub(crate) const fn byte_pos(&self) -> usize {
        self.pos
    }

    /// Decode a nonbinary value (Figure 21's `get_symbol`).
    ///
    /// `states` must have at least [`CONTEXT_SIZE`] entries; a shorter slice
    /// decodes with the missing high states pinned at 128 rather than
    /// panicking, since `indexing_slicing` is denied project-wide.
    pub(crate) fn get_symbol(
        &mut self,
        states: &mut SymbolStates,
        table: &StateTransition,
        signed: bool,
    ) -> i32 {
        let mut s0 = states_get(states, 0);
        if self.get_rac(&mut s0, table) {
            states_set(states, 0, s0);
            return 0;
        }
        states_set(states, 0, s0);

        let mut e = 0usize;
        loop {
            let idx = 1 + e.min(9);
            let mut s = states_get(states, idx);
            let bit = self.get_rac(&mut s, table);
            states_set(states, idx, s);
            if !bit {
                break;
            }
            e += 1;
            // A malformed/adversarial stream cannot force this past a sane
            // bound: e indexes nothing past 9 (min(e,9)), so runaway growth
            // only wastes time, not memory. Still, cap it defensively.
            if e > 64 {
                break;
            }
        }

        let mut a: i64 = 1;
        for i in (0..e).rev() {
            let idx = 22 + i.min(9);
            let mut s = states_get(states, idx);
            let bit = self.get_rac(&mut s, table);
            states_set(states, idx, s);
            a = a * 2 + i64::from(bit);
        }

        if !signed {
            return a.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        }

        let idx = 11 + e.min(10);
        let mut s = states_get(states, idx);
        let negative = self.get_rac(&mut s, table);
        states_set(states, idx, s);
        let v = if negative { -a } else { a };
        v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }
}

#[inline]
fn states_get(states: &SymbolStates, i: usize) -> u8 {
    states.get(i).copied().unwrap_or(128)
}

#[inline]
fn states_set(states: &mut SymbolStates, i: usize, v: u8) {
    if let Some(slot) = states.get_mut(i) {
        *slot = v;
    }
}

/// The byte-oriented range encoder: this crate's own construction (see the
/// module docs) producing a bytestream [`RangeDecoder`] recovers exactly.
#[derive(Debug, Clone)]
pub(crate) struct RangeEncoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl RangeEncoder {
    /// A fresh encoder, matching [`RangeDecoder`]'s `R_0 = 0xFF00`.
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            low: 0,
            range: 0xFF00,
            cache: 0,
            cache_size: 0,
            out: Vec::new(),
        }
    }

    /// Carry-propagating byte shift-out. See the module docs for why this is
    /// needed even though [`RangeDecoder`]'s own `low` never overflows 16
    /// bits: growing `self.low` here by addition, before this truncates it
    /// back down, is exactly the step that can carry.
    fn shift_low(&mut self) {
        let carry = (self.low >> 16) as u8;
        if self.low < 0xFF00 || carry != 0 {
            if self.cache_size > 0 {
                self.out.push(self.cache.wrapping_add(carry));
                for _ in 1..self.cache_size {
                    self.out.push(0xFFu8.wrapping_add(carry));
                }
            }
            self.cache = ((self.low >> 8) & 0xFF) as u8;
            self.cache_size = 0;
        }
        self.cache_size += 1;
        // The shift must happen *within* the 16-bit window: the byte just
        // cached above is bits [8,16) of the old `low`, and it must be
        // discarded here, not re-widened — `(self.low as u16) << 8` in u16
        // arithmetic drops that top byte the way LZMA's `(UInt32)Low << 8`
        // drops its (32-bit-windowed) top byte. Widening the shift to u64
        // *before* truncating (an earlier version of this function did
        // exactly that) instead keeps growing `low` without bound, since
        // nothing ever throws the old top byte away.
        self.low = u64::from((self.low as u16).wrapping_shl(8));
    }

    #[inline]
    fn renormalize(&mut self) {
        let mut guard = 0;
        while self.range < 0x100 {
            self.shift_low();
            self.range <<= 8;
            guard += 1;
            if guard > 8 {
                // range collapsing to 0 would spin forever; this only fires
                // on a state value this encoder itself never produces
                // (0 or 256), so it is a defensive break, not a real path.
                break;
            }
        }
    }

    /// Encode one binary value in context `state`, updating it in place.
    #[inline]
    pub(crate) fn put_rac(&mut self, state: &mut u8, table: &StateTransition, bit: bool) {
        let range_off = (self.range * u32::from(*state)) >> 8;
        let sub = self.range - range_off;
        if bit {
            self.low += u64::from(sub);
            self.range = range_off;
            *state = table.one_state(*state);
        } else {
            self.range = sub;
            *state = table.zero_state(*state);
        }
        self.renormalize();
    }

    /// Write the Sentinel-mode terminator symbol (state 129). Its value is
    /// discarded by the decoder, so any bit works; `false` costs nothing extra.
    #[allow(
        dead_code,
        reason = "kept for API symmetry with RangeDecoder::read_terminator; this crate's own encoder never emits Sentinel mode today, so only the round-trip test in this module calls it"
    )]
    pub(crate) fn write_terminator(&mut self, table: &StateTransition) {
        let mut state = TERMINATOR_STATE;
        self.put_rac(&mut state, table, false);
    }

    /// Encode a nonbinary value (mirrors [`RangeDecoder::get_symbol`]).
    ///
    /// `value` is not restricted to what `signed` implies at the type level —
    /// the caller (sample-difference coding) always passes a value consistent
    /// with `signed`, and an inconsistent one just encodes `abs(value)`
    /// unsigned, which stays decodable.
    pub(crate) fn put_symbol(
        &mut self,
        states: &mut SymbolStates,
        table: &StateTransition,
        value: i32,
        signed: bool,
    ) {
        if value == 0 {
            let mut s0 = states_get(states, 0);
            self.put_rac(&mut s0, table, true);
            states_set(states, 0, s0);
            return;
        }
        let mut s0 = states_get(states, 0);
        self.put_rac(&mut s0, table, false);
        states_set(states, 0, s0);

        let a: i64 = i64::from(value).abs();
        // e = floor(log2(a)), so that a's top bit is bit e (a in [2^e, 2^(e+1)-1]).
        let e = 63 - a.leading_zeros() as usize;

        for i in 0..e {
            let idx = 1 + i.min(9);
            let mut s = states_get(states, idx);
            self.put_rac(&mut s, table, true);
            states_set(states, idx, s);
        }
        {
            let idx = 1 + e.min(9);
            let mut s = states_get(states, idx);
            self.put_rac(&mut s, table, false);
            states_set(states, idx, s);
        }

        for i in (0..e).rev() {
            let idx = 22 + i.min(9);
            let mut s = states_get(states, idx);
            let bit = ((a >> i) & 1) != 0;
            self.put_rac(&mut s, table, bit);
            states_set(states, idx, s);
        }

        if signed {
            let idx = 11 + e.min(10);
            let mut s = states_get(states, idx);
            self.put_rac(&mut s, table, value < 0);
            states_set(states, idx, s);
        }
    }

    /// Flush pending state and return the encoded bytes.
    ///
    /// Two more `shift_low` calls commit `cache` and the final partial byte,
    /// the standard range-encoder flush.
    #[must_use]
    pub(crate) fn finish(mut self) -> Vec<u8> {
        // Three calls, not two: the first flushes whatever was already
        // cached (with any pending carry resolved), and the other two drain
        // the two remaining bytes of a 16-bit-wide `low` (mirroring
        // `RangeDecoder`'s 16-bit `L_i`). Two calls leaves the *second* byte
        // of `low` sitting in `cache`, never written out — found by tracing
        // a 20-bit round trip byte-by-byte until encode and decode disagreed
        // exactly at the point that trailing byte would have been read.
        for _ in 0..3 {
            self.shift_low();
        }
        self.out
    }
}

impl Default for RangeEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `state_transition_delta[1..256]` (RFC 9043 §4.2, inside
/// `Parameters()`) when `coder_type > 1`. Never fails on its own — every
/// `i32` is a valid delta — so unlike the rest of this crate's bitstream
/// parsers, there is no `Result` to thread through.
pub(crate) fn read_state_transition_delta(
    dec: &mut RangeDecoder<'_>,
    table: &StateTransition,
) -> [i32; 255] {
    let mut states = fresh_states();
    let mut delta = [0i32; 255];
    for slot in &mut delta {
        *slot = dec.get_symbol(&mut states, table, true);
    }
    delta
}

/// Encode `state_transition_delta`. Kept for completeness even though this
/// crate's own encoder never sets `coder_type > 1` (see `params.rs`).
pub(crate) fn write_state_transition_delta(
    enc: &mut RangeEncoder,
    table: &StateTransition,
    delta: &[i32; 255],
) {
    let mut states = fresh_states();
    for v in delta {
        enc.put_symbol(&mut states, table, *v, true);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code exercising the coder, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;

    /// The primary correctness gate for everything else in this crate: encode
    /// a long stream of unsigned/signed symbols with a fixed context each,
    /// decode it back, and check exact recovery. Required by the crate's
    /// brief to pass *before* anything is built on top of the range coder.
    #[test]
    fn symbol_round_trip() {
        let table = StateTransition::default_table();
        let values: Vec<i32> = (0..2000)
            .map(|i| {
                let x = (i as u32).wrapping_mul(2_654_435_761_u32).cast_signed();
                // Keep values in a realistic residual-ish range.
                x % 300
            })
            .collect();

        let mut enc = RangeEncoder::new();
        let mut enc_states = fresh_states();
        for &v in &values {
            enc.put_symbol(&mut enc_states, &table, v, true);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let mut dec_states = fresh_states();
        for &v in &values {
            let got = dec.get_symbol(&mut dec_states, &table, true);
            assert_eq!(got, v);
        }
    }

    #[test]
    fn unsigned_symbol_round_trip() {
        let table = StateTransition::default_table();
        let values: Vec<i32> = (0..500).map(|i| (i * 97) % 5000).collect();

        let mut enc = RangeEncoder::new();
        let mut enc_states = fresh_states();
        for &v in &values {
            enc.put_symbol(&mut enc_states, &table, v, false);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let mut dec_states = fresh_states();
        for &v in &values {
            assert_eq!(dec.get_symbol(&mut dec_states, &table, false), v);
        }
    }

    /// Binary values through `get_rac`/`put_rac` directly, independent of the
    /// nonbinary layer built on top.
    #[test]
    fn binary_round_trip() {
        let table = StateTransition::default_table();
        let bits: Vec<bool> = (0..4000u32)
            .map(|i| i.wrapping_mul(2_654_435_761_u32) & 1 == 1)
            .collect();

        let mut enc = RangeEncoder::new();
        let mut state = 128u8;
        for &b in &bits {
            enc.put_rac(&mut state, &table, b);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let mut state = 128u8;
        for &b in &bits {
            assert_eq!(dec.get_rac(&mut state, &table), b);
        }
    }

    #[test]
    fn zero_and_edge_values_round_trip() {
        let table = StateTransition::default_table();
        let values = [
            0,
            1,
            -1,
            2,
            -2,
            255,
            -255,
            65535,
            -65535,
            i32::from(i16::MAX),
        ];

        let mut enc = RangeEncoder::new();
        let mut enc_states = fresh_states();
        for &v in &values {
            enc.put_symbol(&mut enc_states, &table, v, true);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let mut dec_states = fresh_states();
        for &v in &values {
            assert_eq!(dec.get_symbol(&mut dec_states, &table, true), v);
        }
    }

    #[test]
    fn terminator_round_trip_then_more_bytes() {
        // The Sentinel-mode handoff: encode a value, terminate, and confirm
        // byte_pos lands somewhere sane (used by the Golomb-mode handoff).
        let table = StateTransition::default_table();
        let mut enc = RangeEncoder::new();
        let mut states = fresh_states();
        enc.put_symbol(&mut states, &table, 42, true);
        enc.write_terminator(&table);
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes);
        let mut states = fresh_states();
        assert_eq!(dec.get_symbol(&mut states, &table, true), 42);
        dec.read_terminator(&table);
        assert!(dec.byte_pos() <= bytes.len() + 1);
        assert!(dec.byte_pos() >= 2);
    }
}
