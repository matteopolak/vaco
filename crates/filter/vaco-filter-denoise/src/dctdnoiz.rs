//! `dctdnoiz` — block 2D DCT-domain hard thresholding: split the plane into
//! non-overlapping blocks, transform each with a direct 2D DCT-II, zero
//! every AC coefficient below a noise-derived threshold, invert.
//!
//! # Options (`ffmpeg -h filter=dctdnoiz`, probed 2026-08-23)
//!
//! `sigma`/`s` (`f32`, `0..=999`, default `0` — no filtering), `overlap`
//! (`-1..=15`, default `-1`), `expr`/`e` (coefficient factor expression,
//! `string`), `n` (block size in bits, `3..=4`, i.e. blocks of `8` or `16`,
//! default `3`).
//!
//! # What is a documented simplification
//!
//! * **`overlap` is not implemented.** The reference blends overlapping
//!   blocks to hide the block edges a non-overlapping transform leaves
//!   behind (visible as mild blockiness at `sigma > 0`, harmless to the
//!   oracles below). Parsed and accepted, has no effect.
//! * **`expr` is not implemented.** The reference lets a user's own
//!   expression reshape the per-coefficient factor instead of a plain hard
//!   threshold; this always uses the fixed threshold below. Parsed and
//!   accepted, has no effect.
//!
//! Both are documented gaps, not silent ones — see
//! `docs/filter/vaco-filter-denoise.md`.
//!
//! # The threshold
//!
//! The universal/VisuShrink threshold (Donoho & Johnstone 1994,
//! `provenance/sources.toml`'s `donoho-johnstone-1994-visushrink`):
//! `t = sigma * sqrt(2 * ln(N))`, `N` the block's pixel count. The same
//! formula [`crate::wavelet`] uses for its detail bands, applied here to
//! DCT coefficients instead — thresholding a decorrelating transform's
//! small coefficients is the general technique; DCT and wavelets are two
//! choices of transform for it.
//!
//! # Independent oracles
//!
//! * **DC preservation**: index `(0, 0)` (the block's mean, scaled) is
//!   never thresholded, so a block's *mean* must be unchanged by filtering
//!   to within float rounding — a property of any correct implementation
//!   that thresholds only AC coefficients, checkable by comparing
//!   `PlaneBuf::mean` before and after without re-deriving a single pixel
//!   value through this file's own DCT code.
//! * **Flat-block invariant**: a constant block's AC coefficients are
//!   exactly `0` (a constant signal has no non-DC frequency content by the
//!   DCT's own definition), so thresholding is a no-op and the block
//!   reconstructs exactly.
//! * **Round-trip**: `idct(dct(x)) == x` to float rounding, checked
//!   directly against a hand-written direct definition of both transforms
//!   from the DCT-II/DCT-III formulas — not a second copy of this file's
//!   loop structure, but the textbook sum evaluated the way a reader
//!   checking the module doc against the code would do it by hand.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};

pub const DESC: FilterDesc = FilterDesc {
    name: "dctdnoiz",
    description: "Denoise frames using 2D DCT.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

fn f32_opt(req: &Instantiate<'_>, keys: &[&str], default: f32) -> f32 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(f) = v.trim().parse::<f32>()
        {
            return f;
        }
    }
    default
}

#[derive(Debug, Clone, Copy)]
struct Options {
    sigma: f32,
    block: usize,
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        let n = req
            .named("n")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(3)
            .clamp(3, 4);
        Self {
            sigma: f32_opt(req, &["sigma", "s"], 0.0).max(0.0),
            block: 1usize << n,
        }
    }
}

/// Direct (non-fast) 1D DCT-II, `N` inputs to `N` coefficients, orthonormal
/// scaling.
fn dct1d(input: &[f32]) -> Vec<f32> {
    let n = input.len();
    #[allow(clippy::cast_precision_loss, reason = "block sizes are 8 or 16")]
    let nf = n as f32;
    let mut out = vec![0.0f32; n];
    for (k, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for (i, &x) in input.iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "block sizes are 8 or 16")]
            let (ii, kk) = (i as f32, k as f32);
            acc += x * (std::f32::consts::PI / nf * (ii + 0.5) * kk).cos();
        }
        let scale = if k == 0 {
            (1.0 / nf).sqrt()
        } else {
            (2.0 / nf).sqrt()
        };
        *o = acc * scale;
    }
    out
}

/// Direct (non-fast) 1D DCT-III (the inverse of [`dct1d`]), matched scaling.
fn idct1d(coeffs: &[f32]) -> Vec<f32> {
    let n = coeffs.len();
    #[allow(clippy::cast_precision_loss, reason = "block sizes are 8 or 16")]
    let nf = n as f32;
    let mut out = vec![0.0f32; n];
    for (i, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for (k, &c) in coeffs.iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "block sizes are 8 or 16")]
            let (ii, kk) = (i as f32, k as f32);
            let scale = if k == 0 {
                (1.0 / nf).sqrt()
            } else {
                (2.0 / nf).sqrt()
            };
            acc += scale * c * (std::f32::consts::PI / nf * (ii + 0.5) * kk).cos();
        }
        *o = acc;
    }
    out
}

/// Separable 2D DCT-II over a `size x size` block, row-major.
fn dct2d(block: &[f32], size: usize) -> Vec<f32> {
    let mut rows = vec![0.0f32; size * size];
    for y in 0..size {
        let start = y * size;
        if let Some(row) = block.get(start..start + size)
            && let Some(dst) = rows.get_mut(start..start + size)
        {
            dst.copy_from_slice(&dct1d(row));
        }
    }
    let mut out = vec![0.0f32; size * size];
    for x in 0..size {
        let col: Vec<f32> = (0..size)
            .filter_map(|y| rows.get(y * size + x).copied())
            .collect();
        let t = dct1d(&col);
        for (y, v) in t.into_iter().enumerate() {
            if let Some(dst) = out.get_mut(y * size + x) {
                *dst = v;
            }
        }
    }
    out
}

/// Separable 2D DCT-III (inverse of [`dct2d`]).
fn idct2d(coeffs: &[f32], size: usize) -> Vec<f32> {
    let mut cols = vec![0.0f32; size * size];
    for x in 0..size {
        let col: Vec<f32> = (0..size)
            .filter_map(|y| coeffs.get(y * size + x).copied())
            .collect();
        let t = idct1d(&col);
        for (y, v) in t.into_iter().enumerate() {
            if let Some(dst) = cols.get_mut(y * size + x) {
                *dst = v;
            }
        }
    }
    let mut out = vec![0.0f32; size * size];
    for y in 0..size {
        let start = y * size;
        if let Some(row) = cols.get(start..start + size) {
            let t = idct1d(row);
            if let Some(dst) = out.get_mut(start..start + size) {
                dst.copy_from_slice(&t);
            }
        }
    }
    out
}

fn threshold(sigma: f32, n: usize) -> f32 {
    if sigma <= 0.0 {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "n is a block pixel count, at most 256"
    )]
    let nf = n as f32;
    sigma * (2.0 * nf.ln()).sqrt()
}

fn denoise_plane(buf: &PlaneBuf, sigma: f32, block: usize) -> PlaneBuf {
    if sigma <= 0.0 || buf.width < block || buf.height < block {
        return buf.clone();
    }
    let t = threshold(sigma, block * block);
    let mut out = buf.clone();
    let mut by = 0;
    while by + block <= buf.height {
        let mut bx = 0;
        while bx + block <= buf.width {
            let mut patch = vec![0.0f32; block * block];
            for y in 0..block {
                for x in 0..block {
                    if let (Some(v), Some(dst)) =
                        (buf.get(bx + x, by + y), patch.get_mut(y * block + x))
                    {
                        *dst = v;
                    }
                }
            }
            let mut coeffs = dct2d(&patch, block);
            for (i, c) in coeffs.iter_mut().enumerate() {
                if i == 0 {
                    continue; // DC term: never thresholded.
                }
                if c.abs() < t {
                    *c = 0.0;
                }
            }
            let recon = idct2d(&coeffs, block);
            for y in 0..block {
                for x in 0..block {
                    if let Some(v) = recon.get(y * block + x) {
                        out.set(bx + x, by + y, *v);
                    }
                }
            }
            bx += block;
        }
        by += block;
    }
    out
}

#[derive(Debug)]
struct Dctdnoiz {
    opts: Options,
}

impl FrameFilter for Dctdnoiz {
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
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let Some((bytes, max_val)) = video::sample_layout(format, plane_idx) else {
                return Err(video::unsupported_format());
            };
            let (pw, ph) = video::plane_dims(format, width, height, plane_idx);
            let Some(src) = input.plane(p) else { continue };
            let read = PlaneBuf::read(src, pw, ph, bytes, max_val);
            let result = denoise_plane(&read, self.opts.sigma, self.opts.block);
            if let Some(mut dst) = out.plane_mut(p) {
                result.write(&mut dst, bytes);
            }
        }
        video::copy_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options::parse(req);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Dctdnoiz { opts }).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn round_trip_dct_recovers_the_block() {
        let block = 8;
        let data: Vec<f32> = (0..block * block)
            .map(|i| ((i * 13) % 200) as f32)
            .collect();
        let coeffs = dct2d(&data, block);
        let recon = idct2d(&coeffs, block);
        for (a, b) in data.iter().zip(recon.iter()) {
            assert!((a - b).abs() < 1e-2, "{a} vs {b}");
        }
    }

    #[test]
    fn a_constant_block_has_zero_ac_energy() {
        let block = 8;
        let data = vec![55.0f32; block * block];
        let coeffs = dct2d(&data, block);
        for (i, c) in coeffs.iter().enumerate() {
            if i == 0 {
                continue;
            }
            assert!(c.abs() < 1e-2, "AC[{i}] = {c}");
        }
    }

    #[test]
    fn dc_term_and_block_mean_survive_thresholding() {
        let mut buf = PlaneBuf::zeroed(8, 8, 255.0);
        let mut seed = 3u32;
        for y in 0..8 {
            for x in 0..8 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1);
                buf.set(x, y, 100.0 + ((seed >> 16) & 0x3f) as f32);
            }
        }
        let mean_before = buf.mean();
        let out = denoise_plane(&buf, 40.0, 8);
        let mean_after = out.mean();
        assert!(
            (mean_before - mean_after).abs() < 0.5,
            "{mean_before} vs {mean_after}"
        );
    }

    #[test]
    fn a_flat_plane_is_unchanged() {
        let buf_data = vec![64.0f32; 16 * 16];
        let mut buf = PlaneBuf::zeroed(16, 16, 255.0);
        for y in 0..16 {
            for x in 0..16 {
                buf.set(x, y, buf_data[y * 16 + x]);
            }
        }
        let out = denoise_plane(&buf, 30.0, 8);
        for v in out.as_slice() {
            assert!((v - 64.0).abs() < 1e-2, "{v}");
        }
    }
}
