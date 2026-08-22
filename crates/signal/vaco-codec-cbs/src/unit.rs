//! The unit list: what a coded bitstream looks like once framing is removed.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Where a unit came from, when it came from a buffer at all.
///
/// A bitstream filter that only reorders or drops units must be able to say
/// which bytes of the input each surviving unit occupied — that is what lets a
/// caller keep timestamps, `Packet::pos` and side data attached to the right
/// thing. A unit that was *synthesised* (an SPS inserted by a metadata filter)
/// has no such origin and says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitOrigin {
    /// Byte offset of the unit's payload within the fragment it was split from.
    pub offset: usize,
    /// Bytes of framing immediately before `offset` — a start code, or a length
    /// prefix. Kept so a re-assembly in the *same* framing can be byte-exact
    /// even when the source used three-byte start codes in some places and
    /// four-byte ones in others.
    pub framing_len: u8,
}

/// One coded unit: a NAL unit, an OBU, a JPEG marker segment.
///
/// `data` is the unit **as it appears in the bitstream**, framing removed but
/// escaping intact — the escaped byte string, not the de-escaped syntax. That
/// choice is what makes a fragment round-trip byte-exactly through a filter
/// that does not touch a given unit: de-escaping and re-escaping is not the
/// identity (`00 00 03` de-escapes to `00 00` and re-escapes to `00 00 03`, but
/// a trailing `00 00` that was *not* escaped becomes `00 00 03` on the way
/// back), so a layer that stored the de-escaped form would rewrite bytes it was
/// asked to leave alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CbsUnit {
    /// The codec's own unit type: `nal_unit_type`, `obu_type`, marker code.
    ///
    /// A `u32` rather than a codec enum because this type is the codec-agnostic
    /// half of the layer. The codec supplies the meaning; a filter that drops
    /// "types 39 and 40" needs only the number.
    pub unit_type: u32,
    /// The unit's bytes, framing removed, escaping intact.
    pub data: Vec<u8>,
    /// Where the unit came from, or `None` if it was synthesised.
    pub origin: Option<UnitOrigin>,
}

impl CbsUnit {
    /// A synthesised unit — one a filter is inserting rather than one it read.
    #[must_use]
    pub const fn new(unit_type: u32, data: Vec<u8>) -> Self {
        Self {
            unit_type,
            data,
            origin: None,
        }
    }

    /// A unit read from a fragment at a known offset.
    #[must_use]
    pub const fn from_source(unit_type: u32, data: Vec<u8>, origin: UnitOrigin) -> Self {
        Self {
            unit_type,
            data,
            origin: Some(origin),
        }
    }

    /// The unit's length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the unit carries no bytes at all, which no codec permits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// An ordered list of units — one access unit, one sample, or one extradata
/// blob.
///
/// This is the read/modify/write substrate. Everything a bitstream filter does
/// that does not require understanding a unit's *syntax* is an operation on
/// this list: drop units by type, insert a parameter set at the front, replace
/// one unit's bytes, re-assemble in a different framing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CbsFragment {
    units: Vec<CbsUnit>,
    /// Total payload bytes charged against the budget, so
    /// [`CbsFragment::release`] can give exactly that back.
    charged: u64,
}

impl CbsFragment {
    /// An empty fragment.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            units: Vec::new(),
            charged: 0,
        }
    }

    /// The units, in bitstream order.
    #[must_use]
    pub fn units(&self) -> &[CbsUnit] {
        &self.units
    }

    /// The units, mutably. Editing a unit's `data` in place is the cheapest way
    /// for a filter to rewrite one parameter set and leave the rest alone.
    pub fn units_mut(&mut self) -> &mut [CbsUnit] {
        &mut self.units
    }

    /// How many units the fragment holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.units.len()
    }

    /// Whether the fragment holds no units.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Append a unit, charging its bytes against the budget.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] when the fragment would exceed the budget.
    pub fn push(&mut self, unit: CbsUnit, budget: &mut Budget) -> Result<()> {
        let bytes = unit.data.len() as u64;
        budget.charge(bytes)?;
        budget.consume_fuel(1)?;
        self.charged = self.charged.saturating_add(bytes);
        self.units.push(unit);
        Ok(())
    }

    /// Insert a unit at `index`, or at the end if `index` is past it.
    ///
    /// Clamping rather than panicking is deliberate: `indexing_slicing` is
    /// denied workspace-wide precisely so an out-of-range index cannot become a
    /// crash, and "insert at the end" is the only sensible reading of an index
    /// past the end.
    ///
    /// # Errors
    ///
    /// As [`CbsFragment::push`].
    pub fn insert(&mut self, index: usize, unit: CbsUnit, budget: &mut Budget) -> Result<()> {
        let bytes = unit.data.len() as u64;
        budget.charge(bytes)?;
        budget.consume_fuel(1)?;
        self.charged = self.charged.saturating_add(bytes);
        self.units.insert(index.min(self.units.len()), unit);
        Ok(())
    }

    /// Remove the unit at `index` and return it, or `None` if there is none.
    pub fn remove(&mut self, index: usize) -> Option<CbsUnit> {
        if index >= self.units.len() {
            return None;
        }
        let unit = self.units.remove(index);
        self.charged = self.charged.saturating_sub(unit.data.len() as u64);
        Some(unit)
    }

    /// Keep only the units `keep` accepts — `filter_units`, exactly.
    pub fn retain<F: FnMut(&CbsUnit) -> bool>(&mut self, mut keep: F) {
        let mut freed = 0u64;
        self.units.retain(|u| {
            let k = keep(u);
            if !k {
                freed = freed.saturating_add(u.data.len() as u64);
            }
            k
        });
        self.charged = self.charged.saturating_sub(freed);
    }

    /// The index of the first unit of `unit_type`.
    #[must_use]
    pub fn position_of(&self, unit_type: u32) -> Option<usize> {
        self.units.iter().position(|u| u.unit_type == unit_type)
    }

    /// Every unit of `unit_type`, in order.
    pub fn units_of_type(&self, unit_type: u32) -> impl Iterator<Item = &CbsUnit> {
        self.units.iter().filter(move |u| u.unit_type == unit_type)
    }

    /// Replace the bytes of the unit at `index`, keeping its position and
    /// dropping its origin — the bytes are no longer the ones that were read.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if there is no such unit, [`Error::LimitExceeded`]
    /// if the new bytes exceed the budget.
    pub fn replace_data(&mut self, index: usize, data: Vec<u8>, budget: &mut Budget) -> Result<()> {
        let Some(unit) = self.units.get_mut(index) else {
            return Err(Error::InvalidData("no unit at that index"));
        };
        let old = unit.data.len() as u64;
        let new = data.len() as u64;
        if new > old {
            budget.charge(new - old)?;
        } else {
            budget.release(old - new);
        }
        self.charged = self.charged.saturating_add(new).saturating_sub(old);
        unit.data = data;
        unit.origin = None;
        Ok(())
    }

    /// Total payload bytes across every unit, framing excluded.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        self.units.iter().map(|u| u.data.len()).sum()
    }

    /// Give the budget back everything this fragment charged, and empty it.
    ///
    /// A fragment is a scratch buffer reused per packet; without this the
    /// budget counts every packet a long stream ever held.
    pub fn release(&mut self, budget: &mut Budget) {
        budget.release(self.charged);
        self.charged = 0;
        self.units.clear();
    }

    /// Empty the unit list without touching the budget — for the caller that is
    /// about to refill it and would rather keep the charge.
    pub fn clear(&mut self) {
        self.units.clear();
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    #[test]
    fn editing_operations_keep_order() {
        let mut b = budget();
        let mut f = CbsFragment::new();
        for t in [33u32, 34, 1, 1] {
            f.push(CbsUnit::new(t, vec![t as u8; 4]), &mut b).unwrap();
        }
        f.insert(0, CbsUnit::new(32, vec![0xAA]), &mut b).unwrap();
        assert_eq!(
            f.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
            [32, 33, 34, 1, 1]
        );
        f.retain(|u| u.unit_type != 34);
        assert_eq!(
            f.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
            [32, 33, 1, 1]
        );
        assert_eq!(f.position_of(33), Some(1));
        assert_eq!(f.units_of_type(1).count(), 2);
    }

    #[test]
    fn an_index_past_the_end_appends_rather_than_panicking() {
        let mut b = budget();
        let mut f = CbsFragment::new();
        f.insert(99, CbsUnit::new(1, vec![1]), &mut b).unwrap();
        assert_eq!(f.len(), 1);
        assert!(f.remove(99).is_none());
    }

    #[test]
    fn the_budget_is_returned_on_release() {
        let mut b = budget();
        let before = b.committed();
        let mut f = CbsFragment::new();
        for _ in 0..8 {
            f.push(CbsUnit::new(1, vec![0; 1024]), &mut b).unwrap();
        }
        assert!(b.committed() > before);
        f.release(&mut b);
        assert_eq!(b.committed(), before);
        assert!(f.is_empty());
    }

    #[test]
    fn replacing_data_adjusts_the_charge_both_ways() {
        let mut b = budget();
        let mut f = CbsFragment::new();
        f.push(CbsUnit::new(1, vec![0; 100]), &mut b).unwrap();
        let base = b.committed();
        f.replace_data(0, vec![0; 400], &mut b).unwrap();
        assert_eq!(b.committed(), base + 300);
        f.replace_data(0, vec![0; 10], &mut b).unwrap();
        assert_eq!(b.committed(), base - 90);
        assert!(f.units()[0].origin.is_none());
        assert!(f.replace_data(5, Vec::new(), &mut b).is_err());
    }

    #[test]
    fn a_hostile_unit_count_runs_out_of_fuel_rather_than_memory() {
        let mut b = Budget::new(Limits::tiny());
        let mut f = CbsFragment::new();
        let mut pushed = 0u32;
        for _ in 0..1_000_000 {
            if f.push(CbsUnit::new(1, vec![0; 64]), &mut b).is_err() {
                break;
            }
            pushed += 1;
        }
        assert!(pushed < 1_000_000, "the budget never stopped the loop");
    }
}
