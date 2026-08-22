//! Read-and-validate, with the bound spelled out at the call site.
//!
//! # What this is for
//!
//! `ue(v)` is where untrusted bitstreams get interesting. Three separate things
//! can go wrong and they need three separate answers:
//!
//! | Problem | Answer |
//! |---|---|
//! | An absurd prefix (`00000…`) | rejected in constant time by the reader's 31-zero cap — never looped on |
//! | A plausible codeword with an implausible *value* (`num_ref_frames = 3_000_000`) | an explicit `max` at the read site |
//! | A *loop* of plausible reads that never ends (`while (more_rbsp_data())`) | fuel, from [`vaco_limits::Budget`] |
//!
//! The first is structural and free. The second is [`crate::GolombDecode`]'s
//! `*_max` family. The third is what this module adds: a wrapper that charges
//! one unit of fuel per syntax element, so a syntax loop driven by attacker
//! input terminates against a budget rather than against patience.
//!
//! # Why a wrapper and not an argument
//!
//! Same reasoning as `vaco-limits` itself: an `Option<&mut Budget>` parameter
//! gets passed `None`. [`BoundedGolomb`] cannot be constructed without a budget,
//! so a parser that holds one is charging for every element it reads, and a
//! parser that does not simply uses [`GolombDecode`](crate::GolombDecode)
//! directly and is visibly doing so.
//!
//! ```
//! use vaco_bitstream::BitReader;
//! use vaco_limits::{Budget, Limits};
//! use vaco_codec_golomb::BoundedGolomb;
//!
//! let data = [0b1010_1100u8, 0b0110_0000];
//! let mut reader = BitReader::new(&data);
//! let mut budget = Budget::new(Limits::strict());
//! let mut g = BoundedGolomb::new(&mut reader, &mut budget);
//!
//! assert_eq!(g.ue_v(1)?, 0);      // '1'
//! assert_eq!(g.se_v(-8, 8)?, 1);  // '010'
//! # Ok::<(), vaco_core::Error>(())
//! ```

use vaco_bitstream::BitReader;
use vaco_core::Error;
use vaco_limits::Budget;

use crate::GolombDecode;
use crate::tables::{ChromaArrayType, MbPartPredMode};

/// Fuel charged per syntax element read.
///
/// One unit per element rather than per bit: the thing being bounded is the
/// number of *decisions* a malformed stream can make us take, and a fuel unit
/// is defined by `vaco-limits` as roughly one such decision. `Limits::strict`
/// allows 2^26 of them, which is far more syntax elements than any real frame
/// contains and far fewer than an unbounded loop wants.
const FUEL_PER_ELEMENT: u64 = 1;

/// A [`BitReader`] paired with a [`Budget`], where every read is bounded twice:
/// by an explicit value range and by fuel.
///
/// Errors are [`vaco_core::Error`] rather than
/// [`BitstreamError`](vaco_bitstream::BitstreamError) because a budget
/// exhaustion and a bitstream overrun have to be reportable through one `?`, and
/// `vaco-core`'s taxonomy is the one both already convert into.
#[derive(Debug)]
pub struct BoundedGolomb<'r, 'a, 'b> {
    reader: &'r mut BitReader<'a>,
    budget: &'b mut Budget,
}

impl<'r, 'a, 'b> BoundedGolomb<'r, 'a, 'b> {
    /// Pair a reader with a budget.
    pub fn new(reader: &'r mut BitReader<'a>, budget: &'b mut Budget) -> Self {
        Self { reader, budget }
    }

    /// The underlying reader, for the fields that are plain fixed-width bits.
    pub fn reader(&mut self) -> &mut BitReader<'a> {
        self.reader
    }

    /// The underlying budget.
    pub fn budget(&mut self) -> &mut Budget {
        self.budget
    }

    /// `ue(v)`, at most `max`, charged one unit of fuel.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] past the end, [`Error::InvalidData`] on a
    /// malformed codeword or a value above `max`, and whatever
    /// [`Budget::consume_fuel`] returns when the budget is spent.
    pub fn ue_v(&mut self, max: u32) -> Result<u32, Error> {
        self.budget.consume_fuel(FUEL_PER_ELEMENT)?;
        Ok(self.reader.ue_v_max(max)?)
    }

    /// `se(v)`, within `min..=max`, charged one unit of fuel.
    ///
    /// # Errors
    ///
    /// As [`ue_v`](BoundedGolomb::ue_v).
    pub fn se_v(&mut self, min: i32, max: i32) -> Result<i32, Error> {
        self.budget.consume_fuel(FUEL_PER_ELEMENT)?;
        Ok(self.reader.se_v_range(min, max)?)
    }

    /// `te(v)` with ceiling `c_max`, charged one unit of fuel.
    ///
    /// # Errors
    ///
    /// As [`ue_v`](BoundedGolomb::ue_v).
    pub fn te_v(&mut self, c_max: u32) -> Result<u32, Error> {
        self.budget.consume_fuel(FUEL_PER_ELEMENT)?;
        Ok(self.reader.te_v_checked(c_max)?)
    }

    /// `me(v)`, charged one unit of fuel.
    ///
    /// # Errors
    ///
    /// As [`ue_v`](BoundedGolomb::ue_v).
    pub fn me_v(&mut self, chroma: ChromaArrayType, pred: MbPartPredMode) -> Result<u32, Error> {
        self.budget.consume_fuel(FUEL_PER_ELEMENT)?;
        Ok(self.reader.me_v_checked(chroma, pred)?)
    }

    /// Order-`k` `ue(v)`, at most `max`, charged one unit of fuel.
    ///
    /// # Errors
    ///
    /// As [`ue_v`](BoundedGolomb::ue_v).
    pub fn ue_k(&mut self, k: u32, max: u32) -> Result<u32, Error> {
        self.budget.consume_fuel(FUEL_PER_ELEMENT)?;
        Ok(self.reader.ue_k_max(k, max)?)
    }

    /// `n` fixed-width bits, charged one unit of fuel.
    ///
    /// Here so a parser can stay inside the bounded wrapper for a whole syntax
    /// structure rather than reaching back out to the raw reader for the `u(n)`
    /// fields and silently escaping the fuel accounting.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] if fewer than `n` bits remain, plus the budget
    /// errors.
    pub fn u(&mut self, n: u32) -> Result<u32, Error> {
        self.budget.consume_fuel(FUEL_PER_ELEMENT)?;
        Ok(self.reader.try_get(n)?)
    }

    /// A counted loop over `ue(v)` elements, with the count itself bounded.
    ///
    /// The shape that goes wrong most often in real parsers: read a count, then
    /// read that many things. Charging the count against fuel *before* the loop
    /// starts means a declared count of four billion fails immediately instead
    /// of after four billion reads.
    ///
    /// # Errors
    ///
    /// As [`ue_v`](BoundedGolomb::ue_v); the count is rejected if it exceeds
    /// `max_count`.
    pub fn ue_v_counted(&mut self, max_count: u32, max_value: u32) -> Result<Vec<u32>, Error> {
        let count = self.ue_v(max_count)?;
        // Charge the whole loop up front, so an implausible count is refused
        // before any of it runs.
        self.budget.consume_fuel(u64::from(count))?;
        let mut buf = self.budget.alloc::<u32>(count as usize)?;
        buf.clear();
        for _ in 0..count {
            let v = self.reader.ue_v_max(max_value)?;
            buf.push(v);
        }
        Ok(buf)
    }

    /// The end-of-structure check, forwarded from the reader.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] if anything read past the logical end or a
    /// codeword was malformed.
    pub fn check(&self) -> Result<(), Error> {
        Ok(self.reader.check()?)
    }
}
