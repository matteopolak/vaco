//! The seam a codec fills in, and the session that drives it.

use vaco_core::Result;
use vaco_limits::Budget;

use crate::unit::{CbsFragment, CbsUnit};

/// What one codec has to supply for its bitstream to become editable.
///
/// # Why framing is an associated type
///
/// H.26x has two framings (Annex B and length-prefixed) and AV1 has two
/// different ones (Annex B and low-overhead); JPEG has one. A single `Framing`
/// enum here would have to enumerate every codec's, which is exactly the
/// "the core knows about every component" shape the architecture forbids
/// (plan 10 §1.5). It is also a *layer* problem: the H.26x framing types live
/// in `vaco-format-nalu`, which sits above this crate, so naming them here is
/// not merely inelegant, it is impossible.
///
/// So the codec brings its own. This crate never inspects a `Framing` value; it
/// only carries it from the caller to [`CbsCodec::split`].
///
/// # Why splitting is the codec's job too
///
/// Splitting a buffer into units *is* the framing, and the framing is
/// codec-specific. What this crate owns is what happens **after** the split:
/// the unit list, its editing operations, and the typed read/modify/write
/// cycle. Every bitstream filter that does not understand a unit's syntax —
/// `filter_units`, `extract_extradata`, `*_mp4toannexb` — is expressible with
/// [`split`](CbsCodec::split), the [`CbsFragment`] operations, and
/// [`assemble`](CbsCodec::assemble) alone.
pub trait CbsCodec {
    /// The typed syntax of one unit, as a codec-specific enum: an SPS, a PPS,
    /// a sequence header OBU.
    type Content;

    /// How units are delimited in a buffer, as this codec spells it.
    type Framing: Copy + core::fmt::Debug;

    /// A short name, for diagnostics only.
    const NAME: &'static str;

    /// Split a buffer into units.
    ///
    /// The units' `data` must be the bytes as they appear in the buffer —
    /// framing removed, escaping intact — and their `origin` must be filled in,
    /// because that is what lets a caller map a surviving unit back to the
    /// bytes it came from.
    ///
    /// # Errors
    ///
    /// Whatever the codec's framing rejects, plus [`vaco_core::Error::LimitExceeded`]
    /// from the budget.
    fn split(
        &self,
        data: &[u8],
        framing: Self::Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()>;

    /// Write a fragment's units back out in `framing`, appending to `out`.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] when a unit cannot be expressed in the
    /// requested framing — a 70 KiB unit in two-byte length prefixes, say.
    fn assemble(
        &self,
        fragment: &CbsFragment,
        framing: Self::Framing,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()>;

    /// Decode one unit's syntax.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::Unsupported`] for a unit type this codec does not
    /// decode — which is not a failure of the stream, and a caller that is only
    /// editing parameter sets should skip it.
    fn read_unit(&mut self, unit: &CbsUnit, budget: &mut Budget) -> Result<Self::Content>;

    /// Encode syntax back into a unit's bytes — escaping included, so the
    /// result can be assigned straight to [`CbsUnit::data`].
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::Unsupported`] when the codec has no writer for this
    /// content, [`vaco_core::Error::InvalidData`] when a field is out of range.
    fn write_unit(
        &mut self,
        content: &Self::Content,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()>;

    /// The unit type `content` would be written as.
    ///
    /// Needed so an *inserted* unit lands with the right type without the
    /// caller having to know the codec's numbering.
    fn content_unit_type(&self, content: &Self::Content) -> u32;
}

/// A read-modify-write session over one codec's bitstream.
///
/// Holds the codec (parameter-set state included, which is why
/// [`CbsCodec::read_unit`] takes `&mut self`) and nothing else. The fragment
/// and the budget stay with the caller, because a bitstream filter reuses one
/// fragment across every packet and one budget across the whole stream.
///
/// ```text
///   bytes ──split──► CbsFragment ──read_unit──► Content
///                        │                         │
///                        │                      (edit)
///                        │                         │
///   bytes ◄─assemble── CbsFragment ◄─write_unit────┘
/// ```
#[derive(Debug)]
pub struct Cbs<C> {
    codec: C,
}

impl<C: CbsCodec> Cbs<C> {
    /// Start a session over `codec`.
    #[must_use]
    pub const fn new(codec: C) -> Self {
        Self { codec }
    }

    /// The codec, for the state it accumulates — a parameter-set store, say.
    #[must_use]
    pub const fn codec(&self) -> &C {
        &self.codec
    }

    /// The codec, mutably.
    pub const fn codec_mut(&mut self) -> &mut C {
        &mut self.codec
    }

    /// Split `data` into `fragment`, which is cleared first.
    ///
    /// # Errors
    ///
    /// As [`CbsCodec::split`].
    pub fn split(
        &mut self,
        data: &[u8],
        framing: C::Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        fragment.release(budget);
        self.codec.split(data, framing, fragment, budget)
    }

    /// Write `fragment` out in `framing`, appending to `out`.
    ///
    /// # Errors
    ///
    /// As [`CbsCodec::assemble`].
    pub fn assemble(
        &mut self,
        fragment: &CbsFragment,
        framing: C::Framing,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        self.codec.assemble(fragment, framing, out, budget)
    }

    /// Decode the unit at `index`.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] if there is no such unit, otherwise as
    /// [`CbsCodec::read_unit`].
    pub fn read_unit(
        &mut self,
        fragment: &CbsFragment,
        index: usize,
        budget: &mut Budget,
    ) -> Result<C::Content> {
        let Some(unit) = fragment.units().get(index) else {
            return Err(vaco_core::Error::InvalidData("no unit at that index"));
        };
        self.codec.read_unit(unit, budget)
    }

    /// Write `content` over the unit at `index`.
    ///
    /// The unit keeps its position and loses its origin, because its bytes are
    /// no longer the ones that were read.
    ///
    /// # Errors
    ///
    /// As [`CbsCodec::write_unit`] and [`CbsFragment::replace_data`].
    pub fn update_unit(
        &mut self,
        fragment: &mut CbsFragment,
        index: usize,
        content: &C::Content,
        budget: &mut Budget,
    ) -> Result<()> {
        if index >= fragment.len() {
            return Err(vaco_core::Error::InvalidData("no unit at that index"));
        }
        let mut buf = Vec::new();
        self.codec.write_unit(content, &mut buf, budget)?;
        let unit_type = self.codec.content_unit_type(content);
        fragment.replace_data(index, buf, budget)?;
        if let Some(u) = fragment.units_mut().get_mut(index) {
            u.unit_type = unit_type;
        }
        Ok(())
    }

    /// Insert `content` as a new unit at `index`.
    ///
    /// # Errors
    ///
    /// As [`CbsCodec::write_unit`] and [`CbsFragment::insert`].
    pub fn insert_unit(
        &mut self,
        fragment: &mut CbsFragment,
        index: usize,
        content: &C::Content,
        budget: &mut Budget,
    ) -> Result<()> {
        let mut buf = Vec::new();
        self.codec.write_unit(content, &mut buf, budget)?;
        let unit_type = self.codec.content_unit_type(content);
        fragment.insert(index, CbsUnit::new(unit_type, buf), budget)
    }

    /// Split, hand the fragment to `edit`, and assemble the result — the whole
    /// shape of a bitstream filter in one call.
    ///
    /// # Errors
    ///
    /// Whatever `edit` returns, or anything [`Cbs::split`] and
    /// [`Cbs::assemble`] return.
    pub fn transform<F>(
        &mut self,
        data: &[u8],
        framing_in: C::Framing,
        framing_out: C::Framing,
        out: &mut Vec<u8>,
        budget: &mut Budget,
        edit: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut Self, &mut CbsFragment, &mut Budget) -> Result<()>,
    {
        let mut fragment = CbsFragment::new();
        let r = (|| {
            self.split(data, framing_in, &mut fragment, budget)?;
            edit(self, &mut fragment, budget)?;
            self.assemble(&fragment, framing_out, out, budget)
        })();
        fragment.release(budget);
        r
    }
}
