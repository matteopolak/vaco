//! `hue` — rotate the chroma vector and scale saturation.
//!
//! `ffmpeg -h filter=hue`: one video pad in, one out. `h` (hue angle,
//! degrees, expression, default `"0"`), `H` (hue angle, radians, mutually
//! exclusive with `h`), `s` (saturation, `-10..10`, default `"1"`), `b`
//! (brightness, `-10..10`, default `"0"`).
//!
//! # Scope: `h`/`s` as constants, not per-frame expressions
//!
//! The reference accepts `h`/`H`/`s`/`b` as full `vaco-expr`-style
//! expressions (frame number, pts, time base) so they can vary over the
//! course of a stream (the man page's own worked examples are fades). This
//! module implements `h`/`s` as constant `f64` options, parsed once at
//! `create` time — the overwhelmingly common case (a single fixed rotation)
//! and the same simplification this crate already made for `pseudocolor`'s
//! per-channel expressions. Time-varying `h`/`s` are not evaluated; passing
//! an expression that is not a bare number falls back to the default via
//! [`crate::common::parse`]'s usual `Options::default()`-then-overlay
//! behaviour, the same failure mode every other option in this crate has
//! for a value it cannot parse.
//!
//! # Not implemented: `b` (brightness)
//!
//! Measured (`ffmpeg 8.1`, `color=red` under `yuv420p`, `Y=81` baseline):
//! `hue=b=1.0` measures `Y=106` (`+25`), `hue=b=-1.0` measures `Y=55`
//! (`-26`, not `-25`) and `hue=b=2.0` measures `Y=132` (`+51`, not `+50`).
//! The asymmetry around `b=0` (`+25` at `b=1` but `-26` at `b=-1`) rules out
//! a single linear `Y' = Y + k*b` term — the reference's brightness
//! adjustment interacts with `Y` in a way this pass did not pin down in the
//! time available (a gamma-like term is the leading hypothesis, not
//! confirmed). Left parsed but inert, the same shape `colorlevels`'
//! `preserve` and `colorchannelmixer`'s `pc`/`pa` already use in this crate
//! for a reference behaviour that does not decompose the way its name
//! suggests.
//!
//! # Measured: the chroma rotation formula
//!
//! ```text
//! u_dev = U - 128; v_dev = V - 128            (input chroma, centred)
//! rad = h_degrees * pi / 180
//! u_dev' = (u_dev*cos(rad) - v_dev*sin(rad)) * s
//! v_dev' = (u_dev*sin(rad) + v_dev*cos(rad)) * s
//! U' = round(128 + u_dev').clamp(0, 255)
//! V' = round(128 + v_dev').clamp(0, 255)
//! ```
//!
//! Confirmed step by step on `color=red` (`yuv420p`, `Y=81,U=90,V=240`,
//! `u_dev=-38,v_dev=112`):
//!
//! * `h=90,s=1`: predicted `u_dev'=-112,v_dev'=-38` &#8594; `U'=16,V'=90`,
//!   matching the measured output exactly — this pins the rotation
//!   direction and the `(u_dev, v_dev)` argument order (the opposite
//!   `(v_dev, u_dev)` order was checked and does not fit).
//! * `s=2.0,h=0`: predicted `u_dev'=-76,v_dev'=224.clamp=127` &#8594;
//!   `U'=52,V'=255` (`V'` clamps past 255), matching the measured output
//!   and confirming scaling is a hard clamp, not a modulo/wraparound.
//! * `h=45,s=1`: predicted `u_dev'=-106.066,v_dev'=52.33` &#8594;
//!   `U'=round(21.934)=22,V'=round(180.33)=180` — the `U'` value is what
//!   pins **round**, not floor/truncate (`floor(21.934)=21` would have
//!   matched a wrong hypothesis; the reference measures `22`).
//!
//! Distinguishing input: the flat-field, single-hue-angle probe above
//! cannot, on its own, separate "rotate by `h`" from "just scale U/V
//! independently by some `h`-derived constant" — a 45° rotation is what
//! forces a genuinely fractional, non-axis-aligned answer that only the
//! two-dimensional rotation formula reproduces.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "hue",
    description: "Adjust the hue and saturation of the input video.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "hue", help = "Adjust the hue and saturation of the input video")]
pub(crate) struct Opts {
    #[opt(name = "h", help = "set the hue angle degrees expression", default = 0.0, range = -360.0..=360.0, flags(video, filtering))]
    pub h: f64,
    #[opt(name = "s", help = "set the saturation expression", default = 1.0, range = -10.0..=10.0, flags(video, filtering))]
    pub s: f64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    /// `(cos(rad)*s, sin(rad)*s)`, precomputed once — the rotation and
    /// saturation scale never vary within one instance (see this module's
    /// doc for why `h`/`s` are not evaluated as time-varying expressions).
    cos_s: f64,
    sin_s: f64,
}

impl Filter {
    fn new(o: &Opts) -> Self {
        let rad = o.h.to_radians();
        Self {
            cos_s: rad.cos() * o.s,
            sin_s: rad.sin() * o.s,
        }
    }

    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if format.is_rgb() || !sample::is_addressable(format) {
            return;
        }
        let Some(u_comp) = sample::component(format, 1) else { return };
        let Some(v_comp) = sample::component(format, 2) else { return };
        if u_comp.plane == v_comp.plane {
            self.rotate_interleaved(input, u_comp, v_comp);
        } else {
            self.rotate_planar(input, u_comp, v_comp);
        }
    }

    /// `U`/`V` on separate planes (every planar YUV format this crate is
    /// measured against). Reads both planes into row-major buffers first
    /// (so `V` is rotated against `U`'s *original* value, not one already
    /// overwritten in place), computes the rotated pair, then writes both
    /// planes back — three passes rather than one, but it sidesteps
    /// needing two simultaneous mutable borrows of the same `Frame`'s
    /// plane list, which this crate's `Frame`/`PlaneMut` API does not
    /// expose a split-borrow accessor for.
    fn rotate_planar(&self, input: &mut Frame, u_comp: vaco_pixfmt::Component, v_comp: vaco_pixfmt::Component) {
        let big_endian = false;
        let u_max = f64::from(sample::max_value(u_comp));
        let v_max = f64::from(sample::max_value(v_comp));
        let mid_u = f64::midpoint(u_max, 1.0);
        let mid_v = f64::midpoint(v_max, 1.0);

        let Some((u_width, u_samples)) = read_plane(input, u_comp, big_endian) else { return };
        let Some((v_width, v_samples)) = read_plane(input, v_comp, big_endian) else { return };
        let width = u_width.min(v_width);
        let samples = u_samples.len().min(v_samples.len());

        let mut new_u: Vec<u16> = Vec::new();
        let mut new_v: Vec<u16> = Vec::new();
        for (&u, &v) in u_samples.iter().zip(v_samples.iter()).take(samples) {
            let u_dev = f64::from(u) - mid_u;
            let v_dev = f64::from(v) - mid_v;
            let du = u_dev.mul_add(self.cos_s, -(v_dev * self.sin_s));
            let dv = u_dev.mul_add(self.sin_s, v_dev * self.cos_s);
            new_u.push(clamp_round(mid_u + du, u_max));
            new_v.push(clamp_round(mid_v + dv, v_max));
        }

        write_plane(input, u_comp, big_endian, width, &new_u);
        write_plane(input, v_comp, big_endian, width, &new_v);
    }

    /// `U`/`V` sharing one plane (a packed/semi-planar YUV format). Not
    /// measured against the reference in this pass — this crate has no
    /// such format in [`sample::is_addressable`]'s addressable set today,
    /// so this path is defensive scaffolding rather than a confirmed
    /// behaviour, kept simple (one pass, no double-buffering) since it is
    /// unreachable from any format `create` currently accepts.
    fn rotate_interleaved(&self, input: &mut Frame, u_comp: vaco_pixfmt::Component, v_comp: vaco_pixfmt::Component) {
        let big_endian = false;
        let u_max = f64::from(sample::max_value(u_comp));
        let v_max = f64::from(sample::max_value(v_comp));
        let mid_u = f64::midpoint(u_max, 1.0);
        let mid_v = f64::midpoint(v_max, 1.0);
        let Some(mut plane) = input.plane_mut(u_comp.plane as usize) else { return };
        let width = plane
            .row_bytes()
            .checked_div(usize::from(u_comp.step.max(v_comp.step).max(1)))
            .unwrap_or(0);
        for y in 0..plane.rows() {
            let Some(row) = plane.row_mut(y) else { continue };
            for x in 0..width {
                let u = sample::read(row, x, u_comp, big_endian);
                let v = sample::read(row, x, v_comp, big_endian);
                let u_dev = f64::from(u) - mid_u;
                let v_dev = f64::from(v) - mid_v;
                let du = u_dev.mul_add(self.cos_s, -(v_dev * self.sin_s));
                let dv = u_dev.mul_add(self.sin_s, v_dev * self.cos_s);
                let new_u = clamp_round(mid_u + du, u_max);
                let new_v = clamp_round(mid_v + dv, v_max);
                sample::write(row, x, u_comp, big_endian, new_u);
                sample::write(row, x, v_comp, big_endian, new_v);
            }
        }
    }
}

/// Read one component from every row of its plane into one row-major
/// `Vec<u16>`, plus the row width — `None` if the plane does not exist.
/// Ragged rows (should not happen for any format this crate addresses) are
/// handled by recording each row's own length and having the caller `zip`
/// against the other component's buffer, which naturally stops at the
/// shorter side rather than panicking.
fn read_plane(frame: &mut Frame, comp: vaco_pixfmt::Component, big_endian: bool) -> Option<(usize, Vec<u16>)> {
    let plane = frame.plane_mut(comp.plane as usize)?;
    let width = plane.row_bytes().checked_div(usize::from(comp.step.max(1))).unwrap_or(0);
    let mut samples = Vec::new();
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else { continue };
        for x in 0..width {
            samples.push(sample::read(row, x, comp, big_endian));
        }
    }
    Some((width, samples))
}

/// The inverse of [`read_plane`]: write `samples` (row-major, `width`
/// columns per row) back into `comp`'s plane.
fn write_plane(frame: &mut Frame, comp: vaco_pixfmt::Component, big_endian: bool, width: usize, samples: &[u16]) {
    if width == 0 {
        return;
    }
    let Some(mut plane) = frame.plane_mut(comp.plane as usize) else { return };
    let mut rest = samples;
    for y in 0..plane.rows() {
        if rest.len() < width {
            break;
        }
        let (this_row, remaining) = rest.split_at(width);
        rest = remaining;
        if let Some(row) = plane.row_mut(y) {
            for (x, &value) in this_row.iter().enumerate() {
                sample::write(row, x, comp, big_endian, value);
            }
        }
    }
}

/// Round to nearest (measured, not truncated — see this module's doc for
/// the 45° probe that pins this down), then clamp into `0..=max`.
fn clamp_round(value: f64, max: f64) -> u16 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "value is clamped into 0.0..=max, and max fits u16 for every format this crate addresses"
    )]
    {
        value.round().clamp(0.0, max) as u16
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.apply_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let set = FormatSet::video_list(common::formats_where(|f| !f.is_rgb() && sample::is_addressable(f)));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter::new(&opts))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn yuv_frame(u: u8, v: u8) -> Frame {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 2, 2).unwrap();
        frame.plane_mut(1).unwrap().row_mut(0).unwrap()[0] = u;
        frame.plane_mut(2).unwrap().row_mut(0).unwrap()[0] = v;
        frame
    }

    /// Independent oracle: `h=0,s=1` (the identity rotation/scale) must not
    /// change chroma at all.
    #[test]
    fn identity_rotation_is_a_no_op() {
        let f = Filter::new(&Opts { h: 0.0, s: 1.0 });
        let mut frame = yuv_frame(90, 240);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 90);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 240);
    }

    /// Measured against `ffmpeg 8.1`: `color=red` (`U=90,V=240`) under
    /// `hue=h=90` measures `U=16,V=90`. This pins both the rotation
    /// direction and the `(u_dev, v_dev)` argument order — the opposite
    /// order does not reproduce this pair.
    #[test]
    fn ninety_degree_rotation_matches_the_reference() {
        let f = Filter::new(&Opts { h: 90.0, s: 1.0 });
        let mut frame = yuv_frame(90, 240);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 16);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 90);
    }

    /// Measured: `s=2.0` on `U=90,V=240` scales the chroma deviation by 2
    /// and **clamps** rather than wraps — `V`'s deviation of `+112` doubles
    /// to `+224`, which would overflow past 255, and the reference clamps
    /// to `255`, not `255 - 224 = 31` or similar wraparound.
    #[test]
    fn saturation_scale_clamps_rather_than_wraps() {
        let f = Filter::new(&Opts { h: 0.0, s: 2.0 });
        let mut frame = yuv_frame(90, 240);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 52);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 255);
    }

    /// Distinguishing input: a 45-degree rotation forces a genuinely
    /// fractional intermediate result, which is what pins **round** rather
    /// than floor/truncate — measured against `ffmpeg 8.1`: `U=22` (not the
    /// `floor`-consistent `21`).
    #[test]
    fn forty_five_degree_rotation_rounds_rather_than_truncates() {
        let f = Filter::new(&Opts { h: 45.0, s: 1.0 });
        let mut frame = yuv_frame(90, 240);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 22);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 180);
    }

    /// `hue` operates on YUV, not RGB — an `rgb24` frame must pass through
    /// unchanged rather than being misread as if its planes were chroma.
    #[test]
    fn rgb_input_is_left_untouched() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 10;
            row[1] = 20;
            row[2] = 30;
        }
        let f = Filter::new(&Opts { h: 90.0, s: 2.0 });
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[10, 20, 30]);
    }
}
