//! `nlmeans` — Non-local means denoising (Buades, Coll & Morel, 2005): every
//! pixel is replaced by a weighted average of *every* pixel in a search
//! window whose surrounding patch looks similar, not just its immediate
//! spatial neighbours.
//!
//! # Options (`ffmpeg -h filter=nlmeans`, probed 2026-08-23)
//!
//! `s` — denoising strength `h` (`f64`, `1..=30`, default `1`); `p` — patch
//! size (`int`, `0..=99`, default `7`); `pc` — chroma patch size (default
//! `0`, meaning "same as `p`"); `r` — research window size (default `15`);
//! `rc` — chroma research window (default `0`, meaning "same as `r`").
//!
//! # Algorithm
//!
//! For pixel `i` with patch radius `pr = p / 2` and search radius `rr = r /
//! 2`, over every `j` within `rr` of `i`:
//!
//! ```text
//! d2(i, j)  = mean over the patch of (I(i + o) - I(j + o))^2
//! w(i, j)   = exp(-d2(i, j) / h^2)
//! out(i)    = sum_j w(i, j) * I(j) / sum_j w(i, j)
//! ```
//!
//! `h` is `s`, taken directly in the plane's own intensity units (Buades et
//! al.'s "filtering parameter", provenance/sources.toml's
//! `buades-coll-morel-2005-nlmeans` entry) — the reference's `s` range
//! (`1..=30`) is small relative to an 8-bit plane's `0..=255`, matching this
//! reading of it as a strength directly in sample units rather than a
//! normalised fraction.
//!
//! # Independent oracles
//!
//! * **Flat-field invariant**: on a constant plane every patch distance is
//!   `0`, so every weight is equal; the weighted average of identical
//!   values is that value, exactly, regardless of the weight formula's
//!   details.
//! * **Noise-power bound**: on a plane that is constant *plus* independent
//!   per-pixel noise, every patch is — up to the noise — identical to every
//!   other, so NLM degenerates toward an average over the whole search
//!   window and the output's sample variance must fall well below the
//!   input's. This is the paper's own claimed behaviour in the flat-region
//!   case, checked as a variance inequality rather than by re-deriving the
//!   pixel values NLM itself would produce.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};

pub const DESC: FilterDesc = FilterDesc {
    name: "nlmeans",
    description: "Non-local means denoiser.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

fn f64_opt(req: &Instantiate<'_>, key: &str, default: f64) -> f64 {
    req.named(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

fn usize_opt(req: &Instantiate<'_>, key: &str, default: usize) -> usize {
    req.named(key)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

#[derive(Debug, Clone, Copy)]
struct Options {
    h: f32,
    patch: usize,
    patch_chroma: usize,
    research: usize,
    research_chroma: usize,
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        let patch = usize_opt(req, "p", 7);
        let research = usize_opt(req, "r", 15);
        let pc = usize_opt(req, "pc", 0);
        let rc = usize_opt(req, "rc", 0);
        Self {
            h: f64_opt(req, "s", 1.0).max(0.001) as f32,
            patch,
            patch_chroma: if pc == 0 { patch } else { pc },
            research,
            research_chroma: if rc == 0 { research } else { rc },
        }
    }

    fn radii_for(&self, plane: usize) -> (i64, i64) {
        let (p, r) = if plane == 0 {
            (self.patch, self.research)
        } else {
            (self.patch_chroma, self.research_chroma)
        };
        #[allow(
            clippy::integer_division,
            reason = "patch/research radius from a diameter option; truncation toward zero is the intended rounding"
        )]
        let (pr, rr) = ((p / 2).max(1), (r / 2).max(1));
        (
            i64::try_from(pr).unwrap_or(i64::MAX),
            i64::try_from(rr).unwrap_or(i64::MAX),
        )
    }
}

/// Sum of squared differences between the `(2*pr+1)^2` patches centred at
/// `(x1, y1)` and `(x2, y2)`, normalised by patch pixel count.
///
/// Only [`nlmeans_plane_naive`] (kept for the differential test and the
/// benchmark, see its own doc) still calls this directly;
/// [`nlmeans_plane`]'s integral-image path computes the same quantity
/// without re-walking the patch per candidate offset.
pub(crate) fn patch_distance(buf: &PlaneBuf, x1: i64, y1: i64, x2: i64, y2: i64, pr: i64) -> f32 {
    let mut acc = 0.0f32;
    let mut n = 0.0f32;
    for dy in -pr..=pr {
        for dx in -pr..=pr {
            let a = buf.get_clamped(x1 + dx, y1 + dy);
            let b = buf.get_clamped(x2 + dx, y2 + dy);
            acc += (a - b) * (a - b);
            n += 1.0;
        }
    }
    if n > 0.0 { acc / n } else { 0.0 }
}

/// Reference implementation: every one of the `(2*rr+1)^2` candidate offsets
/// re-walks the whole `(2*pr+1)^2` patch from scratch via [`patch_distance`],
/// `O(w*h*(2*rr+1)^2*(2*pr+1)^2)` total. Kept as the correctness oracle
/// [`nlmeans_plane`]'s integral-image reformulation below is checked
/// against (`tests::fast_path_agrees_with_naive_reference`), and as the
/// module's own record of the "obvious" algorithm the fast path replaces.
/// Also the baseline `benches/nlmeans.rs` measures the fast path against.
pub(crate) fn nlmeans_plane_naive(buf: &PlaneBuf, h: f32, pr: i64, rr: i64) -> PlaneBuf {
    let mut out = PlaneBuf::zeroed(buf.width, buf.height, buf.max_val);
    let h2 = (h * h).max(1e-6);
    for y in 0..buf.height {
        for x in 0..buf.width {
            let xi = i64::try_from(x).unwrap_or(i64::MAX);
            let yi = i64::try_from(y).unwrap_or(i64::MAX);
            let mut num = 0.0f32;
            let mut den = 0.0f32;
            for dy in -rr..=rr {
                for dx in -rr..=rr {
                    let (jx, jy) = (xi + dx, yi + dy);
                    let d2 = patch_distance(buf, xi, yi, jx, jy, pr);
                    let w = (-d2 / h2).exp();
                    num += w * buf.get_clamped(jx, jy);
                    den += w;
                }
            }
            out.set(x, y, if den > 0.0 { num / den } else { buf.get_clamped(xi, yi) });
        }
    }
    out
}

/// Zero-padded 2D prefix sum (`(w+1) x (h+1)`) over a flat, row-major `w*h`
/// buffer, so [`rect_sum`] answers any axis-aligned rectangle query in four
/// lookups regardless of the rectangle's size.
fn build_integral(data: &[f32], w: usize, h: usize) -> Vec<f32> {
    let stride = w + 1;
    let mut integral = vec![0.0f32; stride.saturating_mul(h + 1)];
    for y in 0..h {
        let mut row_sum = 0.0f32;
        for x in 0..w {
            row_sum += data.get(y * w + x).copied().unwrap_or(0.0);
            let above = integral.get(y * stride + x + 1).copied().unwrap_or(0.0);
            if let Some(cell) = integral.get_mut((y + 1) * stride + x + 1) {
                *cell = above + row_sum;
            }
        }
    }
    integral
}

/// Inclusive rectangle sum over `[x0, x1] x [y0, y1]` from an integral image
/// [`build_integral`] produced for a `w`-wide source.
fn rect_sum(integral: &[f32], w: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> f32 {
    let stride = w + 1;
    let at = |xx: usize, yy: usize| integral.get(yy * stride + xx).copied().unwrap_or(0.0);
    at(x1 + 1, y1 + 1) - at(x1 + 1, y0) - at(x0, y1 + 1) + at(x0, y0)
}

/// Non-local means, reformulated so `patch_distance`'s own `O((2*pr+1)^2)`
/// re-walk of every candidate patch is replaced by an `O(1)` integral-image
/// lookup: `O(w*h*(2*rr+1)^2)` total rather than
/// `O(w*h*(2*rr+1)^2*(2*pr+1)^2)`, [`nlmeans_plane_naive`]'s cost.
///
/// # The reformulation, and why it is exact rather than approximate
///
/// For a fixed candidate offset `(dx, dy)`, `patch_distance(buf, x, y, x+dx,
/// y+dy, pr)` is the mean, over `(dx', dy')` in the patch, of `(buf[x+dx',
/// y+dy'] - buf[x+dx+dx', y+dy+dy'])^2`. Defining `diff(px, py) = (buf[px,
/// py] - buf[px+dx, py+dy])^2` (both reads clamp-to-edge, same as
/// [`PlaneBuf::get_clamped`] always did), that squared term is exactly
/// `diff(x+dx', y+dy')` — so the *whole patch sum* for pixel `(x, y)` at
/// this offset is a box sum of `diff` centred at `(x, y)`, not merely
/// related to one. Building `diff` once per offset (`O(w*h)`) and its
/// integral image (`O(w*h)`) turns every pixel's patch sum at that offset
/// into one `O(1)` [`rect_sum`] call, which is where the `(2*pr+1)^2` factor
/// disappears.
///
/// `diff` is built over a domain extended by `pr` on every side (`(w +
/// 2*pr) x (h + 2*pr)`) rather than clamping `(x+dx', y+dy')` into `[0, w) x
/// [0, h)` before evaluating it, because `get_clamped` clamps `buf[px, py]`
/// and `buf[px+dx, py+dy]` *independently* — clamping `(px, py)` first and
/// then applying the offset would clamp the wrong one of the two reads
/// whenever a patch tap itself falls outside the plane, silently changing
/// which pixel pair a border patch compares. The extended domain means
/// every patch window used by any real pixel is fully covered by real
/// (correctly independently-clamped) `diff` values, so no further clamping
/// is needed once the domain is built.
pub(crate) fn nlmeans_plane(buf: &PlaneBuf, h: f32, pr: i64, rr: i64) -> PlaneBuf {
    let w = buf.width;
    let ht = buf.height;
    let h2 = (h * h).max(1e-6);
    let upr = usize::try_from(pr).unwrap_or(0);
    let ext_w = w.saturating_add(2 * upr);
    let ext_h = ht.saturating_add(2 * upr);
    let patch_area = {
        let side = 2.0f32 * (pr as f32) + 1.0;
        (side * side).max(1.0)
    };

    let mut num = vec![0.0f32; w.saturating_mul(ht)];
    let mut den = vec![0.0f32; w.saturating_mul(ht)];

    for dy in -rr..=rr {
        for dx in -rr..=rr {
            // `diff(ex, ey)` covers real coordinate `(ex - pr, ey - pr)`, so
            // every patch window a real pixel needs (`[x, x+2pr] x [y,
            // y+2pr]` in extended coordinates) stays fully inside `[0,
            // ext_w) x [0, ext_h)`.
            let mut diff = vec![0.0f32; ext_w.saturating_mul(ext_h)];
            for ey in 0..ext_h {
                let ry = i64::try_from(ey).unwrap_or(0) - pr;
                for ex in 0..ext_w {
                    let rx = i64::try_from(ex).unwrap_or(0) - pr;
                    let a = buf.get_clamped(rx, ry);
                    let b = buf.get_clamped(rx + dx, ry + dy);
                    if let Some(cell) = diff.get_mut(ey * ext_w + ex) {
                        *cell = (a - b) * (a - b);
                    }
                }
            }
            let integral = build_integral(&diff, ext_w, ext_h);

            for y in 0..ht {
                for x in 0..w {
                    let sum = rect_sum(&integral, ext_w, x, y, x + 2 * upr, y + 2 * upr);
                    let d2 = sum / patch_area;
                    let weight = (-d2 / h2).exp();
                    let xi = i64::try_from(x).unwrap_or(i64::MAX);
                    let yi = i64::try_from(y).unwrap_or(i64::MAX);
                    let idx = y * w + x;
                    if let (Some(n), Some(d)) = (num.get_mut(idx), den.get_mut(idx)) {
                        *n += weight * buf.get_clamped(xi + dx, yi + dy);
                        *d += weight;
                    }
                }
            }
        }
    }

    let mut out = PlaneBuf::zeroed(w, ht, buf.max_val);
    for y in 0..ht {
        for x in 0..w {
            let idx = y * w + x;
            let n = num.get(idx).copied().unwrap_or(0.0);
            let d = den.get(idx).copied().unwrap_or(0.0);
            let xi = i64::try_from(x).unwrap_or(i64::MAX);
            let yi = i64::try_from(y).unwrap_or(i64::MAX);
            out.set(x, y, if d > 0.0 { n / d } else { buf.get_clamped(xi, yi) });
        }
    }
    out
}

#[derive(Debug)]
struct NlMeans {
    opts: Options,
}

impl FrameFilter for NlMeans {
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
            let (pr, rr) = self.opts.radii_for(p);
            let result = nlmeans_plane(&read, self.opts.h, pr, rr);
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
        filter: Box::new(Simple::new(NlMeans { opts }).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_flat_field_is_unchanged() {
        let mut buf = PlaneBuf::zeroed(10, 10, 255.0);
        for y in 0..10 {
            for x in 0..10 {
                buf.set(x, y, 90.0);
            }
        }
        let out = nlmeans_plane(&buf, 5.0, 1, 2);
        for v in out.as_slice() {
            assert!((v - 90.0).abs() < 1e-2, "{v}");
        }
    }

    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let n = ((*seed >> 16) & 0xff) as f32;
        n - 127.5
    }

    #[test]
    fn noise_on_a_flat_field_is_reduced() {
        let (w, h) = (12, 12);
        let mut buf = PlaneBuf::zeroed(w, h, 255.0);
        let mut seed = 99u32;
        for y in 0..h {
            for x in 0..w {
                buf.set(x, y, 128.0 + lcg(&mut seed) * 0.3);
            }
        }
        let noisy_var = buf.variance();
        // `h` well above the noise's own scale so patch-distance
        // differences (which are pure noise here, the underlying signal
        // being flat) barely affect the weight — pushing every pixel in the
        // research window toward equal weighting, i.e. toward a plain
        // spatial average over the window.
        let out = nlmeans_plane(&buf, 60.0, 2, 5);
        assert!(
            out.variance() < noisy_var * 0.5,
            "expected substantial variance reduction: {} vs {}",
            out.variance(),
            noisy_var
        );
    }

    /// The integral-image fast path must agree with the brute-force
    /// reference to within float summation-order noise: both compute the
    /// same sums over the same terms (see `nlmeans_plane`'s own doc for why
    /// this is an exact reformulation, not an approximation), just
    /// accumulated in a different order, so exact bit-equality is not
    /// expected but the numbers should be indistinguishable at 8-bit
    /// sample precision.
    ///
    /// Deliberately not a flat or single-axis field: a source that cannot
    /// separate two rules validates neither, and a flat field makes every
    /// patch distance zero on both paths regardless of a border-clamping
    /// mistake — so this uses a
    /// two-axis ramp-plus-checker pattern with real edge content, and a
    /// patch/research radius pair that pushes real patches past the plane's
    /// own border on every side.
    #[test]
    fn fast_path_agrees_with_naive_reference_including_at_the_border() {
        let (w, h) = (14, 11);
        let mut buf = PlaneBuf::zeroed(w, h, 255.0);
        for y in 0..h {
            for x in 0..w {
                let ramp = ((x * 7 + y * 13) % 200) as f32;
                let checker = if (x + 2 * y) % 5 == 0 { 40.0 } else { 0.0 };
                buf.set(x, y, ramp + checker);
            }
        }
        for &(pr, rr) in &[(1i64, 2i64), (3, 3), (2, 4)] {
            let fast = nlmeans_plane(&buf, 8.0, pr, rr);
            let naive = nlmeans_plane_naive(&buf, 8.0, pr, rr);
            for y in 0..h {
                for x in 0..w {
                    let a = fast.get(x, y).unwrap();
                    let b = naive.get(x, y).unwrap();
                    assert!(
                        (a - b).abs() < 0.05,
                        "pr={pr} rr={rr} ({x},{y}): fast={a} naive={b}"
                    );
                }
            }
        }
    }
}
