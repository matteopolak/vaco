//! The stream-group model: a named set of streams that together form one
//! logical unit — a HEIF/AVIF tiled grid image, for instance — the way
//! [`crate::Program`] groups streams into an MPEG-TS programme.
//!
//! Field shapes follow [`crate::Program`]'s conventions — plain `u32` stream
//! indices, `Vec<(String, String)>` metadata — so the two group-like objects
//! a demuxer can report read the same way.

use vaco_core::Disposition;

/// Identifies one [`StreamGroup`] within a file, the way
/// [`crate::Program::id`] identifies a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamGroupIndex(pub u32);

/// A named group of streams that together form one logical unit.
#[derive(Debug, Clone)]
pub struct StreamGroup {
    pub index: StreamGroupIndex,
    /// Container-native identifier, when the container states one — the
    /// `grid` item's `item_ID` for a HEIF tile grid.
    pub id: i64,
    /// Member streams, in container order. For a tile grid this is raster
    /// order, and [`TileGrid::tile_offsets`] is parallel to it.
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

/// ISO/IEC 23008-12 §6.6.2.3's `grid` item: how member tiles compose into
/// one image.
///
/// The tiles are laid out in raster order, each `coded_width / tile_columns`
/// by `coded_height / tile_rows` pixels (every tile of a grid has the same
/// size, §6.6.2.3.1), and the output image is the top-left
/// `output_width × output_height` of that canvas — a grid may be smaller
/// than the tiles that make it up, never larger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileGrid {
    pub tile_rows: u32,
    pub tile_columns: u32,
    /// The full tiled canvas: `tile_columns × tile width` by
    /// `tile_rows × tile height`.
    pub coded_width: u32,
    pub coded_height: u32,
    /// The presented image, cropped from the canvas at
    /// (`horizontal_offset`, `vertical_offset`).
    pub output_width: u32,
    pub output_height: u32,
    pub horizontal_offset: u32,
    pub vertical_offset: u32,
    /// Each member tile's `(horizontal, vertical)` pixel offset on the
    /// canvas, parallel to [`StreamGroup::stream_indices`].
    pub tile_offsets: Vec<(u32, u32)>,
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
                coded_width: 4096,
                coded_height: 4096,
                output_width: 4000,
                output_height: 3000,
                horizontal_offset: 0,
                vertical_offset: 0,
                tile_offsets: Vec::new(),
            }),
        );
        assert!(g.stream_indices.is_empty());
        assert!(g.metadata.is_empty());
        assert_eq!(g.disposition, Disposition::empty());
    }
}
