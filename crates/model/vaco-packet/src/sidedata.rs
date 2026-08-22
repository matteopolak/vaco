//! Packet side data lookup.

use crate::{Packet, PacketSideData};

/// Discriminant of [`PacketSideData`], for lookup and removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PacketSideDataKind {
    Palette,
    NewExtradata,
    DisplayMatrix,
    SkipSamples,
}

impl PacketSideData {
    /// Which kind of side data this is.
    #[must_use]
    pub const fn kind(&self) -> PacketSideDataKind {
        match self {
            Self::Palette(_) => PacketSideDataKind::Palette,
            Self::NewExtradata(_) => PacketSideDataKind::NewExtradata,
            Self::DisplayMatrix(_) => PacketSideDataKind::DisplayMatrix,
            Self::SkipSamples { .. } => PacketSideDataKind::SkipSamples,
        }
    }
}

impl Packet {
    /// The entry of `kind`, if the packet carries one.
    ///
    /// Linear scan: packets carry 0-2 entries.
    #[must_use]
    pub fn side_data(&self, kind: PacketSideDataKind) -> Option<&PacketSideData> {
        self.side_data.iter().find(|d| d.kind() == kind)
    }

    /// Attach `data`, replacing any existing entry of the same kind.
    pub fn set_side_data(&mut self, data: PacketSideData) {
        let kind = data.kind();
        if let Some(slot) = self.side_data.iter_mut().find(|d| d.kind() == kind) {
            *slot = data;
        } else {
            self.side_data.push(data);
        }
    }

    /// Detach and return the entry of `kind`.
    pub fn remove_side_data(&mut self, kind: PacketSideDataKind) -> Option<PacketSideData> {
        let at = self.side_data.iter().position(|d| d.kind() == kind)?;
        Some(self.side_data.remove(at))
    }
}
