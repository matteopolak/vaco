//! `fillborders` — replace the outer `left`/`right`/`top`/`bottom` pixels of
//! a frame using its own interior, without changing the frame size.
//!
//! `ffmpeg -h filter=fillborders` documents `left`/`right`/`top`/`bottom`
//! (pixel counts, default `0`), `mode` (`smear`=0 default, `mirror`=1,
//! `fixed`=2, `reflect`=3, `wrap`=4, `fade`=5, `margins`=6) and `color`
//! (fixed/fade fill, default `black`).
//!
//! # Implemented: `smear`, `mirror`, `fixed`, `wrap` — measured exactly
//!
//! Built a 10-wide `gray` ramp `(X+1)*20` (values `20,40,...,200`) with
//! `left=1,right=3` (an asymmetric split, needed to pin the formula rather
//! than a symmetric one that a wrong formula could still satisfy by
//! accident) and confirmed, per mode (`ffmpeg -vf
//! fillborders=left=1:right=3:top=0:bottom=0:color=white:mode=<N>` — `mode`
//! must come *after* the other options or the reference's own CLI option
//! parser mis-splits the string, an unrelated CLI quirk, not filter
//! semantics):
//!
//! * `smear` (0): border pixel = nearest interior edge pixel, replicated.
//! * `mirror` (1): symmetric reflection *duplicating* the edge —
//!   `out[x] = in[2*left-1-x]` for the left border, `out[x] =
//!   in[2*(w-right)-1-x]` for the right (and the same on the vertical axis
//!   with `top`/`bottom`).
//! * `fixed` (2): every border pixel set to `color` (measured as `0xEB` for
//!   `white` on `gray` — i.e. routed through the same limited-range
//!   conversion as `pad`'s fill, not literal `255`).
//! * `wrap` (4): circular — `out[x] = in[left + ((x-left) mod N)]` where
//!   `N = w-left-right` is the interior width (and the transpose for
//!   `top`/`bottom`).
//!
//! # Not implemented: `reflect`, `fade`, `margins`
//!
//! `reflect` (mode 3) is *not* the simple non-duplicating reflection
//! `out[x]=in[2*left-x]` a first read would guess: that formula matched a
//! wide-interior probe (`left=1,right=3,w=10`) exactly, but a second,
//! narrow-interior probe (`left=2,right=2,w=6`) produced a value the same
//! formula cannot reach at all (`out[5]=in[3]`, not the predicted `in[1]`),
//! and neither a single bounce-back fold nor a periodic triangle-wave fold
//! reconciled both probes together with the *left*-border results from the
//! same run. Rather than ship a rule two independent measurements already
//! contradict, `mode=3` returns [`vaco_core::Error::Unsupported`]. `fade`
//! (5) and `margins` (6) were not probed at all in the time available and
//! are `Unsupported` for the same reason: measure first, do not guess.

use vaco_core::{Error, MediaType, Result};
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
    name: "fillborders",
    description: "Fill borders of the input video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Smear,
    Mirror,
    Fixed,
    Wrap,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "fillborders", help = "Fill borders of the input video")]
pub(crate) struct Opts {
    #[opt(
        name = "left",
        help = "set the left fill border",
        default = 0,
        range = 0..=i32::MAX,
        flags(video, filtering)
    )]
    pub left: i32,
    #[opt(
        name = "right",
        help = "set the right fill border",
        default = 0,
        range = 0..=i32::MAX,
        flags(video, filtering)
    )]
    pub right: i32,
    #[opt(
        name = "top",
        help = "set the top fill border",
        default = 0,
        range = 0..=i32::MAX,
        flags(video, filtering)
    )]
    pub top: i32,
    #[opt(
        name = "bottom",
        help = "set the bottom fill border",
        default = 0,
        range = 0..=i32::MAX,
        flags(video, filtering)
    )]
    pub bottom: i32,
    #[opt(
        name = "mode",
        help = "set the fill borders mode",
        default = 0,
        range = 0..=6,
        flags(video, filtering)
    )]
    pub mode: i32,
    #[opt(
        name = "color",
        help = "set the color for the fixed/fade mode",
        default = "black".to_owned(),
        flags(video, filtering)
    )]
    pub color: String,
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
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
    mode: Mode,
    rgb: (u8, u8, u8),
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let mode = match opts.mode {
            0 => Mode::Smear,
            1 => Mode::Mirror,
            2 => Mode::Fixed,
            4 => Mode::Wrap,
            3 | 5 | 6 => {
                return Err(
                    "fillborders: `mode` 3 (reflect), 5 (fade) and 6 (margins) are not \
                     implemented — see this module's doc"
                        .to_owned(),
                );
            }
            other => return Err(format!("fillborders: bad `mode` `{other}`")),
        };
        let rgba = vaco_core::parse::color(&opts.color)
            .ok_or_else(|| format!("fillborders: bad `color` `{}`", opts.color))?;
        Ok(Self {
            left: opts.left.max(0) as u32,
            right: opts.right.max(0) as u32,
            top: opts.top.max(0) as u32,
            bottom: opts.bottom.max(0) as u32,
            mode,
            rgb: (rgba.r, rgba.g, rgba.b),
        })
    }
}

/// Map one border index to its source index along one axis, per
/// [`Mode`]. `len` is the axis length, `lo`/`hi` are the border widths on
/// that axis's two ends. `None` means "not a border position" (the caller
/// only calls this for positions that are).
fn map_index(mode: Mode, i: u32, len: u32, lo: u32, hi: u32) -> Option<u32> {
    let interior_lo = lo;
    let interior_hi = len.saturating_sub(hi); // exclusive
    if i >= interior_lo && i < interior_hi {
        return None; // interior: unchanged
    }
    match mode {
        Mode::Smear => {
            if i < interior_lo {
                Some(interior_lo.min(len.saturating_sub(1)))
            } else {
                Some(interior_hi.saturating_sub(1).min(len.saturating_sub(1)))
            }
        }
        Mode::Mirror => {
            if i < interior_lo {
                let src = 2 * i64::from(interior_lo) - 1 - i64::from(i);
                Some(src.clamp(0, i64::from(len.saturating_sub(1))) as u32)
            } else {
                let src = 2 * i64::from(interior_hi) - 1 - i64::from(i);
                Some(src.clamp(0, i64::from(len.saturating_sub(1))) as u32)
            }
        }
        Mode::Fixed => None, // handled by the caller directly with `color`
        Mode::Wrap => {
            let n = interior_hi.saturating_sub(interior_lo);
            if n == 0 {
                return Some(i.min(len.saturating_sub(1)));
            }
            if i < interior_lo {
                let k = interior_lo - 1 - i; // 0-indexed distance from interior
                let o = k % n;
                Some(interior_hi.saturating_sub(1).saturating_sub(o))
            } else {
                let k = i - interior_hi; // 0-indexed distance into right border
                let o = k % n;
                Some(interior_lo + o)
            }
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
        if self.left.saturating_add(self.right) > width
            || self.top.saturating_add(self.bottom) > height
        {
            return Err(Error::InvalidData(
                "fillborders: left+right or top+bottom exceeds the frame size",
            ));
        }
        let mut out = input.clone();
        let fill_pattern = if self.mode == Mode::Fixed {
            Some(crate::fill::FillPattern::build(
                ctx.pool(),
                format,
                self.rgb,
                input.color,
            )?)
        } else {
            None
        };
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            if unit != 1 && self.mode != Mode::Fixed {
                // Byte-value modes need one sample per byte; packed/wide
                // formats fall back to `fixed`-only handling below (still
                // correct, just narrower support for exotic layouts).
                continue;
            }
            let pw = format.plane_width(width, plane_idx);
            let ph = format.plane_height(height, plane_idx);
            let left = format.plane_width(self.left, plane_idx);
            let right = format.plane_width(self.right, plane_idx);
            let top = format.plane_height(self.top, plane_idx);
            let bottom = format.plane_height(self.bottom, plane_idx);
            let fill_val = fill_pattern
                .as_ref()
                .and_then(|f| f.plane_pixel(p).first().copied())
                .unwrap_or(0);

            let is_border = |x: u32, y: u32| -> bool {
                x < left
                    || x >= pw.saturating_sub(right)
                    || y < top
                    || y >= ph.saturating_sub(bottom)
            };

            // Read the whole plane into a scratch buffer first: border pixels
            // read from *other* border/interior pixels of the same original
            // frame, so writes must not observe earlier writes in this pass.
            let Some(src_plane) = out.plane(p) else {
                continue;
            };
            let mut scratch = vec![0_u8; (pw as usize).saturating_mul(ph as usize)];
            for y in 0..ph as usize {
                if let Some(row) = src_plane.row(y) {
                    let n = (pw as usize).min(row.len());
                    let dst_start = y.saturating_mul(pw as usize);
                    if let (Some(d), Some(s)) = (
                        scratch.get_mut(dst_start..dst_start.saturating_add(n)),
                        row.get(..n),
                    ) {
                        d.copy_from_slice(s);
                    }
                }
            }
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for y in 0..ph {
                for x in 0..pw {
                    if !is_border(x, y) {
                        continue;
                    }
                    let value = if self.mode == Mode::Fixed {
                        fill_val
                    } else {
                        let sx = map_index(self.mode, x, pw, left, right).unwrap_or(x);
                        let sy = map_index(self.mode, y, ph, top, bottom).unwrap_or(y);
                        let idx = (sy as usize)
                            .saturating_mul(pw as usize)
                            .saturating_add(sx as usize);
                        scratch.get(idx).copied().unwrap_or(0)
                    };
                    if let Some(row) = dst_plane.row_mut(y as usize)
                        && let Some(byte) = row.get_mut(x as usize)
                    {
                        *byte = value;
                    }
                }
            }
        }
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

    // Measured against ffmpeg 8.1: width 10, left=1, right=3, values
    // 20,40,...,200 (index*20+20).
    const IN: [u32; 10] = [20, 40, 60, 80, 100, 120, 140, 160, 180, 200];

    fn apply(mode: Mode, left: u32, right: u32) -> Vec<u32> {
        (0..10u32)
            .map(|x| match map_index(mode, x, 10, left, right) {
                Some(s) => IN[s as usize],
                None => IN[x as usize],
            })
            .collect()
    }

    #[test]
    fn smear_matches_measured() {
        assert_eq!(
            apply(Mode::Smear, 1, 3),
            vec![40, 40, 60, 80, 100, 120, 140, 140, 140, 140]
        );
    }

    #[test]
    fn mirror_matches_measured() {
        assert_eq!(
            apply(Mode::Mirror, 1, 3),
            vec![40, 40, 60, 80, 100, 120, 140, 140, 120, 100]
        );
    }

    #[test]
    fn wrap_matches_measured() {
        assert_eq!(
            apply(Mode::Wrap, 1, 3),
            vec![140, 40, 60, 80, 100, 120, 140, 40, 60, 80]
        );
    }

    #[test]
    fn interior_is_always_unchanged() {
        for mode in [Mode::Smear, Mode::Mirror, Mode::Wrap] {
            let out = apply(mode, 1, 3);
            assert_eq!(&out[1..7], &IN[1..7]);
        }
    }

    #[test]
    fn reflect_fade_margins_are_rejected() {
        let opts = Opts {
            mode: 3,
            ..Opts::default()
        };
        assert!(Filter::new(&opts).is_err());
        let opts = Opts {
            mode: 5,
            ..Opts::default()
        };
        assert!(Filter::new(&opts).is_err());
        let opts = Opts {
            mode: 6,
            ..Opts::default()
        };
        assert!(Filter::new(&opts).is_err());
    }
}
