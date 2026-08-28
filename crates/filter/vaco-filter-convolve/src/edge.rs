//! Shared engine for the "kirsch/prewitt/roberts/scharr/sobel" option class
//! `ffmpeg -h filter=sobel` names — three of the five ([`crate::sobel`],
//! [`crate::prewitt`], [`crate::scharr`]) share this module's two-gradient
//! engine; [`crate::roberts`] and [`crate::kirsch`] are their own modules
//! (different kernel shape and, as measured below, different border
//! behaviour).
//!
//! All five share one option set: `planes` (default `15`), `scale`
//! (`0..=65535`, default `1`), `delta` (`-65535..=65535`, default `0`).
//!
//! # The formula, measured
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray:s=5x5,format=gray8,geq=lum='10*X'" -vf sobel \
//!   -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! Interior comes back `80` for every row (field varies only in `X`, so
//! `Gy=0` everywhere): the standard Sobel `Gx=[-1,0,1;-2,0,2;-1,0,1]` gives
//! `Gx=80` on this ramp, `magnitude=sqrt(80^2+0^2)=80` — an exact match,
//! confirming both the published kernel and `magnitude=sqrt(Gx^2+Gy^2)`
//! (`scale=1`, `delta=0` are this run's defaults). The border comes back
//! `0` on this source, consistent with (but — see below — not sufficient
//! to prove) [`crate::convolution`]'s border rule; this crate's `sobel`/
//! `prewitt` reuse [`crate::convolution::Kernel`] directly.
//!
//! `prewitt` on the same input gives `60` (`Gx=[-1,0,1;-1,0,1;-1,0,1]`,
//! `60 = |{-10+30}| * 3`... i.e. `20*3`).
//!
//! `scharr` gives `20`, not the unnormalised `320` textbook
//! `Gx=[-3,0,3;-10,0,10;-3,0,3]` would produce: the reference divides by
//! `16` before combining (`320/16=20`). That divisor is folded into
//! [`SCHARR_GX`]/[`SCHARR_GY`] via `rdiv=16` rather than left for a caller
//! to apply.
//!
//! # Correction and pin, 2026-08-28: the inherited "zero border" was never
//! actually verified for `sobel` itself; the real rule is `reflect-101`
//!
//! `vaco-conformance`'s argument-vector corpus tried `sobel` against a
//! source varying in *both* `X` and `Y` (`mod(X*7+Y*11,256)`, `20x20`) —
//! the original border claim above was measured on a source varying in
//! `X` only (`10*X`), which makes every row identical and therefore
//! cannot distinguish "the whole border row is forced to `0`" from "the
//! row is computed normally, and it correctly comes back `0` because a
//! `Y`-invariant image really does have zero vertical gradient
//! everywhere, border included." That is exactly the same shape of blind
//! spot `vectorscope`'s frame-size/hit-count conflation and `waveform`'s
//! `intensity=1` saturation were: a source that cannot separate two
//! hypotheses is not evidence for either one.
//!
//! Against the two-axis source, this crate's own (pre-fix) output forced
//! the *entire* top/bottom border row to `0`, but the reference did not:
//! only isolated positions read `0`. A follow-up corner/edge probe (see
//! [`crate::convolution`]'s doc for the full numbers — same `Kernel`
//! engine, so the finding transfers directly) pinned the actual rule as
//! **`reflect-101`**: mirror the out-of-bounds tap back across the border
//! without duplicating the edge pixel, applied independently per axis
//! (simultaneously on both axes at a corner). Both a hard-zero-if-any-tap-
//! OOB rule and plain clamp-to-edge were checked against the same corner
//! and edge cells and neither matches; reflect-101 matches exactly at
//! every point tried, including the corner.
//!
//! **Fixed**: [`crate::convolution::Kernel::value_at`] now samples via
//! [`crate::common::sample_reflect101`] instead of returning a hard zero,
//! so `convolution`, `sobel`, `prewitt`, and `scharr` all share the pinned
//! rule. `roberts` and `kirsch` are separate modules (different kernel
//! shape, not routed through `Kernel::value_at`) with their own,
//! independently measured border behaviour, unaffected by this change.
//! `erosion`/`dilation`/`deflate`/`inflate` also do not go through this
//! engine — they share a different module, `crate::morph`, with its own
//! separately-reasoned border claim; see that module's doc.
//!
//! # `scharr`'s interior divergence: truncation-order hypothesis tested and
//! refuted; the real cause looks like reference-side numerical noise, not
//! a discoverable rule
//!
//! Separately from the border fix above, an exhaustive per-pixel check
//! (every pixel of a two-axis discriminating source, `sobel`/`prewitt`
//! both at zero mismatches) found `scharr`'s magnitude off by a small
//! amount (`1`-`3`) at a large fraction of *interior*, non-border pixels
//! -- not explained by the border rule at all.
//!
//! **Hypothesis tested: per-component truncation before combining, the
//! same shape as `waveform`'s `step = floor(intensity*255)` bug.** If the
//! reference truncated `Gx`/`Gy` to integers before `sqrt(Gx^2+Gy^2)`
//! rather than after, that would explain a systematic, direction-
//! consistent loss of magnitude. It does not hold here: at every mismatch
//! checked, `Gx/rdiv` and `Gy/rdiv` were already exact integers (the raw
//! pre-division sums were exact multiples of `rdiv=16`), so there is no
//! fractional part for truncation order to act on -- the hypothesis has
//! nothing to bite on for the pixels that actually diverge. Refuted, not
//! merely unconfirmed.
//!
//! **What actually falsifies "any formula of `(Gx, Gy)`" entirely**: two
//! *different* real 3x3 windows, sampled directly from the reference's own
//! raw output (not this crate's model of it), produce bit-identical
//! `Gx=192, Gy=704` --
//!
//! ```text
//! window A, centred (9,1):   window B, centred (25,1):
//!   8 105 202                  24 121 218
//! 169  23 133                 137 247 101
//!  74 197  64                 250 117 240
//! ```
//!
//! -- confirmed by hand (`Gx = -3*8+3*202-10*169+10*133-3*74+3*64 = 192`
//! for window A, and the same arithmetic on window B's corners/edges also
//! gives `192`; `Gy` likewise gives `704` for both). Yet real
//! `ffmpeg 8.1 -vf scharr` gives `46` at `(9,1)` and `44` at `(25,1)` --
//! **two provably identical gradient vectors, two different outputs**.
//! No function of `(Gx, Gy)` alone -- truncated, rounded, rescaled, or
//! otherwise -- can produce that. Reproduced deterministically (same
//! result across repeated runs and `-filter_threads 1`), so it is not a
//! threading race either.
//!
//! **Left open, on purpose, past the point most divergences in this
//! campaign get pinned.** The position-dependence for identical inputs
//! is the signature of floating-point/SIMD implementation noise (e.g. an
//! accelerated `sqrt`/`hypot` path whose rounding depends on vector-lane
//! alignment, not on the mathematical inputs) rather than a documented or
//! discoverable behavioural rule. Chasing that further would mean
//! reverse-engineering one specific reference binary's accelerated code
//! path rather than measuring a rule a clean-room reimplementation should
//! match -- the opposite of what pinning `reflect-101` above was. Per
//! `AGENT-CONSTRAINTS.md`: fitting a formula that matches most points,
//! when the evidence already shows no formula of the documented inputs
//! can match all of them, is the mistake this note exists to avoid
//! repeating. `scharr` stays out of the conformance corpus until this
//! either turns out to have a real rule after all, or is reclassified as
//! a permanent ceiling with a `downgrade_reason`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::convolution::{self, Kernel, Mode};

pub(crate) const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub(crate) const SOBEL_GX: &str = "-1 0 1 -2 0 2 -1 0 1";
pub(crate) const SOBEL_GY: &str = "-1 -2 -1 0 0 0 1 2 1";
pub(crate) const PREWITT_GX: &str = "-1 0 1 -1 0 1 -1 0 1";
pub(crate) const PREWITT_GY: &str = "-1 -1 -1 0 0 0 1 1 1";
pub(crate) const SCHARR_GX: &str = "-3 0 3 -10 0 10 -3 0 3";
pub(crate) const SCHARR_GY: &str = "-3 -10 -3 0 0 0 3 10 3";
pub(crate) const SCHARR_RDIV: f64 = 16.0;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "edge", help = "Apply an edge operator")]
pub(crate) struct Opts {
    #[opt(name = "planes", help = "set planes to filter", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
    #[opt(name = "scale", help = "set scale", default = 1.0, range = 0.0..=65535.0, flags(video, filtering))]
    pub scale: f64,
    #[opt(name = "delta", help = "set delta", default = 0.0, range = -65535.0..=65535.0, flags(video, filtering))]
    pub delta: f64,
}

impl Opts {
    pub(crate) fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

/// The two-gradient engine `sobel`/`prewitt`/`scharr` share.
#[derive(Debug)]
pub(crate) struct TwoGradient {
    gx: Kernel,
    gy: Kernel,
    planes: i64,
    scale: f64,
    delta: f64,
}

impl TwoGradient {
    pub(crate) fn new(
        gx: &str,
        gy: &str,
        rdiv: f64,
        opts: &Opts,
    ) -> std::result::Result<Self, String> {
        Ok(Self {
            gx: Kernel::parse(gx, Mode::Square, rdiv, 0.0)?,
            gy: Kernel::parse(gy, Mode::Square, rdiv, 0.0)?,
            planes: opts.planes,
            scale: opts.scale,
            delta: opts.delta,
        })
    }

    fn apply_plane(&self, rows: &[&[u8]], w: i32, h: i32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for y in 0..h {
            let mut row = Vec::new();
            for x in 0..w {
                let gx = self.gx.value_at(rows, x, y, w, h);
                let gy = self.gy.value_at(rows, x, y, w, h);
                let value = convolution::clamp_u8(gx.hypot(gy).mul_add(self.scale, self.delta));
                row.push(value);
            }
            out.push(row);
        }
        out
    }
}

impl FrameFilter for TwoGradient {
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
                self.apply_plane(&rows, pw, ph)
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

pub(crate) fn create_two_gradient(
    desc: FilterDesc,
    gx: &str,
    gy: &str,
    rdiv: f64,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = TwoGradient::new(gx, gy, rdiv, &opts)?;
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

pub(crate) const fn pad_desc(name: &'static str, description: &'static str) -> FilterDesc {
    FilterDesc {
        name,
        description,
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "test code; iterating a fixed interior sub-range of a 2D Vec"
)]
mod tests {
    use super::*;

    /// Documents this module's doc finding rather than pinning a value
    /// against the reference: two real, distinct 3x3 windows measured
    /// from real `ffmpeg 8.1 -vf scharr` output give bit-identical
    /// `(Gx, Gy) = (192, 704)` yet different reference outputs (`46` and
    /// `44`). Any function of `(Gx, Gy)` alone -- including this crate's
    /// own -- necessarily gives the *same* answer for both, so it can
    /// only ever match one of the two real outputs. This is why `scharr`
    /// is left out of the conformance corpus rather than tuned further:
    /// there is no formula of the documented inputs left to try.
    #[test]
    fn scharr_cannot_match_both_real_outputs_for_these_two_identical_gradient_windows() {
        let opts = Opts::default();
        let g = TwoGradient::new(SCHARR_GX, SCHARR_GY, SCHARR_RDIV, &opts).unwrap();

        let window_a: Vec<Vec<u8>> = vec![vec![8, 105, 202], vec![169, 23, 133], vec![74, 197, 64]];
        let window_b: Vec<Vec<u8>> =
            vec![vec![24, 121, 218], vec![137, 247, 101], vec![250, 117, 240]];
        let rows_a: Vec<&[u8]> = window_a.iter().map(Vec::as_slice).collect();
        let rows_b: Vec<&[u8]> = window_b.iter().map(Vec::as_slice).collect();
        let out_a = g.apply_plane(&rows_a, 3, 3);
        let out_b = g.apply_plane(&rows_b, 3, 3);

        // Our engine is a pure function of the window, so identical
        // (Gx, Gy) necessarily produces identical output here: 45,
        // computed from hypot(192/16, 704/16) = hypot(12, 44) = 45.6,
        // truncated. The real reference splits 46/44 across these two
        // windows (see this module's doc) -- neither of which is 45 --
        // proving no single-valued function of (Gx, Gy) can match both.
        assert_eq!(out_a[1][1], out_b[1][1]);
        assert_eq!(out_a[1][1], 45);
    }

    fn ramp(w: usize, h: usize) -> Vec<Vec<u8>> {
        (0..h)
            .map(|_| (0..w).map(|x| (x as u8) * 10).collect())
            .collect()
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn sobel_interior_matches_the_reference() {
        let opts = Opts::default();
        let g = TwoGradient::new(SOBEL_GX, SOBEL_GY, 1.0, &opts).unwrap();
        let img = ramp(5, 5);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = g.apply_plane(&rows, 5, 5);
        assert_eq!(out[2][1], 80);
        assert_eq!(out[2][2], 80);
        assert_eq!(out[2][3], 80);
        assert_eq!(out[2][0], 0);
        assert_eq!(out[2][4], 0);
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn prewitt_interior_matches_the_reference() {
        let opts = Opts::default();
        let g = TwoGradient::new(PREWITT_GX, PREWITT_GY, 1.0, &opts).unwrap();
        let img = ramp(5, 5);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = g.apply_plane(&rows, 5, 5);
        assert_eq!(out[2][2], 60);
    }

    /// Pinned against the reference probe in this module's doc: confirms
    /// the measured `rdiv=16` normalisation.
    #[test]
    fn scharr_interior_matches_the_reference() {
        let opts = Opts::default();
        let g = TwoGradient::new(SCHARR_GX, SCHARR_GY, SCHARR_RDIV, &opts).unwrap();
        let img = ramp(5, 5);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = g.apply_plane(&rows, 5, 5);
        assert_eq!(out[2][2], 20);
    }

    /// The discriminating probe from this module's doc: a two-axis,
    /// all-distinct-values source run through `sobel`'s full magnitude.
    /// Pinned against real `ffmpeg 8.1 -vf sobel` output at the corner
    /// (`0`) and an adjacent edge cell (`8`) — the `ramp` source above
    /// can't tell reflect-101 apart from a hard zero-border rule since it
    /// varies in only one axis; this one can.
    #[test]
    fn sobel_border_uses_reflect_101_confirmed_at_a_corner() {
        let opts = Opts::default();
        let g = TwoGradient::new(SOBEL_GX, SOBEL_GY, 1.0, &opts).unwrap();
        let img: Vec<Vec<u8>> = (0..5)
            .map(|y| (0..5).map(|x| (1 + 10 * y + x) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = g.apply_plane(&rows, 5, 5);
        assert_eq!(out[0][0], 0, "corner: both axes reflect, Gx=Gy=0");
        assert_eq!(out[0][1], 8, "edge: only the row axis reflects");
    }

    /// Independent oracle: a uniform (DC) field has zero gradient
    /// everywhere in the interior for any of these three operators — a
    /// property of "derivative of a constant", not a re-derivation of the
    /// kernel arithmetic.
    #[test]
    fn a_constant_field_has_no_interior_edges() {
        let opts = Opts::default();
        for (gx, gy, rdiv) in [
            (SOBEL_GX, SOBEL_GY, 1.0),
            (PREWITT_GX, PREWITT_GY, 1.0),
            (SCHARR_GX, SCHARR_GY, SCHARR_RDIV),
        ] {
            let g = TwoGradient::new(gx, gy, rdiv, &opts).unwrap();
            let img = vec![vec![128u8; 5]; 5];
            let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
            let out = g.apply_plane(&rows, 5, 5);
            for y in 1..4 {
                for x in 1..4 {
                    assert_eq!(out[y][x], 0, "({x},{y})");
                }
            }
        }
    }
}
