//! `gblur` — Gaussian blur.
//!
//! `ffmpeg -h filter=gblur` documents `sigma` (`0..=1024`, default `0.5`),
//! `steps` (`1..=6`, default `1`), `planes` (default `15`), `sigmaV`
//! (`-1..=1024`, default `-1`, meaning "same as `sigma`").
//!
//! # Not bit-exact: a measured, deliberate scope decision
//!
//! An impulse response probe (`sigma=3`, `steps=1`) against the reference —
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=64x1,format=gray8,geq=lum='if(eq(X,32),255,0)'" \
//!   -vf "gblur=sigma=3:steps=1" -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! — gives a peak of `59` with roughly geometric falloff (`37, 23, 14, 9,
//! 6, 4, 2, 1, 1, 1`, ratio approaching ~1.6 between consecutive taps).
//! That is **not** a truncated discrete Gaussian kernel: a directly
//! normalised `sigma=3` kernel's peak weight is `~0.133`, i.e. a peak of
//! `~34` on this input, and its falloff is a bell curve, not a near-constant
//! ratio. A near-constant tap ratio is the signature of a low-order
//! recursive (IIR) filter — almost certainly the published Young/van
//! Vliet or Deriche recursive Gaussian approximation, which is what
//! `steps` (repeated refining passes) suggests — not a plain FIR
//! convolution at all.
//!
//! Matching that specific IIR construction bit-exactly is out of this
//! crate's time budget (see `docs/filter/vaco-filter-blur.md`): this
//! implementation is a direct, truncated, separable Gaussian FIR
//! convolution instead — mathematically a real Gaussian blur, verified
//! against the properties a Gaussian kernel must have (normalises to `1`,
//! blurring a constant field is the identity), but **not** framecrc-equal
//! to the reference. `steps` is accepted and parsed but does not change
//! behaviour, since this implementation does not use the iterative
//! refinement the reference's algorithm appears to.

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
    name: "gblur",
    description: "Apply Gaussian Blur filter",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "gblur", help = "Apply Gaussian Blur filter")]
pub(crate) struct Opts {
    #[opt(name = "sigma", help = "set sigma", default = 0.5, range = 0.0..=1024.0, flags(video, filtering))]
    pub sigma: f64,
    #[opt(name = "steps", help = "set number of steps", default = 1, range = 1..=6, flags(video, filtering))]
    pub steps: i32,
    #[opt(name = "planes", help = "set planes to filter", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
    #[opt(name = "sigmaV", help = "set vertical sigma", default = -1.0, range = -1.0..=1024.0, flags(video, filtering))]
    pub sigma_v: f64,
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

/// A truncated, normalised discrete Gaussian kernel: `weights[r+i]` is the
/// tap at offset `i` (`-r..=r`), and every kernel sums to (within floating
/// point) `1.0`. `sigma <= 0` degenerates to the identity kernel `[1.0]`.
fn gaussian_kernel(sigma: f64) -> Vec<f64> {
    if sigma <= 0.0 {
        return vec![1.0];
    }
    let radius = ((sigma * 4.0).ceil() as i32).max(1);
    let mut weights: Vec<f64> = (-radius..=radius)
        .map(|i| (-f64::from(i * i) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = weights.iter().sum();
    if sum > 0.0 {
        for w in &mut weights {
            *w /= sum;
        }
    }
    weights
}

fn convolve_1d(rows: &[&[u8]], w: i32, h: i32, kernel: &[f64], horizontal: bool) -> Vec<Vec<u8>> {
    let r = common::to_i32(kernel.len() >> 1);
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let mut acc = 0.0f64;
            for (i, &weight) in kernel.iter().enumerate() {
                let offset = common::to_i32(i) - r;
                let v = if horizontal {
                    common::sample_clamped(rows, x + offset, y, w, h)
                } else {
                    common::sample_clamped(rows, x, y + offset, w, h)
                };
                acc += weight * f64::from(v);
            }
            row.push(clamp_round(acc));
        }
        out.push(row);
    }
    out
}

fn clamp_round(value: f64) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    let rounded = value.round();
    if rounded <= 0.0 {
        0
    } else if rounded >= 255.0 {
        255
    } else {
        rounded as u8
    }
}

fn blur_plane(rows: &[&[u8]], w: i32, h: i32, sigma_x: f64, sigma_y: f64) -> Vec<Vec<u8>> {
    let kx = gaussian_kernel(sigma_x);
    let horiz = convolve_1d(rows, w, h, &kx, true);
    let borrowed: Vec<&[u8]> = horiz.iter().map(Vec::as_slice).collect();
    let ky = gaussian_kernel(sigma_y);
    convolve_1d(&borrowed, w, h, &ky, false)
}

#[derive(Debug)]
pub(crate) struct Filter {
    sigma_x: f64,
    sigma_y: f64,
    planes: i64,
}

impl Filter {
    const fn new(opts: &Opts) -> Self {
        let sigma_y = if opts.sigma_v < 0.0 {
            opts.sigma
        } else {
            opts.sigma_v
        };
        Self {
            sigma_x: opts.sigma,
            sigma_y,
            planes: opts.planes,
        }
    }
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
            let blurred = if common::plane_selected(self.planes, p8) {
                blur_plane(&rows, pw, ph, self.sigma_x, self.sigma_y)
            } else {
                rows.iter().map(|r| (*r).to_vec()).collect()
            };
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
    let filter = Filter::new(&opts);
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

    /// Independent oracle: any Gaussian kernel, by construction, sums to
    /// `1` — a property of "it is a probability-weighted average", not a
    /// re-derivation of the specific weights.
    #[test]
    fn kernel_is_normalized() {
        for sigma in [0.1, 0.5, 1.0, 3.0, 10.0] {
            let k = gaussian_kernel(sigma);
            let sum: f64 = k.iter().sum();
            assert!((sum - 1.0).abs() < 1e-9, "sigma={sigma} sum={sum}");
        }
    }

    /// Independent oracle: blurring a constant field is the identity, for
    /// any normalised convolution kernel — true regardless of what the
    /// specific weights are.
    #[test]
    fn a_constant_field_is_a_fixed_point() {
        let img = vec![vec![150u8; 9]; 9];
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = blur_plane(&rows, 9, 9, 2.0, 2.0);
        for row in out {
            for v in row {
                assert!((i32::from(v) - 150).abs() <= 1, "got {v}");
            }
        }
    }

    /// Sigma `0` (and the identity kernel it produces) must not change the
    /// image at all.
    #[test]
    fn zero_sigma_is_identity() {
        let img: Vec<Vec<u8>> = (0..5)
            .map(|y| (0..5).map(|x| (x * 7 + y) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = blur_plane(&rows, 5, 5, 0.0, 0.0);
        assert_eq!(out, img);
    }
}
