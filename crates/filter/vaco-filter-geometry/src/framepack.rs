//! `framepack` — pack two independent video views into one frame-packed
//! stereoscopic stream.
//!
//! `ffmpeg -h filter=framepack` documents `format` (`sbs`=1 default,
//! `tab`=2, `frameseq`=3, `lines`=6, `columns`=7) and no other options. Pads
//! are named `left`/`right` in, `packed` out.
//!
//! # This is `Paired`, not `Synced` — measured
//!
//! `ffmpeg -h filter=framepack` carries no `eof_action`/`shortest`/
//! `repeatlast`/`ts_sync_mode` section at all (compare `alphamerge`'s,
//! which has one verbatim). Measured directly: feeding a 10-frame left and
//! a 5-frame right input at the same rate produces exactly 5 packed
//! frames, not 10 with the last right-hand frame repeated — and feeding
//! left/right at *different* time bases (`rate=10` vs `rate=5`) is
//! refused outright at configure time (`Left and right time bases
//! differ (1/10 vs 1/5)`), not reconciled the way a framesync filter
//! would. See `vaco_filter_core::adapt::Paired`'s own doc for the general
//! statement this filter is the measurement behind.
//!
//! # Measured: the four spatial layouts, byte for byte
//!
//! Built a 4x2 `left` frame of constant byte `0x10` and a 4x2 `right`
//! frame of constant byte `0x20` (`format=gray`, so there is exactly one
//! plane and one byte per sample), and read the raw output of each
//! `format=`:
//!
//! ```text
//! sbs:     8x2, each row = [left row][right row]                — horizontal concat
//! tab:     4x4, rows 0-1 = left, rows 2-3 = right                — vertical concat
//! lines:   4x4, row 2i = left row i, row 2i+1 = right row i      — row-interleaved
//! columns: 8x2, col 2i = left col i, col 2i+1 = right col i      — column-interleaved
//! ```
//!
//! `lines`/`sbs` and `tab`/`columns` are size-compatible pairs (`lines` is
//! `tab`'s output size with rows shuffled; `columns` is `sbs`'s with
//! columns shuffled), which is the check this module's tests pin alongside
//! the raw bytes.
//!
//! `lines`/`columns` require the two inputs to share exactly one geometry;
//! unlike `sbs`/`tab`, there is no reference behaviour measured for
//! mismatched left/right dimensions under an interleave, so this
//! implementation refuses rather than guesses (`Error::Unsupported`).
//!
//! # Measured: `frameseq` is temporal, not spatial
//!
//! `frameseq` does not pack at all: it emits `left`, then `right`, as two
//! *separate* full-size frames, alternating, at half the input's frame
//! period each — a 1 fps pair in produces two frames covering the same one
//! second, at output `time_base` `1/2` when the input's was `1/1`, `pts`
//! `0` (`left`) then `1` (`right`), `Stereo3D` side data confirms the
//! ordering (`view - left` then `view - right`). Implemented as `time_base
//! = (num, den * 2)` plus a running frame counter, since the measurement
//! only pins the single-pair case; multiple pairs are the direct, and only
//! sensible, generalisation.

use smallvec::SmallVec;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const INPUT_PADS: &[Pad] = &[
    Pad {
        name: "left",
        media_type: MediaType::Video,
    },
    Pad {
        name: "right",
        media_type: MediaType::Video,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "packed",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "framepack",
    description: "Generate a frame packed stereoscopic video",
    inputs: INPUT_PADS,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Sbs,
    Tab,
    Frameseq,
    Lines,
    Columns,
}

impl Layout {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "sbs" | "1" => Some(Self::Sbs),
            "tab" | "2" => Some(Self::Tab),
            "frameseq" | "3" => Some(Self::Frameseq),
            "lines" | "6" => Some(Self::Lines),
            "columns" | "7" => Some(Self::Columns),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "framepack",
    help = "Generate a frame packed stereoscopic video"
)]
pub(crate) struct Opts {
    #[opt(
        name = "format",
        help = "Frame pack output format",
        default = "sbs".to_owned(),
        flags(video, filtering)
    )]
    pub format: String,
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
pub(crate) struct Framepack {
    layout: Layout,
    format: Option<PixFmt>,
    /// Running output frame index, `frameseq` only.
    seq: i64,
}

impl Framepack {
    fn new(layout: Layout) -> Self {
        Self {
            layout,
            format: None,
            seq: 0,
        }
    }

    fn pack(&self, format: PixFmt, left: &Frame, right: &Frame, out: &mut Frame) -> Result<()> {
        let FrameData::Video {
            width: lw,
            height: lh,
            ..
        } = left.data
        else {
            return Err(Error::InvalidData("framepack: left input is not video"));
        };
        let FrameData::Video {
            width: rw,
            height: rh,
            ..
        } = right.data
        else {
            return Err(Error::InvalidData("framepack: right input is not video"));
        };
        match self.layout {
            Layout::Sbs => {
                geom::blit(left, out, format, 0, 0, lw, lh)?;
                geom::blit(right, out, format, lw, 0, rw, rh)?;
            }
            Layout::Tab => {
                geom::blit(left, out, format, 0, 0, lw, lh)?;
                geom::blit(right, out, format, 0, lh, rw, rh)?;
            }
            Layout::Lines => {
                if lw != rw || lh != rh {
                    return Err(Error::Unsupported(
                        "framepack: lines needs left and right the same size",
                    ));
                }
                interleave_lines(left, right, out, format, lh)?;
            }
            Layout::Columns => {
                if lw != rw || lh != rh {
                    return Err(Error::Unsupported(
                        "framepack: columns needs left and right the same size",
                    ));
                }
                interleave_columns(left, right, out, format, lw, lh)?;
            }
            Layout::Frameseq => {
                // Handled by the caller: this is the only layout that does
                // not produce one packed frame.
            }
        }
        Ok(())
    }
}

/// Row `2*i` from `left`, row `2*i + 1` from `right`, for `cell_h` source
/// rows of each — measured layout, see this module's doc.
#[allow(
    clippy::unnecessary_wraps,
    reason = "kept Result-shaped alongside interleave_columns, which is genuinely fallible"
)]
fn interleave_lines(
    left: &Frame,
    right: &Frame,
    out: &mut Frame,
    format: PixFmt,
    cell_h: u32,
) -> Result<()> {
    for p in 0..format.plane_count() {
        let plane_idx = p as u8;
        let rows = format.plane_height(cell_h, plane_idx) as usize;
        let Some(src_l) = left.plane(p) else { continue };
        let Some(src_r) = right.plane(p) else {
            continue;
        };
        let Some(mut dst) = out.plane_mut(p) else {
            continue;
        };
        for row in 0..rows {
            if let Some(src_row) = src_l.row(row)
                && let Some(dst_row) = dst.row_mut(row.saturating_mul(2))
            {
                let n = dst_row.len().min(src_row.len());
                if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
            if let Some(src_row) = src_r.row(row)
                && let Some(dst_row) = dst.row_mut(row.saturating_mul(2).saturating_add(1))
            {
                let n = dst_row.len().min(src_row.len());
                if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
        }
    }
    Ok(())
}

/// Column `2*i` from `left`, column `2*i + 1` from `right`, over `cell_w` x
/// `cell_h` source samples of each — measured layout, see this module's
/// doc.
fn interleave_columns(
    left: &Frame,
    right: &Frame,
    out: &mut Frame,
    format: PixFmt,
    cell_w: u32,
    cell_h: u32,
) -> Result<()> {
    for p in 0..format.plane_count() {
        let plane_idx = p as u8;
        let unit = geom::plane_unit_bytes(format, plane_idx)?;
        let cols = format.plane_width(cell_w, plane_idx) as usize;
        let rows = format.plane_height(cell_h, plane_idx) as usize;
        let Some(src_l) = left.plane(p) else { continue };
        let Some(src_r) = right.plane(p) else {
            continue;
        };
        let Some(mut dst) = out.plane_mut(p) else {
            continue;
        };
        for row in 0..rows {
            let Some(src_l_row) = src_l.row(row) else {
                continue;
            };
            let Some(src_r_row) = src_r.row(row) else {
                continue;
            };
            let Some(dst_row) = dst.row_mut(row) else {
                continue;
            };
            for col in 0..cols {
                let start = col.saturating_mul(unit);
                let dst_l = col.saturating_mul(2).saturating_mul(unit);
                let dst_r = col.saturating_mul(2).saturating_add(1).saturating_mul(unit);
                if let (Some(s), Some(d)) = (
                    src_l_row.get(start..start.saturating_add(unit)),
                    dst_row.get_mut(dst_l..dst_l.saturating_add(unit)),
                ) {
                    d.copy_from_slice(s);
                }
                if let (Some(s), Some(d)) = (
                    src_r_row.get(start..start.saturating_add(unit)),
                    dst_row.get_mut(dst_r..dst_r.saturating_add(unit)),
                ) {
                    d.copy_from_slice(s);
                }
            }
        }
    }
    Ok(())
}

impl PairedFilter for Framepack {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let (Some(left), Some(right)) = (ctx.input_link(0).cloned(), ctx.input_link(1).cloned())
        else {
            return Ok(());
        };
        let (
            LinkFormat::Video {
                format: lf,
                width: lw,
                height: lh,
                time_base: ltb,
                ..
            },
            LinkFormat::Video {
                format: rf,
                width: rw,
                height: rh,
                time_base: rtb,
                ..
            },
        ) = (&left, &right)
        else {
            return Err(Error::Unsupported("framepack: inputs must be video"));
        };
        if *ltb != *rtb {
            return Err(Error::Unsupported(
                "framepack: left and right inputs have different time bases",
            ));
        }
        if *lf != *rf {
            return Err(Error::Unsupported(
                "framepack: left and right inputs have different pixel formats",
            ));
        }
        geom::ensure_addressable(*lf)?;
        self.format = Some(*lf);
        let (out_w, out_h) = match self.layout {
            Layout::Sbs | Layout::Columns => (lw.saturating_add(*rw), (*lh).max(*rh)),
            Layout::Tab | Layout::Lines => ((*lw).max(*rw), lh.saturating_add(*rh)),
            Layout::Frameseq => (*lw, *lh),
        };
        let out_tb = if self.layout == Layout::Frameseq {
            Rational::new(ltb.num, ltb.den.saturating_mul(2))
        } else {
            *ltb
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                format: f,
                width: w,
                height: h,
                time_base: tb,
                ..
            } = &mut out
            {
                *f = *lf;
                *w = out_w;
                *h = out_h;
                *tb = out_tb;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let Some(format) = self.format else {
            return Ok(FrameOut::None);
        };
        let mut iter = inputs.into_iter();
        let Some(left) = iter.next() else {
            return Ok(FrameOut::None);
        };
        let Some(right) = iter.next() else {
            return Ok(FrameOut::One(left));
        };

        if self.layout == Layout::Frameseq {
            let FrameData::Video { .. } = left.data else {
                return Err(Error::InvalidData("framepack: left input is not video"));
            };
            let out_tb = Rational::new(left.time_base.num, left.time_base.den.saturating_mul(2));
            let mut views: SmallVec<[Frame; 4]> = SmallVec::new();
            for mut view in [left, right] {
                view.pts = vaco_core::Timestamp::new(self.seq);
                view.time_base = out_tb;
                view.set_duration_ticks(1);
                self.seq = self.seq.saturating_add(1);
                views.push(view);
            }
            return Ok(FrameOut::from_iter(views));
        }

        let FrameData::Video {
            width: out_w,
            height: out_h,
            ..
        } = left.data
        else {
            return Err(Error::InvalidData("framepack: left input is not video"));
        };
        let (out_w, out_h) = match self.layout {
            Layout::Sbs | Layout::Columns => {
                let FrameData::Video { width: rw, .. } = right.data else {
                    return Err(Error::InvalidData("framepack: right input is not video"));
                };
                (out_w.saturating_add(rw), out_h)
            }
            Layout::Tab | Layout::Lines => {
                let FrameData::Video { height: rh, .. } = right.data else {
                    return Err(Error::InvalidData("framepack: right input is not video"));
                };
                (out_w, out_h.saturating_add(rh))
            }
            Layout::Frameseq => (out_w, out_h),
        };
        let mut out = ctx
            .pool()
            .acquire_video(format, out_w.max(1), out_h.max(1))?;
        self.pack(format, &left, &right, &mut out)?;
        out.pts = left.pts;
        out.time_base = left.time_base;
        out.duration = left.duration;
        out.color = left.color;
        out.sample_aspect_ratio = left.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let layout = Layout::from_name(&opts.format)
        .ok_or_else(|| format!("framepack: bad `format` `{}`", opts.format))?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Paired::new(Framepack::new(layout))),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn gray_frame(pool: &vaco_frame::FramePool, width: u32, height: u32, value: u8) -> Frame {
        let mut f = pool.acquire_video(PixFmt::Gray8, width, height).unwrap();
        if let Some(mut plane) = f.plane_mut(0) {
            for row in plane.rows_mut() {
                for byte in row.iter_mut() {
                    *byte = value;
                }
            }
        }
        f
    }

    fn raw(f: &Frame, width: u32, height: u32) -> Vec<u8> {
        let plane = f.plane(0).unwrap();
        let mut out = Vec::new();
        for y in 0..height as usize {
            let row = plane.row(y).unwrap();
            out.extend_from_slice(&row[..width as usize]);
        }
        out
    }

    fn packed(layout: Layout, lw: u32, lh: u32, lv: u8, rw: u32, rh: u32, rv: u8) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let left = gray_frame(&pool, lw, lh, lv);
        let right = gray_frame(&pool, rw, rh, rv);
        let f = Framepack::new(layout);
        let (out_w, out_h) = match layout {
            Layout::Sbs | Layout::Columns => (lw + rw, lh.max(rh)),
            Layout::Tab | Layout::Lines => (lw.max(rw), lh + rh),
            Layout::Frameseq => (lw, lh),
        };
        let mut out = pool.acquire_video(PixFmt::Gray8, out_w, out_h).unwrap();
        f.pack(PixFmt::Gray8, &left, &right, &mut out).unwrap();
        out
    }

    /// Measured: `sbs` is a plain horizontal concatenation, left then right.
    #[test]
    fn sbs_is_left_then_right_horizontally() {
        let out = packed(Layout::Sbs, 4, 2, 0x10, 4, 2, 0x20);
        assert_eq!(
            raw(&out, 8, 2),
            vec![
                0x10, 0x10, 0x10, 0x10, 0x20, 0x20, 0x20, 0x20, 0x10, 0x10, 0x10, 0x10, 0x20, 0x20,
                0x20, 0x20
            ]
        );
    }

    /// Measured: `tab` is a plain vertical concatenation, left on top.
    #[test]
    fn tab_is_left_then_right_vertically() {
        let out = packed(Layout::Tab, 4, 2, 0x10, 4, 2, 0x20);
        assert_eq!(
            raw(&out, 4, 4),
            vec![
                0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
                0x20, 0x20
            ]
        );
    }

    /// Measured: `lines` alternates whole rows, left first.
    #[test]
    fn lines_alternates_rows_left_first() {
        let out = packed(Layout::Lines, 4, 2, 0x10, 4, 2, 0x20);
        assert_eq!(
            raw(&out, 4, 4),
            vec![
                0x10, 0x10, 0x10, 0x10, 0x20, 0x20, 0x20, 0x20, 0x10, 0x10, 0x10, 0x10, 0x20, 0x20,
                0x20, 0x20
            ]
        );
    }

    /// Measured: `columns` alternates whole columns, left first.
    #[test]
    fn columns_alternates_columns_left_first() {
        let out = packed(Layout::Columns, 4, 2, 0x10, 4, 2, 0x20);
        assert_eq!(
            raw(&out, 8, 2),
            vec![
                0x10, 0x20, 0x10, 0x20, 0x10, 0x20, 0x10, 0x20, 0x10, 0x20, 0x10, 0x20, 0x10, 0x20,
                0x10, 0x20
            ]
        );
    }

    /// `lines` is `tab`'s output with rows shuffled; `columns` is `sbs`'s
    /// with columns shuffled — the same size relationship this module's
    /// doc measures.
    #[test]
    fn lines_and_columns_are_the_same_size_as_tab_and_sbs() {
        let tab = packed(Layout::Tab, 4, 2, 0x10, 4, 2, 0x20);
        let lines = packed(Layout::Lines, 4, 2, 0x10, 4, 2, 0x20);
        let sbs = packed(Layout::Sbs, 4, 2, 0x10, 4, 2, 0x20);
        let columns = packed(Layout::Columns, 4, 2, 0x10, 4, 2, 0x20);
        let FrameData::Video {
            width: tw,
            height: th,
            ..
        } = tab.data
        else {
            unreachable!()
        };
        let FrameData::Video {
            width: lwv,
            height: lhv,
            ..
        } = lines.data
        else {
            unreachable!()
        };
        assert_eq!((tw, th), (lwv, lhv));
        let FrameData::Video {
            width: sw,
            height: sh,
            ..
        } = sbs.data
        else {
            unreachable!()
        };
        let FrameData::Video {
            width: cw,
            height: ch,
            ..
        } = columns.data
        else {
            unreachable!()
        };
        assert_eq!((sw, sh), (cw, ch));
    }

    #[test]
    fn format_names_and_numeric_codes_both_parse() {
        for (name, code) in [
            ("sbs", "1"),
            ("tab", "2"),
            ("frameseq", "3"),
            ("lines", "6"),
            ("columns", "7"),
        ] {
            assert_eq!(Layout::from_name(name), Layout::from_name(code));
            assert!(Layout::from_name(name).is_some());
        }
        assert_eq!(Layout::from_name("nonsense"), None);
    }
}
