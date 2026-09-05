//! Tile CTB geometry from the PPS, ITU-T H.265 §§6.5 and 7.3.2.3.
//!
//! This is the decoder's first tile prerequisite, not tile decoding: it maps
//! the raster CTB coordinates to the rectangular tile that owns them and
//! exposes the neighbour availability change at a tile edge. The decoder
//! still refuses tile pictures before CABAC reconstruction or filtering, so a
//! caller cannot accidentally turn this geometry into cross-tile pixels.

use vaco_core::{Error, Result};
use vaco_parse_hevc::pps::Tiles;

/// The rectangular CTB partition signalled by one PPS.
///
/// Boundaries are stored as half-open CTB coordinates. A picture with two
/// uniform columns over eight CTBs therefore has column boundaries `[0, 4, 8]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileLayout {
    column_boundaries: Vec<u32>,
    row_boundaries: Vec<u32>,
    num_columns: u32,
    loop_filter_across_tiles: bool,
}

impl TileLayout {
    /// Derive the CTB partition from the PPS's `tiles_enabled_flag` payload.
    ///
    /// The explicit widths/heights contain all but the final column/row, as
    /// specified by §7.3.2.3; the final extent is the remaining picture. The
    /// dimensions are CTB counts (`PicWidthInCtbsY` and `PicHeightInCtbsY`),
    /// not luma sample dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::InvalidData`] when the partition has no
    /// columns/rows, exceeds the picture, or its explicit extents do not form
    /// a complete positive partition.
    pub fn from_pps(tiles: &Tiles, ctbs_x: u32, ctbs_y: u32) -> Result<Self> {
        let column_boundaries = boundaries(
            tiles.num_columns,
            ctbs_x,
            tiles.uniform_spacing,
            &tiles.column_widths,
            "vaco-codec-hevc: invalid tile column partition",
        )?;
        let row_boundaries = boundaries(
            tiles.num_rows,
            ctbs_y,
            tiles.uniform_spacing,
            &tiles.row_heights,
            "vaco-codec-hevc: invalid tile row partition",
        )?;
        Ok(Self {
            column_boundaries,
            row_boundaries,
            num_columns: tiles.num_columns,
            loop_filter_across_tiles: tiles.loop_filter_across_tiles,
        })
    }

    /// Return the row-major tile index owning CTB `(x, y)`, or `None` outside
    /// the picture. This is the tile ID used by §6.5's neighbour rules.
    #[must_use]
    pub fn tile_at(&self, x: u32, y: u32) -> Option<u32> {
        let column = boundary_index(&self.column_boundaries, x)?;
        let row = boundary_index(&self.row_boundaries, y)?;
        row.checked_mul(self.num_columns)?.checked_add(column)
    }

    /// Whether two CTBs share one tile, with out-of-picture coordinates
    /// treated as unavailable rather than as a tile match.
    #[must_use]
    pub fn same_tile(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> bool {
        self.tile_at(x0, y0)
            .zip(self.tile_at(x1, y1))
            .is_some_and(|(left, right)| left == right)
    }

    /// Whether the CTB immediately to the left is an available spatial
    /// neighbour under §6.5. A tile edge makes it unavailable.
    #[must_use]
    pub fn left_available(&self, x: u32, y: u32) -> bool {
        x > 0 && self.same_tile(x - 1, y, x, y)
    }

    /// Whether the CTB immediately above is an available spatial neighbour
    /// under §6.5. A tile edge makes it unavailable.
    #[must_use]
    pub fn above_available(&self, x: u32, y: u32) -> bool {
        y > 0 && self.same_tile(x, y - 1, x, y)
    }

    /// Return the PPS `loop_filter_across_tiles_enabled_flag` value.
    #[must_use]
    pub const fn loop_filter_across_tiles(&self) -> bool {
        self.loop_filter_across_tiles
    }
}

#[allow(
    clippy::integer_division,
    reason = "uniform tile boundaries are the floor divisions required by H.265 §6.5"
)]
fn boundaries(
    count: u32,
    total: u32,
    uniform: bool,
    explicit: &[u32],
    error: &'static str,
) -> Result<Vec<u32>> {
    if count == 0 || total == 0 || count > total {
        return Err(Error::InvalidData(error));
    }
    let mut result = Vec::new();
    result.push(0);
    if uniform {
        if !explicit.is_empty() {
            return Err(Error::InvalidData(error));
        }
        for index in 1..count {
            let boundary = (u64::from(index) * u64::from(total)) / u64::from(count);
            let boundary = u32::try_from(boundary).map_err(|_| Error::InvalidData(error))?;
            result.push(boundary);
        }
    } else {
        let expected = usize::try_from(count.saturating_sub(1)).unwrap_or(usize::MAX);
        if explicit.len() != expected {
            return Err(Error::InvalidData(error));
        }
        let mut sum = 0u32;
        for &width in explicit {
            if width == 0 {
                return Err(Error::InvalidData(error));
            }
            sum = sum.checked_add(width).ok_or(Error::InvalidData(error))?;
            if sum >= total {
                return Err(Error::InvalidData(error));
            }
            result.push(sum);
        }
    }
    result.push(total);
    Ok(result)
}

fn boundary_index(boundaries: &[u32], coordinate: u32) -> Option<u32> {
    boundaries
        .windows(2)
        .position(|window| {
            window
                .first()
                .zip(window.get(1))
                .is_some_and(|(&start, &end)| coordinate >= start && coordinate < end)
        })
        .and_then(|index| u32::try_from(index).ok())
}
