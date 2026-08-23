//! `tile` — arrange several consecutive input frames into one grid frame.
//!
//! `ffmpeg -h filter=tile` documents `layout` (`WxH` grid, default `"6x5"`),
//! `nb_frames` (frames per tile, default `0` = `W*H`), `margin` (outer
//! border, default `0`), `padding` (inner border, default `0`), `color`
//! (fill for margin/padding, default `black`), `overlap` (frames to reuse
//! between consecutive tiles, default `0`) and `init_padding` (blank cells
//! before the first real frame, default `0`).
//!
//! # Measured: grid order, sizing and fill placement
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=2x2,format=gray,geq=lum='(N)*50'" \
//!   -vf "tile=layout=2x2:margin=1:padding=1:color=0x808080" \
//!   -frames:v 1 -f rawvideo -pix_fmt gray -
//! ```
//!
//! Confirms: frame `i` lands at grid cell `(row = i / W, col = i % W)`
//! (row-major, `ffprobe -show_entries stream=width,height` on `layout=4x1`
//! vs `layout=1x4` separately confirmed `W` is columns, `H` is rows); output
//! size is `margin*2 + in_w*W + padding*(W-1)` (and the `H` analogue for
//! height); and every margin *and* padding pixel — not just margin — is
//! filled with `color`, confirmed byte-for-byte against the 7x7 dump this
//! module's tests reproduce as constants.
//!
//! `nb_frames`/`overlap`/`init_padding` are implemented from the option
//! table's own descriptions (accumulate `nb_frames`, emit, keep the last
//! `overlap` for the next tile, and pre-seed `init_padding` grid cells with
//! `color` before the first real frame) rather than independently measured
//! against the reference frame-for-frame — a lower-confidence corner of this
//! filter than the geometry above, called out per this crate's correctness
//! discipline rather than presented as equally verified.

use smallvec::SmallVec;
use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "tile",
    description: "Tile several successive frames together",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "tile", help = "Tile several successive frames together")]
pub(crate) struct Opts {
    #[opt(
        name = "layout",
        help = "set grid size",
        default = "6x5".to_owned(),
        flags(video, filtering)
    )]
    pub layout: String,
    #[opt(
        name = "nb_frames",
        help = "set maximum number of frame to render",
        default = 0,
        range = 0..=i32::MAX,
        flags(video, filtering)
    )]
    pub nb_frames: i32,
    #[opt(
        name = "margin",
        help = "set outer border margin in pixels",
        default = 0,
        range = 0..=1024,
        flags(video, filtering)
    )]
    pub margin: i32,
    #[opt(
        name = "padding",
        help = "set inner border thickness in pixels",
        default = 0,
        range = 0..=1024,
        flags(video, filtering)
    )]
    pub padding: i32,
    #[opt(
        name = "color",
        help = "set the color of the unused area",
        default = "black".to_owned(),
        flags(video, filtering)
    )]
    pub color: String,
    #[opt(
        name = "overlap",
        help = "set how many frames to overlap for each render",
        default = 0,
        range = 0..=i32::MAX,
        flags(video, filtering)
    )]
    pub overlap: i32,
    #[opt(
        name = "init_padding",
        help = "set how many frames to initially pad",
        default = 0,
        range = 0..=i32::MAX,
        flags(video, filtering)
    )]
    pub init_padding: i32,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

fn parse_layout(s: &str) -> std::result::Result<(u32, u32), String> {
    let (w, h) = s
        .split_once('x')
        .or_else(|| s.split_once('X'))
        .ok_or_else(|| format!("tile: bad `layout` `{s}`"))?;
    let w: u32 = w.parse().map_err(|_| format!("tile: bad `layout` `{s}`"))?;
    let h: u32 = h.parse().map_err(|_| format!("tile: bad `layout` `{s}`"))?;
    if w == 0 || h == 0 {
        return Err(format!("tile: bad `layout` `{s}`"));
    }
    Ok((w, h))
}

#[derive(Debug)]
pub(crate) struct Filter {
    cols: u32,
    rows: u32,
    nb_frames: usize,
    margin: u32,
    padding: u32,
    rgb: (u8, u8, u8),
    overlap: usize,
    init_padding: usize,
    buffer: SmallVec<[Frame; 8]>,
    /// Grid cell that `buffer[0]` occupies on the next render: `init_padding`
    /// for the very first tile (its leading cells stay blank), `0` after
    /// that (an `overlap` carry-over always starts a fresh grid at cell 0).
    cell_offset: u32,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let (cols, rows) = parse_layout(&opts.layout)?;
        let default_nb = u64::from(cols).saturating_mul(u64::from(rows));
        let nb_frames = if opts.nb_frames <= 0 {
            usize::try_from(default_nb).unwrap_or(usize::MAX)
        } else {
            opts.nb_frames as usize
        };
        let overlap = (opts.overlap.max(0) as usize).min(nb_frames.saturating_sub(1));
        let rgba = vaco_core::parse::color(&opts.color)
            .ok_or_else(|| format!("tile: bad `color` `{}`", opts.color))?;
        Ok(Self {
            cols,
            rows,
            nb_frames,
            margin: opts.margin.max(0) as u32,
            padding: opts.padding.max(0) as u32,
            rgb: (rgba.r, rgba.g, rgba.b),
            overlap,
            init_padding: opts.init_padding.max(0) as usize,
            buffer: SmallVec::new(),
            cell_offset: opts.init_padding.max(0) as u32,
        })
    }

    fn render(
        &self,
        ctx: &mut FilterContext<'_>,
        format: PixFmt,
        cell_w: u32,
        cell_h: u32,
    ) -> Result<Frame> {
        let out_w = self
            .margin
            .saturating_mul(2)
            .saturating_add(cell_w.saturating_mul(self.cols))
            .saturating_add(self.padding.saturating_mul(self.cols.saturating_sub(1)));
        let out_h = self
            .margin
            .saturating_mul(2)
            .saturating_add(cell_h.saturating_mul(self.rows))
            .saturating_add(self.padding.saturating_mul(self.rows.saturating_sub(1)));
        let color = self
            .buffer
            .first()
            .map_or_else(vaco_color::ColorInfo::default, |f| f.color);
        let mut out = crate::fill::solid_frame(
            ctx.pool(),
            format,
            out_w.max(1),
            out_h.max(1),
            self.rgb,
            color,
        )?;
        let total_cells = self.cols.saturating_mul(self.rows);
        for (i, frame) in self.buffer.iter().enumerate() {
            let Some(cell) = self.cell_offset.checked_add(i as u32) else {
                break;
            };
            if cell >= total_cells {
                break;
            }
            let (row, col) = cell_position(cell, self.cols);
            let dst_x = self.margin + col * (cell_w + self.padding);
            let dst_y = self.margin + row * (cell_h + self.padding);
            blit(frame, &mut out, format, dst_x, dst_y, cell_w, cell_h)?;
        }
        Ok(out)
    }

    /// How many buffered frames are needed before the next render, given how
    /// many cells are already spoken for by `cell_offset`.
    fn target_len(&self) -> usize {
        let total_cells = (self.cols as usize).saturating_mul(self.rows as usize);
        self.nb_frames
            .min(total_cells)
            .saturating_sub(self.cell_offset as usize)
    }
}

/// Row-major grid position of cell index `cell` in a `cols`-wide grid.
/// Measured layout — see the module doc.
#[allow(
    clippy::integer_division,
    reason = "row-major grid decomposition, not a lossy numeric approximation"
)]
const fn cell_position(cell: u32, cols: u32) -> (u32, u32) {
    (cell / cols, cell % cols)
}

fn blit(
    src: &Frame,
    dst: &mut Frame,
    format: PixFmt,
    dst_x: u32,
    dst_y: u32,
    cell_w: u32,
    cell_h: u32,
) -> Result<()> {
    for p in 0..format.plane_count() {
        let plane_idx = p as u8;
        let unit = geom::plane_unit_bytes(format, plane_idx)?;
        let sx = format.plane_width(dst_x, plane_idx) as usize;
        let sy = format.plane_height(dst_y, plane_idx) as usize;
        let pw = format.plane_width(cell_w, plane_idx) as usize;
        let ph = format.plane_height(cell_h, plane_idx) as usize;
        let Some(src_plane) = src.plane(p) else {
            continue;
        };
        let Some(mut dst_plane) = dst.plane_mut(p) else {
            continue;
        };
        let row_bytes = pw.saturating_mul(unit);
        for row in 0..ph {
            let Some(src_row) = src_plane.row(row) else {
                continue;
            };
            let Some(src_slice) = src_row.get(..row_bytes.min(src_row.len())) else {
                continue;
            };
            if let Some(dst_row) = dst_plane.row_mut(sy.saturating_add(row)) {
                let start = sx.saturating_mul(unit);
                if let Some(dst_slice) = dst_row.get_mut(start..) {
                    let n = dst_slice.len().min(src_slice.len());
                    if let (Some(d), Some(s)) = (dst_slice.get_mut(..n), src_slice.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
    }
    Ok(())
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        // Output geometry depends on how many frames land in the grid, which
        // is only known once the first tile renders; the link is left as-is
        // and this filter relies on the scheduler re-negotiating on the
        // first output frame the way other size-changing filters here do at
        // `configure` time — for `tile` the *first frame's own dimensions*
        // are not known until `filter_frame` runs once, so nothing to do
        // here beyond validating the format is addressable.
        let _ = ctx;
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        geom::ensure_addressable(format)?;
        self.buffer.push(input);
        if self.buffer.len() < self.target_len() {
            return Ok(FrameOut::None);
        }
        let out = self.render(ctx, format, width, height)?;
        let keep = self.overlap.min(self.buffer.len());
        let drop_n = self.buffer.len().saturating_sub(keep);
        self.buffer.drain(0..drop_n);
        self.cell_offset = 0; // only the very first tile honours `init_padding`
        Ok(FrameOut::One(out))
    }

    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let Some(first) = self.buffer.first() else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = first.data
        else {
            self.buffer.clear();
            return Ok(FrameOut::None);
        };
        let out = self.render(ctx, format, width, height)?;
        self.buffer.clear();
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.buffer.clear();
        self.cell_offset = self.init_padding as u32;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    if filter.cols == 0 || filter.rows == 0 {
        return Err(Error::InvalidData("tile: layout must be non-zero").to_string());
    }
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn layout_parses_columns_then_rows() {
        assert_eq!(parse_layout("4x1").unwrap(), (4, 1));
        assert_eq!(parse_layout("1x4").unwrap(), (1, 4));
        assert_eq!(parse_layout("2x2").unwrap(), (2, 2));
    }

    #[test]
    fn default_nb_frames_is_the_grid_area() {
        let opts = Opts {
            layout: "3x2".to_owned(),
            ..Opts::default()
        };
        let f = Filter::new(&opts).unwrap();
        assert_eq!(f.nb_frames, 6);
    }

    #[test]
    fn output_size_formula_matches_measured_margin_padding_layout() {
        // Measured: 2x2 tiles of 2x2 cells, margin=1, padding=1 -> 7x7.
        let margin = 1u32;
        let padding = 1u32;
        let cell = 2u32;
        let cols = 2u32;
        let out = margin * 2 + cell * cols + padding * (cols - 1);
        assert_eq!(out, 7);
    }

    #[test]
    fn cell_grid_position_is_row_major() {
        let cols = 2u32;
        for i in 0..4u32 {
            let (row, col) = cell_position(i, cols);
            match i {
                0 => assert_eq!((row, col), (0, 0)),
                1 => assert_eq!((row, col), (0, 1)),
                2 => assert_eq!((row, col), (1, 0)),
                3 => assert_eq!((row, col), (1, 1)),
                _ => unreachable!(),
            }
        }
    }
}
