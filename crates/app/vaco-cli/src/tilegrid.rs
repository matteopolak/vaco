//! Automatic composition of a HEIF/AVIF tile grid.
//!
//! An `ffmpeg -i grid.avif out.png` with no `-map` writes the *composed*
//! picture, not tile 0 — **measured**: a 2×2 grid of 64×64 AV1 tiles decoded
//! to one 128×128 frame that matched the four tile decodes laid out at the
//! `stream_group`'s own `tile_*_offset`s, byte for byte. The reference does
//! that through its stream-group machinery; this crate does it by
//! synthesising the `-filter_complex` graph a user would otherwise have to
//! write — `xstack=grid=COLSxROWS` over the tile streams in raster order
//! (every tile of a grid is the same size, so the fixed grid layout *is*
//! the offset table), then a `crop` to the grid's stated output size — and
//! letting the ordinary complex-graph path build, decode and map it. One
//! mechanism, not a second one.
//!
//! Only the *primary* grid (the group carrying `default`, i.e. `pitm`) is
//! composed, and only when the invocation has no `-filter_complex` of its
//! own and no `-map` — the same conditions under which automatic stream
//! selection runs at all. A non-primary grid stays reachable by mapping its
//! tile streams by hand.

use vaco_format_core::{Disposition, StreamGroup, StreamGroupKind};

use crate::input::InputFile;

/// One synthesised graph: which input it composes and its `-filter_complex`
/// text, whose single labelled output pad the video auto-pick then selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesizedGrid {
    pub file: usize,
    pub text: String,
}

/// The graphs to append to the invocation's `-filter_complex` list, one per
/// input file that has a primary tile grid, in input order.
#[must_use]
pub fn synthesize(inputs: &[InputFile]) -> Vec<SynthesizedGrid> {
    inputs
        .iter()
        .enumerate()
        .filter_map(|(file, input)| {
            let group = input
                .demuxer
                .stream_groups()
                .iter()
                .find(|g| g.disposition.contains(Disposition::DEFAULT))?;
            let text = graph_text(file as u32, group)?;
            Some(SynthesizedGrid { file, text })
        })
        .collect()
}

/// The `-filter_complex` text composing `group`'s tiles from `file`.
///
/// `None` for a group that is not a tile grid, has no members, or whose
/// member count is not `tile_rows × tile_columns` — nothing is composed
/// rather than something wrong.
#[must_use]
pub fn graph_text(file: u32, group: &StreamGroup) -> Option<String> {
    let StreamGroupKind::TileGrid(grid) = &group.kind else {
        return None;
    };
    let cells = usize::try_from(u64::from(grid.tile_rows) * u64::from(grid.tile_columns)).ok()?;
    if group.stream_indices.is_empty() || cells != group.stream_indices.len() {
        return None;
    }
    let inputs: String = group
        .stream_indices
        .iter()
        .map(|s| format!("[{file}:{s}]"))
        .collect();
    let label = format!("[vaco_tilegrid_{file}_{}]", group.index.0);
    let crop = format!(
        "crop={}:{}:{}:{}",
        grid.output_width, grid.output_height, grid.horizontal_offset, grid.vertical_offset
    );
    if group.stream_indices.len() == 1 {
        return Some(format!("{inputs}{crop}{label}"));
    }
    Some(format!(
        "{inputs}xstack=inputs={}:grid={}x{},{crop}{label}",
        group.stream_indices.len(),
        grid.tile_columns,
        grid.tile_rows,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vaco_format_core::{StreamGroupIndex, TileGrid};

    fn grid(members: &[u32], offsets: &[(u32, u32)]) -> StreamGroup {
        let (tile_rows, tile_columns) = match members.len() {
            1 => (1, 1),
            2 => (1, 2),
            _ => (2, 2),
        };
        let mut g = StreamGroup::new(
            StreamGroupIndex(0),
            StreamGroupKind::TileGrid(TileGrid {
                tile_rows,
                tile_columns,
                coded_width: 128,
                coded_height: 128,
                output_width: 120,
                output_height: 100,
                horizontal_offset: 0,
                vertical_offset: 0,
                tile_offsets: offsets.to_vec(),
            }),
        );
        g.stream_indices = members.to_vec();
        g
    }

    #[test]
    fn a_two_by_two_grid_stacks_then_crops() {
        let g = grid(&[0, 1, 2, 3], &[(0, 0), (64, 0), (0, 64), (64, 64)]);
        assert_eq!(
            graph_text(0, &g).as_deref(),
            Some(
                "[0:0][0:1][0:2][0:3]xstack=inputs=4:grid=2x2,crop=120:100:0:0[vaco_tilegrid_0_0]"
            )
        );
    }

    #[test]
    fn a_single_tile_grid_only_crops() {
        let g = grid(&[3], &[(0, 0)]);
        assert_eq!(
            graph_text(1, &g).as_deref(),
            Some("[1:3]crop=120:100:0:0[vaco_tilegrid_1_0]")
        );
    }

    #[test]
    fn a_member_count_that_is_not_the_grid_composes_nothing() {
        assert!(graph_text(0, &grid(&[0, 1, 2], &[(0, 0), (64, 0), (0, 64)])).is_none());
        assert!(graph_text(0, &grid(&[], &[])).is_none());
    }
}
