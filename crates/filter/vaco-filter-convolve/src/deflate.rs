//! `deflate` — [`crate::inflate`]'s dual: shrink each pixel towards the
//! truncating average of its fixed 8-neighbourhood, capped by `threshold`.
//!
//! `ffmpeg -h filter=deflate` documents the identical `threshold0..3` option
//! set as `inflate` (`ffmpeg -h filter=deflate` even prints the shared
//! `deflate/inflate AVOptions:` header) — no `coordinates` option.
//!
//! # Measured: shrinks, never grows
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=5x5,format=gray8,geq=lum='if(eq(X,2)*eq(Y,2),100,50)'" \
//!   -vf deflate -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! The centre (self `100`, all eight neighbours `50`, average `50`) comes
//! back `50` — the average is below self, so `deflate` pulls it down. Every
//! other pixel (self `50`, average of its own neighbourhood `56.25`
//! truncated `56`, which is *above* self) is left unchanged: `deflate` only
//! ever shrinks, the mirror image of [`crate::inflate`]'s "only ever grows".
//! Border and threshold behaviour are the same engine — see
//! [`crate::morph::apply_plane`]'s `DeflateAvg` arm and [`crate::inflate`]'s
//! doc for the measurements that pinned down truncation and the
//! cap-not-gate threshold rule.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::morph::{self, MorphParams, Op};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "deflate",
    description: "Apply deflate effect",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "deflate", help = "Apply deflate effect")]
pub(crate) struct Opts {
    #[opt(name = "threshold0", help = "set threshold for 1st plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold0: i32,
    #[opt(name = "threshold1", help = "set threshold for 2nd plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold1: i32,
    #[opt(name = "threshold2", help = "set threshold for 3rd plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold2: i32,
    #[opt(name = "threshold3", help = "set threshold for 4th plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold3: i32,
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

    fn threshold(&self, plane: u8) -> i32 {
        match plane {
            0 => self.threshold0,
            1 => self.threshold1,
            2 => self.threshold2,
            _ => self.threshold3,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Opts,
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
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let params = MorphParams {
                coordinates: 0,
                threshold: self.opts.threshold(p8),
            };
            let filtered = morph::apply_plane(&rows, pw, ph, Op::DeflateAvg, params);
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in filtered.iter().enumerate() {
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
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter { opts })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn plane(rows: &[&[u8]], w: i32, h: i32, threshold: i32) -> Vec<Vec<u8>> {
        morph::apply_plane(
            rows,
            w,
            h,
            Op::DeflateAvg,
            MorphParams {
                coordinates: 0,
                threshold,
            },
        )
    }

    /// The exact 5x5 probe from this module's doc.
    fn probe_image() -> Vec<Vec<u8>> {
        let mut img = vec![vec![50u8; 5]; 5];
        if let Some(row) = img.get_mut(2)
            && let Some(px) = row.get_mut(2)
        {
            *px = 100;
        }
        img
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn matches_the_reference_on_the_uniform_background_probe() {
        let img = probe_image();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 5, 5, 65535);
        assert_eq!(out[2][2], 50, "average of eight 50s is below self (100)");
        assert_eq!(out[1][1], 50, "average (56) is above self (50): no shrink");
    }

    /// Pinned: `threshold` caps the shrink rather than gating it.
    #[test]
    fn threshold_caps_the_shrink() {
        let img = probe_image();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 5, 5, 10);
        assert_eq!(out[2][2], 90, "max(50, 100-10)");
    }

    /// Independent oracle: a constant plane is a fixed point.
    #[test]
    fn a_constant_plane_never_changes() {
        let rows_owned = vec![vec![91u8; 6]; 6];
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 6, 6, 65535);
        for row in out {
            assert!(row.iter().all(|&v| v == 91));
        }
    }

    /// Independent oracle: `deflate` never raises a pixel — the mirror of
    /// [`crate::inflate`]'s "never decreases" property.
    #[test]
    fn deflate_never_increases_a_pixel() {
        let img: Vec<Vec<u8>> = (0..7)
            .map(|y| (0..7).map(|x| ((x * 17 + y * 31) % 251) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 7, 7, 65535);
        for y in 0..7 {
            for x in 0..7 {
                assert!(out[y][x] <= img[y][x], "({x},{y})");
            }
        }
    }
}
