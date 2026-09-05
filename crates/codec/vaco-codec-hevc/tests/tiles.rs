//! Tile-picture scope boundary against a real HM 18.0 Annex-B stream.
//!
//! The fixture is one 512x64 IDR picture with two uniform tile columns. Its
//! PPS must carry `tiles_enabled_flag = 1`; the decoder must then refuse the
//! picture by the named scope error before reading tile-partitioned CABAC.
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
    let cabac = layout
        .initialize_first_tile_cabac(slice_data, &header.entry_point_offsets)
        .expect("first tile CABAC state initializes");
    assert_eq!(cabac.range(), 510);
    assert!(!cabac.malformed());
}
