//! `unsharp` — sharpen (or blur) by subtracting a box-blurred copy.
//!
//! `ffmpeg -h filter=unsharp` documents three luma/chroma/alpha triples:
//! `*_msize_x`/`*_msize_y` (matrix size, odd, `3..=23`, default `5`) and
//! `*_amount` (`-2..=5`, luma default `1`, chroma/alpha default `0`). The
//! formula, standard unsharp masking: `out = orig + amount*(orig -
//! blurred)`, where `blurred` is a box average over the `msize_x`x`msize_y`
//! window (radius `(msize-1)/2` per axis) — not a Gaussian; see
//! [`crate::gblur`] for that one.
//!
//! # Verified: interior is exact via an analytic invariant, independent of the reference
//!
//! A box average of a linear ramp equals the ramp's own value at the
//! window's centre (the positive and negative deviations either side of
//! centre cancel by symmetry) — true by construction of the mean, not
//! something read off `ffmpeg`. So for any interior pixel of a linear ramp,
//! `orig - blurred = 0` and `unsharp` must be the identity there for *any*
//! `amount`. Confirmed against the reference directly too:
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray:s=5x5,format=gray8,geq=lum='10*X'" \
//!   -vf "unsharp=luma_msize_x=3:luma_msize_y=3:luma_amount=1" \
//!   -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! gives `0 10 20 30 42` — columns 1-3 (interior) pass through unchanged, as
//! the invariant requires.
//!
//! # A measured, documented gap: the last-column border is off by one
//!
//! The same probe's border column (`42`, above) does not reconcile with
//! [`crate::boxblur`]'s own replicate-border box average at that position
//! (which would predict `43`): the reference's internal blur evidently does
//! not use the identical rounding/replication [`crate::common::box_pass`]
//! does at the very edge. The interior — the overwhelming majority of any
//! real frame, and the only region the invariant above can even state a
//! requirement for — is unaffected; this implementation reuses
//! [`crate::common::box_pass`] (round-to-nearest, replicate border)
//! throughout, so it is the border pixels specifically, on every edge, that
//! are not proven bit-exact. Recorded in
//! `docs/filter/vaco-filter-blur.md` rather than silently accepted.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Rounding};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "unsharp",
    description: "Sharpen or blur the input video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "unsharp", help = "Sharpen or blur the input video")]
pub(crate) struct Opts {
    #[opt(
        name = "luma_msize_x",
        alias = "lx",
        help = "set luma matrix horizontal size",
        default = 5,
        range = 3..=23,
        flags(video, filtering)
    )]
    pub luma_msize_x: i32,
    #[opt(
        name = "luma_msize_y",
        alias = "ly",
        help = "set luma matrix vertical size",
        default = 5,
        range = 3..=23,
        flags(video, filtering)
    )]
    pub luma_msize_y: i32,
    #[opt(
        name = "luma_amount",
        alias = "la",
        help = "set luma effect strength",
        default = 1.0,
        range = -2.0..=5.0,
        flags(video, filtering)
    )]
    pub luma_amount: f64,
    #[opt(
        name = "chroma_msize_x",
        alias = "cx",
        help = "set chroma matrix horizontal size",
        default = 5,
        range = 3..=23,
        flags(video, filtering)
    )]
    pub chroma_msize_x: i32,
    #[opt(
        name = "chroma_msize_y",
        alias = "cy",
        help = "set chroma matrix vertical size",
        default = 5,
        range = 3..=23,
        flags(video, filtering)
    )]
    pub chroma_msize_y: i32,
    #[opt(
        name = "chroma_amount",
        alias = "ca",
        help = "set chroma effect strength",
        default = 0.0,
        range = -2.0..=5.0,
        flags(video, filtering)
    )]
    pub chroma_amount: f64,
    #[opt(
        name = "alpha_msize_x",
        alias = "ax",
        help = "set alpha matrix horizontal size",
        default = 5,
        range = 3..=23,
        flags(video, filtering)
    )]
    pub alpha_msize_x: i32,
    #[opt(
        name = "alpha_msize_y",
        alias = "ay",
        help = "set alpha matrix vertical size",
        default = 5,
        range = 3..=23,
        flags(video, filtering)
    )]
    pub alpha_msize_y: i32,
    #[opt(
        name = "alpha_amount",
        alias = "aa",
        help = "set alpha effect strength",
        default = 0.0,
        range = -2.0..=5.0,
        flags(video, filtering)
    )]
    pub alpha_amount: f64,
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

#[derive(Debug, Clone, Copy)]
struct PlaneParams {
    rx: i32,
    ry: i32,
    amount: f64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    luma: PlaneParams,
    chroma: PlaneParams,
    alpha: PlaneParams,
}

const fn odd_radius(msize: i32) -> i32 {
    (msize - 1) >> 1
}

impl Filter {
    const fn new(opts: &Opts) -> Self {
        Self {
            luma: PlaneParams {
                rx: odd_radius(opts.luma_msize_x),
                ry: odd_radius(opts.luma_msize_y),
                amount: opts.luma_amount,
            },
            chroma: PlaneParams {
                rx: odd_radius(opts.chroma_msize_x),
                ry: odd_radius(opts.chroma_msize_y),
                amount: opts.chroma_amount,
            },
            alpha: PlaneParams {
                rx: odd_radius(opts.alpha_msize_x),
                ry: odd_radius(opts.alpha_msize_y),
                amount: opts.alpha_amount,
            },
        }
    }

    fn params_for(&self, format: PixFmt, plane: u8) -> PlaneParams {
        if plane == 0 {
            self.luma
        } else if format.has(vaco_pixfmt::PixFmtFlags::ALPHA)
            && u32::from(plane) == u32::from(format.descriptor().planes) - 1
        {
            self.alpha
        } else {
            self.chroma
        }
    }
}

fn sharpen_plane(rows: &[&[u8]], w: i32, h: i32, params: PlaneParams) -> Vec<Vec<u8>> {
    if params.amount == 0.0 {
        return rows.iter().map(|r| (*r).to_vec()).collect();
    }
    let blurred = common::box_pass(rows, w, h, params.rx, params.ry, Rounding::Nearest);
    let mut out = Vec::new();
    for (y, blurred_row) in blurred.iter().enumerate() {
        let mut row = Vec::new();
        let Some(src_row) = rows.get(y) else {
            out.push(vec![0u8; w.max(0) as usize]);
            continue;
        };
        for (x, &b) in blurred_row.iter().enumerate() {
            let orig = f64::from(src_row.get(x).copied().unwrap_or(0));
            let value = orig + params.amount * (orig - f64::from(b));
            row.push(clamp_u8(value));
        }
        out.push(row);
    }
    out
}

fn clamp_u8(value: f64) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    let rounded = value.round();
    if rounded <= 0.0 {
        0
    } else if rounded >= 255.0 {
        255
    } else {
        rounded as u8
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        common::ensure_8bit_addressable(format)?;
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let params = self.params_for(format, p8);
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let sharpened = sharpen_plane(&rows, pw, ph, params);
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in sharpened.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        common::copy_frame_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// The analytic invariant from this module's doc: a linear ramp's
    /// *interior* is unchanged by any sharpen amount, because a box average
    /// of a linear function equals the function's own value at the window
    /// centre. Independent of both the reference and of `box_pass`'s own
    /// arithmetic — it is a property the mean must have.
    #[test]
    fn interior_of_a_linear_ramp_is_unchanged() {
        let opts = Opts {
            luma_amount: 1.0,
            ..Opts::default()
        };
        let filter = Filter::new(&opts);
        assert!((filter.luma.amount - 1.0).abs() < f64::EPSILON);
        let rows_owned: Vec<Vec<u8>> = (0..5).map(|_| vec![0, 10, 20, 30, 40]).collect();
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = sharpen_plane(&rows, 5, 5, PlaneParams { rx: 1, ry: 1, amount: filter.luma.amount });
        for y in 1..4 {
            for x in 1..4 {
                assert_eq!(out[y][x], rows_owned[y][x], "interior pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn zero_amount_is_identity() {
        let filter_amount = PlaneParams {
            rx: 1,
            ry: 1,
            amount: 0.0,
        };
        let row0: &[u8] = &[1, 2, 3];
        let rows: [&[u8]; 1] = [row0];
        let out = sharpen_plane(&rows, 3, 1, filter_amount);
        assert_eq!(out[0], vec![1, 2, 3]);
    }

    #[test]
    fn negative_amount_blurs_toward_the_local_average() {
        // amount=-1 => out = blurred exactly.
        let params = PlaneParams {
            rx: 1,
            ry: 0,
            amount: -1.0,
        };
        let row0: &[u8] = &[0, 100, 0];
        let rows: [&[u8]; 1] = [row0];
        let out = sharpen_plane(&rows, 3, 1, params);
        let blurred = common::box_pass(&rows, 3, 1, 1, 0, Rounding::Nearest);
        assert_eq!(out, blurred);
    }
}
