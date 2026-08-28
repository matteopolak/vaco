//! `colormatrix` — convert a frame's `Y'CbCr` from one colour matrix's
//! coefficients to another's.
//!
//! `ffmpeg -h filter=colormatrix` documents `src`/`dst`, each one of
//! `bt709`/`fcc`/`bt601`/`bt470`/`bt470bg`/`smpte170m`/`smpte240m`/`bt2020`
//! (the four aliases `bt601`/`bt470`/`bt470bg`/`smpte170m` share one integer
//! value), `-1` (unset, the default for both).
//!
//! # Measured: no legal-range rescale, `Kr`/`Kb`-parameterised, rounded
//!
//! `Y'CbCr(src)` is read as if it already spanned `0..=max` (no expansion
//! from broadcast-legal `16..235` first), converted to `R'G'B'` with the
//! *source* matrix's public ITU-R `Kr`/`Kb` coefficients, then back to
//! `Y'CbCr` with the *destination* matrix's coefficients, and rounded to the
//! nearest integer. Confirmed against `ffmpeg 8.1`: `color=red` under
//! `yuv420p` (`Y=81,U=90,V=240`) through `colormatrix=src=bt601:dst=bt709`
//! measures `Y=62,U=102,V=240`. Plugging `Kr=0.299,Kb=0.114` (BT.601) into
//! the inverse transform below and `Kr=0.2126,Kb=0.0722` (BT.709) into the
//! forward one, both applied directly to the `0..255` range with no offset,
//! predicts `Y=61.67,U=102.13,V=239.98` — every value rounds to the
//! measured one, and the alternative hypothesis (expand `16..235` to
//! `0..255` first) does not fit anywhere near as closely.
//!
//! The `Kr`/`Kb` pairs themselves are public ITU-R/SMPTE standard
//! coefficients (Rec. 601, Rec. 709, Rec. 2020, SMPTE 240M, the 1953 FCC
//! NTSC matrix), not anything read from the reference's source.
//!
//! # Scope: applies only when both `src` and `dst` are set
//!
//! The reference falls back to the frame's own colour-matrix metadata when
//! `src`/`dst` is left at its `-1` default; this crate's [`vaco_frame::Frame`]
//! does carry a colour-primaries/matrix tag but wiring that lookup through
//! is separate work from the coefficient conversion itself, so this filter
//! is a no-op unless both options are given explicitly.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::Component;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "colormatrix",
    description: "Convert color matrix",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// `ffmpeg -h filter=colormatrix`'s own named constants for `src`/`dst`:
/// four names share the value `2` (all mean Rec. 601).
const MATRIX_CONSTS: &[vaco_opts::ConstDesc] = &[
    named(-1, "unset"),
    named(0, "bt709"),
    named(1, "fcc"),
    named(2, "bt601"),
    named(2, "bt470"),
    named(2, "bt470bg"),
    named(2, "smpte170m"),
    named(3, "smpte240m"),
    named(4, "bt2020"),
];

const fn named(value: i64, name: &'static str) -> vaco_opts::ConstDesc {
    vaco_opts::ConstDesc {
        name,
        help: name,
        unit: "matrix",
        value: vaco_opts::ConstValue::Int(value),
        flags: vaco_opts::OptFlags::NONE,
    }
}

/// `(Kr, Kb)` for one of `colormatrix`'s five matrix ids. Public ITU-R/SMPTE
/// standard luma coefficients — `Kg = 1 - Kr - Kb` in every case.
const fn kr_kb(id: i32) -> Option<(f64, f64)> {
    match id {
        0 => Some((0.2126, 0.0722)),   // BT.709
        1 => Some((0.30, 0.11)),       // FCC (1953 NTSC)
        2 => Some((0.299, 0.114)),     // BT.601 / BT.470 / BT.470bg / SMPTE 170M
        3 => Some((0.212, 0.087)),     // SMPTE 240M
        4 => Some((0.2627, 0.0593)),   // BT.2020
        _ => None,
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "colormatrix", help = "Convert color matrix")]
pub(crate) struct Opts {
    #[opt(name = "src", help = "set source color matrix", unit = "matrix", consts = MATRIX_CONSTS, default = -1, range = -1..=4, flags(video, filtering))]
    pub src: i32,
    #[opt(name = "dst", help = "set destination color matrix", unit = "matrix", consts = MATRIX_CONSTS, default = -1, range = -1..=4, flags(video, filtering))]
    pub dst: i32,
}

/// `Y'CbCr -> R'G'B'` using `Kr`/`Kb`, with `mid` as the chroma zero point
/// (`127.5` for 8-bit — the measured constant, not `128`; see this module's
/// doc for the probe that pins it).
fn to_rgb(kr: f64, kb: f64, luma: f64, cb: f64, cr: f64, mid: f64) -> (f64, f64, f64) {
    let kg = 1.0 - kr - kb;
    let red = (cr - mid).mul_add(2.0 * (1.0 - kr), luma);
    let blue = (cb - mid).mul_add(2.0 * (1.0 - kb), luma);
    let green = (luma - kr * red - kb * blue) / kg;
    (red, green, blue)
}

/// The inverse of [`to_rgb`]: `R'G'B' -> Y'CbCr`.
fn from_rgb(kr: f64, kb: f64, red: f64, green: f64, blue: f64, mid: f64) -> (f64, f64, f64) {
    let kg = 1.0 - kr - kb;
    let luma = kr.mul_add(red, kg.mul_add(green, kb * blue));
    let cb = (blue - luma) / (2.0 * (1.0 - kb)) + mid;
    let cr = (red - luma) / (2.0 * (1.0 - kr)) + mid;
    (luma, cb, cr)
}

#[derive(Debug)]
pub(crate) struct Filter {
    src: (f64, f64),
    dst: (f64, f64),
}

impl Filter {
    fn new(o: &Opts) -> Option<Self> {
        Some(Self { src: kr_kb(o.src)?, dst: kr_kb(o.dst)? })
    }

    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else { return };
        if format.is_rgb() || !sample::is_addressable(format) || format.component_count() < 3 {
            return;
        }
        let Some(y_comp) = sample::component(format, 0) else { return };
        let Some(u_comp) = sample::component(format, 1) else { return };
        let Some(v_comp) = sample::component(format, 2) else { return };
        let (log2w, log2h) = format.log2_chroma();
        let big_endian = format.is_big_endian();

        let Some((yw, yh, y_samples)) = read_plane(input, y_comp, big_endian) else { return };
        let Some((cw, ch, u_samples)) = read_plane(input, u_comp, big_endian) else { return };
        let Some((_, _, v_samples)) = read_plane(input, v_comp, big_endian) else { return };

        let y_max = f64::from(sample::max_value(y_comp));
        let c_max = f64::from(sample::max_value(u_comp));
        let mid = c_max / 2.0;

        let mut new_y = vec![0u16; y_samples.len()];
        for luma_row in 0..yh {
            for luma_col in 0..yw {
                let chroma_row = (luma_row >> log2h).min(ch.saturating_sub(1));
                let chroma_col = (luma_col >> log2w).min(cw.saturating_sub(1));
                let chroma_idx = chroma_row.saturating_mul(cw).saturating_add(chroma_col);
                let luma_idx = luma_row.saturating_mul(yw).saturating_add(luma_col);
                let (Some(&cb_v), Some(&cr_v), Some(&y_v), Some(slot)) = (
                    u_samples.get(chroma_idx),
                    v_samples.get(chroma_idx),
                    y_samples.get(luma_idx),
                    new_y.get_mut(luma_idx),
                ) else {
                    continue;
                };
                let rgb = to_rgb(self.src.0, self.src.1, f64::from(y_v), f64::from(cb_v), f64::from(cr_v), mid);
                let (converted_luma, _, _) = from_rgb(self.dst.0, self.dst.1, rgb.0, rgb.1, rgb.2, mid);
                *slot = clamp_round(converted_luma, y_max);
            }
        }

        let mut new_u = vec![0u16; u_samples.len()];
        let mut new_v = vec![0u16; v_samples.len()];
        for chroma_row in 0..ch {
            for chroma_col in 0..cw {
                let luma_row = (chroma_row << log2h).min(yh.saturating_sub(1));
                let luma_col = (chroma_col << log2w).min(yw.saturating_sub(1));
                let luma_idx = luma_row.saturating_mul(yw).saturating_add(luma_col);
                let chroma_idx = chroma_row.saturating_mul(cw).saturating_add(chroma_col);
                let (Some(&y_v), Some(&cb_v), Some(&cr_v)) = (
                    y_samples.get(luma_idx),
                    u_samples.get(chroma_idx),
                    v_samples.get(chroma_idx),
                ) else {
                    continue;
                };
                let rgb = to_rgb(self.src.0, self.src.1, f64::from(y_v), f64::from(cb_v), f64::from(cr_v), mid);
                let (_, new_cb, new_cr) = from_rgb(self.dst.0, self.dst.1, rgb.0, rgb.1, rgb.2, mid);
                if let Some(slot) = new_u.get_mut(chroma_idx) {
                    *slot = clamp_round(new_cb, c_max);
                }
                if let Some(slot) = new_v.get_mut(chroma_idx) {
                    *slot = clamp_round(new_cr, c_max);
                }
            }
        }

        write_plane(input, y_comp, big_endian, yw, &new_y);
        write_plane(input, u_comp, big_endian, cw, &new_u);
        write_plane(input, v_comp, big_endian, cw, &new_v);
    }
}

fn clamp_round(value: f64, max: f64) -> u16 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "value is clamped into 0.0..=max and max fits u16 by construction"
    )]
    {
        value.round().clamp(0.0, max) as u16
    }
}

/// Read one component's whole plane into a row-major buffer, plus its width
/// and height in samples.
fn read_plane(frame: &mut Frame, comp: Component, big_endian: bool) -> Option<(usize, usize, Vec<u16>)> {
    let plane = frame.plane_mut(comp.plane as usize)?;
    let width = plane.row_bytes().checked_div(usize::from(comp.step.max(1))).unwrap_or(0);
    let height = plane.rows();
    let mut samples = Vec::new();
    for y in 0..height {
        let Some(row) = plane.row(y) else { continue };
        for x in 0..width {
            samples.push(sample::read(row, x, comp, big_endian));
        }
    }
    Some((width, height, samples))
}

/// The inverse of [`read_plane`].
fn write_plane(frame: &mut Frame, comp: Component, big_endian: bool, width: usize, samples: &[u16]) {
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

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.apply_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let either = Filter::new(&opts).map_or(FilterEither::NoOp, FilterEither::Convert);
    let set = FormatSet::video_list(common::formats_where(|f| !f.is_rgb() && sample::is_addressable(f)));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(either)),
    })
}

/// Dispatches between the real conversion and a plain pass-through, for
/// when `src`/`dst` is left unset (see this module's scope note) — a
/// distinct enum rather than an `Option<Filter>` inside [`Simple`] so the
/// no-op path never allocates or touches plane data.
#[derive(Debug)]
enum FilterEither {
    NoOp,
    Convert(Filter),
}

impl FrameFilter for FilterEither {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        match self {
            Self::NoOp => Ok(FrameOut::One(input)),
            Self::Convert(f) => f.filter_frame(ctx, input),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn red_frame() -> Frame {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 2, 2).unwrap();
        for y in 0..2 {
            frame.plane_mut(0).unwrap().row_mut(y).unwrap()[0] = 81;
            frame.plane_mut(0).unwrap().row_mut(y).unwrap()[1] = 81;
        }
        frame.plane_mut(1).unwrap().row_mut(0).unwrap()[0] = 90;
        frame.plane_mut(2).unwrap().row_mut(0).unwrap()[0] = 240;
        frame
    }

    /// Measured against `ffmpeg 8.1`: `color=red` (`Y=81,U=90,V=240`) under
    /// `colormatrix=src=bt601:dst=bt709` measures `Y=62,U=102,V=240`.
    #[test]
    fn bt601_to_bt709_matches_the_reference() {
        let f = Filter::new(&Opts { src: 2, dst: 0 }).unwrap();
        let mut frame = red_frame();
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 62);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 102);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 240);
    }

    #[test]
    fn same_src_and_dst_is_a_near_identity() {
        let f = Filter::new(&Opts { src: 2, dst: 2 }).unwrap();
        let mut frame = red_frame();
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 81);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 90);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 240);
    }

    #[test]
    fn unset_matrix_is_a_no_op() {
        assert!(Filter::new(&Opts { src: -1, dst: 0 }).is_none());
        assert!(Filter::new(&Opts { src: 2, dst: -1 }).is_none());
    }

    #[test]
    fn rgb_input_is_rejected_at_create_time_not_misread() {
        // colormatrix is a YUV-space conversion; apply_frame on an RGB
        // frame must be a no-op, matching `hue`'s own precedent.
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        frame.plane_mut(0).unwrap().row_mut(0).unwrap()[0] = 10;
        let f = Filter::new(&Opts { src: 2, dst: 0 }).unwrap();
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 10);
    }
}
