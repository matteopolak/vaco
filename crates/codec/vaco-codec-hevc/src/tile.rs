//! Tile CTB geometry from the PPS, ITU-T H.265 §§6.5 and 7.3.2.3.
//!
//! These are the decoder's tile prerequisites, not tile decoding: they map
//! raster CTB coordinates to their rectangular tile, expose neighbour
//! availability at tile edges, and initialize independent CABAC state. The
//! decoder still refuses tile pictures before CTB reconstruction or filtering,
//! so a caller cannot accidentally turn this state into cross-tile pixels.

use crate::cabac_ctx::ContextBank;
use vaco_codec_cabac::{CabacDecoder, ContextModel};
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

/// CABAC arithmetic and context state initialized at one tile boundary.
///
/// This is an opaque state holder: CTB syntax consumption remains in the
/// decoder's refused tile path until tile-local reconstruction is proven.
#[derive(Debug)]
pub struct TileCabacState<'a> {
    cabac: CabacDecoder<'a>,
    contexts: ContextBank,
    first_ctb_split: Option<bool>,
    first_ctb_child_split: Option<bool>,
    first_ctb_grandchild_split: Option<bool>,
    first_ctb_leaf_nxn: Option<bool>,
    first_ctb_leaf_prev_intra: Option<bool>,
    first_ctb_leaf_mpm_prefix: Option<bool>,
    first_ctb_leaf_mpm_suffix: Option<bool>,
    first_ctb_leaf_luma_mode: Option<u8>,
    first_ctb_leaf_second_prev_intra: Option<bool>,
    first_ctb_leaf_second_mpm_prefix: Option<bool>,
    first_ctb_leaf_second_mpm_suffix: Option<bool>,
    first_ctb_leaf_second_luma_mode: Option<u8>,
}

impl TileCabacState<'_> {
    /// Return the §9.3.1.2 arithmetic interval after initialization.
    #[must_use]
    pub const fn range(&self) -> u32 {
        self.cabac.range()
    }

    /// Whether the arithmetic initializer or its nine-bit read was malformed.
    #[must_use]
    pub const fn malformed(&self) -> bool {
        self.cabac.malformed()
    }

    /// Return the first `split_cu_flag` context after §9.3.2.2 initialization.
    #[must_use]
    #[allow(
        clippy::indexing_slicing,
        reason = "the context bank's split_cu_flag table has a fixed first entry"
    )]
    pub const fn first_split_cu_context(&self) -> ContextModel {
        self.contexts.split_cu_flag[0]
    }

    /// Decode the first CTB's explicit `split_cu_flag`.
    ///
    /// At a tile's first full-size CTB, both spatial neighbours are
    /// unavailable, so §7.3.8.4 selects context index 0. This consumes exactly
    /// that one decision bin and leaves all later CTB syntax untouched.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::InvalidData`] when the supplied geometry is
    /// inconsistent, and [`vaco_core::Error::Unsupported`] when the flag is
    /// inferred rather than explicitly coded.
    pub fn decode_first_ctb_split_flag(
        &mut self,
        ctb_log2_size: u32,
        min_cb_log2_size: u32,
        ctb_in_bounds: bool,
    ) -> Result<bool> {
        if ctb_log2_size < min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile CTB dimensions are invalid",
            ));
        }
        if !ctb_in_bounds || ctb_log2_size == min_cb_log2_size {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile CTB split flag is inferred",
            ));
        }
        let context = self
            .contexts
            .split_cu_flag
            .first_mut()
            .ok_or(Error::InvalidData(
                "vaco-codec-hevc: split_cu_flag context is missing",
            ))?;
        let split = self.cabac.decode_decision(context) != 0;
        self.first_ctb_split = Some(split);
        Ok(split)
    }

    /// Decode the top-left child `split_cu_flag` after the CTB split.
    ///
    /// The child is the first coding-quadtree node inside the tile's first
    /// CTB, so its left and above neighbours are still unavailable and its
    /// context index is 0. The parent result must have been decoded by
    /// [`Self::decode_first_ctb_split_flag`] and be 1 before this read.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the parent split was not
    /// established or the child flag is inferred, and
    /// [`vaco_core::Error::InvalidData`] for inconsistent dimensions.
    pub fn decode_first_ctb_child_split_flag(
        &mut self,
        child_log2_size: u32,
        min_cb_log2_size: u32,
        child_in_bounds: bool,
    ) -> Result<bool> {
        if self.first_ctb_split != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile CTB child split parent is not set",
            ));
        }
        if child_log2_size < min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile CTB child dimensions are invalid",
            ));
        }
        if !child_in_bounds || child_log2_size == min_cb_log2_size {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile CTB child split flag is inferred",
            ));
        }
        let context = self
            .contexts
            .split_cu_flag
            .first_mut()
            .ok_or(Error::InvalidData(
                "vaco-codec-hevc: split_cu_flag context is missing",
            ))?;
        let split = self.cabac.decode_decision(context) != 0;
        self.first_ctb_child_split = Some(split);
        Ok(split)
    }

    /// Decode the top-left grandchild `split_cu_flag` after the child split.
    ///
    /// This is the first coding-quadtree node inside the first child, so its
    /// left and above neighbours remain unavailable and context index 0 still
    /// applies. The child result must have been decoded and be 1 before this
    /// read.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the child split was not
    /// established or the grandchild flag is inferred, and
    /// [`vaco_core::Error::InvalidData`] for inconsistent dimensions.
    pub fn decode_first_ctb_grandchild_split_flag(
        &mut self,
        grandchild_log2_size: u32,
        min_cb_log2_size: u32,
        grandchild_in_bounds: bool,
    ) -> Result<bool> {
        if self.first_ctb_child_split != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile CTB grandchild split parent is not set",
            ));
        }
        if grandchild_log2_size < min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile CTB grandchild dimensions are invalid",
            ));
        }
        if !grandchild_in_bounds || grandchild_log2_size == min_cb_log2_size {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile CTB grandchild split flag is inferred",
            ));
        }
        let context = self
            .contexts
            .split_cu_flag
            .first_mut()
            .ok_or(Error::InvalidData(
                "vaco-codec-hevc: split_cu_flag context is missing",
            ))?;
        let split = self.cabac.decode_decision(context) != 0;
        self.first_ctb_grandchild_split = Some(split);
        Ok(split)
    }

    /// Decode the top-left leaf's intra `part_mode` after a grandchild split.
    ///
    /// The grandchild's top-left child is a minimum-size coding unit, so its
    /// `part_mode` is the single context-coded bin from §7.3.8.5. A zero bin
    /// means `PART_NxN`, and a one bin means `PART_2Nx2N`.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the grandchild did not
    /// split, and [`vaco_core::Error::InvalidData`] for inconsistent leaf
    /// dimensions.
    pub fn decode_first_ctb_grandchild_leaf_part_mode(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<bool> {
        if self.first_ctb_grandchild_split != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile grandchild leaf parent is not split",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile grandchild leaf dimensions are invalid",
            ));
        }
        let context = self
            .contexts
            .part_size
            .first_mut()
            .ok_or(Error::InvalidData(
                "vaco-codec-hevc: part_mode context is missing",
            ))?;
        let is_nxn = self.cabac.decode_decision(context) == 0;
        self.first_ctb_leaf_nxn = Some(is_nxn);
        Ok(is_nxn)
    }

    /// Decode the first 4x4 PU's `prev_intra_luma_pred_flag`.
    ///
    /// A minimum-size intra leaf with `PART_NxN` has four PUs. This consumes
    /// only the first flag, before any MPM or rem-mode syntax, as required by
    /// §7.3.8.5. The flag uses the first initialized context entry, matching
    /// the decoder's intra-CU path.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the leaf was not
    /// established as `PART_NxN`, and [`vaco_core::Error::InvalidData`] for
    /// inconsistent leaf dimensions.
    pub fn decode_first_ctb_leaf_prev_intra_luma_pred_flag(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<bool> {
        if self.first_ctb_leaf_nxn != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile leaf has no NxN intra PU flags",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let context = self
            .contexts
            .prev_intra_luma_pred
            .first_mut()
            .ok_or(Error::InvalidData(
                "vaco-codec-hevc: prev_intra_luma_pred context is missing",
            ))?;
        let prev = self.cabac.decode_decision(context) != 0;
        self.first_ctb_leaf_prev_intra = Some(prev);
        Ok(prev)
    }

    /// Decode the first bypass-coded prefix bin of the first PU's `mpm_idx`.
    ///
    /// This bin follows a `prev_intra_luma_pred_flag` of 1 in §7.3.8.5. A zero
    /// selects MPM index 0; a one requires one more bypass bin, which remains
    /// unconsumed here.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the preceding flag was
    /// not established as 1, and [`vaco_core::Error::InvalidData`] for
    /// inconsistent leaf dimensions.
    pub fn decode_first_ctb_leaf_mpm_idx_prefix(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<bool> {
        if self.first_ctb_leaf_prev_intra != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile leaf has no explicit mpm_idx",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let prefix = self.cabac.decode_bypass() != 0;
        self.first_ctb_leaf_mpm_prefix = Some(prefix);
        Ok(prefix)
    }

    /// Decode the second bypass-coded prefix bin of the first PU's `mpm_idx`.
    ///
    /// The first prefix bin must be 1 before this bin exists. A zero selects
    /// MPM index 1 and a one selects index 2; the resolved mode itself remains
    /// unconsumed.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the first prefix bin was
    /// not established as 1, and [`vaco_core::Error::InvalidData`] for
    /// inconsistent leaf dimensions.
    pub fn decode_first_ctb_leaf_mpm_idx_suffix(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<bool> {
        if self.first_ctb_leaf_mpm_prefix != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile leaf has no mpm_idx suffix bin",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let suffix = self.cabac.decode_bypass() != 0;
        self.first_ctb_leaf_mpm_suffix = Some(suffix);
        Ok(suffix)
    }

    /// Resolve the first PU's measured MPM index to its luma mode.
    ///
    /// At the top-left PU of the first CTB in a tile, both neighbours are
    /// unavailable, so §8.4.2 derives `[PLANAR, DC, VER]`. This bounded step
    /// supports only the fixture's measured index 1 (`DC`); another index is
    /// refused before any later intra syntax is consumed.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the measured MPM index
    /// is not 1, and [`vaco_core::Error::InvalidData`] for inconsistent leaf
    /// dimensions.
    pub fn resolve_first_ctb_leaf_luma_mode(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<u8> {
        if self.first_ctb_leaf_mpm_suffix != Some(false) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile leaf MPM index is not the measured mode",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let mode =
            *crate::intra_mode::mpm_list(crate::intra_mode::DC_IDX, crate::intra_mode::DC_IDX)
                .get(1)
                .ok_or(Error::InvalidData(
                    "vaco-codec-hevc: first tile MPM list is incomplete",
                ))?;
        self.first_ctb_leaf_luma_mode = Some(mode);
        Ok(mode)
    }

    /// Decode the second 4x4 PU's `prev_intra_luma_pred_flag`.
    ///
    /// The first PU's MPM index and luma mode must be resolved before the
    /// second PU is visited in §7.3.8.5 order. This consumes only that next
    /// context-coded flag; its MPM/rem-mode syntax remains unconsumed.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the first PU mode was
    /// not the measured `INTRA_DC`, and [`vaco_core::Error::InvalidData`] for
    /// inconsistent leaf dimensions.
    pub fn decode_first_ctb_leaf_second_prev_intra_luma_pred_flag(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<bool> {
        if self.first_ctb_leaf_luma_mode != Some(crate::intra_mode::DC_IDX) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile leaf first PU mode is not resolved",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let context = self
            .contexts
            .prev_intra_luma_pred
            .first_mut()
            .ok_or(Error::InvalidData(
                "vaco-codec-hevc: prev_intra_luma_pred context is missing",
            ))?;
        let prev = self.cabac.decode_decision(context) != 0;
        self.first_ctb_leaf_second_prev_intra = Some(prev);
        Ok(prev)
    }

    /// Decode the second PU's first bypass-coded `mpm_idx` prefix bin.
    ///
    /// A `prev_intra_luma_pred_flag` of 1 selects this truncated-unary form
    /// in §7.3.8.5. A zero prefix selects MPM index 0; a one requires one more
    /// bypass bin, which remains unconsumed here.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the second PU has no
    /// explicit MPM index, and [`vaco_core::Error::InvalidData`] for
    /// inconsistent leaf dimensions.
    pub fn decode_first_ctb_leaf_second_mpm_idx_prefix(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<bool> {
        if self.first_ctb_leaf_second_prev_intra != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile second PU has no explicit mpm_idx",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let prefix = self.cabac.decode_bypass() != 0;
        self.first_ctb_leaf_second_mpm_prefix = Some(prefix);
        Ok(prefix)
    }

    /// Decode the second PU's second bypass-coded `mpm_idx` prefix bin.
    ///
    /// The first prefix bin must be 1 before this bin exists in §7.3.8.5. A
    /// zero selects MPM index 1 and a one selects index 2; mode resolution
    /// remains unconsumed here.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] when the first prefix bin was
    /// not established as 1, and [`vaco_core::Error::InvalidData`] for
    /// inconsistent leaf dimensions.
    pub fn decode_first_ctb_leaf_second_mpm_idx_suffix(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<bool> {
        if self.first_ctb_leaf_second_mpm_prefix != Some(true) {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile second PU has no mpm_idx suffix bin",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let suffix = self.cabac.decode_bypass() != 0;
        self.first_ctb_leaf_second_mpm_suffix = Some(suffix);
        Ok(suffix)
    }

    /// Resolve the second PU's measured MPM index to its luma mode.
    ///
    /// The first PU to the left is the established `INTRA_DC` mode and the
    /// above neighbour is unavailable at the first CTB's top edge. Section
    /// 8.4.2 therefore derives `[PLANAR, DC, VER]`; this bounded step accepts
    /// only the fixture's measured MPM index 2 (`VER`). The following PU's
    /// syntax remains unconsumed.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::Unsupported`] unless the two preceding
    /// luma modes establish the measured index, and
    /// [`vaco_core::Error::InvalidData`] for inconsistent leaf dimensions.
    pub fn resolve_first_ctb_leaf_second_luma_mode(
        &mut self,
        leaf_log2_size: u32,
        min_cb_log2_size: u32,
    ) -> Result<u8> {
        if self.first_ctb_leaf_second_mpm_suffix != Some(true)
            || self.first_ctb_leaf_luma_mode != Some(crate::intra_mode::DC_IDX)
        {
            return Err(Error::Unsupported(
                "vaco-codec-hevc: first tile second PU MPM index is not the measured mode",
            ));
        }
        if leaf_log2_size != min_cb_log2_size {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile leaf dimensions are invalid",
            ));
        }
        let mode = *crate::intra_mode::mpm_list(
            crate::intra_mode::DC_IDX,
            crate::intra_mode::DC_IDX,
        )
        .get(2)
        .ok_or(Error::InvalidData(
            "vaco-codec-hevc: first tile MPM list is incomplete",
        ))?;
        self.first_ctb_leaf_second_luma_mode = Some(mode);
        Ok(mode)
    }
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

    /// Borrow the first tile's coded substream for CABAC initialization.
    ///
    /// The returned bytes begin immediately after the aligned slice header;
    /// `CabacDecoder::new` can therefore apply §9.3.1.2 directly. No bins are
    /// consumed here, and an empty first range is refused before any decoder
    /// state is constructed.
    ///
    /// # Errors
    ///
    /// Propagates the range validation from
    /// [`Self::tile_substream_byte_ranges`] and rejects an empty first range.
    pub fn first_tile_substream<'a>(
        &self,
        data: &'a [u8],
        entry_point_offsets: &[u32],
    ) -> Result<&'a [u8]> {
        let ranges = self.tile_substream_byte_ranges(data.len(), entry_point_offsets)?;
        let (start, end) = ranges.first().copied().ok_or(Error::InvalidData(
            "vaco-codec-hevc: tile substream is missing",
        ))?;
        if start == end {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile substream is empty",
            ));
        }
        data.get(start..end).ok_or(Error::InvalidData(
            "vaco-codec-hevc: tile substream range is invalid",
        ))
    }

    /// Initialize CABAC state for the first tile substream.
    ///
    /// The input starts immediately after the aligned slice header, so
    /// [`CabacDecoder::new`] applies §9.3.1.2's arithmetic-state
    /// initialization to the tile-local range. This consumes only the
    /// initializer's mandatory nine bits; no CTB syntax bins are consumed
    /// until a tile reconstruction path is proven.
    ///
    /// # Errors
    ///
    /// Propagates tile range validation and rejects an empty or malformed
    /// first substream before returning a decoder.
    pub fn initialize_first_tile_cabac<'a>(
        &self,
        data: &'a [u8],
        entry_point_offsets: &[u32],
    ) -> Result<CabacDecoder<'a>> {
        let first = self.first_tile_substream(data, entry_point_offsets)?;
        let decoder = CabacDecoder::new(first);
        if decoder.malformed() {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: first tile CABAC initialization is malformed",
            ));
        }
        Ok(decoder)
    }

    /// Initialize one fresh CABAC engine for every tile substream.
    ///
    /// Each range is an independent §9.3.1.2 arithmetic state boundary. The
    /// initializer consumes only its mandatory nine-bit offset; no CTB syntax
    /// bins are consumed until tile reconstruction is proven.
    ///
    /// # Errors
    ///
    /// Propagates tile range validation and rejects an empty or malformed
    /// substream before returning any tile decoder state.
    pub fn initialize_tile_cabac_substreams<'a>(
        &self,
        data: &'a [u8],
        entry_point_offsets: &[u32],
    ) -> Result<Vec<CabacDecoder<'a>>> {
        let ranges = self.tile_substream_byte_ranges(data.len(), entry_point_offsets)?;
        let mut decoders = Vec::new();
        for (start, end) in ranges {
            if start == end {
                return Err(Error::InvalidData(
                    "vaco-codec-hevc: tile substream is empty",
                ));
            }
            let bytes = data.get(start..end).ok_or(Error::InvalidData(
                "vaco-codec-hevc: tile substream range is invalid",
            ))?;
            let decoder = CabacDecoder::new(bytes);
            if decoder.malformed() {
                return Err(Error::InvalidData(
                    "vaco-codec-hevc: tile CABAC initialization is malformed",
                ));
            }
            decoders.push(decoder);
        }
        Ok(decoders)
    }

    /// Initialize arithmetic and syntax contexts for every tile substream.
    ///
    /// `slice_qp` is `SliceQPY` from §7.4.7.1. Every tile receives a fresh
    /// §9.3.2.2 context bank alongside its independent §9.3.1.2 arithmetic
    /// state. This consumes no CTB syntax bins.
    ///
    /// # Errors
    ///
    /// Returns [`vaco_core::Error::InvalidData`] for a QP outside 0..=51,
    /// propagates tile range validation, and rejects an empty or malformed
    /// substream before returning any tile state.
    pub fn initialize_tile_cabac_states<'a>(
        &self,
        data: &'a [u8],
        entry_point_offsets: &[u32],
        slice_qp: i8,
    ) -> Result<Vec<TileCabacState<'a>>> {
        if !(0..=51).contains(&slice_qp) {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: tile slice QP is out of range",
            ));
        }
        let ranges = self.tile_substream_byte_ranges(data.len(), entry_point_offsets)?;
        let mut states = Vec::new();
        for (start, end) in ranges {
            if start == end {
                return Err(Error::InvalidData(
                    "vaco-codec-hevc: tile substream is empty",
                ));
            }
            let bytes = data.get(start..end).ok_or(Error::InvalidData(
                "vaco-codec-hevc: tile substream range is invalid",
            ))?;
            let cabac = CabacDecoder::new(bytes);
            if cabac.malformed() {
                return Err(Error::InvalidData(
                    "vaco-codec-hevc: tile CABAC initialization is malformed",
                ));
            }
            states.push(TileCabacState {
                cabac,
                contexts: ContextBank::new(slice_qp),
                first_ctb_split: None,
                first_ctb_child_split: None,
                first_ctb_grandchild_split: None,
                first_ctb_leaf_nxn: None,
                first_ctb_leaf_prev_intra: None,
                first_ctb_leaf_mpm_prefix: None,
                first_ctb_leaf_mpm_suffix: None,
                first_ctb_leaf_luma_mode: None,
                first_ctb_leaf_second_prev_intra: None,
                first_ctb_leaf_second_mpm_prefix: None,
                first_ctb_leaf_second_mpm_suffix: None,
                first_ctb_leaf_second_luma_mode: None,
            });
        }
        Ok(states)
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

    /// Whether CTB `(x, y)` starts a fresh tile-local CABAC substream.
    ///
    /// For tiles-only slices, §9.3.1.2 initializes arithmetic decoding at the
    /// first CTB of each tile substream; later CTBs continue that tile's state.
    /// This reports the state boundary without constructing a context bank or
    /// permitting the still-refused reconstruction path to consume bytes.
    #[must_use]
    pub fn starts_new_tile_cabac_substream(&self, x: u32, y: u32) -> bool {
        self.tile_local_ctb_address(x, y)
            .is_some_and(|(_, local_address)| local_address == 0)
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
