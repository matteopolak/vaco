//! `colorlevels` — remap each RGB(A) channel's input range to an output
//! range, clamping outside it.
//!
//! `ffmpeg -h filter=colorlevels` documents four `{r,g,b,a}imin`/
//! `{r,g,b,a}imax` (input black/white points, `-1..1`, default `0`/`1`)
//! and `{r,g,b,a}omin`/`{r,g,b,a}omax` (output black/white points,
//! `0..1`, default `0`/`1`) per channel, plus `preserve` (colour-
//! preservation mode, default `none`).
//!
//! Four probes on `rgb24` (`romin=0.2:romax=0.8` and `rimin=0.2:
//! rimax=0.8`, varied R):
//!
//! ```text
//! t = clamp((in/max - imin) / (imax - imin), 0, 1)
//! out = floor((omin + t * (omax - omin)) * max)
//! ```
//!
//! confirmed at an input white point (`R=255` under `romin=0.2:romax=0.8`
//! measures `0xcc = 0.2 + 0.8*0.6`), an input value clamping *past* the
//! input white point (`R=255` under `rimin=0.2:rimax=0.8` still measures
//! `255`, not something beyond it), and exactly at the input black point
//! (`R=51 = 0.2*255` under the same options measures `0`). The
//! `floor`-not-`round` rule this crate's sibling `vaco-filter-lut`
//! measured independently is consistent here too (the interior probe,
//! `R=128`, landed on an exact integer so it does not disambiguate on its
//! own, but nothing contradicts it either).
//!
//! ```text
//! ffmpeg -f lavfi -i "color=red:s=2x2,format=yuv420p" -vf colorlevels=romin=0.2 -f null -
//! # -> forces an rgb24 conversion, same restriction as colorkey/lut3d.
//! ```
//!
//! The seven colour-preservation modes need the same reverse-engineering
//! this crate already declined for `colorchannelmixer`'s `pc`/`pa` — the
//! reference does not document the blending formula in `-h` output.
//! `preserve` is parsed and validated but has no effect.

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
    name: "colorlevels",
    description: "Adjust the color levels",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "colorlevels", help = "Adjust the color levels")]
pub(crate) struct Opts {
    #[opt(name = "rimin", help = "set input red black point", default = 0.0, range = -1.0..=1.0, flags(video, filtering))]
    pub rimin: f64,
    #[opt(name = "gimin", help = "set input green black point", default = 0.0, range = -1.0..=1.0, flags(video, filtering))]
    pub gimin: f64,
    #[opt(name = "bimin", help = "set input blue black point", default = 0.0, range = -1.0..=1.0, flags(video, filtering))]
    pub bimin: f64,
    #[opt(name = "aimin", help = "set input alpha black point", default = 0.0, range = -1.0..=1.0, flags(video, filtering))]
    pub aimin: f64,
    #[opt(name = "rimax", help = "set input red white point", default = 1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub rimax: f64,
    #[opt(name = "gimax", help = "set input green white point", default = 1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub gimax: f64,
    #[opt(name = "bimax", help = "set input blue white point", default = 1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub bimax: f64,
    #[opt(name = "aimax", help = "set input alpha white point", default = 1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub aimax: f64,
    #[opt(name = "romin", help = "set output red black point", default = 0.0, range = 0.0..=1.0, flags(video, filtering))]
    pub romin: f64,
    #[opt(name = "gomin", help = "set output green black point", default = 0.0, range = 0.0..=1.0, flags(video, filtering))]
    pub gomin: f64,
    #[opt(name = "bomin", help = "set output blue black point", default = 0.0, range = 0.0..=1.0, flags(video, filtering))]
    pub bomin: f64,
    #[opt(name = "aomin", help = "set output alpha black point", default = 0.0, range = 0.0..=1.0, flags(video, filtering))]
    pub aomin: f64,
    #[opt(name = "romax", help = "set output red white point", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub romax: f64,
    #[opt(name = "gomax", help = "set output green white point", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub gomax: f64,
    #[opt(name = "bomax", help = "set output blue white point", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub bomax: f64,
    #[opt(name = "aomax", help = "set output alpha white point", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub aomax: f64,
    #[opt(
        name = "preserve",
        help = "set preserve color mode (not implemented; parsed only)",
        unit = "preserve_color",
        consts = crate::common::PRESERVE_COLOR_CONSTS,
        default = 0,
        range = 0..=6,
        flags(video, filtering)
    )]
    pub preserve: i32,
}

/// One channel's `(imin, imax, omin, omax)`.
type Range = (f64, f64, f64, f64);

#[derive(Debug)]
pub(crate) struct Filter {
    /// `ranges[channel]`, in positional order (R/G/B/A or Y/U/V/A).
    ranges: [Range; 4],
}

impl Filter {
    fn new(o: &Opts) -> Self {
        Self {
            ranges: [
                (o.rimin, o.rimax, o.romin, o.romax),
                (o.gimin, o.gimax, o.gomin, o.gomax),
                (o.bimin, o.bimax, o.bomin, o.bomax),
                (o.aimin, o.aimax, o.aomin, o.aomax),
            ],
        }
    }

    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if !format.is_rgb() || !sample::is_addressable(format) {
            return;
        }
        let big_endian = format.is_big_endian();
        let n = format.component_count().min(4);
        for ch in 0..n {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let Some(&(imin, imax, omin, omax)) = self.ranges.get(ch) else {
                continue;
            };
            let max = f64::from(sample::max_value(comp));
            let denom = imax - imin;
            let Some(mut plane) = input.plane_mut(comp.plane as usize) else {
                continue;
            };
            let w = plane
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..plane.rows() {
                let Some(row) = plane.row_mut(y) else {
                    continue;
                };
                for x in 0..w {
                    let v = f64::from(sample::read(row, x, comp, big_endian)) / max;
                    let t = if denom.abs() < 1e-12 {
                        0.0
                    } else {
                        ((v - imin) / denom).clamp(0.0, 1.0)
                    };
                    let out = (omin + t * (omax - omin)) * max;
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "out clamped to [0, max] and max fits u16 by construction; truncation is the measured reference rule"
                    )]
                    let out_v = out.clamp(0.0, max) as u16;
                    sample::write(row, x, comp, big_endian, out_v);
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.apply_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    if opts.preserve != 0 {
        return Err(
            "colorlevels: `preserve` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string(),
        );
    }
    let set = FormatSet::video_list(common::formats_where(|f| {
        f.is_rgb() && sample::is_addressable(f)
    }));
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

    fn opts_with_output_range(romin: f64, romax: f64) -> Opts {
        Opts {
            romin,
            romax,
            ..Opts::default()
        }
    }

    #[test]
    fn identity_range_is_a_no_op() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 77;
            row[1] = 133;
            row[2] = 210;
        }
        let f = Filter::new(&Opts::default());
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[77, 133, 210]);
    }

    #[test]
    fn measured_against_the_reference_output_range() {
        // Measured: ffmpeg 8.1, colorlevels=romin=0.2:romax=0.8 on rgb24
        // 0xff0000 (this module's doc).
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 255;
            row[1] = 0;
            row[2] = 0;
        }
        let f = Filter::new(&opts_with_output_range(0.2, 0.8));
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 0xcc);
    }

    #[test]
    fn measured_against_the_reference_input_range_clips() {
        // Measured: rimin=0.2:rimax=0.8 on R=51 (=0.2*255, the input black
        // point) maps to 0; R=255 (past the input white point) clips to
        // 255, not something beyond it.
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        let o = Opts {
            rimin: 0.2,
            rimax: 0.8,
            ..Opts::default()
        };
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 51;
        }
        let f = Filter::new(&o);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 0);

        let mut frame2 = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        frame2.plane_mut(0).unwrap().row_mut(0).unwrap()[0] = 255;
        f.apply_frame(&mut frame2);
        assert_eq!(frame2.plane(0).unwrap().row(0).unwrap()[0], 255);
    }
}
