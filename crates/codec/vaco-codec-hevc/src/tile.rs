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
    num_rows: u32,
    ctbs_y: u32,
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
            num_rows: tiles.num_rows,
            ctbs_y,
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

    /// Return the number of tile-local CABAC substreams in a full picture.
    ///
    /// With WPP disabled, one substream covers each tile in tile-scan order;
    /// its byte range is separated from the next one by an entry-point offset.
    #[must_use]
    pub fn tile_substream_count(&self) -> Option<u32> {
        self.num_columns.checked_mul(self.num_rows)
    }

    /// Return the full-picture entry-point offset count for this layout.
    ///
    /// This is the §7.4.7.1 count for a picture-wide slice: tiles-only uses
    /// one substream per tile, while tiles plus WPP uses one per tile column
    /// and CTB row. A slice may carry fewer offsets when it covers less than
    /// the full picture; this method deliberately does not guess that range.
    #[must_use]
    pub fn entry_point_offset_count(&self, wpp: bool) -> Option<u32> {
        let substreams = if wpp {
            self.num_columns.checked_mul(self.ctbs_y)?
        } else {
            self.tile_substream_count()?
        };
        substreams.checked_sub(1)
    }

    /// Convert tiles-only entry-point lengths into tile-scan byte ranges.
    ///
    /// `data_len` and each entry-point length are measured from the first byte
    /// after the aligned slice header, as required by §7.4.7.1. The returned
    /// ranges are half-open and ordered by tile ID. This helper deliberately
    /// validates only the partition; it does not initialize CABAC or expose
    /// samples to the still-refused tile decoder path.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::InvalidData`] when the count is not one less
    /// than the tile count, an offset overflows, or the ranges exceed the
    /// supplied slice-data length.
    pub fn tile_substream_byte_ranges(
        &self,
        data_len: usize,
        entry_point_offsets: &[u32],
    ) -> Result<Vec<(usize, usize)>> {
        let substreams = self.tile_substream_count().ok_or(Error::InvalidData(
            "vaco-codec-hevc: tile substream count overflow",
        ))?;
        let expected = usize::try_from(substreams.saturating_sub(1)).unwrap_or(usize::MAX);
        if entry_point_offsets.len() != expected {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: tile entry point count does not match tile substreams",
            ));
        }
        let mut ranges = Vec::new();
        let mut start = 0usize;
        for &offset in entry_point_offsets {
            let length = usize::try_from(offset).map_err(|_| {
                Error::InvalidData("vaco-codec-hevc: tile entry point offset too large")
            })?;
            if length == 0 {
                return Err(Error::InvalidData(
                    "vaco-codec-hevc: tile entry point offset is zero",
                ));
            }
            let end = start.checked_add(length).ok_or(Error::InvalidData(
                "vaco-codec-hevc: tile entry point offset overflow",
            ))?;
            if end > data_len {
                return Err(Error::InvalidData(
                    "vaco-codec-hevc: tile entry point exceeds slice data",
                ));
            }
            ranges.push((start, end));
            start = end;
        }
        ranges.push((start, data_len));
        Ok(ranges)
    }

    /// Return `(tile_id, tile-local raster CTB address)` for a CTB.
    ///
    /// The tile-local address is the address consumed by a tile substream,
    /// not the picture-raster `CtbAddrInRs`. It resets at every tile edge.
    #[must_use]
    pub fn tile_local_ctb_address(&self, x: u32, y: u32) -> Option<(u32, u32)> {
        let column = boundary_index(&self.column_boundaries, x)?;
        let row = boundary_index(&self.row_boundaries, y)?;
        let column_index = usize::try_from(column).ok()?;
        let row_index = usize::try_from(row).ok()?;
        let column_start = *self.column_boundaries.get(column_index)?;
        let row_start = *self.row_boundaries.get(row_index)?;
        let column_end = *self.column_boundaries.get(column_index.checked_add(1)?)?;
        let local_x = x.checked_sub(column_start)?;
        let local_y = y.checked_sub(row_start)?;
        let width = column_end.checked_sub(column_start)?;
        let local_address = local_y.checked_mul(width)?.checked_add(local_x)?;
        let tile_id = row.checked_mul(self.num_columns)?.checked_add(column)?;
        Some((tile_id, local_address))
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
