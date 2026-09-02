//! The byte-oriented binary range *decoder* (RFC 9043 §3.8.1) and the
//! nonbinary `get_symbol` built on top of it (§3.8.1.2) -- decode only, since
//! this crate never produces a Configuration Record, only reads one.
//!
//! `Vaco-Spec-Ref: rfc9043 range coder, RFC 9043 §3.8.1.1 (Figures 14-20:
//! decode update equations, initial values, refill/get_rac pseudocode) and
//! §3.8.1.2 (Figure 21: get_symbol contexts)`.

/// Number of adaptive states a single [`get_symbol`](RangeDecoder::get_symbol)
/// context occupies: 1 (zero flag) + 10 (exponent unary) + 11 (sign) + 10
/// (mantissa). RFC 9043 §4.2 calls this `CONTEXT_SIZE`.
pub(crate) const CONTEXT_SIZE: usize = 32;

/// The default state transition table for `one_state` (RFC 9043 §3.8.1.5,
/// Figure 24): `zero_state[i] = 256 - one_state[256 - i]` is derived from
/// this at lookup time rather than stored twice.
///
/// `Vaco-Spec-Ref: rfc9043 RFC 9043 §3.8.1.5, Figure 24 (default_state_transition, 256 entries)`
#[rustfmt::skip]
const DEFAULT_STATE_TRANSITION: [u8; 256] = [
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

/// `one_state[i] = default_state_transition[i] + delta[i]`, RFC 9043 Figure
/// 22, applied on top of the default table. `zero_state[i] = 256 -
/// one_state[256 - i]`, Figure 23.
#[derive(Debug, Clone)]
pub(crate) struct StateTransition {
    one_state: [u8; 256],
}

impl StateTransition {
    /// The default table, no delta applied.
    ///
    /// The *only* table this crate ever decodes against: `Parameters()`'s
    /// own fields (including `state_transition_delta` itself, when present)
    /// are always read with the fixed default table per RFC 9043 §4.2 --
    /// `state_transition_delta` only ever changes the table slice *data*
    /// decodes with later, which this crate never reads. So unlike
    /// `vaco-codec-ffv1`'s own copy of this type, there is no `with_delta`
    /// constructor here: [`skip_state_transition_delta`] consumes and
    /// discards those 255 symbols to keep later fields correctly aligned,
    /// without ever needing the custom table they would produce.
    #[must_use]
    pub(crate) const fn default_table() -> Self {
        Self {
            one_state: DEFAULT_STATE_TRANSITION,
        }
    }

    #[inline]
    fn one_state(&self, s: u8) -> u8 {
        self.one_state.get(usize::from(s)).copied().unwrap_or(0)
    }

    #[inline]
    fn zero_state(&self, s: u8) -> u8 {
        let idx = 256u16 - u16::from(s);
        let one = self.one_state(idx as u8);
        (256u16 - u16::from(one)) as u8
    }
}

/// A reusable set of [`CONTEXT_SIZE`] adaptive states, all starting at 128 —
/// what `get_symbol` (Figure 21) addresses as `state + offset`.
pub(crate) type SymbolStates = [u8; CONTEXT_SIZE];

/// Fresh states for one context: every byte 128, per RFC 9043's "all set to
/// 128" initial-state convention.
#[must_use]
pub(crate) const fn fresh_states() -> SymbolStates {
    [128; CONTEXT_SIZE]
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

/// The byte-oriented range decoder (RFC 9043 §3.8.1.1, Figures 11-20).
#[derive(Debug, Clone)]
pub(crate) struct RangeDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    low: u32,
    range: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Start decoding `data` from its first byte (Figure 11-13's `R_0`,
    /// `L_0`, `j_0`).
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

    /// Decode a nonbinary value (Figure 21's `get_symbol`).
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

/// Read and discard `state_transition_delta` (RFC 9043 §4.2.4): 255 signed
/// symbols in their own fresh context, using the fixed default table --
/// consumed only to keep the decoder's byte position correctly advanced for
/// every field `Parameters()` reads after it, never to reconstruct the
/// custom table itself (this crate has no frame data to apply it to).
pub(crate) fn skip_state_transition_delta(dec: &mut RangeDecoder<'_>, table: &StateTransition) {
    let mut states = fresh_states();
    for _ in 0..255 {
        let _ = dec.get_symbol(&mut states, table, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_freshly_constructed_decoder_starts_at_byte_two() {
        let dec = RangeDecoder::new(&[1, 2, 3, 4]);
        assert_eq!(dec.pos, 2);
    }

    #[test]
    fn get_symbol_is_deterministic_for_the_same_input() {
        let table = StateTransition::default_table();
        let run = || {
            let mut dec = RangeDecoder::new(&[0x4a, 0x91, 0x3c, 0x7e, 0x02, 0xff, 0x10, 0x88]);
            let mut states = fresh_states();
            (0..4)
                .map(|_| dec.get_symbol(&mut states, &table, false))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }
}
