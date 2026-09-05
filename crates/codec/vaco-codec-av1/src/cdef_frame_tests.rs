//! Frame-level CDEF selection and immutable-source regression tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "small fixed reconstruction fixture"
)]
use super::*;
use crate::cdef::{CdefParams, Strength};

fn context(budget: &mut Budget) -> FrameCtx {
    let fixture = include_bytes!("../tests/fixtures/flat128.obu");
    let list = units(fixture, Av1Framing::ObuStream);
    let sequence = list
        .iter()
        .find(|unit| unit.header.obu_type == ObuType::SEQUENCE_HEADER)
        .unwrap();
    let seq = SequenceHeader::parse(sequence.payload(fixture), budget).unwrap();
    let frame = list
        .iter()
        .find(|unit| unit.header.obu_type == ObuType::FRAME)
        .unwrap();
    let mut reader = vaco_bitstream::BitReader::new(frame.payload(fixture));
    let mut header = FrameHeader::parse_from_reader(&mut reader, &seq, 0, 0).unwrap();
    header.coded_lossless = false;
    header.cdef_bits = 0;
    header.cdef = CdefParams {
        enabled: true,
        damping: 6,
        y: [Strength {
            primary: 15,
            secondary: 4,
        }; 8],
        uv: [Strength::default(); 8],
    };
    let mut pic = Picture::new(budget, 16, 16, 0, 0, true).unwrap();
    for y in 0..16 {
        for x in 0..16 {
            pic.y
                .set(x, y, u16::try_from(70 + y * 5 + (x % 2) * 3).unwrap());
        }
    }
    FrameCtx {
        header,
        seq_mono: true,
        subsampling_x: false,
        subsampling_y: false,
        bit_depth: 8,
        mi_cols: 4,
        mi_rows: 4,
        use_128x128_superblock: false,
        enable_intra_edge_filter: false,
        enable_filter_intra: false,
        cdef_idx: vec![0],
        cdef_stride: 1,
        grid: vec![
            MiCell {
                skip: true,
                ..MiCell::default()
            };
            16
        ],
        pic,
        pre_cdef: None,
        restoration_units: std::array::from_fn(|_| Vec::new()),
        last_quant: Vec::new(),
    }
}

#[test]
fn each_non_skipped_4x4_cell_enables_its_8x8_region() {
    for active in [0, 1, 4, 5] {
        let mut budget = Budget::new(Limits::default());
        let mut ctx = context(&mut budget);
        let before = ctx.pic.y.as_slice().to_vec();
        apply_cdef(&mut ctx, &mut budget).unwrap();
        assert_eq!(ctx.pic.y.as_slice(), before);
        ctx.grid[active].skip = false;
        apply_cdef(&mut ctx, &mut budget).unwrap();
        let mut changed = 0;
        for y in 0..16 {
            for x in 0..16 {
                let index = y * 16 + x;
                if x < 8 && y < 8 {
                    changed += usize::from(ctx.pic.y.as_slice()[index] != before[index]);
                } else {
                    assert_eq!(ctx.pic.y.as_slice()[index], before[index]);
                }
            }
        }
        assert!(changed > 0, "filter must change the active textured region");
    }
}

#[test]
fn missing_unit_index_preserves_non_skipped_samples() {
    let mut budget = Budget::new(Limits::default());
    let mut ctx = context(&mut budget);
    ctx.grid.fill(MiCell::default());
    ctx.cdef_idx[0] = -1;
    let before = ctx.pic.y.as_slice().to_vec();
    apply_cdef(&mut ctx, &mut budget).unwrap();
    assert_eq!(ctx.pic.y.as_slice(), before);
}

#[test]
fn zero_index_bits_still_assign_entry_zero_on_the_first_non_skip_block() {
    let mut budget = Budget::new(Limits::default());
    let mut ctx = context(&mut budget);
    ctx.cdef_idx[0] = -1;
    let mut tile = TileState {
        sd: SymbolDecoder::new(&[0; 8], true),
        cdf: TileCdf::new(1),
        above_level: std::array::from_fn(|_| Vec::new()),
        above_dc: std::array::from_fn(|_| Vec::new()),
        left_level: std::array::from_fn(|_| Vec::new()),
        left_dc: std::array::from_fn(|_| Vec::new()),
        block_decoded: std::array::from_fn(|_| BlockDecoded::new(0, 0)),
        current_q_index: 1,
        mi_row_start: 0,
        mi_row_end: 4,
        mi_col_start: 0,
        mi_col_end: 4,
        ref_sgr_xqd: [[-32, 31]; 3],
        ref_lr_wiener: [[[3, -7, 15]; 2]; 3],
    };
    read_cdef(&mut ctx, &mut tile, 0, 0, BLOCK_8X8, true);
    assert_eq!(ctx.cdef_idx[0], -1);
    read_cdef(&mut ctx, &mut tile, 0, 0, BLOCK_8X8, false);
    assert_eq!(ctx.cdef_idx[0], 0);
}
