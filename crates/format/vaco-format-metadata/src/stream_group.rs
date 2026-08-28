//! The stream-group model: a named set of streams that together form one
//! logical unit — a HEIF/AVIF tiled grid image, for instance — the way
//! [`vaco_format_core::Program`] groups streams into an MPEG-TS programme.
//!
//! Sketched in plan 18 §1.1 as part of `vaco-format-core`'s object model but
//! never landed there. It lives here instead of being invented as a
//! duplicate: nothing currently constructs one, and wiring
//! `Demuxer::stream_groups()` into the trait is a `vaco-format-core` change
//! this crate cannot make. A future change there can depend on this type
//! rather than defining its own.
//!
//! Field shapes follow [`vaco_format_core::Program`]'s established
//! conventions — plain `u32` stream indices, `Vec<(String, String)>`
//! metadata — rather than the plan's own sketch, which used types
//! (`StreamIndex`, `Metadata`) that were never built. One model, not two.

use vaco_core::Disposition;

/// Identifies one [`StreamGroup`] within a file, the way
/// [`vaco_format_core::Program::id`] identifies a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamGroupIndex(pub u32);

/// A named group of streams that together form one logical unit.
#[derive(Debug, Clone)]
pub struct StreamGroup {
    pub index: StreamGroupIndex,
    /// Container-native identifier, when the container states one.
    pub id: i64,
    /// Member streams, in container order.
    pub stream_indices: Vec<u32>,
    pub disposition: Disposition,
    pub metadata: Vec<(String, String)>,
    pub kind: StreamGroupKind,
}

impl StreamGroup {
    /// An empty group with no members yet, identified by `index`.
    #[must_use]
    pub fn new(index: StreamGroupIndex, kind: StreamGroupKind) -> Self {
        Self {
            index,
            id: 0,
            stream_indices: Vec::new(),
            disposition: Disposition::empty(),
            metadata: Vec::new(),
            kind,
        }
    }
}

/// What kind of logical unit a [`StreamGroup`] describes.
///
/// `#[non_exhaustive]`: plan 18 §1.1 also names an IAMF audio-element/mix
/// variant and an LCEVC-enhancement variant, neither of which has a
/// container crate producing one yet. Adding those later is additive; a
/// caller matching on this type already has to handle a kind it does not
/// recognise.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StreamGroupKind {
    /// ISO/IEC 23008-12 `grid` derived image item: a tiled still image, as
    /// HEIF/AVIF express a large picture as a grid of smaller coded tiles.
    TileGrid(TileGrid),
}

/// ISO/IEC 23008-12's `grid` item: how member tiles compose into one image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileGrid {
    pub tile_rows: u32,
    pub tile_columns: u32,
    pub output_width: u32,
    pub output_height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_group_has_no_members_and_no_disposition() {
        let g = StreamGroup::new(
            StreamGroupIndex(0),
            StreamGroupKind::TileGrid(TileGrid {
                tile_rows: 2,
                tile_columns: 2,
                output_width: 4096,
                output_height: 4096,
            }),
        );
        assert!(g.stream_indices.is_empty());
        assert!(g.metadata.is_empty());
        assert_eq!(g.disposition, Disposition::empty());
    }

    #[test]
    fn indices_are_ordered_and_comparable() {
        assert!(StreamGroupIndex(0) < StreamGroupIndex(1));
        assert_eq!(StreamGroupIndex(3), StreamGroupIndex(3));
    }
}
