//! `xstack` — arrange `N` video inputs into a custom grid layout.
//!
//! `ffmpeg -h filter=xstack` (2026-08-28): `inputs` (`2..=INT_MAX`, default
//! `2`), `layout` (a free-form per-input `x_y` expression string), `grid`
//! (`<image_size>`, e.g. `"2x2"`), `shortest` (bool, default `false`),
//! `fill` (a colour, default `"none"`).
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo` sources)
//!
//! With neither `layout` nor `grid` given, the reference only accepts the
//! **default `inputs=2`** case, and lays it out exactly like `hstack`
//! (side by side) — confirmed directly; `inputs=4` with no `layout`/`grid`
//! is a hard `configure` error ("Invalid argument"), not a guessed
//! default grid.
//!
//! `grid=COLSxROWS` arranges inputs in row-major (raster) order: input `i`
//! goes to grid cell `(i % cols, i / cols)`. Confirmed with a `2x2` grid
//! and four distinct flat values — input `0` lands top-left, `1`
//! top-right, `2` bottom-left, `3` bottom-right, each cell exactly its own
//! input's size (all four `8x8` in the probe, giving a `16x16` output).
//!
//! # Not measured/implemented
//!
//! The free-form `layout=` string (`planning/16-filters.md`'s own "shared
//! layout parser" dependency — a small expression language for
//! per-input `x_y` position strings referencing other inputs' `w`/`h`) is
//! **not implemented**; `create` rejects it with a clean error. `fill`
//! (the colour for grid cells with no matching input) is not implemented
//! either: this module requires `inputs == cols * rows` exactly, and
//! requires every cell in a given column to share that column's width and
//! every cell in a given row to share that row's height (an assumption
//! this pass's one `2x2`-uniform-size probe cannot rule out being
//! stricter than the reference actually is — a genuinely mixed-size grid
//! was not measured).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

use crate::common;

const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "xstack",
    description: "Stack video inputs into custom layout.",
    inputs: &[],
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "xstack", help = "Stack video inputs into custom layout.")]
pub(crate) struct Opts {
    #[opt(name = "inputs", help = "set number of inputs", default = 2, range = 2..=64, flags(video, filtering))]
    pub inputs: i64,
    #[opt(name = "layout", help = "set custom layout", default = String::new(), flags(video, filtering))]
    pub layout: String,
    #[opt(name = "grid", help = "set fixed size grid layout", default = String::new(), flags(video, filtering))]
    pub grid: String,
    #[opt(name = "shortest", help = "force termination when the shortest input terminates", default = false, flags(video, filtering))]
    pub shortest: bool,
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

#[derive(Debug)]
pub(crate) struct Filter {
    n: usize,
    shortest: bool,
    cols: u32,
    rows: u32,
    /// Each input's own (width, height), resolved once in `configure`.
    sizes: Vec<(u32, u32)>,
    /// Per-column width and per-row height, resolved once in `configure`.
    col_widths: Vec<u32>,
    row_heights: Vec<u32>,
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::uniform(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts {
            shortest: self.shortest,
            ..FrameSyncOpts::default()
        }
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let mut sizes = Vec::new();
        let mut format = None;
        for i in 0..self.n {
            let Some(LinkFormat::Video {
                format: f,
                width,
                height,
                ..
            }) = ctx.input_link(i).cloned()
            else {
                return Ok(());
            };
            common::ensure_addressable(f)?;
            format.get_or_insert(f);
            sizes.push((width, height));
        }
        let cols = usize::try_from(self.cols).unwrap_or(1).max(1);
        let rows = usize::try_from(self.rows).unwrap_or(1).max(1);
        let mut col_widths = vec![0u32; cols];
        let mut row_heights = vec![0u32; rows];
        for (i, &(w, h)) in sizes.iter().enumerate() {
            let c = i % cols;
            #[allow(
                clippy::integer_division,
                reason = "raster grid-cell row index is an exact floor by construction"
            )]
            let r = i / cols;
            if let Some(slot) = col_widths.get_mut(c) {
                if *slot != 0 && *slot != w {
                    return Err(vaco_core::Error::Unsupported(
                        "xstack: every input in the same grid column must share its width",
                    ));
                }
                *slot = w;
            }
            if let Some(slot) = row_heights.get_mut(r) {
                if *slot != 0 && *slot != h {
                    return Err(vaco_core::Error::Unsupported(
                        "xstack: every input in the same grid row must share its height",
                    ));
                }
                *slot = h;
            }
        }
        self.sizes = sizes;
        self.col_widths = col_widths;
        self.row_heights = row_heights;
        let Some(format) = format else {
            return Ok(());
        };
        let total_width: u32 = self
            .col_widths
            .iter()
            .copied()
            .fold(0u32, u32::saturating_add);
        let total_height: u32 = self
            .row_heights
            .iter()
            .copied()
            .fold(0u32, u32::saturating_add);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { width: w, height: h, format: fmt, .. } = &mut out {
                *w = total_width;
                *h = total_height;
                *fmt = format;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(format) = event.get(0).and_then(|f| match &f.data {
            FrameData::Video { format, .. } => Some(*format),
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => None,
        }) else {
            return Ok(FrameOut::None);
        };
        let total_width: u32 = self
            .col_widths
            .iter()
            .copied()
            .fold(0u32, u32::saturating_add);
        let total_height: u32 = self
            .row_heights
            .iter()
            .copied()
            .fold(0u32, u32::saturating_add);
        let mut out = ctx.pool().acquire_video(format, total_width, total_height)?;
        let cols = usize::try_from(self.cols).unwrap_or(1).max(1);
        let plane_count = format.plane_count();
        for plane in 0..plane_count {
            for i in 0..self.n {
                let Some(frame) = event.get(i) else { continue };
                let Some(src) = frame.plane(plane) else { continue };
                let Some(mut dst) = out.plane_mut(plane) else { continue };
                let c = i % cols;
                #[allow(
                    clippy::integer_division,
                    reason = "raster grid-cell row index is an exact floor by construction"
                )]
                let r = i / cols;
                let x_off: u32 = self.col_widths.iter().take(c).copied().sum();
                let y_off: u32 = self.row_heights.iter().take(r).copied().sum();
                let x_off_p = common::to_i32(format.plane_width(x_off, plane as u8)).max(0);
                let y_off_p = common::to_i32(format.plane_height(y_off, plane as u8)).max(0);
                let (_, cell_h) = self.sizes.get(i).copied().unwrap_or((0, 0));
                let ph = common::to_i32(format.plane_height(cell_h, plane as u8)).max(0);
                let Ok(x_off_p) = usize::try_from(x_off_p) else {
                    continue;
                };
                let Ok(y_off_p) = usize::try_from(y_off_p) else {
                    continue;
                };
                for y in 0..ph {
                    let Ok(uy) = usize::try_from(y) else { continue };
                    let Some(src_row) = src.row(uy) else { continue };
                    let Some(dst_row) = dst.row_mut(y_off_p + uy) else {
                        continue;
                    };
                    if let Some(seg) = dst_row.get_mut(x_off_p..x_off_p + src_row.len()) {
                        seg.copy_from_slice(src_row);
                    }
                }
            }
        }
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if !opts.layout.is_empty() {
        return Err(
            "xstack: `layout=` (the free-form per-input position language) is not implemented; \
             use `grid=COLSxROWS` instead"
                .to_owned(),
        );
    }
    let n = usize::try_from(opts.inputs).unwrap_or(2).max(2);
    let (cols, rows) = if opts.grid.is_empty() {
        if n != 2 {
            return Err(
                "xstack: no `layout=`/`grid=` given and `inputs` is not the default `2` — \
                 the reference itself has no implicit layout beyond the 2-input case"
                    .to_owned(),
            );
        }
        (2u32, 1u32)
    } else {
        let Some((cols, rows)) = vaco_core::parse::image_size(&opts.grid) else {
            return Err(format!("xstack: bad `grid` `{}`", opts.grid));
        };
        if usize::try_from(cols).unwrap_or(0).saturating_mul(usize::try_from(rows).unwrap_or(0))
            != n
        {
            return Err(format!(
                "xstack: `grid={}` ({}x{} = {} cells) does not match `inputs={n}`",
                opts.grid,
                cols,
                rows,
                u64::from(cols) * u64::from(rows)
            ));
        }
        (cols, rows)
    };
    let input_pads = pads::video(n).ok_or_else(|| "xstack: too many inputs".to_owned())?;
    let filter = Filter {
        n,
        shortest: opts.shortest,
        cols,
        rows,
        sizes: Vec::new(),
        col_widths: Vec::new(),
        row_heights: Vec::new(),
    };
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..DESC
        },
        formats: NodeFormats::passthrough(n, 1, MediaType::Video, req.instance),
        filter: Box::new(Synced::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "xstack",
            instance: "xstack",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe: `inputs=4` with no `layout`/
    /// `grid` is a clean error, not a guessed default grid.
    #[test]
    fn no_layout_no_grid_beyond_two_inputs_is_a_clean_error() {
        let req = Instantiate {
            name: "xstack",
            instance: "xstack",
            args: Some("inputs=4"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn grid_2x2_with_four_inputs_is_creatable() {
        let req = Instantiate {
            name: "xstack",
            instance: "xstack",
            args: Some("inputs=4:grid=2x2"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn grid_mismatched_with_inputs_is_a_clean_error() {
        let req = Instantiate {
            name: "xstack",
            instance: "xstack",
            args: Some("inputs=4:grid=3x3"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn layout_string_is_not_implemented_and_says_so() {
        let req = Instantiate {
            name: "xstack",
            instance: "xstack",
            args: Some("inputs=2:layout=0_0|w0_0"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
