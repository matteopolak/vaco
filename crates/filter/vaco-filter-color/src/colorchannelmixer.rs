//! `colorchannelmixer` — mix components through a 4x4 gain matrix.
//!
//! `ffmpeg -h filter=colorchannelmixer` documents sixteen gains (`rr`..`aa`,
//! each -2..2, default the identity matrix) plus `pc` (preserve-colour mode,
//! default `none`) and `pa` (preserve-colour amount, default 0).
//!
//! # Measured: the matrix applies positionally, not just to RGB
//!
//! ```text
//! ffmpeg -f lavfi -i "color=red,format=yuv420p" -vf colorchannelmixer=rr=1 -f null -
//! # -> stays yuv420p; no conversion to RGB happens.
//! ```
//!
//! So `rr`/`rg`/`rb`/`ra` are really "channel 0's output gains from channels
//! 0/1/2/3", named for the RGB case but applied the same way to Y/U/V/A.
//! This crate's [`sample`] module already treats channels positionally, so
//! this filter needs no format restriction beyond addressability.
//!
//! # Measured: the formula, from a controlled probe
//!
//! `color=0x006400` (R=0,G=100,B=0) through `rg=1:rr=0` (identity
//! otherwise) produces `(100,100,0)`: `out_r = rr*R + rg*G + rb*B + ra*A`,
//! gains applied to the raw sample value directly (no 0..1 normalisation),
//! then rounded and clamped to the component's range.
//!
//! # Not implemented: `pc`/`pa`
//!
//! The seven preserve-colour modes (`lum`/`max`/`avg`/`sum`/`nrm`/`pwr`)
//! blend the mixed result back toward a luminance/energy-preserving
//! variant, and the reference does not document the blending formula in
//! `-h` output. Reproducing it exactly would mean either reading the
//! reference's source (D7 forbids that) or an extensive per-mode probing
//! pass out of scope for this crate's time budget. `pc`/`pa` are parsed and
//! validated but have no effect — matching this crate's existing precedent
//! for a parsed-but-inert option (`vaco-filter-video-format::format`'s
//! `color_spaces`/`color_ranges`/`alpha_modes`).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "colorchannelmixer",
    description: "Adjust colors by mixing color channels",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "colorchannelmixer", help = "Adjust colors by mixing color channels")]
pub(crate) struct Opts {
    #[opt(name = "rr", help = "set the red gain for the red channel", default = 1.0, range = -2.0..=2.0, flags(video, filtering))]
    pub rr: f64,
    #[opt(name = "rg", help = "set the green gain for the red channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub rg: f64,
    #[opt(name = "rb", help = "set the blue gain for the red channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub rb: f64,
    #[opt(name = "ra", help = "set the alpha gain for the red channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub ra: f64,
    #[opt(name = "gr", help = "set the red gain for the green channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub gr: f64,
    #[opt(name = "gg", help = "set the green gain for the green channel", default = 1.0, range = -2.0..=2.0, flags(video, filtering))]
    pub gg: f64,
    #[opt(name = "gb", help = "set the blue gain for the green channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub gb: f64,
    #[opt(name = "ga", help = "set the alpha gain for the green channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub ga: f64,
    #[opt(name = "br", help = "set the red gain for the blue channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub br: f64,
    #[opt(name = "bg", help = "set the green gain for the blue channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub bg: f64,
    #[opt(name = "bb", help = "set the blue gain for the blue channel", default = 1.0, range = -2.0..=2.0, flags(video, filtering))]
    pub bb: f64,
    #[opt(name = "ba", help = "set the alpha gain for the blue channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub ba: f64,
    #[opt(name = "ar", help = "set the red gain for the alpha channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub ar: f64,
    #[opt(name = "ag", help = "set the green gain for the alpha channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub ag: f64,
    #[opt(name = "ab", help = "set the blue gain for the alpha channel", default = 0.0, range = -2.0..=2.0, flags(video, filtering))]
    pub ab: f64,
    #[opt(name = "aa", help = "set the alpha gain for the alpha channel", default = 1.0, range = -2.0..=2.0, flags(video, filtering))]
    pub aa: f64,
    #[opt(
        name = "pc",
        help = "set the preserve color mode (not implemented; parsed only)",
        unit = "preserve_color",
        consts = crate::common::PRESERVE_COLOR_CONSTS,
        default = 0,
        range = 0..=6,
        flags(video, filtering)
    )]
    pub pc: i32,
    #[opt(
        name = "pa",
        help = "set the preserve color amount (not implemented; parsed only)",
        default = 0.0,
        range = 0.0..=1.0,
        flags(video, filtering)
    )]
    pub pa: f64,
}

/// One output channel's gains from input channels 0..3, in that order.
type Row = [f64; 4];

#[derive(Debug)]
pub(crate) struct Filter {
    /// `rows[out_channel] = [gain_from_0, gain_from_1, gain_from_2, gain_from_3]`.
    rows: [Row; 4],
}

impl Filter {
    fn new(o: &Opts) -> Self {
        Self {
            rows: [
                [o.rr, o.rg, o.rb, o.ra],
                [o.gr, o.gg, o.gb, o.ga],
                [o.br, o.bg, o.bb, o.ba],
                [o.ar, o.ag, o.ab, o.aa],
            ],
        }
    }

    fn mix_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if !sample::is_addressable(format) {
            return;
        }
        let n = format.component_count().min(4);
        let in_comps: Vec<_> = (0..n).filter_map(|c| sample::component(format, c)).collect();
        let big_endian = format.is_big_endian();
        // Snapshot every input channel's samples before writing any of them:
        // an in-place mix must not let channel 0's new value feed channel
        // 1's computation when they happen to share a plane's bytes.
        let mut originals: Vec<Vec<Vec<u16>>> = Vec::new();
        for &comp in &in_comps {
            let Some(plane) = input.plane(comp.plane as usize) else {
                originals.push(Vec::new());
                continue;
            };
            let rows = plane.rows();
            let width = row_width(plane.row_bytes(), comp.step);
            let mut rows_out = Vec::new();
            for y in 0..rows {
                let Some(row) = plane.row(y) else { continue };
                rows_out.push((0..width).map(|x| sample::read(row, x, comp, big_endian)).collect());
            }
            originals.push(rows_out);
        }
        for (out_ch, comp) in in_comps.iter().enumerate() {
            let Some(&gains) = self.rows.get(out_ch) else {
                continue;
            };
            let comp = *comp;
            let max = f64::from(sample::max_value(comp));
            let Some(mut plane) = input.plane_mut(comp.plane as usize) else {
                continue;
            };
            let width = row_width(plane.row_bytes(), comp.step);
            for y in 0..plane.rows() {
                let Some(row) = plane.row_mut(y) else { continue };
                for x in 0..width {
                    let mut acc = 0.0f64;
                    for (src_ch, gain) in gains.iter().enumerate() {
                        let v = originals
                            .get(src_ch)
                            .and_then(|rows| rows.get(y))
                            .and_then(|r| r.get(x))
                            .copied()
                            .unwrap_or(0);
                        acc += gain * f64::from(v);
                    }
                    let clamped = acc.round().clamp(0.0, max);
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamped to [0, max] where max fits in u16 by construction"
                    )]
                    let out_v = clamped as u16;
                    sample::write(row, x, comp, big_endian, out_v);
                }
            }
        }
    }
}

fn row_width(row_bytes: usize, step: u8) -> usize {
    row_bytes.checked_div(usize::from(step.max(1))).unwrap_or(0)
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.mix_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter::new(&opts))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn opts(rr: f64, rg: f64) -> Opts {
        Opts {
            rr,
            rg,
            rb: 0.0,
            ra: 0.0,
            gr: 0.0,
            gg: 1.0,
            gb: 0.0,
            ga: 0.0,
            br: 0.0,
            bg: 0.0,
            bb: 1.0,
            ba: 0.0,
            ar: 0.0,
            ag: 0.0,
            ab: 0.0,
            aa: 1.0,
            pc: 0,
            pa: 0.0,
        }
    }

    #[test]
    fn hand_computed_matrix_on_a_single_pixel() {
        // Independent oracle: the formula documented above, computed by
        // hand for R=0,G=100,B=0 with rr=0,rg=1 -> out_r should be 100.
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 0; // R
            row[1] = 100; // G
            row[2] = 0; // B
        }
        let f = Filter::new(&opts(0.0, 1.0));
        f.mix_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[100, 100, 0]);
    }

    #[test]
    fn identity_matrix_is_a_no_op() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 12;
            row[1] = 34;
            row[2] = 56;
        }
        let f = Filter::new(&opts(1.0, 0.0));
        f.mix_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[12, 34, 56]);
    }

    #[test]
    fn overflow_clamps_rather_than_wraps() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 200;
            row[1] = 200;
            row[2] = 0;
        }
        let f = Filter::new(&opts(1.0, 1.0));
        f.mix_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row[0], 255);
    }
}
