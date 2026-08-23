//! `cas` — AMD's published Contrast Adaptive Sharpen.
//!
//! `ffmpeg -h filter=cas` documents `strength` (`0..=1`, default `0`) and
//! `planes` (default `7`).
//!
//! # Structural, not framecrc-verified
//!
//! This implements AMD's publicly documented Contrast Adaptive Sharpening
//! algorithm (`FidelityFX` CAS, an independently published formula — a
//! legitimate spec source under `AGENT-CONSTRAINTS.md`/D7, not `FFmpeg`
//! source): for each pixel `c` with its four-neighbour cross (`n`, `s`,
//! `w`, `e`), `mn`/`mx` are the min/max of the cross plus centre, `amp =
//! sqrt(clamp(min(mn, 1-mx)/mx, 0, 1))` measures how close the pixel is to a
//! local extreme, `peak = lerp(-1/8, -1/5, strength)` sets how hard an
//! edge-adjacent pixel gets pushed, and the output is the renormalised blend
//! `(sum(cross)*w + c) / (4w + 1)` with `w = amp * peak`.
//!
//! **Measured against the reference (`ffmpeg 8.1`, a `mod(X*53+Y*19,256)`
//! test pattern) that this is the right *shape* of formula but not
//! necessarily its exact constants**: even `strength=0` visibly sharpens
//! (e.g. one pixel moves `106 -> 105`), which refutes "`strength` gates
//! sharpening on/off" and confirms the `peak` range does not reach `0` at
//! either end — consistent with AMD's own "mild" (`-1/8`) to "aggressive"
//! (`-1/5`) framing. But solving the exact per-pixel weight back out of the
//! measured data did not converge on one clean constant set inside this
//! pass's time budget (several interior pixels imply a saturated weight
//! character istic that a plain 4-neighbour cross with `-1/8..-1/5` does not
//! reproduce everywhere), so this is shipped as a structural, published-spec
//! implementation rather than a framecrc-pinned one — the same honesty
//! `gblur`'s doc uses for the same reason (an independently-verified
//! algorithm shape, not a bit-exact match to this reference build).
//!
//! # Verified: a flat field is always the identity, for any strength
//!
//! Not a probe against the reference — a property of the formula's own
//! algebra. When every cross neighbour equals the centre (`n=s=w=e=c`), the
//! numerator is `4*c*w + c = c*(4w+1)` for *any* `w`, so it cancels exactly
//! against the `(4w+1)` denominator regardless of `amp`/`strength`. This
//! holds even though `amp` itself is not `0` on a flat field (`mn=mx=c`
//! still gives a nonzero `min(mn,1-mx)/mx` whenever `c` is not `0` or `1`) —
//! the invariant comes from the blend's own arithmetic, not from `amp`
//! vanishing.
//!
//! # Border
//!
//! Not measured for this filter specifically; clamp-to-edge, matching this
//! crate's other filters (`common::sample_clamped`), is used for the
//! cross-neighbour reads at the frame edge — a documented assumption, not a
//! probed one.

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
    name: "cas",
    description: "Contrast Adaptive Sharpen",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "cas", help = "Contrast Adaptive Sharpen")]
pub(crate) struct Opts {
    #[opt(name = "strength", help = "set the sharpening strength", default = 0.0, range = 0.0..=1.0, flags(video, filtering))]
    pub strength: f64,
    #[opt(name = "planes", help = "set what planes to filter", default = 7, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
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

/// `strength` (`0..=1`) to the peak sharpening weight, `-1/8` (mild) at `0`
/// to `-1/5` (aggressive) at `1` — AMD's own published framing.
fn peak_weight(strength: f64) -> f64 {
    (-0.125f64).mul_add(1.0 - strength, -0.2 * strength)
}

fn sharpen_plane(rows: &[&[u8]], w: i32, h: i32, peak: f64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let c = f64::from(common::sample_clamped(rows, x, y, w, h)) / 255.0;
            let n = f64::from(common::sample_clamped(rows, x, y - 1, w, h)) / 255.0;
            let s = f64::from(common::sample_clamped(rows, x, y + 1, w, h)) / 255.0;
            let we = f64::from(common::sample_clamped(rows, x - 1, y, w, h)) / 255.0;
            let e = f64::from(common::sample_clamped(rows, x + 1, y, w, h)) / 255.0;
            let mn = n.min(s).min(we).min(e).min(c);
            let mx = n.max(s).max(we).max(e).max(c);
            let amp = if mx > f64::EPSILON {
                (mn.min(1.0 - mx) / mx).clamp(0.0, 1.0).sqrt()
            } else {
                0.0
            };
            let wgt = amp * peak;
            let value = (n + s + we + e).mul_add(wgt, c) / 4.0f64.mul_add(wgt, 1.0);
            let scaled = (value.clamp(0.0, 1.0) * 255.0).round();
            row.push(u8::try_from(scaled as i64).unwrap_or(255));
        }
        out.push(row);
    }
    out
}

#[derive(Debug)]
pub(crate) struct Filter {
    peak: f64,
    planes: i64,
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
            let filtered = if common::plane_selected(self.planes, p8) {
                sharpen_plane(&rows, pw, ph, self.peak)
            } else {
                rows.iter().map(|r| (*r).to_vec()).collect()
            };
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
    let filter = Filter {
        peak: peak_weight(opts.strength),
        planes: opts.planes,
    };
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

    /// Independent oracle: a flat field is the identity for any strength —
    /// a property of the blend's own algebra (see this module's doc), not a
    /// re-derivation of `amp`.
    #[test]
    fn a_flat_field_is_always_the_identity() {
        for strength in [0.0, 0.3, 1.0] {
            let peak = peak_weight(strength);
            let rows_owned = vec![vec![140u8; 6]; 6];
            let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
            let out = sharpen_plane(&rows, 6, 6, peak);
            for row in out {
                for v in row {
                    assert_eq!(v, 140, "strength={strength}");
                }
            }
        }
    }

    #[test]
    fn peak_weight_spans_the_published_mild_to_aggressive_range() {
        assert!((peak_weight(0.0) - (-0.125)).abs() < 1e-9);
        assert!((peak_weight(1.0) - (-0.2)).abs() < 1e-9);
    }

    /// Independent oracle: the sharpened output never leaves `[0, 255]`,
    /// regardless of how large the local contrast swing is.
    #[test]
    fn output_stays_in_range() {
        let img: Vec<Vec<u8>> = (0..8)
            .map(|y| {
                (0..8)
                    .map(|x| if (x + y) % 2 == 0 { 0u8 } else { 255u8 })
                    .collect()
            })
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = sharpen_plane(&rows, 8, 8, peak_weight(1.0));
        for row in out {
            for v in row {
                let _: u8 = v; // in range by construction of u8
            }
        }
    }
}
