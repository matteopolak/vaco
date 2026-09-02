//! `remap` — remap each output pixel from an absolute source coordinate
//! read from two 16-bit map planes. `format=gray` only; see this
//! module's doc for `format=color`'s disposition.
//!
//! `ffmpeg -h filter=remap` (2026-08-28): `format` (`color`/`gray`,
//! default `color`), `fill` (a colour, default `"black"`). Three fixed
//! inputs (`source`, `xmap`, `ymap`), no framesync surface — `Paired`,
//! the same shape [`crate::displace`] uses.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo` sources)
//!
//! The map planes are **16-bit `gray16le`** — confirmed by feeding
//! 8-bit maps and observing `ffmpeg -v verbose` auto-insert a
//! `gray -> gray16le` conversion ahead of `remap` (its native format,
//! unlike [`crate::displace`]'s plain 8-bit maps), then re-measuring
//! with genuine `gray16le` map files to get real, uninflated values.
//! Each map holds an **absolute source pixel coordinate**, not an offset
//! and not a normalised fraction:
//!
//! ```text
//! output(x, y) = in_range(xmap(x,y), width) && in_range(ymap(x,y), height)
//!              ? source(xmap(x,y), ymap(x,y))
//!              : fill
//! ```
//!
//! Confirmed directly: an identity map (`xmap(x,y)=x`, `ymap(x,y)=y`)
//! reproduces the source frame exactly, pixel for pixel, on a
//! per-pixel-distinct `4x4` gradient. Confirmed that **either** axis
//! alone out of range triggers `fill` (a valid `x` with an out-of-range
//! `y` fills the whole frame), not just both together.
//!
//! `fill`'s colour, for `format=gray`, is **not** the plain component
//! average one might expect — it goes through the full-range-RGB-to-
//! limited-range-BT.709-luma conversion:
//!
//! ```text
//! fill_gray = round(16 + 219 * (0.2126*r + 0.7152*g + 0.0722*b) / 255)
//! ```
//!
//! Pinned at four colours: `black` (`0,0,0`) → `16` exactly; `white`
//! (`255,255,255`) → `235` exactly (both with no fractional part to
//! round — `16` and `235` are precisely BT.709's own limited-range
//! black/white points, not this crate's guess); `red` (`255,0,0`) →
//! `63`; a mid grey (`128,128,128`) → `126`. All four match the formula
//! above exactly, including its rounding. `219`/`255`/`16`/`235` and the
//! `0.2126`/`0.7152`/`0.0722` luma weights are ITU-R BT.709's own
//! published coefficients (D7 merger doctrine — a published international
//! standard's numbers, not this crate's invention or the reference's own
//! table).
//!
//! # Not measured/implemented
//!
//! `format=color` (the reference's own default) — a genuinely different,
//! unmeasured code path (does the *source* also need this limited-range
//! treatment when it is itself RGB? does a `yuv420p` source pass through
//! unconverted? neither was probed). This module implements `format=gray`
//! only and `create` rejects `format=color`/the unset default with a
//! clean error rather than guessing at behaviour for the far more common
//! case ffmpeg users actually reach for. Non-luma planes when the source
//! has more than one (not reached here since only `format=gray` is
//! implemented). Bit depths above 8 for the source. Map dimensions
//! differing from the source's own were not probed — this module assumes
//! (does not enforce) they match.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[
    Pad {
        name: "source",
        media_type: MediaType::Video,
    },
    Pad {
        name: "xmap",
        media_type: MediaType::Video,
    },
    Pad {
        name: "ymap",
        media_type: MediaType::Video,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "remap",
    description: "Remap pixels.",
    inputs: VIDEO_PAD,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

/// ITU-R BT.709's own published full-range-RGB -> limited-range-luma
/// coefficients and offset — a standard, not this crate's or the
/// reference's data (D7 merger doctrine). See the module doc's four-point
/// pin.
#[must_use]
pub(crate) fn bt709_limited_luma(r: u8, g: u8, b: u8) -> u8 {
    let luma = 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b);
    let y = 16.0 + 219.0 * luma / 255.0;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "16 + 219*(0..255)/255 always lands in 16..=235, inside u8's range"
    )]
    {
        y.round().clamp(0.0, 255.0) as u8
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "remap", help = "Remap pixels.")]
pub(crate) struct Opts {
    #[opt(name = "format", help = "set output format", default = "color".to_owned(), flags(video, filtering))]
    pub format: String,
    #[opt(name = "fill", help = "set the color of the unmapped pixels", default = "black".to_owned(), flags(video, filtering))]
    pub fill: String,
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
    fill_gray: u8,
}

fn read_u16le(row: &[u8], x: usize) -> Option<u16> {
    let base = x.checked_mul(2)?;
    let hi = row.get(base.checked_add(1)?)?;
    let lo = row.get(base)?;
    Some(u16::from_le_bytes([*lo, *hi]))
}

impl PairedFilter for Filter {
    fn input_count(&self) -> usize {
        3
    }

    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        mut inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        if inputs.len() != 3 {
            return Ok(FrameOut::None);
        }
        let ymap = inputs.pop();
        let xmap = inputs.pop();
        let source = inputs.pop();
        let (Some(source), Some(xmap), Some(ymap)) = (source, xmap, ymap) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = source.data
        else {
            return Ok(FrameOut::One(source));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(source));
        }
        let Some(src_plane) = source.plane(0) else {
            return Ok(FrameOut::One(source));
        };
        let Some(xmap_plane) = xmap.plane(0) else {
            return Ok(FrameOut::One(source));
        };
        let Some(ymap_plane) = ymap.plane(0) else {
            return Ok(FrameOut::One(source));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let Some(mut dst0) = out.plane_mut(0) else {
            return Ok(FrameOut::One(source));
        };
        let w = u16::try_from(width).unwrap_or(u16::MAX);
        let h = u16::try_from(height).unwrap_or(u16::MAX);
        for y in 0..height {
            let uy = y as usize;
            let Some(xrow) = xmap_plane.row(uy) else {
                continue;
            };
            let Some(yrow) = ymap_plane.row(uy) else {
                continue;
            };
            let Some(dst_row) = dst0.row_mut(uy) else {
                continue;
            };
            for x in 0..width {
                let ux = x as usize;
                let (Some(sx), Some(sy)) = (read_u16le(xrow, ux), read_u16le(yrow, ux)) else {
                    continue;
                };
                let out_val = if sx < w && sy < h {
                    src_plane
                        .row(sy as usize)
                        .and_then(|r| r.get(sx as usize))
                        .copied()
                        .unwrap_or(self.fill_gray)
                } else {
                    self.fill_gray
                };
                if let Some(px) = dst_row.get_mut(ux) {
                    *px = out_val;
                }
            }
        }
        out.pts = source.pts;
        out.time_base = source.time_base;
        out.duration = source.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if opts.format != "gray" {
        return Err(
            "remap: `format=color` (the default) is not implemented; pass `format=gray`".to_owned(),
        );
    }
    let rgba = vaco_core::parse::color(&opts.fill)
        .ok_or_else(|| format!("remap: bad `fill` colour `{}`", opts.fill))?;
    let fill_gray = bt709_limited_luma(rgba.r, rgba.g, rgba.b);
    let filter = Filter { fill_gray };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(3, 1, MediaType::Video, req.instance),
        filter: Box::new(Paired::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_format_gray() {
        let req = Instantiate {
            name: "remap",
            instance: "remap",
            args: Some("format=gray"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn format_color_is_not_implemented() {
        let req = Instantiate {
            name: "remap",
            instance: "remap",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned against the reference's four-colour probe in this module's
    /// doc: `black`/`white` land exactly on BT.709's own limited-range
    /// points, `red` and mid-grey confirm the weighted formula, not a
    /// plain average.
    #[test]
    fn fill_colour_matches_bt709_limited_range_luma() {
        assert_eq!(bt709_limited_luma(0, 0, 0), 16);
        assert_eq!(bt709_limited_luma(255, 255, 255), 235);
        assert_eq!(bt709_limited_luma(255, 0, 0), 63);
        assert_eq!(bt709_limited_luma(128, 128, 128), 126);
    }

    #[test]
    fn bad_fill_colour_is_a_clean_error() {
        let req = Instantiate {
            name: "remap",
            instance: "remap",
            args: Some("format=gray:fill=not-a-colour"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned: `read_u16le` decodes little-endian, matching the
    /// reference's own `gray16le` map format.
    #[test]
    fn read_u16le_is_little_endian() {
        let row = [0x34u8, 0x12, 0xff, 0x00];
        assert_eq!(read_u16le(&row, 0), Some(0x1234));
        assert_eq!(read_u16le(&row, 1), Some(0x00ff));
    }
}
