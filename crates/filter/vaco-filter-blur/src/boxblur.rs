//! `boxblur` — repeated box (moving-average) blur, independently per plane
//! group.
//!
//! `ffmpeg -h filter=boxblur` documents six options in three
//! luma/chroma/alpha pairs: `luma_radius`/`lr` (string, default `"2"`),
//! `luma_power`/`lp` (int, default `2`), and the same two for `chroma_*`
//! (radius default empty, power default `-1`) and `alpha_*` (same defaults).
//! A `-1` power or an empty radius means "inherit the luma setting", per the
//! reference's own option help ("How many times..." with no independent
//! default documented for chroma/alpha beyond the sentinel).
//!
//! # What is implemented versus the reference's string radius
//!
//! The reference's `luma_radius` is a full expression (it accepts things
//! like `min(h,w)/10`); this crate parses it as a plain non-negative
//! integer only. Documented gap, not a silent guess — see
//! `docs/filter/vaco-filter-blur.md`.
//!
//! # Measured: replicate border, round-to-nearest
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=5x5,format=gray8,geq=lum='if(eq(X,0)*eq(Y,0),255,0)'" \
//!   -vf "boxblur=luma_radius=1:luma_power=1" -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! Corner pixel comes out `113` (not `28 = 255/9`, the zero-padded answer):
//! the 3x3 window around `(0,0)` sees four replicated copies of the corner
//! itself, `4*255/9 = 113.3 -> 113`. Confirmed at three more positions; see
//! [`crate::common::box_pass`]'s test of the same name. This is the
//! opposite convention from [`crate::edge`]'s convolution family, which
//! forces a hard zero at any pixel whose kernel would read out of bounds —
//! two different filters, two different measured border rules, not one
//! guessed rule applied everywhere.
//!
//! # Measured: `boxblur` rounds to nearest, `avgblur` truncates
//!
//! The same corner-impulse probe against `avgblur=sizeX=1` gives `113, 56,
//! ..` where `boxblur` gives `113, 57, ..` at position `(0,1)` — the
//! difference is `510/9 = 56.67`, which `boxblur` rounds up and `avgblur`
//! truncates down. See [`crate::avgblur`]'s doc for its own probe.

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
    name: "boxblur",
    description: "Blur the input",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "boxblur", help = "Blur the input")]
pub(crate) struct Opts {
    #[opt(
        name = "luma_radius",
        alias = "lr",
        help = "Radius of the luma blurring box",
        default = "2".to_owned(),
        flags(video, filtering)
    )]
    pub luma_radius: String,
    #[opt(
        name = "luma_power",
        alias = "lp",
        help = "How many times should the boxblur be applied to luma",
        default = 2,
        range = 0..=1024,
        flags(video, filtering)
    )]
    pub luma_power: i32,
    #[opt(
        name = "chroma_radius",
        alias = "cr",
        help = "Radius of the chroma blurring box",
        default = String::new(),
        flags(video, filtering)
    )]
    pub chroma_radius: String,
    #[opt(
        name = "chroma_power",
        alias = "cp",
        help = "How many times should the boxblur be applied to chroma",
        default = -1,
        range = -1..=1024,
        flags(video, filtering)
    )]
    pub chroma_power: i32,
    #[opt(
        name = "alpha_radius",
        alias = "ar",
        help = "Radius of the alpha blurring box",
        default = String::new(),
        flags(video, filtering)
    )]
    pub alpha_radius: String,
    #[opt(
        name = "alpha_power",
        alias = "ap",
        help = "How many times should the boxblur be applied to alpha",
        default = -1,
        range = -1..=1024,
        flags(video, filtering)
    )]
    pub alpha_power: i32,
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

fn parse_radius(s: &str) -> std::result::Result<i32, String> {
    s.trim().parse::<i32>().map_err(|_| {
        format!("boxblur: bad radius expression `{s}` (only plain integers are implemented)")
    })
}

/// Resolved radius/power for one plane group.
#[derive(Debug, Clone, Copy)]
struct PlaneParams {
    radius: i32,
    power: i32,
}

#[derive(Debug)]
pub(crate) struct Filter {
    luma: PlaneParams,
    chroma: PlaneParams,
    alpha: PlaneParams,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let luma_radius = parse_radius(&opts.luma_radius)?;
        let luma = PlaneParams {
            radius: luma_radius,
            power: opts.luma_power,
        };
        let chroma_radius = if opts.chroma_radius.trim().is_empty() {
            luma_radius
        } else {
            parse_radius(&opts.chroma_radius)?
        };
        let chroma = PlaneParams {
            radius: chroma_radius,
            power: if opts.chroma_power < 0 {
                opts.luma_power
            } else {
                opts.chroma_power
            },
        };
        let alpha_radius = if opts.alpha_radius.trim().is_empty() {
            luma_radius
        } else {
            parse_radius(&opts.alpha_radius)?
        };
        let alpha = PlaneParams {
            radius: alpha_radius,
            power: if opts.alpha_power < 0 {
                opts.luma_power
            } else {
                opts.alpha_power
            },
        };
        Ok(Self {
            luma,
            chroma,
            alpha,
        })
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

fn blur_plane(rows: &[&[u8]], w: i32, h: i32, params: PlaneParams) -> Vec<Vec<u8>> {
    if params.radius <= 0 || params.power <= 0 {
        return rows.iter().map(|r| r.to_vec()).collect();
    }
    let mut current: Vec<Vec<u8>> = rows.iter().map(|r| r.to_vec()).collect();
    for _ in 0..params.power {
        let borrowed: Vec<&[u8]> = current.iter().map(Vec::as_slice).collect();
        current = common::box_pass(
            &borrowed,
            w,
            h,
            params.radius,
            params.radius,
            Rounding::Nearest,
        );
    }
    current
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
            let blurred = blur_plane(&rows, pw, ph, params);
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in blurred.iter().enumerate() {
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
    let filter = Filter::new(&opts)?;
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

    #[test]
    fn default_radius_and_power_parse() {
        let opts = Opts::default();
        assert_eq!(opts.luma_radius, "2");
        assert_eq!(opts.luma_power, 2);
        let filter = Filter::new(&opts).unwrap();
        assert_eq!(filter.luma.radius, 2);
        assert_eq!(filter.luma.power, 2);
        // chroma/alpha inherit luma when unset.
        assert_eq!(filter.chroma.radius, 2);
        assert_eq!(filter.chroma.power, 2);
    }

    #[test]
    fn zero_radius_is_identity() {
        let opts = Opts {
            luma_radius: "0".to_owned(),
            ..Opts::default()
        };
        let filter = Filter::new(&opts).unwrap();
        let row0: &[u8] = &[1, 2, 3];
        let rows: [&[u8]; 1] = [row0];
        let out = blur_plane(&rows, 3, 1, filter.luma);
        assert_eq!(out[0], vec![1, 2, 3]);
    }

    /// An independent oracle that does not re-derive `box_pass`: a uniform
    /// (DC) plane must come back out exactly unchanged by any number of box
    /// blur passes, because the average of a constant is that constant. This
    /// holds regardless of radius, power, or the border convention — it is a
    /// property the *output* must have, not a second implementation of the
    /// same formula (the class of oracle `AGENT-CONSTRAINTS.md` warns a
    /// from-scratch transcription cannot be).
    #[test]
    fn a_constant_plane_is_a_fixed_point_of_any_number_of_passes() {
        for radius in [1, 2, 5] {
            for power in [1, 3] {
                let opts = Opts {
                    luma_radius: radius.to_string(),
                    luma_power: power,
                    ..Opts::default()
                };
                let filter = Filter::new(&opts).unwrap();
                let rows_owned = vec![vec![200u8; 9]; 9];
                let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
                let out = blur_plane(&rows, 9, 9, filter.luma);
                for row in out {
                    assert!(row.iter().all(|&v| v == 200));
                }
            }
        }
    }
}
