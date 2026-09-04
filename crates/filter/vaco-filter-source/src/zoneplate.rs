//! `zoneplate` — the classic zone-plate test signal: a 2D (plus time) chirp
//! used to probe a system's frequency response and aliasing behaviour.
//!
//! `ffmpeg -h filter=zoneplate` documents `size`/`s`, `rate`/`r`,
//! `duration`/`d`, `sar`, `precision` (LUT bits, 4..16, default 10), and the
//! phase coefficients `xo`, `yo`, `to`, `k0`, `kx`, `ky`, `kt`, `kxt`, `kyt`,
//! `kxy`, `kx2`, `ky2`, `kt2`, `ku`, `kv`.
//!
//! # What is measured, and what is not
//!
//! The zone-plate signal itself is a published test-pattern concept (BBC
//! Research & Development's zone-plate test cards; see e.g. Sandbank &
//! Stott's work on the format): intensity is a sinusoid of a phase that is a
//! quadratic-plus-linear polynomial in `x`, `y` and `t`. With **every**
//! `k*`/`xo`/`yo`/`to` at its documented default of `0`, `ffmpeg -f lavfi -i
//! zoneplate=size=WxH -f rawvideo -pix_fmt yuv444p -frames:v 1 -` prints a
//! flat `Y = 128, U = 128, V = 128` frame at every size probed — `sin(0) =
//! 0`, mapped to the mid-grey point of an 8-bit plane. That default case is
//! reproduced here **exactly**.
//!
//! For non-zero coefficients, probing `kx2=50` on a 32×16 frame shows a
//! symmetric chirp centred on the frame (confirming the polynomial is taken
//! relative to the frame centre, i.e. `(x - w/2)`, not `x` itself) whose
//! period shortens away from the centre, as a chirp must. Fitting the
//! phase-per-`kx2`-per-`dx²` constant from that probe did **not** converge to
//! a clean value across several `dx` — `dx=1` implies a different constant
//! than `dx=2..4` by a factor of ~1.5, which is consistent with the
//! reference building its sine values from a `precision`-bit fixed-point LUT
//! rather than a direct `sin()` call, and quantisation noise dominating at
//! the smallest phases. Reverse-engineering that fixed-point construction
//! bit-for-bit from black-box samples was out of reach in the time
//! available.
//!
//! So: this module implements the textbook floating-point formula below,
//! calibrated so that `k* = 1` advances the phase by exactly one full turn
//! (`2π`) per unit of the relevant coordinate product — a normalisation this
//! crate chose, not one measured from the reference. **The all-zero default
//! is exact. Non-zero `k*` coefficients are algorithmically faithful to the
//! zone-plate concept but not calibrated to the reference's numeric scale or
//! its `precision`-bit LUT quantisation** — the same honesty `gblur`'s
//! `docs/filter/vaco-filter-blur.md` gives its IIR-vs-FIR Gaussian gap.  See
//! `docs/filter/vaco-filter-source.md`.
//!
//! ```text
//! dx = x - xo - w/2,  dy = y - yo - h/2,  dt = t - to
//! phase = 2*pi * (k0 + kx*dx + ky*dy + kt*dt
//!                 + kxt*dx*dt + kyt*dy*dt + kxy*dx*dy
//!                 + kx2*dx*dx + ky2*dy*dy + kt2*dt*dt)
//! Y = 128 + 127 * sin(phase)
//! U = 128 + 127 * sin(phase + 2*pi*ku)
//! V = 128 + 127 * sin(phase + 2*pi*kv)
//! ```

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration::from_micros(-1);

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "zoneplate", help = "generate zone-plate")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
    #[opt(name = "precision", help = "set LUT precision", default = 10, range = 4..=16, flags(filtering))]
    pub precision: i32,
    #[opt(name = "xo", help = "set X-axis offset", default = 0, flags(filtering))]
    pub xo: i32,
    #[opt(name = "yo", help = "set Y-axis offset", default = 0, flags(filtering))]
    pub yo: i32,
    #[opt(name = "to", help = "set T-axis offset", default = 0, flags(filtering))]
    pub to: i32,
    #[opt(name = "k0", help = "set 0-order phase", default = 0, flags(filtering))]
    pub k0: i32,
    #[opt(
        name = "kx",
        help = "set 1-order X-axis phase",
        default = 0,
        flags(filtering)
    )]
    pub kx: i32,
    #[opt(
        name = "ky",
        help = "set 1-order Y-axis phase",
        default = 0,
        flags(filtering)
    )]
    pub ky: i32,
    #[opt(
        name = "kt",
        help = "set 1-order T-axis phase",
        default = 0,
        flags(filtering)
    )]
    pub kt: i32,
    #[opt(
        name = "kxt",
        help = "set X-axis*T-axis product phase",
        default = 0,
        flags(filtering)
    )]
    pub kxt: i32,
    #[opt(
        name = "kyt",
        help = "set Y-axis*T-axis product phase",
        default = 0,
        flags(filtering)
    )]
    pub kyt: i32,
    #[opt(
        name = "kxy",
        help = "set X-axis*Y-axis product phase",
        default = 0,
        flags(filtering)
    )]
    pub kxy: i32,
    #[opt(
        name = "kx2",
        help = "set 2-order X-axis phase",
        default = 0,
        flags(filtering)
    )]
    pub kx2: i32,
    #[opt(
        name = "ky2",
        help = "set 2-order Y-axis phase",
        default = 0,
        flags(filtering)
    )]
    pub ky2: i32,
    #[opt(
        name = "kt2",
        help = "set 2-order T-axis phase",
        default = 0,
        flags(filtering)
    )]
    pub kt2: i32,
    #[opt(
        name = "ku",
        help = "set 0-order U-color phase",
        default = 0,
        flags(filtering)
    )]
    pub ku: i32,
    #[opt(
        name = "kv",
        help = "set 0-order V-color phase",
        default = 0,
        flags(filtering)
    )]
    pub kv: i32,
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

pub const DESC: FilterDesc = FilterDesc {
    name: "zoneplate",
    description: "Generate zone-plate",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Coeffs {
    pub xo: f64,
    pub yo: f64,
    pub k0: f64,
    pub kx: f64,
    pub ky: f64,
    pub kt: f64,
    pub kxt: f64,
    pub kyt: f64,
    pub kxy: f64,
    pub kx2: f64,
    pub ky2: f64,
    pub kt2: f64,
    pub ku: f64,
    pub kv: f64,
    pub to: f64,
}

const TAU: f64 = std::f64::consts::TAU;

/// `(Y, U, V)` for one pixel, per the module doc's formula. `t` is the
/// (fractional) frame index.
pub(crate) fn yuv_at(x: u32, y: u32, w: u32, h: u32, t: f64, c: &Coeffs) -> (u8, u8, u8) {
    let dx = f64::from(x) - c.xo - f64::from(w) / 2.0;
    let dy = f64::from(y) - c.yo - f64::from(h) / 2.0;
    let dt = t - c.to;
    let turns = c.k0
        + c.kx * dx
        + c.ky * dy
        + c.kt * dt
        + c.kxt * dx * dt
        + c.kyt * dy * dt
        + c.kxy * dx * dy
        + c.kx2 * dx * dx
        + c.ky2 * dy * dy
        + c.kt2 * dt * dt;
    let phase = TAU * turns;
    let quantize = |p: f64| -> u8 {
        let v = 128.0 + 127.0 * p.sin();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "v is in [1, 255] by construction"
        )]
        {
            v.round().clamp(0.0, 255.0) as u8
        }
    };
    (
        quantize(phase),
        quantize(phase + TAU * c.ku),
        quantize(phase + TAU * c.kv),
    )
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    coeffs: Coeffs,
    frame_rate: Rational,
    sar: Rational,
    total_frames: Option<u64>,
    next: i64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width,
                height,
                time_base,
                frame_rate,
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
                *sample_aspect_ratio = self.sar;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_frames.is_some_and(|n| self.next as u64 >= n) {
            return Ok(None);
        }
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Yuv444p, self.width, self.height)?;
        let (w, h) = (self.width, self.height);
        #[allow(
            clippy::cast_precision_loss,
            reason = "frame index precision loss is irrelevant at any duration this runs for"
        )]
        let t = self.next as f64;
        for plane_idx in 0..3usize {
            if let Some(mut plane) = frame.plane_mut(plane_idx) {
                for row_idx in 0..plane.rows() {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "plane.rows() == h, which fits in u32"
                    )]
                    let yy = row_idx as u32;
                    if let Some(row) = plane.row_mut(row_idx) {
                        for (x, px) in row.iter_mut().enumerate() {
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "x < w, which fits in u32"
                            )]
                            let xx = x as u32;
                            let (yv, uv, vv) = yuv_at(xx, yy, w, h, t, &self.coeffs);
                            *px = match plane_idx {
                                0 => yv,
                                1 => uv,
                                _ => vv,
                            };
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.set_duration_ticks(1);
        frame.sample_aspect_ratio = self.sar;
        self.next = self.next.saturating_add(1);
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(self.next)
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    let rate = opts.rate.0;
    let total_frames = if opts.duration.as_micros() < 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * rate.to_f64())
                .round()
                .max(0.0) as u64,
        )
    };
    // `precision` (the LUT-bit-depth option) has no effect here: this crate
    // computes `sin` directly rather than through a fixed-point LUT. See the
    // module doc.
    let _ = opts.precision;
    let coeffs = Coeffs {
        xo: f64::from(opts.xo),
        yo: f64::from(opts.yo),
        k0: f64::from(opts.k0),
        kx: f64::from(opts.kx),
        ky: f64::from(opts.ky),
        kt: f64::from(opts.kt),
        kxt: f64::from(opts.kxt),
        kyt: f64::from(opts.kyt),
        kxy: f64::from(opts.kxy),
        kx2: f64::from(opts.kx2),
        ky2: f64::from(opts.ky2),
        kt2: f64::from(opts.kt2),
        ku: f64::from(opts.ku),
        kv: f64::from(opts.kv),
        to: f64::from(opts.to),
    };
    let source = Source {
        width,
        height,
        coeffs,
        frame_rate: rate,
        sar: opts.sar,
        total_frames,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Yuv444p)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: Coeffs = Coeffs {
        xo: 0.0,
        yo: 0.0,
        k0: 0.0,
        kx: 0.0,
        ky: 0.0,
        kt: 0.0,
        kxt: 0.0,
        kyt: 0.0,
        kxy: 0.0,
        kx2: 0.0,
        ky2: 0.0,
        kt2: 0.0,
        ku: 0.0,
        kv: 0.0,
        to: 0.0,
    };

    #[test]
    fn all_zero_coefficients_is_flat_mid_grey_matching_the_reference() {
        for x in 0..16 {
            for y in 0..16 {
                assert_eq!(yuv_at(x, y, 32, 32, 0.0, &ZERO), (128, 128, 128));
            }
        }
    }

    #[test]
    fn the_chirp_is_symmetric_about_the_frame_centre() {
        let mut c = ZERO;
        c.kx2 = 0.001;
        let (w, h) = (32, 16);
        for dx in 1..16i64 {
            let left = yuv_at((16 - dx) as u32, 0, w, h, 0.0, &c);
            let right = yuv_at((16 + dx) as u32, 0, w, h, 0.0, &c);
            assert_eq!(left, right, "dx={dx}");
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "zoneplate",
            instance: "zoneplate",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
