//! `vectorscope` — a `256x256` chroma/component scatter plot, one cell per
//! `(x-component, y-component)` value pair, redrawn fresh every frame.
//!
//! `ffmpeg -h filter=vectorscope` (2026-08-28): `mode`/`m` (`gray`/`tint`
//! (both value `0`)/`color`/`color2`/`color3`/`color4`/`color5`, default
//! `gray`), `x`/`y` (component index `0..=2`, default `1`/`2` — `Y`, `U`,
//! `V` for a YUV source), `intensity`/`i` (`0..=1`, default `0.004`),
//! `envelope`/`e` (`none`/`instant`/`peak`/`peak+instant`, default
//! `none`), `graticule`/`g` (`none`/`green`/`color`/`invert`, default
//! `none`), `opacity`/`o`, `bgopacity`/`b`, `flags`/`f`, `colorspace`/`c`,
//! `tint0`/`tint1`. Output is a fixed `256x256 yuv444p` canvas — no `size`
//! option exists, confirmed by its absence from `-h` and by the output
//! staying `256x256` regardless of input size.
//!
//! # Measured (`ffmpeg 8.1`, real filtergraphs; a `geq`-built source gives
//! an exact, controlled component value **and** an exact controlled *hit
//! count* per cell, by painting exactly `N` pixels of one frame with the
//! test chroma and the rest with a different, off-cell chroma)
//!
//! **The per-frame histogram is not persistent across frames — it is
//! rebuilt from scratch every frame from that frame's own pixels alone.**
//! Confirmed by aiming a single-pixel hit at cell A on frame `0`, then a
//! different cell B on frames `1..4`: cell A goes dark again the very
//! next frame, it does not linger. (`envelope=peak`/`instant` presumably
//! change this — not measured, since only the default `envelope=none` is
//! implemented.)
//!
//! **The coordinate mapping**: `col = value(component_x)` (unflipped),
//! `row = 255 - value(component_y)` (flipped) — confirmed independently
//! of *which* component is assigned to which axis (`x=0:y=1` maps the
//! `Y`/`U` pair the same unflipped/flipped way `x=1:y=2`'s `U`/`V` pair
//! does), so the flip is a property of the *axis*, not of a particular
//! component identity.
//!
//! **The intensity formula, found exact and not fitted**: for each cell,
//! let `count` be the number of the current frame's pixels landing on it.
//! Then:
//!
//! ```text
//! per_hit = floor(255 * intensity)     // truncated once, not per-hit
//! Y       = clamp(count * per_hit, 0, 255)
//! U = V   = 127 if count > 0 else 128
//! ```
//!
//! Pinned at `35` independent `(count, intensity)` pairs spanning
//! `intensity` `0.004`/`0.008`/`0.02`/`0.05`/`0.1`/`1.0` and `count` from
//! `0` to `100`, all exact — including the two points (`count=50`,
//! `count=100` at the default `intensity=0.004`) that a naive
//! `round(255*count*intensity)` gets wrong by one, which is what a
//! previous pass's characterisation-only attempt was seeing before this
//! measurement found the real, exact, "truncate the per-hit step once"
//! rule. The chroma-marker rule (`127`/`128`, not a gradient) was the
//! second half of the same probe: it is a binary "was this cell touched
//! at all", independent of `count` or `intensity` magnitude.
//!
//! # Not implemented
//!
//! `mode=color`/`color2`/`color3`/`color4`/`color5` (hue-coded output —
//! only the monochrome `gray`/`tint` default is implemented).
//! `envelope=instant`/`peak`/`peak+instant` (cross-frame persistence — a
//! materially different, stateful measurement this pass did not attempt).
//! `graticule` overlays, `opacity`/`bgopacity`, `colorspace`, `tint0`/
//! `tint1`. Bit depths above 8, and non-`yuv444p` chroma layouts (`x`/`y`
//! still accept any of the three planes, but the source itself must
//! already be full-resolution, 8-bit, three-plane YUV).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "vectorscope",
    description: "Video vectorscope.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const SIZE: u32 = 256;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "vectorscope", help = "Video vectorscope.")]
pub(crate) struct Opts {
    #[opt(name = "mode", alias = "m", help = "set vectorscope mode", default = "gray".to_string(), flags(video, filtering))]
    pub mode: String,
    #[opt(name = "x", help = "set color component on X axis", default = 1u32, range = 0..=2, flags(video, filtering))]
    pub x: u32,
    #[opt(name = "y", help = "set color component on Y axis", default = 2u32, range = 0..=2, flags(video, filtering))]
    pub y: u32,
    #[opt(name = "intensity", alias = "i", help = "set intensity", default = 0.004, range = 0.0..=1.0, flags(video, filtering))]
    pub intensity: f64,
    #[opt(name = "envelope", alias = "e", help = "set envelope", default = "none".to_string(), flags(video, filtering))]
    pub envelope: String,
    #[opt(name = "graticule", alias = "g", help = "set graticule", default = "none".to_string(), flags(video, filtering))]
    pub graticule: String,
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
pub(crate) struct Filter {
    x: usize,
    y: usize,
    intensity: f64,
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { width, height, .. } = &mut out {
                *width = SIZE;
                *height = SIZE;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { width, height, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let Some(px) = input.plane(self.x) else {
            return Ok(FrameOut::One(input));
        };
        let Some(py) = input.plane(self.y) else {
            return Ok(FrameOut::One(input));
        };
        let px_rows: Vec<&[u8]> = px.rows_iter().collect();
        let py_rows: Vec<&[u8]> = py.rows_iter().collect();

        let cells = (SIZE * SIZE) as usize;
        let mut counts = vec![0u32; cells];
        let h = usize::try_from(height).unwrap_or(0);
        let w = usize::try_from(width).unwrap_or(0);
        for row in 0..h {
            let Some(xr) = px_rows.get(row) else { continue };
            let Some(yr) = py_rows.get(row) else { continue };
            for col in 0..w {
                let (Some(&xv), Some(&yv)) = (xr.get(col), yr.get(col)) else {
                    continue;
                };
                let out_col = usize::from(xv);
                let out_row = 255 - usize::from(yv);
                if let Some(c) = counts.get_mut(out_row * (SIZE as usize) + out_col) {
                    *c = c.saturating_add(1);
                }
            }
        }

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "255*intensity is in 0.0..=255.0 by construction (intensity is clamped to 0..=1)"
        )]
        let per_hit = (255.0 * self.intensity).floor() as u32;

        let mut out = ctx.pool().acquire_video(PixFmt::Yuv444p, SIZE, SIZE)?;
        if let Some(mut y_plane) = out.plane_mut(0) {
            for (row_idx, row) in y_plane.rows_mut().enumerate() {
                for (col_idx, dst) in row.iter_mut().enumerate() {
                    let Some(&count) = counts.get(row_idx * (SIZE as usize) + col_idx) else {
                        continue;
                    };
                    let v = count.saturating_mul(per_hit).min(255);
                    #[allow(clippy::cast_possible_truncation, reason = "just clamped to 0..=255 above")]
                    {
                        *dst = v as u8;
                    }
                }
            }
        }
        for plane_idx in [1usize, 2] {
            if let Some(mut chroma) = out.plane_mut(plane_idx) {
                for (row_idx, row) in chroma.rows_mut().enumerate() {
                    for (col_idx, dst) in row.iter_mut().enumerate() {
                        let touched = counts
                            .get(row_idx * (SIZE as usize) + col_idx)
                            .is_some_and(|&c| c > 0);
                        *dst = if touched { 127 } else { 128 };
                    }
                }
            }
        }

        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }
}

fn create_with(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if opts.mode != "gray" && opts.mode != "tint" {
        return Err(format!(
            "vectorscope: mode `{}` not implemented — only `gray`/`tint` are",
            opts.mode
        ));
    }
    if opts.envelope != "none" {
        return Err(format!(
            "vectorscope: envelope `{}` not implemented — only `none` is",
            opts.envelope
        ));
    }
    if opts.graticule != "none" {
        return Err(format!(
            "vectorscope: graticule `{}` not implemented — only `none` is",
            opts.graticule
        ));
    }
    let filter = Filter {
        x: opts.x as usize,
        y: opts.y as usize,
        intensity: opts.intensity,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::converter(
            FormatSet::video_exact(PixFmt::Yuv444p),
            FormatSet::video_exact(PixFmt::Yuv444p),
            req.instance,
        ),
        filter: Box::new(Simple::new(filter)),
    })
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    create_with(req)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "vectorscope",
            instance: "vs",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn unimplemented_mode_is_a_clean_error() {
        let req = Instantiate {
            name: "vectorscope",
            instance: "vs",
            args: Some("mode=color"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned against the measured `count * floor(255*intensity)` formula
    /// at the points that distinguish it from a naive
    /// `round(255*count*intensity)`: `count=50`/`100` at the default
    /// `intensity=0.004` give `50`/`100`, not `51`/`102`.
    #[test]
    fn intensity_formula_matches_the_measured_exact_rule() {
        let per_hit = |i: f64| (255.0 * i).floor() as u32;
        assert_eq!(per_hit(0.004), 1);
        assert_eq!(50u32.saturating_mul(per_hit(0.004)).min(255), 50);
        assert_eq!(100u32.saturating_mul(per_hit(0.004)).min(255), 100);
        assert_eq!(per_hit(0.008), 2);
        assert_eq!(20u32.saturating_mul(per_hit(0.008)).min(255), 40);
        assert_eq!(per_hit(0.1), 25);
        assert_eq!(20u32.saturating_mul(per_hit(0.1)).min(255), 255);
    }
}
