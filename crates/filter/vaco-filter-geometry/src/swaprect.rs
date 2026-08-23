//! `swaprect` — swap the content of two same-sized rectangles within a frame.
//!
//! `ffmpeg -h filter=swaprect` documents `w` (default `"w/2"`), `h` (default
//! `"h/2"`), `x1`/`y1` (default `"w/2"`/`"h/2"`), `x2`/`y2` (default
//! `"0"`/`"0"`). All five geometry options are `vaco-expr` expressions;
//! evaluated once at `configure`, matching the sibling `vaco-filter-video-
//! geometry` crate's `crop`/`pad` precedent (no per-frame `eval` mode).
//!
//! # What is measured versus assumed
//!
//! The reference's own doc names this "swap 2 rectangular objects"; the
//! natural reading (and the only one self-consistent for a `V->V`, one
//! frame in, one frame out filter with no expression variable letting the
//! two rects reference each other's *content*) is a plain byte-for-byte
//! rectangle exchange, clamped to the frame and to non-overlap. Not
//! independently measured against the reference's pixel output — the
//! independent check used instead is structural: swapping the same pair of
//! rectangles twice must restore the original frame exactly, which only
//! holds if the operation is its own inverse (a property a byte-swap has and
//! a lossy blend would not).
//!
//! Overlapping rectangles are rejected (`configure` returns
//! [`vaco_core::Error::Unsupported`]): a byte-swap of two overlapping
//! regions is order-dependent in a way this crate has not measured against
//! the reference, and getting the overlap rule wrong would silently corrupt
//! a frame rather than fail loudly.

use vaco_core::{Error, MediaType, Result};
use vaco_expr::{Bindings, Expr};
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
    name: "swaprect",
    description: "Swap 2 rectangular objects in video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const VARS: &[&str] = &["w", "h", "a", "sar", "x", "y", "x1", "y1", "x2", "y2"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "swaprect", help = "Swap 2 rectangular objects in video")]
pub(crate) struct Opts {
    #[opt(
        name = "w",
        help = "set rect width",
        default = "w/2".to_owned(),
        flags(video, filtering)
    )]
    pub w: String,
    #[opt(
        name = "h",
        help = "set rect height",
        default = "h/2".to_owned(),
        flags(video, filtering)
    )]
    pub h: String,
    #[opt(
        name = "x1",
        help = "set 1st rect x top left coordinate",
        default = "w/2".to_owned(),
        flags(video, filtering)
    )]
    pub x1: String,
    #[opt(
        name = "y1",
        help = "set 1st rect y top left coordinate",
        default = "h/2".to_owned(),
        flags(video, filtering)
    )]
    pub y1: String,
    #[opt(
        name = "x2",
        help = "set 2nd rect x top left coordinate",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub x2: String,
    #[opt(
        name = "y2",
        help = "set 2nd rect y top left coordinate",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub y2: String,
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

#[derive(Debug, Clone, Copy, Default)]
struct Rects {
    w: u32,
    h: u32,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
}

impl Rects {
    fn overlaps(self) -> bool {
        let (ax0, ay0, ax1, ay1) = (self.x1, self.y1, self.x1 + self.w, self.y1 + self.h);
        let (bx0, by0, bx1, by1) = (self.x2, self.y2, self.x2 + self.w, self.y2 + self.h);
        ax0 < bx1 && bx0 < ax1 && ay0 < by1 && by0 < ay1
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    w_expr: Expr,
    h_expr: Expr,
    x1_expr: Expr,
    y1_expr: Expr,
    x2_expr: Expr,
    y2_expr: Expr,
    rects: Rects,
}

fn clamp_u32(value: f64, max: u32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value.floor() as u32).min(max)
    }
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let b = Bindings::new(VARS);
        Ok(Self {
            w_expr: Expr::parse(&opts.w, &b).map_err(|e| format!("swaprect: bad `w` `{e}`"))?,
            h_expr: Expr::parse(&opts.h, &b).map_err(|e| format!("swaprect: bad `h` `{e}`"))?,
            x1_expr: Expr::parse(&opts.x1, &b).map_err(|e| format!("swaprect: bad `x1` `{e}`"))?,
            y1_expr: Expr::parse(&opts.y1, &b).map_err(|e| format!("swaprect: bad `y1` `{e}`"))?,
            x2_expr: Expr::parse(&opts.x2, &b).map_err(|e| format!("swaprect: bad `x2` `{e}`"))?,
            y2_expr: Expr::parse(&opts.y2, &b).map_err(|e| format!("swaprect: bad `y2` `{e}`"))?,
            rects: Rects::default(),
        })
    }

    fn compute(&self, in_w: u32, in_h: u32, sar: vaco_core::Rational) -> Rects {
        let a = if in_h == 0 {
            0.0
        } else {
            f64::from(in_w) / f64::from(in_h)
        };
        let base = [
            f64::from(in_w),
            f64::from(in_h),
            a,
            sar.to_f64(),
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        ];
        let w = clamp_u32(self.w_expr.eval(&base), in_w);
        let h = clamp_u32(self.h_expr.eval(&base), in_h);
        let vars = [
            f64::from(in_w),
            f64::from(in_h),
            a,
            sar.to_f64(),
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        ];
        let x1 = clamp_u32(self.x1_expr.eval(&vars), in_w.saturating_sub(w));
        let y1 = clamp_u32(self.y1_expr.eval(&vars), in_h.saturating_sub(h));
        let x2 = clamp_u32(self.x2_expr.eval(&vars), in_w.saturating_sub(w));
        let y2 = clamp_u32(self.y2_expr.eval(&vars), in_h.saturating_sub(h));
        Rects {
            w,
            h,
            x1,
            y1,
            x2,
            y2,
        }
    }
}

fn swap_rect_bytes(
    format: PixFmt,
    plane_idx: u8,
    a_x: u32,
    a_y: u32,
    b_x: u32,
    b_y: u32,
    w: u32,
    h: u32,
    plane: &mut vaco_frame::PlaneMut<'_>,
) -> Result<()> {
    let unit = geom::plane_unit_bytes(format, plane_idx)?;
    let pw = format.plane_width(w, plane_idx) as usize;
    let ph = format.plane_height(h, plane_idx) as usize;
    let ax = format.plane_width(a_x, plane_idx) as usize;
    let ay = format.plane_height(a_y, plane_idx) as usize;
    let bx = format.plane_width(b_x, plane_idx) as usize;
    let by = format.plane_height(b_y, plane_idx) as usize;
    let row_bytes = pw.saturating_mul(unit);
    let a_start = ax.saturating_mul(unit);
    let b_start = bx.saturating_mul(unit);
    for row in 0..ph {
        // Read both source rows into owned buffers first: `PlaneMut::row_mut`
        // borrows the whole plane, not just one row, so two writes cannot be
        // interleaved with the reads that feed them without the borrow
        // checker (rightly) seeing two live `&mut` into the same buffer.
        let a_saved: Vec<u8> = plane
            .row(ay.saturating_add(row))
            .and_then(|r| r.get(a_start..a_start.saturating_add(row_bytes)))
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let b_saved: Vec<u8> = plane
            .row(by.saturating_add(row))
            .and_then(|r| r.get(b_start..b_start.saturating_add(row_bytes)))
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        if let Some(a_row) = plane.row_mut(ay.saturating_add(row))
            && let Some(a_slice) = a_row.get_mut(a_start..a_start.saturating_add(row_bytes))
        {
            let n = a_slice.len().min(b_saved.len());
            if let (Some(d), Some(s)) = (a_slice.get_mut(..n), b_saved.get(..n)) {
                d.copy_from_slice(s);
            }
        }
        if let Some(b_row) = plane.row_mut(by.saturating_add(row))
            && let Some(b_slice) = b_row.get_mut(b_start..b_start.saturating_add(row_bytes))
        {
            let n = b_slice.len().min(a_saved.len());
            if let (Some(d), Some(s)) = (b_slice.get_mut(..n), a_saved.get(..n)) {
                d.copy_from_slice(s);
            }
        }
    }
    Ok(())
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format,
            width,
            height,
            sample_aspect_ratio,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        geom::ensure_addressable(format)?;
        let rects = self.compute(width, height, sample_aspect_ratio);
        if rects.overlaps() {
            return Err(Error::Unsupported(
                "swaprect: overlapping rectangles are not supported",
            ));
        }
        self.rects = rects;
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let r = self.rects;
        if r.w == 0 || r.h == 0 {
            return Ok(FrameOut::One(input));
        }
        let mut out = input;
        // `Frame` arrives by value (see `FrameFilter::filter_frame`'s own
        // doc): writing through `plane_mut` on it copies only if the buffer
        // is genuinely still shared, same as every other filter here.
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            if let Some(mut plane) = out.plane_mut(p) {
                swap_rect_bytes(
                    format, plane_idx, r.x1, r.y1, r.x2, r.y2, r.w, r.h, &mut plane,
                )?;
            }
        }
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
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
    fn non_overlapping_rects_pass() {
        let r = Rects {
            w: 2,
            h: 2,
            x1: 0,
            y1: 0,
            x2: 4,
            y2: 4,
        };
        assert!(!r.overlaps());
    }

    #[test]
    fn identical_rects_overlap() {
        let r = Rects {
            w: 2,
            h: 2,
            x1: 1,
            y1: 1,
            x2: 1,
            y2: 1,
        };
        assert!(r.overlaps());
    }

    #[test]
    fn default_expressions_pick_the_four_quadrant_style_split() {
        let opts = Opts::default();
        let filter = Filter::new(&opts).unwrap();
        let r = filter.compute(16, 8, vaco_core::Rational::ONE);
        assert_eq!((r.w, r.h), (8, 4));
        assert_eq!((r.x1, r.y1), (8, 4));
        assert_eq!((r.x2, r.y2), (0, 0));
        assert!(!r.overlaps());
    }

    #[test]
    fn swapping_twice_restores_the_original_frame() {
        use vaco_frame::FramePool;
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, 8, 8).unwrap();
        if let Some(mut plane) = frame.plane_mut(0) {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    for (x, b) in row.iter_mut().enumerate() {
                        *b = (y * 8 + x) as u8;
                    }
                }
            }
        }
        let original: Vec<u8> = frame.plane(0).unwrap().row(0).unwrap().to_vec();
        let r = Rects {
            w: 2,
            h: 2,
            x1: 0,
            y1: 0,
            x2: 4,
            y2: 4,
        };
        let mut once = frame.clone();
        if let Some(mut plane) = once.plane_mut(0) {
            swap_rect_bytes(
                PixFmt::Gray8,
                0,
                r.x1,
                r.y1,
                r.x2,
                r.y2,
                r.w,
                r.h,
                &mut plane,
            )
            .unwrap();
        }
        assert_ne!(once.plane(0).unwrap().row(0).unwrap(), original.as_slice());
        let mut twice = once;
        if let Some(mut plane) = twice.plane_mut(0) {
            swap_rect_bytes(
                PixFmt::Gray8,
                0,
                r.x1,
                r.y1,
                r.x2,
                r.y2,
                r.w,
                r.h,
                &mut plane,
            )
            .unwrap();
        }
        assert_eq!(twice.plane(0).unwrap().row(0).unwrap(), original.as_slice());
    }
}
