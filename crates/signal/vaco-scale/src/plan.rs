//! Lowering a conversion request into a fixed sequence of passes.
//!
//! Planning is deterministic and does not search. It emits the same three
//! stages every time — resample up, colour, resample down — and then deletes the
//! ones that turn out to be identities. A clever planner and a clever optimiser
//! interact unpredictably and neither can be tested alone (plan 17 §A.4.2), so
//! there is only the one.
//!
//! # The three stages, and why they are in that order
//!
//! A colour matrix mixes channels, so every channel it touches has to be at one
//! resolution. That forces:
//!
//! ```text
//!   unpack  ->  resample each channel to the "mid" grid
//!           ->  colour matrix (only if the mid grid is common)
//!           ->  resample each channel to its destination grid
//!           ->  pack
//! ```
//!
//! When no matrix is needed the mid grid *is* the destination grid, the third
//! stage vanishes, and every channel resamples exactly once with its own bank —
//! which is also how a chroma subsampling change and a chroma siting change come
//! out for free, as a size ratio and a phase on one bank.
//!
//! # Channel order is not the planner's problem
//!
//! `rgb24 -> bgr24` and `nv12 -> nv21` are identity plans. Channel indices are
//! *logical* in `vaco-pixfmt`, so a permutation is entirely a matter of where
//! [`crate::geometry`] reads and writes, and never reaches this module.

use std::sync::Arc;

use vaco_core::{Error, Result};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;
use vaco_simd::KernelSet as _;

use crate::colour::{self, ColorStage, Space};
use crate::fast::ScaleKernels;
use crate::filter::{FilterBank, FilterSpec, Kernel, build_bank, compose_after_point};
use crate::geometry::{ComponentLayout, FormatLayout, MAX_COMPS, ceil_shr};
use crate::options::{DitherKind, ScaleOptions};
use crate::spec::ImageSpec;

/// A resampling axis pair for one channel.
#[derive(Debug, Clone, Default)]
pub struct Resample {
    /// Horizontal bank, `None` when the axis is unchanged.
    pub h: Option<Arc<FilterBank>>,
    /// Vertical bank, `None` when the axis is unchanged.
    pub v: Option<Arc<FilterBank>>,
}

impl Resample {
    fn is_identity(&self) -> bool {
        self.h.is_none() && self.v.is_none()
    }
}

/// How one channel is obtained when the source format does not carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synthetic {
    /// Read it from the source.
    No,
    /// Fill with a constant: opaque alpha, or the chroma neutral point.
    Constant(i32),
}

/// Per-channel plan.
#[derive(Debug, Clone)]
pub struct ChannelPlan {
    /// Source grid size. Meaningless for a synthetic channel.
    pub src: (u32, u32),
    /// Grid the colour stage runs on.
    pub mid: (u32, u32),
    /// Destination grid size.
    pub dst: (u32, u32),
    /// Source depth, before expansion to the working depth.
    pub src_depth: u8,
    /// Destination depth.
    pub dst_depth: u8,
    /// Source -> mid.
    pub up: Resample,
    /// Mid -> destination.
    pub down: Resample,
    /// Whether the source carries this channel at all.
    pub synthetic: Synthetic,
    /// Whether this channel is written to the destination.
    pub written: bool,
}

/// What kind of work the conversion actually is.
#[derive(Debug, Clone)]
pub enum PlanKind {
    /// The two pictures are the same description: copy planes and stop.
    Copy,
    /// Unpack, resample, colour, resample, pack.
    General(Box<General>),
}

/// The general path's parameters.
#[derive(Debug, Clone)]
pub struct General {
    /// One entry per logical channel.
    pub ch: [ChannelPlan; MAX_COMPS],
    /// Channels the pipeline carries.
    pub live: usize,
    /// The colour stage.
    pub colour: ColorStage,
    /// Resolved dither method.
    pub dither: DitherKind,
    /// Integer precision every intermediate is carried at.
    pub work_depth: u8,
    /// Kernels resolved once, here, so the indirect call is paid per row.
    pub kernels: ScaleKernels,
}

/// A fully lowered conversion.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Source description.
    pub src: ImageSpec,
    /// Destination description.
    pub dst: ImageSpec,
    /// Source layout.
    pub src_layout: FormatLayout,
    /// Destination layout.
    pub dst_layout: FormatLayout,
    /// The work.
    pub kind: PlanKind,
    /// Destination rows one band covers, always a multiple of the destination's
    /// vertical chroma decimation so bands tile every plane exactly.
    pub band_rows: u32,
}

/// Rows of output one worker takes at a time. Below this, band setup costs more
/// than the band saves.
const MIN_BAND_ROWS: u32 = 16;

impl Plan {
    /// Whether this conversion copies its input unchanged.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        matches!(self.kind, PlanKind::Copy)
    }

    /// Lower a request.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a format or matrix outside the implemented
    /// set, [`Error::InvalidData`] for a degenerate size, and
    /// [`Error::LimitExceeded`] if the coefficient banks would not fit.
    pub fn build(
        budget: &mut Budget,
        src: &ImageSpec,
        dst: &ImageSpec,
        opts: &ScaleOptions,
    ) -> Result<Self> {
        let src_layout = ComponentLayout::derive(src.format, src.width, src.height)?;
        let dst_layout = ComponentLayout::derive(dst.format, dst.width, dst.height)?;

        let src = apply_range_override(src, opts.src_range_full);
        let dst = apply_range_override(dst, opts.dst_range_full);
        check_matrix(&src)?;
        check_matrix(&dst)?;

        if src.is_same_picture(&dst) && src_layout.is_trivially_addressed() {
            return Ok(Self {
                src,
                dst,
                src_layout,
                dst_layout,
                kind: PlanKind::Copy,
                band_rows: dst.height.max(1),
            });
        }

        let work_depth = src_layout
            .max_depth()
            .max(dst_layout.max_depth())
            .clamp(8, 16);
        let colour = colour::build(budget, &src, &dst, opts, work_depth)?;

        let out_comps = dst_layout.ncomp;
        let matrix_inputs = if colour.needs_common_resolution() {
            3
        } else {
            0
        };
        let live = out_comps.max(matrix_inputs).min(MAX_COMPS);

        let common_mid = colour.needs_common_resolution();
        let luma = opts.luma_kernel();
        let chroma = opts.chroma_kernel();
        let max_taps = opts.max_taps.clamp(1, 1024) as usize;

        let mut ch: [ChannelPlan; MAX_COMPS] = std::array::from_fn(|_| ChannelPlan {
            src: (0, 0),
            mid: (0, 0),
            dst: (0, 0),
            src_depth: work_depth,
            dst_depth: work_depth,
            up: Resample::default(),
            down: Resample::default(),
            synthetic: Synthetic::Constant(0),
            written: false,
        });

        for (c, slot) in ch.iter_mut().enumerate().take(live) {
            let sc = src_layout.comp(c);
            let dc = dst_layout.comp(c);
            let is_chroma = c == 1 || c == 2;
            let kernel = if is_chroma { chroma } else { luma };

            // Destination grid: the channel's own if it is written, otherwise
            // the mid grid (it exists only to feed the matrix).
            let dst_grid = dc.map_or_else(
                || mid_grid(&dst, &dst_layout, c, common_mid),
                |layout| (layout.width, layout.height),
            );
            let mid_grid_c = if common_mid {
                (dst.width, dst.height)
            } else {
                dst_grid
            };

            let (src_grid, synthetic, src_depth) = if let Some(s) = sc {
                ((s.width, s.height), Synthetic::No, s.depth)
            } else {
                let fill = synthetic_fill(&src, c, work_depth);
                (mid_grid_c, Synthetic::Constant(fill), work_depth)
            };

            let up = if synthetic == Synthetic::No {
                if common_mid && is_chroma && (src_layout.log2_w > 0 || src_layout.log2_h > 0) {
                    // The reference resamples chroma onto the *destination's*
                    // chroma grid and then replicates it onto the luma grid; it
                    // does not interpolate chroma at full resolution. Composing
                    // the two banks reproduces that exactly and costs one pass,
                    // not two. `full_chroma_int` opts into the interpolated
                    // form, which is what the flag has always meant.
                    let via = (
                        ceil_shr(mid_grid_c.0, src_layout.log2_w),
                        ceil_shr(mid_grid_c.1, src_layout.log2_h),
                    );
                    if opts
                        .flags
                        .contains(crate::options::SwsFlags::FULL_CHROMA_INT)
                    {
                        build_axes(
                            budget,
                            kernel,
                            src_grid,
                            mid_grid_c,
                            phase_pair(opts, common_mid, is_chroma),
                            max_taps,
                        )?
                    } else {
                        replicated_up(
                            budget,
                            kernel,
                            src_grid,
                            via,
                            mid_grid_c,
                            phase_pair(opts, common_mid, is_chroma),
                            max_taps,
                        )?
                    }
                } else {
                    build_axes(
                        budget,
                        kernel,
                        src_grid,
                        mid_grid_c,
                        phase_pair(opts, common_mid, is_chroma),
                        max_taps,
                    )?
                }
            } else {
                Resample::default()
            };
            let down = if common_mid && mid_grid_c != dst_grid {
                // Horizontal chroma decimation out of a full-resolution R'G'B'
                // picture is a plain pair average in the reference, whatever the
                // scaler flag says; only the vertical axis takes the selected
                // kernel. Measured with a single-pixel impulse — see the docs.
                let h_kernel = if is_chroma { Kernel::Area } else { kernel };
                let mut r = build_axes(
                    budget,
                    h_kernel,
                    mid_grid_c,
                    dst_grid,
                    down_phase(opts, is_chroma),
                    max_taps,
                )?;
                if is_chroma && h_kernel != kernel {
                    let v = build_axes(
                        budget,
                        kernel,
                        mid_grid_c,
                        dst_grid,
                        down_phase(opts, is_chroma),
                        max_taps,
                    )?;
                    r.v = v.v;
                }
                r
            } else {
                Resample::default()
            };

            *slot = ChannelPlan {
                src: src_grid,
                mid: mid_grid_c,
                dst: dst_grid,
                src_depth,
                dst_depth: dc.map_or(work_depth, |d| d.depth),
                up,
                down,
                synthetic,
                written: dc.is_some(),
            };
        }

        let dither = resolve_dither(opts.dither, &ch, live, work_depth);
        let sub_h = 1u32 << dst_layout.log2_h;
        let band_rows = MIN_BAND_ROWS.next_multiple_of(sub_h).max(sub_h);

        Ok(Self {
            src,
            dst,
            src_layout,
            dst_layout,
            kind: PlanKind::General(Box::new(General {
                ch,
                live,
                colour,
                dither,
                work_depth,
                kernels: ScaleKernels::select(),
            })),
            band_rows,
        })
    }

    /// A one-line-per-stage dump, for `-v debug` and for tests.
    #[must_use]
    pub fn explain(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{} {}x{} -> {} {}x{}",
            self.src.format.name(),
            self.src.width,
            self.src.height,
            self.dst.format.name(),
            self.dst.width,
            self.dst.height
        );
        match &self.kind {
            PlanKind::Copy => {
                let _ = writeln!(s, "  copy");
            }
            PlanKind::General(g) => {
                let _ = writeln!(s, "  work depth {}", g.work_depth);
                for (c, p) in g.ch.iter().enumerate().take(g.live) {
                    let _ = writeln!(
                        s,
                        "  ch{c} {:?} {}x{} -> {}x{} -> {}x{}{}{}",
                        p.synthetic,
                        p.src.0,
                        p.src.1,
                        p.mid.0,
                        p.mid.1,
                        p.dst.0,
                        p.dst.1,
                        if p.up.is_identity() { "" } else { " up" },
                        if p.down.is_identity() { "" } else { " down" },
                    );
                }
                let _ = writeln!(s, "  colour {:?}", g.colour);
                let _ = writeln!(s, "  dither {:?}", g.dither);
            }
        }
        s
    }
}

fn apply_range_override(spec: &ImageSpec, force_full: bool) -> ImageSpec {
    if !force_full {
        return *spec;
    }
    let mut out = *spec;
    out.color.range = vaco_color::ColorRange::Full;
    out
}

fn check_matrix(spec: &ImageSpec) -> Result<()> {
    if matches!(spec.space(), Space::Rgb) {
        return Ok(());
    }
    if colour::matrix_is_supported(spec.effective_matrix(), spec.color.primaries) {
        Ok(())
    } else {
        Err(Error::Unsupported(
            "matrix coefficients without a linear R'G'B' form",
        ))
    }
}

/// The grid a channel the destination does not carry should live on.
fn mid_grid(dst: &ImageSpec, dst_layout: &FormatLayout, c: usize, common: bool) -> (u32, u32) {
    if common || c == 0 || c == 3 {
        return (dst.width, dst.height);
    }
    (
        ceil_shr(dst.width, dst_layout.log2_w),
        ceil_shr(dst.height, dst_layout.log2_h),
    )
}

/// What a channel the source does not carry should be filled with.
fn synthetic_fill(src: &ImageSpec, c: usize, depth: u8) -> i32 {
    if c == 3 {
        // Opaque.
        return (1i32 << depth) - 1;
    }
    // A missing chroma channel is the neutral point, which is exactly what the
    // colour stage's bias already subtracts, so the two cancel.
    match src.effective_range() {
        vaco_color::ColorRange::Full => 1i32 << (depth - 1),
        _ => 128i32 << (depth - 8),
    }
}

/// `(horizontal, vertical)` siting phases for the up pass.
fn phase_pair(opts: &ScaleOptions, common_mid: bool, is_chroma: bool) -> [(f64, f64); 2] {
    if !is_chroma {
        return [(0.0, 0.0), (0.0, 0.0)];
    }
    let ps_h = chr_pos(opts.src_h_chr_pos);
    let ps_v = chr_pos(opts.src_v_chr_pos);
    // When the mid grid is the full-resolution one it has no siting of its own.
    let (pd_h, pd_v) = if common_mid {
        (0.0, 0.0)
    } else {
        (chr_pos(opts.dst_h_chr_pos), chr_pos(opts.dst_v_chr_pos))
    };
    [(ps_h, pd_h), (ps_v, pd_v)]
}

fn down_phase(opts: &ScaleOptions, is_chroma: bool) -> [(f64, f64); 2] {
    if !is_chroma {
        return [(0.0, 0.0), (0.0, 0.0)];
    }
    [
        (0.0, chr_pos(opts.dst_h_chr_pos)),
        (0.0, chr_pos(opts.dst_v_chr_pos)),
    ]
}

/// `-513` is the reference's "unset" sentinel and means no shift, which is what
/// the reference measurably applies by default.
fn chr_pos(v: i32) -> f64 {
    if v <= -513 { 0.0 } else { f64::from(v) / 256.0 }
}

/// Build a chroma up pass as "resample to the destination's chroma grid, then
/// replicate onto the luma grid".
#[allow(
    clippy::too_many_arguments,
    reason = "three grids and a phase pair; naming a struct for one call site is worse"
)]
fn replicated_up(
    budget: &mut Budget,
    kernel: Kernel,
    from: (u32, u32),
    via: (u32, u32),
    to: (u32, u32),
    phases: [(f64, f64); 2],
    max_taps: usize,
) -> Result<Resample> {
    let inner = build_axes(budget, kernel, from, via, phases, max_taps)?;
    Ok(Resample {
        h: compose_axis(budget, inner.h, from.0, via.0, to.0, kernel, phases[0])?,
        v: compose_axis(budget, inner.v, from.1, via.1, to.1, kernel, phases[1])?,
    })
}

/// One axis of [`replicated_up`].
fn compose_axis(
    budget: &mut Budget,
    inner: Option<Arc<FilterBank>>,
    from: u32,
    via: u32,
    to: u32,
    kernel: Kernel,
    phase: (f64, f64),
) -> Result<Option<Arc<FilterBank>>> {
    if via == to {
        return Ok(inner);
    }
    let point = build_bank(
        budget,
        &FilterSpec {
            kernel: Kernel::Point,
            src_len: via as usize,
            dst_len: to as usize,
            phase_src: 0.0,
            phase_dst: 0.0,
            max_taps: 1,
        },
    )?;
    let inner = if let Some(b) = inner {
        b
    } else {
        Arc::new(build_bank(
            budget,
            &FilterSpec {
                kernel,
                src_len: from as usize,
                dst_len: via as usize,
                phase_src: phase.0,
                phase_dst: phase.1,
                max_taps: 1,
            },
        )?)
    };
    let composed = compose_after_point(budget, &inner, &point)?;
    if composed.is_identity() {
        return Ok(None);
    }
    Ok(Some(Arc::new(composed)))
}

fn build_axes(
    budget: &mut Budget,
    kernel: Kernel,
    from: (u32, u32),
    to: (u32, u32),
    phases: [(f64, f64); 2],
    max_taps: usize,
) -> Result<Resample> {
    let mut out = Resample::default();
    let [(ph_s, ph_d), (pv_s, pv_d)] = phases;
    if from.0 != to.0 || ph_s != 0.0 || ph_d != 0.0 {
        out.h = Some(Arc::new(build_bank(
            budget,
            &FilterSpec {
                kernel,
                src_len: from.0 as usize,
                dst_len: to.0 as usize,
                phase_src: ph_s,
                phase_dst: ph_d,
                max_taps,
            },
        )?));
    }
    if from.1 != to.1 || pv_s != 0.0 || pv_d != 0.0 {
        out.v = Some(Arc::new(build_bank(
            budget,
            &FilterSpec {
                kernel,
                src_len: from.1 as usize,
                dst_len: to.1 as usize,
                phase_src: pv_s,
                phase_dst: pv_d,
                max_taps,
            },
        )?));
    }
    // A bank that turned out to be an identity is worse than no bank.
    if out.h.as_ref().is_some_and(|b| b.is_identity()) {
        out.h = None;
    }
    if out.v.as_ref().is_some_and(|b| b.is_identity()) {
        out.v = None;
    }
    Ok(out)
}

fn resolve_dither(
    requested: DitherKind,
    ch: &[ChannelPlan; MAX_COMPS],
    live: usize,
    work_depth: u8,
) -> DitherKind {
    match requested {
        DitherKind::None => DitherKind::None,
        DitherKind::Bayer => DitherKind::Bayer,
        DitherKind::Auto => {
            let drops = ch
                .iter()
                .take(live)
                .any(|p| p.written && p.dst_depth < work_depth);
            if drops {
                DitherKind::Bayer
            } else {
                DitherKind::None
            }
        }
    }
}

/// Whether a conversion between two formats can be planned at all.
#[must_use]
pub fn supports_conversion(src: PixFmt, dst: PixFmt) -> bool {
    crate::geometry::supports_input(src) && crate::geometry::supports_output(dst)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn plan(s: PixFmt, sw: u32, sh: u32, d: PixFmt, dw: u32, dh: u32) -> Plan {
        let mut b = Budget::new(Limits::permissive());
        Plan::build(
            &mut b,
            &ImageSpec::new(s, sw, sh),
            &ImageSpec::new(d, dw, dh),
            &ScaleOptions::default(),
        )
        .expect("plans")
    }

    #[test]
    fn identical_specs_are_a_copy() {
        assert!(plan(PixFmt::Yuv420p, 64, 64, PixFmt::Yuv420p, 64, 64).is_noop());
    }

    #[test]
    fn channel_permutation_needs_no_colour_stage() {
        let p = plan(PixFmt::Rgb24, 64, 64, PixFmt::Bgr24, 64, 64);
        let PlanKind::General(g) = &p.kind else {
            panic!("expected a general plan");
        };
        assert_eq!(g.colour, ColorStage::None);
        assert!(g.ch.iter().take(g.live).all(|c| c.up.is_identity()));
    }

    #[test]
    fn subsampling_change_is_one_bank_per_channel_and_no_matrix() {
        let p = plan(PixFmt::Yuv420p, 64, 64, PixFmt::Yuv444p, 64, 64);
        let PlanKind::General(g) = &p.kind else {
            panic!("expected a general plan");
        };
        assert_eq!(g.colour, ColorStage::None);
        assert!(g.ch[0].up.is_identity(), "luma is untouched");
        assert!(g.ch[1].up.v.is_some() && g.ch[1].up.h.is_some());
        assert!(
            g.ch[1].down.is_identity(),
            "no second pass without a matrix"
        );
    }

    #[test]
    fn a_matrix_forces_a_common_grid_and_a_second_pass() {
        let p = plan(PixFmt::Rgb24, 64, 64, PixFmt::Yuv420p, 64, 64);
        let PlanKind::General(g) = &p.kind else {
            panic!("expected a general plan");
        };
        assert!(matches!(
            g.colour,
            ColorStage::Affine(_) | ColorStage::Float(_)
        ));
        assert_eq!(g.ch[1].mid, (64, 64));
        assert_eq!(g.ch[1].dst, (32, 32));
        assert!(!g.ch[1].down.is_identity());
    }

    #[test]
    fn alpha_is_synthesised_when_the_source_has_none() {
        let p = plan(PixFmt::Yuv420p, 32, 32, PixFmt::Rgba, 32, 32);
        let PlanKind::General(g) = &p.kind else {
            panic!("expected a general plan");
        };
        assert_eq!(g.live, 4);
        assert_eq!(g.ch[3].synthetic, Synthetic::Constant(255));
    }

    #[test]
    fn depth_reduction_turns_dither_on_by_itself() {
        let p = plan(PixFmt::Yuv420p10le, 32, 32, PixFmt::Yuv420p, 32, 32);
        let PlanKind::General(g) = &p.kind else {
            panic!("expected a general plan");
        };
        assert_eq!(g.work_depth, 10);
        assert_eq!(g.dither, DitherKind::Bayer);
    }

    #[test]
    fn unsupported_formats_are_refused_at_plan_time() {
        let mut b = Budget::new(Limits::permissive());
        let r = Plan::build(
            &mut b,
            &ImageSpec::new(PixFmt::Pal8, 16, 16),
            &ImageSpec::new(PixFmt::Rgb24, 16, 16),
            &ScaleOptions::default(),
        );
        assert!(r.is_err());
    }
}
