//! `smptebars` (SD, 7 bars + reversal + PLUGE) and `smptehdbars` (ARIB/RP
//! 219-style HD bars + grey ramp + PLUGE), in `yuv420p`.
//!
//! `ffmpeg -h filter=smptebars`/`smptehdbars` document only `size`/`s`,
//! `rate`/`r`, `duration`/`d` and `sar` — the bar pattern itself is fixed,
//! same shape as the sibling `vaco-filter-video-source` crate's
//! `pal100bars`/`pal75bars` (its `bars.rs` module is this one's model).
//!
//! # The layout (measured, not read)
//!
//! Both patterns were probed at the reference's own default 320×240
//! (`ffmpeg -f lavfi -i smptebars=size=320x240 -f rawvideo -pix_fmt
//! yuv420p -frames:v 1 -`, and the HD variant) and every row band's exact
//! `Y`/`Cb`/`Cr` triple recorded per segment.
//!
//! **`smptebars`**: rows `0..2h/3` are 7 equal-width 75%-amplitude colour
//! bars (white, yellow, cyan, green, magenta, red, blue); rows
//! `2h/3..2h/3+h/12` are the same 7 bars in reverse brightness order
//! separated by black; the bottom quarter is the classic PLUGE row
//! (`-I`, white, `+Q`, black, blacker-than-black, black, whiter-than-black,
//! black) at measured, unequal segment widths.
//!
//! **`smptehdbars`**: rows `0..7h/12` are 40% grey flanking 7 full-amplitude
//! colour bars; the next `h/12` rows are a narrower blue-check row; the
//! next `h/12` rows are a linear luma ramp (`Y` from 0 to 252 across its
//! span, `Cb = Cr = 128`); the bottom quarter is a PLUGE row.
//!
//! **Exact at the measured 320×240 default.** Segment boundaries at other
//! sizes are this crate's own proportional scaling of the measured
//! fractions (`boundary(w) = round(measured_boundary * w / 320)`), not
//! independently re-measured — see `docs/filter/vaco-filter-source.md`.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration(-1);
type Yuv = (u8, u8, u8);

/// Scales a boundary measured at `320` wide to width `w`.
#[allow(
    clippy::integer_division,
    reason = "segment/row boundaries are all deliberate floor divisions of pixel position"
)]
fn scale(measured: u32, w: u32) -> u32 {
    ((u64::from(measured) * u64::from(w)) / 320) as u32
}

// ------------------------------------------------------------- smptebars

const SD_TOP: [Yuv; 7] = [
    (180, 128, 128),
    (162, 44, 142),
    (131, 156, 44),
    (112, 72, 58),
    (84, 184, 198),
    (65, 100, 212),
    (35, 212, 114),
];
const SD_REVERSAL: [Yuv; 7] = [
    (35, 212, 114),
    (19, 128, 128),
    (84, 184, 198),
    (19, 128, 128),
    (131, 156, 44),
    (19, 128, 128),
    (180, 128, 128),
];
// (end_x at w=320, value), covering the PLUGE row left to right.
const SD_PLUGE: [(u32, Yuv); 8] = [
    (57, (57, 156, 97)),
    (115, (235, 128, 128)),
    (173, (44, 171, 147)),
    (229, (16, 128, 128)),
    (245, (7, 128, 128)),
    (261, (16, 128, 128)),
    (277, (24, 128, 128)),
    (319, (16, 128, 128)),
];

#[allow(
    clippy::integer_division,
    reason = "segment/row boundaries are all deliberate floor divisions of pixel position"
)]
fn sd_pixel(x: u32, y: u32, w: u32, h: u32) -> Yuv {
    let top_h = h * 2 / 3;
    let reversal_h = top_h + h / 12;
    #[allow(clippy::cast_possible_truncation, reason = "index in 0..7")]
    let seg7 = ((u64::from(x) * 7) / u64::from(w.max(1))).min(6) as usize;
    if y < top_h {
        SD_TOP.get(seg7).copied().unwrap_or((0, 128, 128))
    } else if y < reversal_h {
        SD_REVERSAL.get(seg7).copied().unwrap_or((0, 128, 128))
    } else {
        for &(measured_end, v) in &SD_PLUGE {
            if x <= scale(measured_end, w) {
                return v;
            }
        }
        SD_PLUGE.last().map_or((0, 128, 128), |&(_, v)| v)
    }
}

pub(crate) fn sd_frame_pixel(x: u32, y: u32, w: u32, h: u32) -> Yuv {
    sd_pixel(x, y, w.max(1), h.max(1))
}

// ----------------------------------------------------------- smptehdbars

const HD_GREY: Yuv = (104, 128, 128);
const HD_BARS: [Yuv; 7] = [
    (180, 128, 128),
    (168, 44, 136),
    (145, 147, 44),
    (133, 63, 52),
    (63, 193, 204),
    (51, 109, 212),
    (28, 212, 120),
];
const HD_ROW2: [(u32, Yuv); 4] = [(39, (188, 128, 128)), (73, (57, 128, 128)), (277, (180, 128, 128)), (319, (32, 128, 128))];
const HD_RAMP_START: u32 = 74;
const HD_RAMP_END: u32 = 277;
const HD_RAMP_LEFT_A: (u32, Yuv) = (39, (219, 128, 128));
const HD_RAMP_LEFT_B: (u32, Yuv) = (73, (44, 128, 128));
const HD_RAMP_RIGHT_TAIL: Yuv = (63, 128, 128);
const HD_PLUGE: [(u32, Yuv); 11] = [
    (39, (49, 128, 128)),
    (91, (16, 128, 128)),
    (159, (235, 128, 128)),
    (187, (16, 128, 128)),
    (199, (12, 128, 128)),
    (211, (16, 128, 128)),
    (223, (20, 128, 128)),
    (235, (16, 128, 128)),
    (247, (25, 128, 128)),
    (277, (16, 128, 128)),
    (319, (49, 128, 128)),
];

#[allow(
    clippy::integer_division,
    reason = "segment/row boundaries are all deliberate floor divisions of pixel position"
)]
fn hd_pixel(x: u32, y: u32, w: u32, h: u32) -> Yuv {
    let bars_h = h * 7 / 12;
    let row2_h = bars_h + h / 12;
    let ramp_h = row2_h + h / 12;
    let (grey_end, tail_start) = (scale(39, w), scale(278, w));
    if y < bars_h {
        if x <= grey_end || x >= tail_start {
            return HD_GREY;
        }
        let span = tail_start.saturating_sub(grey_end).max(1);
        #[allow(clippy::cast_possible_truncation, reason = "index in 0..7")]
        let seg7 = ((u64::from(x - grey_end) * 7) / u64::from(span)).min(6) as usize;
        return HD_BARS.get(seg7).copied().unwrap_or(HD_GREY);
    }
    if y < row2_h {
        for &(measured_end, v) in &HD_ROW2 {
            if x <= scale(measured_end, w) {
                return v;
            }
        }
        return HD_ROW2.last().map_or(HD_GREY, |&(_, v)| v);
    }
    if y < ramp_h {
        let start = scale(HD_RAMP_START, w);
        let end = scale(HD_RAMP_END, w);
        if x <= scale(HD_RAMP_LEFT_A.0, w) {
            return HD_RAMP_LEFT_A.1;
        }
        if x < start {
            return HD_RAMP_LEFT_B.1;
        }
        if x > end {
            return HD_RAMP_RIGHT_TAIL;
        }
        let span = end.saturating_sub(start).max(1);
        let pair_count = (span / 2).max(1);
        let pair_idx = (x - start) / 2;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "value stays within u8 range by construction"
        )]
        let v = ((u64::from(pair_idx) * 252) / u64::from(pair_count)) as u8;
        return (v, 128, 128);
    }
    for &(measured_end, v) in &HD_PLUGE {
        if x <= scale(measured_end, w) {
            return v;
        }
    }
    HD_PLUGE.last().map_or(HD_GREY, |&(_, v)| v)
}

pub(crate) fn hd_frame_pixel(x: u32, y: u32, w: u32, h: u32) -> Yuv {
    hd_pixel(x, y, w.max(1), h.max(1))
}

// ------------------------------------------------------------- shared

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "smptebars", help = "generate SMPTE color bars")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    hd: bool,
    frame_rate: Rational,
    sar: Rational,
    total_frames: Option<u64>,
    next: i64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width,
                height,
                time_base,
                frame_rate,
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
                *sample_aspect_ratio = self.sar;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_frames.is_some_and(|n| self.next as u64 >= n) {
            return Ok(None);
        }
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Yuv420p, self.width, self.height)?;
        let (w, h) = (self.width, self.height);
        let pick = if self.hd { hd_frame_pixel } else { sd_frame_pixel };
        if let Some(mut plane) = frame.plane_mut(0) {
            for row_idx in 0..plane.rows() {
                #[allow(clippy::cast_possible_truncation, reason = "row < h")]
                let yy = row_idx as u32;
                if let Some(row) = plane.row_mut(row_idx) {
                    for (x, px) in row.iter_mut().enumerate() {
                        #[allow(clippy::cast_possible_truncation, reason = "x < w")]
                        let xx = x as u32;
                        *px = pick(xx, yy, w, h).0;
                    }
                }
            }
        }
        // Chroma planes, subsampled 2x2.
        for (plane_idx, pick_component) in [(1usize, 1usize), (2, 2)] {
            if let Some(mut plane) = frame.plane_mut(plane_idx) {
                for row_idx in 0..plane.rows() {
                    #[allow(clippy::cast_possible_truncation, reason = "row < h/2")]
                    let yy = (row_idx as u32) * 2;
                    if let Some(row) = plane.row_mut(row_idx) {
                        for (x, px) in row.iter_mut().enumerate() {
                            #[allow(clippy::cast_possible_truncation, reason = "x < w/2")]
                            let xx = (x as u32) * 2;
                            let yuv = pick(xx, yy, w, h);
                            *px = if pick_component == 1 { yuv.1 } else { yuv.2 };
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.duration = vaco_core::Duration(1);
        frame.sample_aspect_ratio = self.sar;
        self.next = self.next.saturating_add(1);
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(self.next)
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

fn make_instance(req: &Instantiate<'_>, desc: FilterDesc, hd: bool) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    let rate = opts.rate.0;
    let total_frames = if opts.duration.0 < 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * rate.to_f64())
                .round()
                .max(0.0) as u64,
        )
    };
    let source = Source {
        width,
        height,
        hd,
        frame_rate: rate,
        sar: opts.sar,
        total_frames,
        next: 0,
    };
    Ok(Instance {
        desc,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Yuv420p)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

pub mod sd {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, make_instance};

    pub const DESC: FilterDesc = FilterDesc {
        name: "smptebars",
        description: "Generate SMPTE color bars",
        inputs: &[],
        outputs: &[Pad {
            name: "default",
            media_type: MediaType::Video,
        }],
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        make_instance(req, DESC, false)
    }
}

pub mod hd {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, make_instance};

    pub const DESC: FilterDesc = FilterDesc {
        name: "smptehdbars",
        description: "Generate SMPTE HD color bars",
        inputs: &[],
        outputs: &[Pad {
            name: "default",
            media_type: MediaType::Video,
        }],
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        make_instance(req, DESC, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sd_top_bars_match_measured_reference() {
        assert_eq!(sd_frame_pixel(10, 0, 320, 240), (180, 128, 128));
        assert_eq!(sd_frame_pixel(310, 0, 320, 240), (35, 212, 114));
    }

    #[test]
    fn sd_pluge_matches_measured_reference() {
        assert_eq!(sd_frame_pixel(70, 200, 320, 240), (235, 128, 128));
        assert_eq!(sd_frame_pixel(240, 200, 320, 240), (7, 128, 128));
    }

    #[test]
    fn hd_top_bars_match_measured_reference() {
        assert_eq!(hd_frame_pixel(10, 0, 320, 240), (104, 128, 128));
        assert_eq!(hd_frame_pixel(60, 0, 320, 240), (180, 128, 128));
    }

    #[test]
    fn hd_ramp_endpoints_match_measured_reference() {
        assert_eq!(hd_frame_pixel(74, 160, 320, 240).0, 0);
        // Just short of the end, per the measured pair-index formula.
        let last = hd_frame_pixel(276, 160, 320, 240).0;
        assert!(last > 240, "ramp should be near its top end: {last}");
    }

    #[test]
    fn creatable_with_no_arguments() {
        for name in ["smptebars", "smptehdbars"] {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            let desc = if name == "smptebars" { sd::DESC } else { hd::DESC };
            assert!(make_instance(&req, desc, name == "smptehdbars").is_ok());
        }
    }
}
