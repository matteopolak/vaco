//! `epx` — pixel-art upscaling by 2x or 3x using the Scale2x/Scale3x
//! algorithm (Andrea Mazzoleni's `AdvanceMAME` project, itself a
//! generalisation of Eric Johnston's original EPX/AdvMAME2x rule).
//!
//! `ffmpeg -h filter=epx` (2026-08-28): `n` (`2..=3`, default `3`).
//!
//! # Clean-room source: `scale2x.it`, not the reference
//!
//! The algorithm is published independently of any `FFmpeg` source, at
//! `https://www.scale2x.it/algorithm` (`provenance/sources.toml`'s
//! `scale2x-algorithm`), which gives the exact comparison rules below for
//! both scale factors. This is a from-specification implementation, not a
//! transcription of `vf_scale2xsai`/`vf_hqx`-style code (D7).
//!
//! For a 3x3 neighbourhood
//!
//! ```text
//! A B C
//! D E F
//! G H I
//! ```
//!
//! (only the four orthogonal neighbours `B`/`D`/`F`/`H` and the centre `E`
//! ever participate — the corners `A`/`C`/`G`/`I` are read by the
//! specification's own diagram but never compared), `n=2` produces a 2x2
//! block:
//!
//! ```text
//! if B != H && D != F:
//!     E0(top-left)     = D==B ? D : E
//!     E1(top-right)    = B==F ? F : E
//!     E2(bottom-left)  = D==H ? D : E
//!     E3(bottom-right) = H==F ? F : E
//! else:
//!     E0 = E1 = E2 = E3 = E
//! ```
//!
//! and `n=3` a 3x3 block (`E4`, the centre, is always exactly `E`):
//!
//! ```text
//! if B != H && D != F:
//!     E0 = D==B ? D : E
//!     E1 = ((D==B && E!=C) || (B==F && E!=A)) ? B : E
//!     E2 = B==F ? F : E
//!     E3 = ((D==B && E!=G) || (D==H && E!=A)) ? D : E
//!     E5 = ((B==F && E!=I) || (H==F && E!=C)) ? F : E
//!     E6 = D==H ? D : E
//!     E7 = ((D==H && E!=I) || (H==F && E!=G)) ? H : E
//!     E8 = H==F ? F : E
//! else:
//!     every Ei = E
//! ```
//!
//! Border pixels read via clamp-to-edge, per the specification.
//!
//! # Measured: `n=2` matches the reference exactly
//!
//! A 4x4 checkerboard-ish `gray` source through `ffmpeg -bitexact -vf
//! epx=n=2` matched this formula byte-for-byte at every one of several
//! hand-checked pixels, including one genuinely exercising every branch (a
//! corner pixel with `B!=H` and `D!=F` both true, verifying the individual
//! `E0..E3` sub-rules, not just the "all-copy" fallback every other tested
//! pixel happened to take). `n=3` is implemented from the same public
//! specification but was not independently re-probed against the reference
//! in the time available — a smaller, but real, gap versus `n=2`.
//!
//! # Not implemented: bit depths above 8

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "epx",
    description: "Scale the input using EPX algorithm.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "epx", help = "Scale the input using EPX algorithm.")]
pub(crate) struct Opts {
    #[opt(name = "n", help = "set scale factor", default = 3, range = 2..=3, flags(video, filtering))]
    pub n: i64,
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
    n: u32,
}

/// Clamp-to-edge sample of `rows` (already collected per-plane) at signed
/// coordinates.
fn sample(rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> u8 {
    let cy = y.clamp(0, h.saturating_sub(1).max(0));
    let cx = x.clamp(0, w.saturating_sub(1).max(0));
    let (Ok(uy), Ok(ux)) = (usize::try_from(cy), usize::try_from(cx)) else {
        return 0;
    };
    rows.get(uy).and_then(|r| r.get(ux)).copied().unwrap_or(0)
}

/// One pixel's `n=2` output block: `[top-left, top-right, bottom-left,
/// bottom-right]`, per this module's specification (`B`/`D`/`F`/`H` there
/// are `top`/`left`/`right`/`bottom` here).
fn expand2(rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> [u8; 4] {
    let top = sample(rows, x, y - 1, w, h);
    let left = sample(rows, x - 1, y, w, h);
    let center = sample(rows, x, y, w, h);
    let right = sample(rows, x + 1, y, w, h);
    let bottom = sample(rows, x, y + 1, w, h);
    if top == bottom || left == right {
        return [center; 4];
    }
    [
        if left == top { left } else { center },
        if top == right { right } else { center },
        if left == bottom { left } else { center },
        if bottom == right { right } else { center },
    ]
}

/// One pixel's `n=3` output block, row-major (`[top row; middle row;
/// bottom row]`), per this module's specification (`B`/`D`/`F`/`H`/`A`/`C`/
/// `G`/`I` there are `top`/`left`/`right`/`bottom`/`top_left`/`top_right`/
/// `bottom_left`/`bottom_right` here).
fn expand3(rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> [u8; 9] {
    let top = sample(rows, x, y - 1, w, h);
    let left = sample(rows, x - 1, y, w, h);
    let center = sample(rows, x, y, w, h);
    let right = sample(rows, x + 1, y, w, h);
    let bottom = sample(rows, x, y + 1, w, h);
    if top == bottom || left == right {
        return [center; 9];
    }
    let top_left = sample(rows, x - 1, y - 1, w, h);
    let top_right = sample(rows, x + 1, y - 1, w, h);
    let bottom_left = sample(rows, x - 1, y + 1, w, h);
    let bottom_right = sample(rows, x + 1, y + 1, w, h);
    let e0 = if left == top { left } else { center };
    let e2 = if top == right { right } else { center };
    let e6 = if left == bottom { left } else { center };
    let e8 = if bottom == right { right } else { center };
    let e1 = if (left == top && center != top_right) || (top == right && center != top_left) {
        top
    } else {
        center
    };
    let e3 = if (left == top && center != bottom_left) || (left == bottom && center != top_left) {
        left
    } else {
        center
    };
    let e5 = if (top == right && center != bottom_right) || (bottom == right && center != top_right)
    {
        right
    } else {
        center
    };
    let e7 = if (left == bottom && center != bottom_right)
        || (bottom == right && center != bottom_left)
    {
        bottom
    } else {
        center
    };
    [e0, e1, e2, e3, center, e5, e6, e7, e8]
}

/// Dispatch on `n`, padding the `n=2` block's unused trailing slots with
/// its own centre value (never read by the caller, which only asks for
/// `n*n` entries).
fn expand(rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32, n: u32) -> [u8; 9] {
    if n == 2 {
        let [b0, b1, b2, b3] = expand2(rows, x, y, w, h);
        let fill = sample(rows, x, y, w, h);
        [b0, b1, b2, b3, fill, fill, fill, fill, fill]
    } else {
        expand3(rows, x, y, w, h)
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            width,
            height,
            sample_aspect_ratio,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width: w,
                height: h,
                sample_aspect_ratio: sar,
                ..
            } = &mut out
            {
                *w = width.saturating_mul(self.n);
                *h = height.saturating_mul(self.n);
                // The output pixel grid is `n` times denser in both axes;
                // the *display* aspect ratio is unchanged, so SAR divides
                // by `n` in each axis and therefore stays put overall.
                *sar = sample_aspect_ratio;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(input));
        }
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let out_w = width.saturating_mul(self.n);
        let out_h = height.saturating_mul(self.n);
        let mut out = ctx.pool().acquire_video(format, out_w, out_h)?;
        let n = self.n;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let rows: Vec<&[u8]> = (0..ph.max(0))
                .map(|y| {
                    usize::try_from(y)
                        .ok()
                        .and_then(|uy| src_plane.row(uy))
                        .unwrap_or(&[])
                })
                .collect();
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for y in 0..ph {
                for x in 0..pw {
                    let block = expand(&rows, x, y, pw, ph, n);
                    for by in 0..n {
                        let Ok(oy) = usize::try_from(u32::try_from(y).unwrap_or(0) * n + by) else {
                            continue;
                        };
                        let Some(dst_row) = dst_plane.row_mut(oy) else {
                            continue;
                        };
                        for bx in 0..n {
                            let Ok(ox) = usize::try_from(u32::try_from(x).unwrap_or(0) * n + bx)
                            else {
                                continue;
                            };
                            let Some(&value) = block.get((by * n + bx) as usize) else {
                                continue;
                            };
                            if let Some(px) = dst_row.get_mut(ox) {
                                *px = value;
                            }
                        }
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
    #[allow(
        clippy::cast_sign_loss,
        reason = "range = 2..=3 is enforced by the option schema"
    )]
    let filter = Filter { n: opts.n as u32 };
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

    /// Pinned against the reference probe in this module's doc: a corner
    /// pixel with `B!=H` and `D!=F` both true, exercising every `n=2`
    /// sub-rule rather than only the "all-copy" fallback.
    #[test]
    fn matches_the_measured_n2_corner_case() {
        // src: [[200,50,200,50],[50,200,50,200],[200,50,200,50],[50,200,50,200]]
        let r0: &[u8] = &[200, 50, 200, 50];
        let r1: &[u8] = &[50, 200, 50, 200];
        let r2: &[u8] = &[200, 50, 200, 50];
        let rows: [&[u8]; 3] = [r0, r1, r2];
        let out = expand2(&rows, 0, 0, 4, 3);
        assert_eq!([out[0], out[1], out[2], out[3]], [200, 200, 200, 50]);
    }

    #[test]
    fn a_flat_field_is_a_fixed_point() {
        let r0: &[u8] = &[7, 7, 7];
        let r1: &[u8] = &[7, 7, 7];
        let r2: &[u8] = &[7, 7, 7];
        let rows: [&[u8]; 3] = [r0, r1, r2];
        assert_eq!(expand2(&rows, 1, 1, 3, 3), [7; 4]);
        assert_eq!(expand3(&rows, 1, 1, 3, 3), [7; 9]);
    }

    #[test]
    fn n3_centre_subpixel_is_always_the_source_pixel() {
        let r0: &[u8] = &[1, 2, 3];
        let r1: &[u8] = &[4, 5, 6];
        let r2: &[u8] = &[7, 8, 9];
        let rows: [&[u8]; 3] = [r0, r1, r2];
        let out = expand3(&rows, 1, 1, 3, 3);
        assert_eq!(out[4], 5);
    }

    proptest::proptest! {
        /// Invariant: every output sub-pixel is always one of the five
        /// values the formula ever reads from (`center` and its four
        /// orthogonal neighbours) — the algorithm only ever copies, never
        /// blends.
        #[test]
        fn every_subpixel_is_one_of_the_five_input_values(
            a in 0u8..=255, b in 0u8..=255, c in 0u8..=255,
            d in 0u8..=255, e in 0u8..=255, f in 0u8..=255,
            g in 0u8..=255, h in 0u8..=255, i in 0u8..=255,
        ) {
            let r0: &[u8] = &[a, b, c];
            let r1: &[u8] = &[d, e, f];
            let r2: &[u8] = &[g, h, i];
            let rows: [&[u8]; 3] = [r0, r1, r2];
            let allowed = [b, d, e, f, h];
            let out3 = expand3(&rows, 1, 1, 3, 3);
            for &v in &out3 {
                proptest::prop_assert!(allowed.contains(&v));
            }
        }
    }
}
