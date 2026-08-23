//! `scroll` — scroll the input video horizontally and/or vertically, with
//! wraparound.
//!
//! `ffmpeg -h filter=scroll` documents `horizontal`/`h` and `vertical`/`v`
//! (speed as a fraction of width/height per frame, `-1..1`, default `0`) and
//! `hpos`/`vpos` (initial position, `0..1`, default `0`). All four
//! implemented.
//!
//! # Measured: the shift formula and its rounding
//!
//! Built a 4x1 `gray` row `[0,64,128,192]` and ran `scroll=h=0.25` for three
//! frames:
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=4x1,format=gray,geq=lum='X*64'" \
//!   -vf scroll=h=0.25 -frames:v 3 -f rawvideo -pix_fmt gray -
//! ```
//!
//! Frame 0 is the identity; frame 1 is `[64,128,192,0]`; frame 2 is
//! `[128,192,0,64]`. So `out[x] = in[(x + shift) mod w]` with `shift`
//! advancing by `h * w` every frame (here `0.25 * 4 = 1`) — a *left*
//! circular shift, i.e. the window scrolls right through the source.
//! `horizontal=-0.25` produced the mirror-image right shift, confirming the
//! sign. `hpos=0.5` on a stationary (`h=0`) row produced `shift = hpos * w`
//! at frame 0, so `hpos`/`vpos` are the same fractional-of-dimension unit as
//! `horizontal`/`vertical`, just applied once rather than accumulated.
//! Vertical behaves identically on the other axis (same probe, transposed).
//!
//! Position is tracked as an accumulating `f64` (not recomputed from an
//! absolute frame count) so that a non-integer-pixel-per-frame speed still
//! advances smoothly frame to frame; the modulus is taken fresh each frame
//! from the accumulated float, floored, and wrapped into `[0, w)`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "scroll",
    description: "Scroll input video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "scroll", help = "Scroll input video")]
pub(crate) struct Opts {
    #[opt(
        name = "horizontal",
        alias = "h",
        help = "set the horizontal scrolling speed",
        default = 0.0,
        range = -1.0..=1.0,
        flags(video, filtering)
    )]
    pub horizontal: f64,
    #[opt(
        name = "vertical",
        alias = "v",
        help = "set the vertical scrolling speed",
        default = 0.0,
        range = -1.0..=1.0,
        flags(video, filtering)
    )]
    pub vertical: f64,
    #[opt(
        name = "hpos",
        help = "set initial horizontal position",
        default = 0.0,
        range = 0.0..=1.0,
        flags(video, filtering)
    )]
    pub hpos: f64,
    #[opt(
        name = "vpos",
        help = "set initial vertical position",
        default = 0.0,
        range = 0.0..=1.0,
        flags(video, filtering)
    )]
    pub vpos: f64,
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

/// Fold a continuous shift into `[0, len)`, then floor to a whole-sample
/// offset. Empty planes (`len == 0`) shift by nothing.
fn wrapped_shift(shift: f64, len: u32) -> u32 {
    if len == 0 {
        return 0;
    }
    let l = f64::from(len);
    let m = shift.rem_euclid(l);
    (m.floor() as u32).min(len - 1)
}

#[derive(Debug)]
pub(crate) struct Filter {
    h: f64,
    v: f64,
    pos_x: f64,
    pos_y: f64,
}

impl Filter {
    pub(crate) const fn new(opts: &Opts) -> Self {
        Self {
            h: opts.horizontal,
            v: opts.vertical,
            pos_x: opts.hpos,
            pos_y: opts.vpos,
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        geom::ensure_addressable(format)?;
        let shift_x = wrapped_shift(self.pos_x * f64::from(width), width);
        let shift_y = wrapped_shift(self.pos_y * f64::from(height), height);
        self.pos_x += self.h;
        self.pos_y += self.v;

        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            let pw = format.plane_width(width, plane_idx);
            let ph = format.plane_height(height, plane_idx);
            let sx = format
                .plane_width(shift_x, plane_idx)
                .min(pw.saturating_sub(1));
            let sy = format
                .plane_height(shift_y, plane_idx)
                .min(ph.saturating_sub(1));
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for oy in 0..ph {
                let src_y = (oy + sy) % ph.max(1);
                let Some(src_row) = src_plane.row(src_y as usize) else {
                    continue;
                };
                let Some(dst_row) = dst_plane.row_mut(oy as usize) else {
                    continue;
                };
                for ox in 0..pw {
                    let src_x = (ox + sx) % pw.max(1);
                    let s_start = (src_x as usize).saturating_mul(unit);
                    let d_start = (ox as usize).saturating_mul(unit);
                    if let (Some(s), Some(d)) = (
                        src_row.get(s_start..s_start.saturating_add(unit)),
                        dst_row.get_mut(d_start..d_start.saturating_add(unit)),
                    ) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = input.color;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
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
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn matches_measured_frame_sequence() {
        // Measured against ffmpeg 8.1: 4-wide row [0,64,128,192],
        // scroll=h=0.25 (shift=1/frame). Frame N's output is a left
        // circular shift by N.
        let width = 4u32;
        for (n, expect) in [(0u32, 0u32), (1, 1), (2, 2), (3, 3)] {
            let shift = wrapped_shift(0.25 * f64::from(width) * f64::from(n), width);
            assert_eq!(shift, expect % width);
        }
    }

    #[test]
    fn negative_speed_wraps_the_other_way() {
        // Measured: h=-0.25 on width 4 after one frame gives shift=3 (a
        // right shift), matching -1 mod 4 = 3.
        assert_eq!(wrapped_shift(-0.25 * 4.0, 4), 3);
    }

    #[test]
    fn hpos_sets_the_initial_offset_directly() {
        assert_eq!(wrapped_shift(0.5 * 4.0, 4), 2);
    }

    #[test]
    fn zero_speed_and_zero_pos_is_never_shifted() {
        for n in 0..10u32 {
            assert_eq!(wrapped_shift(0.0 * f64::from(n), 8), 0);
        }
    }

    proptest::proptest! {
        #[test]
        fn shift_is_always_a_valid_index_or_zero_length(
            shift in -100.0f64..100.0, len in 0u32..64,
        ) {
            let s = wrapped_shift(shift, len);
            if len == 0 {
                proptest::prop_assert_eq!(s, 0);
            } else {
                proptest::prop_assert!(s < len);
            }
        }
    }
}
