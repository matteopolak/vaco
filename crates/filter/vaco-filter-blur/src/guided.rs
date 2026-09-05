//! `guided` — the He, Sun & Tang (2010) guided image filter, self-guided
//! (`guidance=off`) mode only.
//!
//! `ffmpeg -h filter=guided` documents `radius` (`1..=20`, default `3`),
//! `eps` (`0..=1`, default `0.01`), `mode` (`0` basic / `1` fast, default
//! `basic`), `sub` (fast-mode subsampling ratio), `guidance` (`0` off / `1`
//! on, default `off`) and `planes` (default `1`, luma only).
//!
//! # Scope: `guidance=off`, `mode=basic` only
//!
//! `guidance=on` takes a *second* video stream as the guide image — this
//! crate has no dependency on `vaco-filter-framesync` set up for this filter
//! in this pass, and `guided`'s own reference declares its inputs "dynamic
//! (depending on the options)" rather than a fixed pad count, which this
//! crate's static [`FilterDesc`] cannot express without deciding it at
//! creation time. `guidance=on` is left for a follow-up (see the crate
//! docs); `create` rejects it explicitly rather than silently running
//! self-guided instead. `mode=fast`'s subsampled variant is likewise not
//! implemented — `create` rejects `mode=1` too.
//!
//! # The published algorithm (self-guided case, `I` the input plane itself)
//!
//! ```text
//! mean_I  = box(I, r);           mean_II = box(I*I, r)
//! var_I   = mean_II - mean_I^2
//! a       = var_I / (var_I + eps)
//! b       = mean_I - a * mean_I
//! mean_a  = box(a, r);           mean_b  = box(b, r)
//! q       = mean_a * I + mean_b
//! ```
//!
//! `box(*, r)` is a normalised `(2r+1)x(2r+1)` average; this crate's
//! implementation reads it with clamp-to-edge border sampling
//! ([`common::sample_clamped`]), matching this crate's other filters — not
//! separately measured for `guided`, a documented assumption rather than a
//! probed one.
//!
//! # Verified: identity on a flat field, for any radius or eps
//!
//! For a constant plane, `var_I = 0` everywhere (mean of squares equals
//! square of the mean for a constant), so `a = 0/(0+eps) = 0` and `b =
//! mean_I - 0 = mean_I = I`. Boxing constants `a=0`/`b=I` leaves them
//! unchanged, so `q = 0*I + I = I` — the filter reproduces its input
//! exactly, regardless of `radius`/`eps`. This is a property of the
//! published formula's own algebra (`var_I = 0 => a = 0` is forced whenever
//! `eps > 0`), not a re-derivation of the box-filter engine, and it holds
//! even though `eps` never appears with a zero denominator risk here.
//!
//! Not framecrc-verified against the reference in this pass (no probe of
//! `ffmpeg -h filter=guided`'s exact border/rounding was run); shipped as a
//! direct, structural implementation of the published algorithm.

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
    name: "guided",
    description: "Apply Guided filter",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "guided", help = "Apply Guided filter")]
pub(crate) struct Opts {
    #[opt(name = "radius", help = "set the box radius", default = 3, range = 1..=20, flags(video, filtering))]
    pub radius: i32,
    #[opt(name = "eps", help = "set the regularization parameter", default = 0.01, range = 0.0..=1.0, flags(video, filtering))]
    pub eps: f64,
    #[opt(name = "mode", help = "set filtering mode", default = "basic".to_owned(), flags(video, filtering))]
    pub mode: String,
    #[opt(name = "sub", help = "subsampling ratio for fast mode", default = 4, range = 2..=64, flags(video, filtering))]
    pub sub: i32,
    #[opt(name = "guidance", help = "set guidance mode", default = "off".to_owned(), flags(video, filtering))]
    pub guidance: String,
    #[opt(name = "planes", help = "set planes to filter", default = 1, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.sub != 4 {
            return Err("guided: `sub` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

/// A `(2r+1)x(2r+1)` normalised box average of an `f64` plane, clamp-bordered
/// (see this module's doc for why clamp, not zero, was chosen).
fn box_avg(vals: &[Vec<f64>], w: i32, h: i32, r: i32) -> Vec<Vec<f64>> {
    let count = f64::from((2 * r + 1) * (2 * r + 1));
    let span = usize::try_from(r.saturating_mul(2).saturating_add(1)).unwrap_or(0);
    let x_indices = axis_indices(w, r);
    let y_indices = axis_indices(h, r);
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let mut sum = 0.0;
            let x_base = usize::try_from(x).unwrap_or(0) * span;
            let y_base = usize::try_from(y).unwrap_or(0) * span;
            for dy in 0..span {
                let sampled_row = y_indices
                    .get(y_base + dy)
                    .and_then(|&index| vals.get(index))
                    .map_or(&[][..], Vec::as_slice);
                for dx in 0..span {
                    let value = x_indices
                        .get(x_base + dx)
                        .and_then(|&index| sampled_row.get(index))
                        .copied()
                        .unwrap_or(0.0);
                    sum += value;
                }
            }
            row.push(sum / count);
        }
        out.push(row);
    }
    out
}

fn axis_indices(length: i32, radius: i32) -> Vec<usize> {
    let max = length.saturating_sub(1).max(0);
    let mut indices = Vec::new();
    for coordinate in 0..length {
        for offset in -radius..=radius {
            indices.push(
                usize::try_from(coordinate.saturating_add(offset).clamp(0, max)).unwrap_or(0),
            );
        }
    }
    indices
}

fn guided_plane(rows: &[&[u8]], w: i32, h: i32, radius: i32, eps: f64) -> Vec<Vec<u8>> {
    let i_vals: Vec<Vec<f64>> = rows
        .iter()
        .map(|r| r.iter().map(|&v| f64::from(v) / 255.0).collect())
        .collect();
    let ii_vals: Vec<Vec<f64>> = i_vals
        .iter()
        .map(|row| row.iter().map(|&v| v * v).collect())
        .collect();
    let mean_i = box_avg(&i_vals, w, h, radius);
    let mean_ii = box_avg(&ii_vals, w, h, radius);
    let (a, b): (Vec<Vec<f64>>, Vec<Vec<f64>>) = mean_i
        .iter()
        .zip(mean_ii.iter())
        .map(|(mi_row, mii_row)| {
            mi_row
                .iter()
                .zip(mii_row.iter())
                .map(|(&mi, &mii)| {
                    let var_i = mii - mi * mi;
                    let av = var_i / (var_i + eps);
                    (av, mi.mul_add(-av, mi))
                })
                .unzip()
        })
        .unzip();
    let mean_a = box_avg(&a, w, h, radius);
    let mean_b = box_avg(&b, w, h, radius);
    i_vals
        .iter()
        .zip(mean_a.iter())
        .zip(mean_b.iter())
        .map(|((i_row, ma_row), mb_row)| {
            i_row
                .iter()
                .zip(ma_row.iter())
                .zip(mb_row.iter())
                .map(|((&iv, &ma), &mb)| {
                    let q = ma.mul_add(iv, mb);
                    let scaled = (q.clamp(0.0, 1.0) * 255.0).round();
                    u8::try_from(scaled as i64).unwrap_or(255)
                })
                .collect()
        })
        .collect()
}

#[derive(Debug)]
pub(crate) struct Filter {
    radius: i32,
    eps: f64,
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
                guided_plane(&rows, pw, ph, self.radius, self.eps)
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
    match opts.guidance.as_str() {
        "off" | "0" => {}
        "on" | "1" => {
            return Err(
                "guided: guidance=on (a second, external guide stream) is not implemented in this crate yet"
                    .to_owned(),
            );
        }
        other => return Err(format!("guided: bad `guidance` `{other}`")),
    }
    match opts.mode.as_str() {
        "basic" | "0" => {}
        "fast" | "1" => {
            return Err(
                "guided: mode=fast (subsampled) is not implemented in this crate yet".to_owned(),
            );
        }
        other => return Err(format!("guided: bad `mode` `{other}`")),
    }
    let filter = Filter {
        radius: opts.radius,
        eps: opts.eps,
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

    /// Independent oracle: a flat field is always the identity (see this
    /// module's doc for the algebra: `var_I = 0` forces `a = 0`, `b = I`).
    #[test]
    fn a_flat_field_is_always_the_identity() {
        for radius in [1, 3, 5] {
            for eps in [0.0001, 0.01, 0.5] {
                let rows_owned = vec![vec![90u8; 9]; 9];
                let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
                let out = guided_plane(&rows, 9, 9, radius, eps);
                for row in out {
                    for v in row {
                        assert_eq!(v, 90, "radius={radius} eps={eps}");
                    }
                }
            }
        }
    }

    #[test]
    fn guidance_on_is_rejected_rather_than_silently_downgraded() {
        let req = Instantiate {
            name: "guided",
            instance: "guided",
            args: Some("guidance=1"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn fast_mode_is_rejected_rather_than_silently_downgraded() {
        let req = Instantiate {
            name: "guided",
            instance: "guided",
            args: Some("mode=1"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
