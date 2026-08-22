//! The arithmetic decoding engine, ITU-T H.264 clause 9.3.3.2.
//!
//! Four operations, and everything H.264 and H.265 do with CABAC is built from
//! them:
//!
//! | Spec | Here | Clause |
//! |---|---|---|
//! | `DecodeDecision` | [`CabacDecoder::decode_decision`] | 9.3.3.2.1 |
//! | `RenormD` | folded into all three | 9.3.3.2.2 |
//! | `DecodeBypass` | [`CabacDecoder::decode_bypass`] | 9.3.3.2.3 |
//! | `DecodeTerminate` | [`CabacDecoder::decode_terminate`] | 9.3.3.2.4 |
//!
//! # The invariant everything rests on
//!
//! **`ivlOffset < ivlCurrRange`, always.** The specification states it as a
//! constraint on conforming bitstreams (clause 9.3.1.2 forbids an initial
//! offset of 510 or 511); this implementation *enforces* it, because it is also
//! the bound that keeps `offset` from growing without limit.
//!
//! Consider `DecodeBypass` with the invariant violated: `offset ← 2·offset + 1`,
//! then subtract `range` if it is at least `range`. With `offset ≥ range` that
//! map is `x ↦ 2x + 1 − range`, whose fixed point is `range − 1`; start above it
//! and the value doubles away every bin until it overflows. Under
//! `overflow-checks` — which the fuzzing profile turns on deliberately — that is
//! a panic on a malformed bitstream, which is exactly the bug class this project
//! exists not to have.
//!
//! So [`CabacDecoder::new`] clamps a non-conforming initial offset and records
//! it in [`CabacDecoder::malformed`]. Every operation then provably preserves
//! `offset < range ≤ 510`, and nothing here can overflow whatever the input is.
//! `tests/spec.rs` asserts the invariant after every operation over
//! pseudorandom input, and the fuzz target asserts it too.
//!
//! # The shape of the inner loop was chosen by measurement, and the
//! # specification won
//!
//! Two "obvious" optimisations were implemented first and both are **slower**.
//! `benches/cabac.rs` keeps all four combinations so the result stays visible:
//!
//! | Decision | Renormalisation | skewed corpus | even corpus |
//! |---|---|---|---|
//! | **branchy (spec)** | **per-bit (spec)** | **15.7 µs** | **17.2 µs** |
//! | branchy | whole-width | 23.5 µs | 22.8 µs |
//! | branchless | per-bit | 21.7 µs | 24.0 µs |
//! | branchless | whole-width | 29.0 µs | 30.3 µs |
//!
//! (8192 bins, 64 contexts, Apple M5, min of 300 samples, three runs agreeing.)
//!
//! **Branchless decision costs ~35%.** The reasoning for it was that the
//! MPS/LPS outcome is a coin flip and therefore the worst thing to leave to a
//! predictor. That reasoning is wrong about the machine: replacing the branch
//! with masked selects makes every step of the bin depend on the previous one,
//! and the out-of-order engine can no longer start the next bin's table load
//! before the current one resolves. Speculating and occasionally being wrong
//! beats never speculating — and it beats it on the *even* corpus too, where
//! prediction is at its worst, which is what rules out "the benchmark was too
//! skewed" as an explanation.
//!
//! **Whole-width renormalisation costs ~45%.** `RenormD` is specified as a
//! per-bit loop, and computing the shift count from `leading_zeros` to do one
//! `BitReader::get(n)` looks strictly better. It is not: `get` with a *variable*
//! width carries an internal `if n == 0` early return, a `min(32)` clamp and a
//! variable `64 - n` shift, none of which survive when the width is the
//! constant 1. The loop body is cheaper than the thing meant to replace it, and
//! it runs zero or one times in the overwhelming majority of bins.
//!
//! So the engine below is written the way clause 9.3.3.2 writes it. The one
//! optimisation that is kept is the packed context byte, because it removes the
//! `pStateIdx == 0` test *without* removing a branch the processor was
//! predicting well — it turns a conditional into a different table index, which
//! is free.

use vaco_bitstream::{BitReader, Padded};

use crate::ContextModel;
use crate::tables::{LPS_RANGE, TRANS};

/// `ivlCurrRange` at initialisation, clause 9.3.1.2.
const INITIAL_RANGE: u32 = 510;
/// Bits of `ivlOffset` read at initialisation, clause 9.3.1.2.
const OFFSET_BITS: u32 = 9;
/// Ceiling on a bypass Exp-Golomb prefix. See [`CabacDecoder::decode_bypass_egk`].
const MAX_EGK_PREFIX: u32 = 32;

/// The CABAC arithmetic decoding engine.
///
/// Owns a [`BitReader`], so a codec can hand it a NAL payload and take the
/// reader back afterwards to finish the byte-aligned tail. The engine holds no
/// contexts: a codec's context set is its own array, and every decision names
/// the context it uses. That is what lets one engine serve H.264 and H.265,
/// whose context sets have nothing in common.
///
/// # Example
///
/// ```
/// use vaco_codec_cabac::{CabacDecoder, ContextModel};
///
/// let data = [0x1D, 0xA2, 0x7F, 0x00, 0x11, 0x22, 0x33, 0x44];
/// let mut d = CabacDecoder::new(&data);
/// let mut ctx = ContextModel::init_h264(20, 30, 26);
///
/// let bin = d.decode_decision(&mut ctx);
/// assert!(bin == 0 || bin == 1);
/// assert!(d.offset() < d.range());  // the engine invariant, always
/// ```
#[derive(Debug)]
pub struct CabacDecoder<'a> {
    /// `ivlCurrRange`, 2–510.
    range: u32,
    /// `ivlOffset`, always strictly below `range`.
    offset: u32,
    reader: BitReader<'a>,
    /// The bitstream violated a "shall" the engine depends on.
    malformed: bool,
    /// A `DecodeTerminate` has returned 1.
    terminated: bool,
}

impl<'a> CabacDecoder<'a> {
    /// Initialise from a byte slice, clause 9.3.1.2.
    ///
    /// `ivlCurrRange` is 510 and `ivlOffset` is the next nine bits. An offset of
    /// 510 or 511 is non-conforming; it is clamped and
    /// [`malformed`](CabacDecoder::malformed) is set.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self::from_reader(BitReader::new(data))
    }

    /// Initialise from a [`Padded`] buffer, which keeps the reader on its fast
    /// path 56 bytes past the logical end of the data.
    ///
    /// The form a decoder should use in steady state: a CABAC slice is
    /// thousands of renormalisations, and each one is a `BitReader::get`.
    #[must_use]
    pub fn new_padded(p: Padded<'a>) -> Self {
        Self::from_reader(BitReader::new_padded(p))
    }

    /// Initialise over a reader positioned at the first CABAC byte.
    ///
    /// For a slice whose header was parsed with the same reader: clause 9.3.1.2
    /// requires byte alignment before initialisation, so this aligns first.
    #[must_use]
    pub fn from_reader(mut reader: BitReader<'a>) -> Self {
        reader.align();
        let offset = reader.get(OFFSET_BITS);
        // Clause 9.3.1.2: "the value ... shall not be equal to 510 or 511".
        // Enforced rather than assumed — see the module documentation.
        let malformed = offset >= INITIAL_RANGE;
        Self {
            range: INITIAL_RANGE,
            offset: if malformed { INITIAL_RANGE - 1 } else { offset },
            reader,
            malformed,
            terminated: false,
        }
    }

    // ------------------------------------------------------------ inspection

    /// `ivlCurrRange`.
    #[must_use]
    #[inline]
    pub const fn range(&self) -> u32 {
        self.range
    }

    /// `ivlOffset`.
    #[must_use]
    #[inline]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Whether the bitstream broke a constraint the engine relies on, or the
    /// reader ran past the end.
    ///
    /// Decoding continues either way and returns deterministic values — past
    /// the end the reader supplies zeros — so a codec checks this once per
    /// syntax structure rather than after every bin, exactly as it does with
    /// [`BitReader::check`](vaco_bitstream::BitReader::check).
    #[must_use]
    #[inline]
    pub const fn malformed(&self) -> bool {
        self.malformed || self.reader.overrun()
    }

    /// The underlying reader, for the byte-aligned tail after
    /// `end_of_slice_flag`.
    #[must_use]
    pub fn into_reader(self) -> BitReader<'a> {
        self.reader
    }

    /// Borrow the reader — to ask how many bits are left, most usefully.
    #[must_use]
    pub const fn reader(&self) -> &BitReader<'a> {
        &self.reader
    }

    // -------------------------------------------------------------- the engine

    /// `RenormD`, clause 9.3.3.2.2, written as the specification writes it.
    ///
    /// Measured faster than the whole-width alternative by ~45%; see the module
    /// documentation for why. The loop runs at most seven times — `range` is at
    /// least 2 whenever this is called — and zero or one time for almost every
    /// bin.
    #[inline(always)]
    #[allow(
        clippy::inline_always,
        reason = "the innermost step of the innermost loop in video decoding; \
                  an out-of-line call here spills range, offset and the reader's \
                  cache out of registers"
    )]
    fn renorm(&mut self) {
        while self.range < 256 {
            self.range <<= 1;
            self.offset = (self.offset << 1) | self.reader.get_bit();
        }
    }

    /// `DecodeDecision`, clause 9.3.3.2.1 — one context-coded bin.
    ///
    /// # What is kept from the specification, and what is not
    ///
    /// The MPS/LPS branch is kept, because removing it measured 35% *slower* —
    /// see the module documentation. What is removed is the
    /// `if (pStateIdx == 0) valMPS = 1 - valMPS` inside the LPS arm: with the
    /// context packed as `(pStateIdx << 1) | valMPS`, that conditional is folded
    /// into the transition table at compile time, so the LPS arm is one indexed
    /// load rather than a load and a test. That trades a conditional for a
    /// different table offset, which costs nothing.
    ///
    /// Both table accesses are provably in bounds — `state` is a `u8` and `q` is
    /// masked to 0–3, so neither index can exceed 511 in a 512-entry table — so
    /// the `get`/`unwrap_or_default` form compiles to a plain load with no check
    /// and no panic path.
    #[inline(always)]
    #[allow(
        clippy::inline_always,
        reason = "measured: see benches/cabac.rs. The engine state must stay in \
                  registers across a run of bins or the whole design is pointless."
    )]
    pub fn decode_decision(&mut self, ctx: &mut ContextModel) -> u32 {
        let state = ctx.0 as usize;
        let q = ((self.range >> 6) & 3) as usize;
        let lps_range = u32::from(
            LPS_RANGE
                .get((state >> 1) * 4 + q)
                .copied()
                .unwrap_or_default(),
        );

        // range is 256..=510 here and lps_range is at most 240, so the MPS
        // sub-interval is at least 16 and the subtraction cannot underflow.
        self.range -= lps_range;

        let mps = state as u32 & 1;
        let bin = if self.offset >= self.range {
            // LPS: the offset falls in the upper sub-interval.
            self.offset -= self.range;
            self.range = lps_range;
            // The high half of TRANS carries the LPS successor *including* the
            // valMPS flip at pStateIdx 0.
            ctx.0 = TRANS.get(256 + state).copied().unwrap_or_default();
            1 - mps
        } else {
            ctx.0 = TRANS.get(state).copied().unwrap_or_default();
            mps
        };

        self.renorm();
        bin
    }

    /// `DecodeBypass`, clause 9.3.3.2.3 — one bin with no context and no
    /// adaptation.
    ///
    /// No renormalisation: bypass doubles `offset` instead of halving `range`,
    /// which is the whole point of it.
    #[inline(always)]
    #[allow(
        clippy::inline_always,
        reason = "as decode_decision; bypass bins arrive in runs and the state \
                  must stay in registers across them"
    )]
    pub fn decode_bypass(&mut self) -> u32 {
        self.offset = (self.offset << 1) | self.reader.get_bit();
        let bin = u32::from(self.offset >= self.range);
        self.offset -= self.range & 0u32.wrapping_sub(bin);
        bin
    }

    /// `n` bypass bins as one value, MSB first — the `FL` binarization of clause
    /// 9.3.3.1.1 and the tail of every `UEGk` suffix.
    ///
    /// # Measured: batching the reads does not pay
    ///
    /// The obvious optimisation is to pull all `n` bits out of the reader in one
    /// `get(n)`, since they do not depend on each other, leaving only the
    /// comparison chain serial. That was implemented and measured **1.2x
    /// slower** than the plain loop (`benches/cabac.rs`,
    /// `bypass_eight_at_a_time` against `bypass_one_at_a_time`): the serial
    /// comparison chain dominates completely, and the variable-width `get`
    /// carries the same internal branches that made whole-width renormalisation
    /// a loss.
    ///
    /// So this is a loop, and the value of the method is the interface — a
    /// fixed-length field read as one call — rather than a speed-up.
    ///
    /// `n` above 32 is clamped; it cannot panic.
    #[inline]
    pub fn decode_bypass_bits(&mut self, n: u32) -> u32 {
        let n = n.min(32);
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | self.decode_bypass();
        }
        value
    }

    /// `DecodeTerminate`, clause 9.3.3.2.4 — the `end_of_slice_flag` decision.
    ///
    /// Returns 1 when the stream terminates here. On 1 no renormalisation
    /// happens, which is what leaves the reader positioned for the byte-aligned
    /// tail; on 0 it renormalises by at most one bit and decoding continues.
    ///
    /// # The one place the engine deviates from the literal specification
    ///
    /// Clause 9.3.3.2.4 reduces `ivlCurrRange` by 2 *before* the comparison and
    /// leaves it reduced either way. On the terminating path that can leave
    /// `ivlOffset >= ivlCurrRange` — which is harmless in the specification,
    /// because decoding stops there and the variables are never read again.
    ///
    /// It is not harmless here. The engine's `offset < range` invariant is what
    /// bounds `offset`, and a caller that keeps decoding past a terminating bin
    /// — a malformed stream, or a codec bug — would then have `offset` doubling
    /// away every bypass bin until it overflows. So the reduction is **not
    /// committed** on the terminating path: `range` keeps its pre-call value,
    /// the invariant survives, and the returned bin is identical. Nothing a
    /// conforming decoder observes changes, because a conforming decoder stops.
    ///
    /// [`terminated`](CabacDecoder::terminated) records that it happened.
    #[inline]
    pub fn decode_terminate(&mut self) -> u32 {
        // range is at least 256 on entry, so this stays at or above 254.
        let reduced = self.range - 2;
        if self.offset >= reduced {
            self.terminated = true;
            return 1;
        }
        self.range = reduced;
        self.renorm();
        0
    }

    /// Whether a `DecodeTerminate` has returned 1.
    ///
    /// A codec should stop decoding bins at that point and finish the
    /// byte-aligned tail through [`into_reader`](CabacDecoder::into_reader).
    /// Continuing is safe — see [`decode_terminate`](CabacDecoder::decode_terminate)
    /// — but the bins are meaningless.
    #[must_use]
    #[inline]
    pub const fn terminated(&self) -> bool {
        self.terminated
    }

    // ---------------------------------------------------------- binarizations

    /// `U` / `TU` — unary and truncated unary, clause 9.3.3.1.
    ///
    /// Reads context-coded bins until a 0 arrives or `c_max` bins have been
    /// read, and returns how many 1 bins there were. `ctx` is the context used
    /// for **every** bin; a codec whose `ctxIdx` varies with the bin index
    /// drives [`decode_decision`](CabacDecoder::decode_decision) itself, since
    /// that derivation is per-syntax-element and belongs to the codec.
    ///
    /// `c_max` bounds the loop, and it must: a truncated-unary prefix with no
    /// ceiling is a hang waiting for a bitstream that never sends a 0.
    pub fn decode_tu(&mut self, ctx: &mut ContextModel, c_max: u32) -> u32 {
        let mut n = 0;
        while n < c_max {
            if self.decode_decision(ctx) == 0 {
                break;
            }
            n += 1;
        }
        n
    }

    /// The `EGk` suffix of clause 9.3.3.1.3, decoded entirely in bypass mode.
    ///
    /// The binarization is a run of 1 bins, a terminating 0, then `k + run`
    /// bits, and the value accumulates `2^k`, `2^(k+1)`, … over the run. It is
    /// the same code as order-`k` Exp-Golomb with the prefix bits inverted,
    /// which is why `vaco-codec-golomb` and this share a definition but not an
    /// implementation.
    ///
    /// # Bounding
    ///
    /// The prefix run is capped at 32. Nothing in the bitstream terminates it —
    /// an all-ones buffer is a well-formed run of a million 1 bins — so the
    /// ceiling is the only thing between this and a fuzz hang. Reaching it sets
    /// [`malformed`](CabacDecoder::malformed) and returns the value accumulated
    /// so far.
    pub fn decode_bypass_egk(&mut self, k: u32) -> u32 {
        let mut k = k.min(31);
        let mut value: u32 = 0;
        let mut run = 0;
        while self.decode_bypass() == 1 {
            value = value.saturating_add(1u32.checked_shl(k).unwrap_or(0));
            k += 1;
            run += 1;
            if run >= MAX_EGK_PREFIX || k > 31 {
                self.malformed = true;
                return value;
            }
        }
        value.saturating_add(self.decode_bypass_bits(k))
    }

    /// `UEGk`, clause 9.3.3.1.3 — a context-coded truncated-unary prefix, a
    /// bypass `EGk` suffix, and an optional bypass sign bit.
    ///
    /// This is the shape `mvd_lX` and `coeff_abs_level_minus1` use. `ctx` is
    /// used for the whole prefix; see [`decode_tu`](CabacDecoder::decode_tu) for
    /// why a per-bin `ctxIdx` is the codec's business.
    ///
    /// `signed_val_flag` selects whether a sign bit follows a non-zero value,
    /// matching the specification's parameter of the same name.
    pub fn decode_uegk(
        &mut self,
        ctx: &mut ContextModel,
        u_coff: u32,
        k: u32,
        signed_val_flag: bool,
    ) -> i32 {
        let prefix = self.decode_tu(ctx, u_coff);
        let mut value = prefix;
        if prefix >= u_coff {
            value = value.saturating_add(self.decode_bypass_egk(k));
        }
        // `value` cannot exceed i32::MAX after this, so both the negation and
        // the cast are exact.
        let magnitude = value.min(i32::MAX.cast_unsigned()).cast_signed();
        // Clause 9.3.3.1.3: the sign bit is present only for a non-zero value.
        if signed_val_flag && magnitude != 0 && self.decode_bypass() == 1 {
            return -magnitude;
        }
        magnitude
    }
}
