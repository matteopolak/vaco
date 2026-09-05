//! Tile-picture scope boundary against a real HM 18.0 Annex-B stream.
//!
//! The fixture is one 512x64 IDR picture with two uniform tile columns. Its
//! PPS must carry `tiles_enabled_flag = 1`; the decoder must then refuse the
//! picture by the named scope error before tile-partitioned reconstruction.
//! The 1,813-byte HM 18.0 stream has SHA-256
//! `e7ede7ded9e07974097809c4bacda3492a6634a216a8d8b5c8920a3ceb3c91f2`.
//! A black-box `ffmpeg` decode is 49,152 visible yuv420p bytes with MD5
//! `6ccc33b0cd92240a275d30a05de031cc`.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code over one fixed, checked-in conformance-shaped fixture"
)]

use vaco_bitstream::BitReader;
use vaco_codec_cabac::ContextModel;
use vaco_codec_core::Decoder;
use vaco_codec_hevc::{HevcDecoder, TileLayout};
use vaco_core::Error;
use vaco_format_nalu::{Framing, RbspBuf, units};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_hevc::pps::Tiles;
use vaco_parse_hevc::{HevcNalHeader, HevcParser, SliceHeader};

const HEVC: &[u8] = include_bytes!("fixtures/tiles_512x64.hevc");

fn packet(bytes: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::default());
    Packet::from_slice(&mut budget, bytes).expect("fixture packet allocation")
}

#[test]
fn tile_pps_is_rejected_by_name_before_cabac_decode() {
    let limits = Limits::default();
    let mut parser = HevcParser::new(limits.clone());
    let info = parser
        .push_access_unit(HEVC, Framing::AnnexB)
        .expect("valid tile fixture parses");
    assert_eq!(info.picture_type, Some('I'));

    let pps = parser
        .parameter_sets()
        .get_pps(0)
        .expect("fixture references PPS zero");
    let tiles = pps.tiles.as_ref().expect("fixture PPS enables tiles");
    assert_eq!((tiles.num_columns, tiles.num_rows), (2, 1));
    assert!(tiles.uniform_spacing);

    let mut decoder = HevcDecoder::new(limits);
    let error = decoder
        .send_packet(Some(&packet(HEVC)))
        .expect_err("tile pictures must remain out of scope");
    assert!(matches!(
        error,
        Error::Unsupported("vaco-codec-hevc: tiles are not supported")
    ));
}

#[test]
fn tile_pps_maps_raster_ctbs_and_blocks_cross_tile_neighbours() {
    let limits = Limits::default();
    let mut parser = HevcParser::new(limits);
    parser
        .push_access_unit(HEVC, Framing::AnnexB)
        .expect("valid tile fixture parses");

    let pps = parser
        .parameter_sets()
        .get_pps(0)
        .expect("fixture references PPS zero");
    let tiles = pps.tiles.as_ref().expect("fixture PPS enables tiles");
    let sps = parser
        .parameter_sets()
        .get_sps(pps.sps_id)
        .expect("fixture references SPS zero");
    assert_eq!((sps.pic_width_in_ctbs(), sps.pic_height_in_ctbs()), (8, 1));
    let layout = TileLayout::from_pps(tiles, sps.pic_width_in_ctbs(), sps.pic_height_in_ctbs())
        .expect("fixture tile geometry is valid");

    assert_eq!(layout.tile_at(0, 0), Some(0));
    assert_eq!(layout.tile_at(3, 0), Some(0));
    assert_eq!(layout.tile_at(4, 0), Some(1));
    assert_eq!(layout.tile_at(7, 0), Some(1));
    assert_eq!(layout.tile_substream_count(), Some(2));
    assert_eq!(layout.entry_point_offset_count(false), Some(1));
    assert_eq!(layout.tile_local_ctb_address(0, 0), Some((0, 0)));
    assert_eq!(layout.tile_local_ctb_address(3, 0), Some((0, 3)));
    assert_eq!(layout.tile_local_ctb_address(4, 0), Some((1, 0)));
    assert_eq!(layout.tile_local_ctb_address(7, 0), Some((1, 3)));
    assert!(layout.starts_new_tile_cabac_substream(0, 0));
    assert!(!layout.starts_new_tile_cabac_substream(3, 0));
    assert!(layout.starts_new_tile_cabac_substream(4, 0));
    assert!(!layout.starts_new_tile_cabac_substream(7, 0));
    assert!(layout.left_available(1, 0));
    assert!(!layout.left_available(4, 0));
    assert!(layout.left_available(5, 0));
    assert!(!layout.above_available(0, 0));
}

#[test]
fn nonuniform_tile_widths_and_heights_leave_edges_unavailable() {
    let tiles = Tiles {
        num_columns: 3,
        num_rows: 2,
        uniform_spacing: false,
        column_widths: vec![1, 2],
        row_heights: vec![1],
        loop_filter_across_tiles: false,
    };
    let layout = TileLayout::from_pps(&tiles, 6, 3).expect("positive partition is valid");

    assert_eq!(layout.tile_at(0, 0), Some(0));
    assert_eq!(layout.tile_at(1, 0), Some(1));
    assert_eq!(layout.tile_at(3, 0), Some(2));
    assert_eq!(layout.tile_at(0, 1), Some(3));
    assert!(!layout.left_available(1, 0));
    assert!(layout.left_available(2, 0));
    assert!(!layout.above_available(0, 1));
    assert!(!layout.above_available(1, 1));
    assert!(layout.above_available(1, 2));
}

#[test]
fn malformed_first_tile_cabac_initializer_is_refused() {
    let tiles = Tiles {
        num_columns: 1,
        num_rows: 1,
        uniform_spacing: true,
        column_widths: Vec::new(),
        row_heights: Vec::new(),
        loop_filter_across_tiles: false,
    };
    let layout = TileLayout::from_pps(&tiles, 1, 1).expect("one tile is valid");
    let error = layout
        .initialize_first_tile_cabac(&[0], &[])
        .expect_err("a one-byte CABAC initializer overruns the nine-bit offset");
    assert!(matches!(
        error,
        Error::InvalidData("vaco-codec-hevc: first tile CABAC initialization is malformed")
    ));
}

#[test]
fn malformed_later_tile_cabac_initializer_is_refused() {
    let tiles = Tiles {
        num_columns: 2,
        num_rows: 1,
        uniform_spacing: true,
        column_widths: Vec::new(),
        row_heights: Vec::new(),
        loop_filter_across_tiles: false,
    };
    let layout = TileLayout::from_pps(&tiles, 2, 1).expect("two tiles are valid");
    let error = layout
        .initialize_tile_cabac_substreams(&[0, 0, 0xff, 0xe0], &[2])
        .expect_err("the second tile's 511 offset is forbidden");
    assert!(matches!(
        error,
        Error::InvalidData("vaco-codec-hevc: tile CABAC initialization is malformed")
    ));
}

#[test]
fn inferred_first_ctb_split_flag_is_refused() {
    let tiles = Tiles {
        num_columns: 1,
        num_rows: 1,
        uniform_spacing: true,
        column_widths: Vec::new(),
        row_heights: Vec::new(),
        loop_filter_across_tiles: false,
    };
    let layout = TileLayout::from_pps(&tiles, 1, 1).expect("one tile is valid");
    let mut state = layout
        .initialize_tile_cabac_states(&[0, 0], &[], 22)
        .expect("the two-byte arithmetic initializer is valid")
        .pop()
        .expect("one tile state exists");
    let error = state
        .decode_first_ctb_split_flag(3, 3, true)
        .expect_err("the split flag is inferred at the minimum coding-block size");
    assert!(matches!(
        error,
        Error::Unsupported("vaco-codec-hevc: first tile CTB split flag is inferred")
    ));
}

#[test]
fn real_tile_slice_header_has_one_tile_entry_point_offset() {
    let limits = Limits::default();
    let mut parser = HevcParser::new(limits.clone());
    parser
        .push_access_unit(HEVC, Framing::AnnexB)
        .expect("valid tile fixture parses");
    let pps = parser
        .parameter_sets()
        .get_pps(0)
        .expect("fixture references PPS zero");
    let sps = parser
        .parameter_sets()
        .get_sps(pps.sps_id)
        .expect("fixture references SPS zero");
    let tiles = pps.tiles.as_ref().expect("fixture PPS enables tiles");
    let layout = TileLayout::from_pps(tiles, sps.pic_width_in_ctbs(), sps.pic_height_in_ctbs())
        .expect("fixture tile geometry is valid");
    let mut budget = Budget::new(limits);
    let mut rbsp = RbspBuf::new();
    let (header, slice_data) = units(HEVC, Framing::AnnexB)
        .find_map(|nal| {
            let nal_header = HevcNalHeader::parse(nal.data)?;
            if !nal_header.nal_unit_type.has_slice_header() {
                return None;
            }
            rbsp.fill(nal.data, &mut budget).ok()?;
            let mut reader = BitReader::new(rbsp.as_slice());
            reader.skip(16);
            let header =
                SliceHeader::parse_data(&mut reader, nal_header, sps, pps, &mut budget).ok()?;
            let header_len = usize::try_from(reader.bit_pos().div_ceil(8)).ok()?;
            Some((header, nal.data.get(header_len..)?))
        })
        .expect("fixture has a parseable VCL slice header");
    assert_eq!(header.entry_point_offsets.len(), 1);
    let ranges = layout
        .tile_substream_byte_ranges(slice_data.len(), &header.entry_point_offsets)
        .expect("fixture entry point partitions both tile substreams");
    assert_eq!(
        ranges,
        vec![
            (0, header.entry_point_offsets[0] as usize),
            (header.entry_point_offsets[0] as usize, slice_data.len())
        ]
    );
    let tile_cabac = layout
        .initialize_tile_cabac_substreams(slice_data, &header.entry_point_offsets)
        .expect("each tile CABAC state initializes");
    assert_eq!(tile_cabac.len(), 2);
    for cabac in &tile_cabac {
        assert_eq!(cabac.range(), 510);
        assert!(!cabac.malformed());
    }
    let slice_qp = i8::try_from(26 + pps.init_qp_minus26 + header.qp_delta)
        .expect("fixture slice QP fits the signed context API");
    let mut tile_states = layout
        .initialize_tile_cabac_states(slice_data, &header.entry_point_offsets, slice_qp)
        .expect("each tile syntax context bank initializes");
    assert_eq!(tile_states.len(), 2);
    for state in &tile_states {
        assert_eq!(state.range(), 510);
        assert!(!state.malformed());
        assert_eq!(
            state.first_split_cu_context(),
            ContextModel::init_hevc(139, slice_qp)
        );
    }
    let ctb_log2_size = u32::from(sps.log2_min_cb_size) + u32::from(sps.log2_diff_max_min_cb_size);
    let first_split = tile_states
        .get_mut(0)
        .expect("first tile state exists")
        .decode_first_ctb_split_flag(ctb_log2_size, u32::from(sps.log2_min_cb_size), true)
        .expect("first tile CTB carries an explicit split flag");
    let second_split = tile_states
        .get_mut(1)
        .expect("second tile state exists")
        .decode_first_ctb_split_flag(ctb_log2_size, u32::from(sps.log2_min_cb_size), true)
        .expect("second tile CTB carries an explicit split flag");
    assert_eq!((first_split, second_split), (true, true));
    let first_child_split = tile_states
        .get_mut(0)
        .expect("first tile state exists")
        .decode_first_ctb_child_split_flag(ctb_log2_size - 1, u32::from(sps.log2_min_cb_size), true)
        .expect("first tile child carries an explicit split flag");
    let second_child_split = tile_states
        .get_mut(1)
        .expect("second tile state exists")
        .decode_first_ctb_child_split_flag(ctb_log2_size - 1, u32::from(sps.log2_min_cb_size), true)
        .expect("second tile child carries an explicit split flag");
    assert_eq!((first_child_split, second_child_split), (true, true));
    let first_grandchild_split = tile_states
        .get_mut(0)
        .expect("first tile state exists")
        .decode_first_ctb_grandchild_split_flag(
            ctb_log2_size - 2,
            u32::from(sps.log2_min_cb_size),
            true,
        )
        .expect("first tile grandchild carries an explicit split flag");
    let second_grandchild_split = tile_states
        .get_mut(1)
        .expect("second tile state exists")
        .decode_first_ctb_grandchild_split_flag(
            ctb_log2_size - 2,
            u32::from(sps.log2_min_cb_size),
            true,
        )
        .expect("second tile grandchild carries an explicit split flag");
    assert_eq!(
        (first_grandchild_split, second_grandchild_split),
        (true, false)
    );
    let first_leaf_is_nxn = tile_states
        .get_mut(0)
        .expect("first tile state exists")
        .decode_first_ctb_grandchild_leaf_part_mode(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect("first tile grandchild leaf carries part_mode");
    assert!(first_leaf_is_nxn);
    let second_leaf_error = tile_states
        .get_mut(1)
        .expect("second tile state exists")
        .decode_first_ctb_grandchild_leaf_part_mode(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect_err("unsplit second grandchild has no leaf part_mode");
    assert!(matches!(
        second_leaf_error,
        Error::Unsupported("vaco-codec-hevc: first tile grandchild leaf parent is not split")
    ));
    let first_prev_intra_flag = tile_states
        .get_mut(0)
        .expect("first tile state exists")
        .decode_first_ctb_leaf_prev_intra_luma_pred_flag(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect("first tile NxN leaf carries a prev-intra flag");
    assert!(first_prev_intra_flag);
    let first_mpm_prefix = tile_states
        .get_mut(0)
        .expect("first tile state exists")
        .decode_first_ctb_leaf_mpm_idx_prefix(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect("first tile explicit-MPM leaf carries a prefix bin");
    assert!(first_mpm_prefix);
    let first_mpm_suffix = tile_states
        .get_mut(0)
        .expect("first tile state exists")
        .decode_first_ctb_leaf_mpm_idx_suffix(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect("first tile MPM index carries its suffix bin");
    assert!(!first_mpm_suffix);
    let second_mpm_suffix_error = tile_states
        .get_mut(1)
        .expect("second tile state exists")
        .decode_first_ctb_leaf_mpm_idx_suffix(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect_err("second tile has no MPM suffix bin");
    assert!(matches!(
        second_mpm_suffix_error,
        Error::Unsupported("vaco-codec-hevc: first tile leaf has no mpm_idx suffix bin")
    ));
    let second_mpm_error = tile_states
        .get_mut(1)
        .expect("second tile state exists")
        .decode_first_ctb_leaf_mpm_idx_prefix(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect_err("second tile has no explicit-MPM leaf");
    assert!(matches!(
        second_mpm_error,
        Error::Unsupported("vaco-codec-hevc: first tile leaf has no explicit mpm_idx")
    ));
    let second_prev_intra_error = tile_states
        .get_mut(1)
        .expect("second tile state exists")
        .decode_first_ctb_leaf_prev_intra_luma_pred_flag(
            u32::from(sps.log2_min_cb_size),
            u32::from(sps.log2_min_cb_size),
        )
        .expect_err("second tile has no NxN leaf PU flags");
    assert!(matches!(
        second_prev_intra_error,
        Error::Unsupported("vaco-codec-hevc: first tile leaf has no NxN intra PU flags")
    ));
    let cabac = layout
        .initialize_first_tile_cabac(slice_data, &header.entry_point_offsets)
        .expect("first tile CABAC state initializes");
    assert_eq!(cabac.range(), 510);
    assert!(!cabac.malformed());
}
