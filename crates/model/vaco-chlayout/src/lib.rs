//! Channel layouts.
//!
//! Models the modern three-way distinction rather than a bare bitmask: a mask
//! cannot express more than 63 channels, cannot express ambisonics, and cannot
//! express "8 channels of unknown position", all of which occur in real files.

use smallvec::SmallVec;

/// A single speaker position, or an ambisonic component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Channel {
    FrontLeft,
    FrontRight,
    FrontCenter,
    LowFrequency,
    BackLeft,
    BackRight,
    SideLeft,
    SideRight,
    // ... generated
    /// Present in the stream but carrying no defined position.
    Unknown,
    /// A gap: the slot exists but carries nothing.
    Unused,
}

/// How a layout's channel positions are described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOrder {
    /// Count is known, positions are not.
    Unspecified,
    /// Positions given by a bitmask in the conventional order.
    Native,
    /// An explicit per-index map, permitting gaps and arbitrary order.
    Custom(SmallVec<[Channel; 8]>),
    /// ACN-ordered ambisonic components, optionally with non-diegetic extras.
    Ambisonic {
        order: u8,
        extra: SmallVec<[Channel; 2]>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelLayout {
    pub channels: u32,
    pub order: ChannelOrder,
    mask: u64,
}

impl ChannelLayout {
    pub const MONO: Self = Self {
        channels: 1,
        order: ChannelOrder::Native,
        mask: 0x4,
    };
    pub const STEREO: Self = Self {
        channels: 2,
        order: ChannelOrder::Native,
        mask: 0x3,
    };

    /// Parse a CLI-facing layout string such as `5.1` or `FL+FR`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let _ = name;
        todo!("P0-03 freeze: named layouts, then `+`-joined channel ids")
    }

    /// The canonical name, if this layout has one.
    #[must_use]
    pub fn name(&self) -> Option<&'static str> {
        todo!("P0-03 freeze: reverse lookup over the named-layout table")
    }

    #[must_use]
    pub fn channel_at(&self, index: u32) -> Option<Channel> {
        let _ = index;
        todo!("P0-03 freeze: dispatch on `order`")
    }
}
