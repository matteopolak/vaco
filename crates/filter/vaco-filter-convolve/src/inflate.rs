//! `inflate` — grow each pixel towards the truncating average of its fixed
//! 8-neighbourhood, capped by `threshold`.
//!
//! `ffmpeg -h filter=inflate` documents `threshold0..3` (`0..=65535`, default
//! `65535`, one per plane) — no `coordinates` option, unlike
//! [`crate::dilation`]/[`crate::erosion`]: the neighbourhood is always the
//! full fixed 3x3 ring.
//!
//! # Measured: average, not maximum — and which way it rounds
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=5x5,format=gray8,geq=lum='if(eq(X,2)*eq(Y,2),10,\
//!   if((eq(Y,1))*(gte(X,1))*(lte(X,3)),100,0))'" -vf inflate -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! Centre `(2,2)` (self `10`, neighbours `100,100,100,0,0,0,0,0`, sum `300`)
//! comes back `37`, not `38`: `300/8 = 37.5`, and the reference **truncates**
//! rather than rounds — refuting round-to-nearest with a single measurement,
//! the same shape of check `avgblur`'s doc uses against `boxblur`. A pixel
//! whose neighbourhood average is at or below its own value is left
//! unchanged (confirmed: every `100`-valued neighbour above, whose own
//! neighbourhood average is well below `100`, comes back unchanged) —
//! `inflate` only ever grows a pixel, it does not shrink one; that is
//! [`crate::deflate`]'s job. `threshold0=5` on the same probe caps the centre
//! at `min(37, 10+5) = 15`, matched exactly — the same "cap, don't gate" rule
//! [`crate::morph`]'s `dilation`/`erosion` doc measured, applied to an
//! average instead of a maximum.
//!
//! # Border: clamp-to-edge, confirmed
//!
//! The same probe's pixel `(0,2)` (top row, no row `-1`) computes its
//! average by clamping each of the 8 fixed offsets independently — three of
//! them (`(-1,-1)`, `(-1,0)`, `(-1,1)`) land back on row `0` — giving
//! `sum=300` (three neighbours read as `0,0,0`, three as `100,100,100` from
//! row 1, one pair from row 0 itself, `0,0`), average `37.5` truncated to
//! `37`, matching the reference exactly. See [`crate::morph::apply_plane`]'s
//! `InflateAvg` arm for the shared engine.

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
    name: "inflate",
    description: "Apply inflate effect",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "inflate", help = "Apply inflate effect")]
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
            let filtered = morph::apply_plane(&rows, pw, ph, Op::InflateAvg, params);
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
            Op::InflateAvg,
            MorphParams {
                coordinates: 0,
                threshold,
            },
        )
    }

    /// The exact 5x5 probe from this module's doc: `(2,2)=10`, row 1
    /// columns 1..=3 `=100`, everything else `0`.
    fn probe_image() -> Vec<Vec<u8>> {
        vec![
            vec![0, 0, 0, 0, 0],
            vec![0, 100, 100, 100, 0],
            vec![0, 0, 10, 0, 0],
            vec![0, 0, 0, 0, 0],
            vec![0, 0, 0, 0, 0],
        ]
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn matches_the_reference_on_the_asymmetric_probe() {
        let img = probe_image();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 5, 5, 65535);
        assert_eq!(out[2][2], 37, "300/8 = 37.5 truncated, not rounded");
        assert_eq!(out[0][2], 37, "clamp-to-edge border, same truncation");
        assert_eq!(out[1][1], 100, "a local maximum never grows past itself");
    }

    /// Pinned: `threshold` caps the growth rather than gating it.
    #[test]
    fn threshold_caps_growth() {
        let img = probe_image();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 5, 5, 5);
        assert_eq!(out[2][2], 15, "min(37, 10+5)");
    }

    /// Independent oracle: a constant plane is a fixed point (its own
    /// average always equals itself, so the `avg > self` gate never fires).
    #[test]
    fn a_constant_plane_never_changes() {
        let rows_owned = vec![vec![77u8; 6]; 6];
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 6, 6, 65535);
        for row in out {
            assert!(row.iter().all(|&v| v == 77));
        }
    }

    /// Independent oracle: `inflate` never lowers a pixel — the output of
    /// any pixel is always `>=` its input, a property of "grow towards the
    /// average, only when the average is higher" that does not depend on
    /// re-deriving the exact averaging formula.
    #[test]
    fn inflate_never_decreases_a_pixel() {
        let img: Vec<Vec<u8>> = (0..7)
            .map(|y| (0..7).map(|x| ((x * 17 + y * 31) % 251) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = plane(&rows, 7, 7, 65535);
        for y in 0..7 {
            for x in 0..7 {
                assert!(out[y][x] >= img[y][x], "({x},{y})");
            }
        }
    }
}
