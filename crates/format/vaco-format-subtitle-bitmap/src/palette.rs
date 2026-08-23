//! An indexed colour table, up to 256 entries.
//!
//! The entry type is [`vaco_core::Rgba`], not a type of this crate's own:
//! `vaco-core::parse` already defines exactly this shape (`r`/`g`/`b`/`a` as
//! `u8`, plus a `TRANSPARENT` constant) for `-vf`-style colour options, and
//! `cargo xtask dup-check` (D19) is right to insist a second, independently
//! defined "four `u8`s" type is the same concept rather than a new one —
//! reusing it here is the fix, not a `DISTINCT` entry explaining the
//! duplication away.

use vaco_core::{Error, Result};

pub use vaco_core::Rgba;

/// An indexed colour table: every format in this family paints with a
/// palette of at most 256 entries rather than direct colour, which is the one
/// fact `dvbsub`, `sup` and `vobsub` share regardless of how differently each
/// one packs the pixels that index into it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Palette {
    entries: Vec<Rgba>,
}

impl Palette {
    /// No format in this family has ever declared more colours than a byte
    /// can index.
    pub const MAX_ENTRIES: usize = 256;

    /// # Errors
    /// [`Error::InvalidData`] if `entries.len() > `[`Self::MAX_ENTRIES`].
    pub fn new(entries: Vec<Rgba>) -> Result<Self> {
        if entries.len() > Self::MAX_ENTRIES {
            return Err(Error::InvalidData("palette: more than 256 entries"));
        }
        Ok(Self { entries })
    }

    /// The colour at `index`, or `None` past the end of this palette — a
    /// short palette (fewer than 256 entries stated) is not itself an error;
    /// a pixel indexing past it is a decoder-time concern, not this type's.
    #[must_use]
    pub fn get(&self, index: u8) -> Option<Rgba> {
        self.entries.get(usize::from(index)).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn entries(&self) -> &[Rgba] {
        &self.entries
    }

    /// Pack as 256 little-endian `0xAARRGGBB` words: the wire shape
    /// [`vaco_packet::PacketSideData::Palette`] carries.
    ///
    /// This is **this project's own convention**, modelled on the reference's
    /// `AV_PKT_DATA_PALETTE` layout (256 entries × 4 bytes, so 1024 bytes
    /// total) — not a measured fact, since nothing outside this workspace
    /// observes an in-memory packet's side data. Entries past
    /// [`Palette::len`] pack as [`Rgba::TRANSPARENT`].
    #[must_use]
    pub fn pack_argb32(&self) -> [u8; Self::MAX_ENTRIES * 4] {
        let mut out = [0u8; Self::MAX_ENTRIES * 4];
        for (i, colour) in self.entries.iter().enumerate().take(Self::MAX_ENTRIES) {
            let word = u32::from_be_bytes([colour.a, colour.r, colour.g, colour.b]);
            let bytes = word.to_le_bytes();
            let start = i.saturating_mul(4);
            if let Some(slot) = out.get_mut(start..start.saturating_add(4)) {
                slot.copy_from_slice(&bytes);
            }
        }
        out
    }

    /// The inverse of [`Palette::pack_argb32`], always exactly 256 entries.
    #[must_use]
    pub fn unpack_argb32(bytes: &[u8; Self::MAX_ENTRIES * 4]) -> Self {
        let mut entries = Vec::new();
        for chunk in bytes.chunks_exact(4) {
            let word_bytes: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
            let word = u32::from_le_bytes(word_bytes);
            let [a, r, g, b] = word.to_be_bytes();
            entries.push(Rgba::new(r, g, b, a));
        }
        Self { entries }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn more_than_256_entries_is_rejected() {
        let entries = vec![Rgba::TRANSPARENT; 257];
        assert!(Palette::new(entries).is_err());
    }

    #[test]
    fn get_past_the_stated_entries_is_none_not_a_panic() {
        let p = Palette::new(vec![Rgba::new(1, 2, 3, 4)]).unwrap();
        assert_eq!(p.get(0), Some(Rgba::new(1, 2, 3, 4)));
        assert_eq!(p.get(255), None);
    }

    #[test]
    fn pack_unpack_round_trips_the_stated_entries() {
        let mut entries = vec![Rgba::TRANSPARENT; 256];
        if let Some(e) = entries.get_mut(3) {
            *e = Rgba::new(10, 20, 30, 40);
        }
        let p = Palette::new(entries.clone()).unwrap();
        let packed = p.pack_argb32();
        let unpacked = Palette::unpack_argb32(&packed);
        assert_eq!(unpacked.entries(), entries.as_slice());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// [`Palette::pack_argb32`] / [`Palette::unpack_argb32`] is a
        /// bijection on exactly-256-entry palettes: any RGBA table survives
        /// the round trip byte-for-byte.
        #[test]
        fn pack_argb32_round_trips(colours in proptest::collection::vec(
            (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()),
            256..=256,
        )) {
            let entries: Vec<Rgba> = colours
                .into_iter()
                .map(|(r, g, b, a)| Rgba::new(r, g, b, a))
                .collect();
            let palette = Palette::new(entries.clone()).unwrap();
            let round_tripped = Palette::unpack_argb32(&palette.pack_argb32());
            prop_assert_eq!(round_tripped.entries(), entries.as_slice());
        }
    }
}
