//! `haldclut` — apply a 3D lookup table decoded from a Hald CLUT image
//! carried as a second video input.
//!
//! `ffmpeg -h filter=haldclut` documents `clut` (first/all, default all)
//! and `interp` (default tetrahedral), plus the shared
//! `vaco-filter-framesync` surface.
//!
//! # The Hald layout: measured against `haldclutsrc` (a publicly specified
//! technique, not reference-internal behaviour)
//!
//! `haldclutsrc=level=8` produces a `512x512` `rgb24` image
//! (`512 = 8^3`), and its pixel content matches the well-known Hald CLUT
//! convention exactly: flatten a 3D LUT of size `N = level^2` with index
//! `r + g*N + b*N^2` (red fastest) into a square raster of side `level^3`,
//! row-major. Confirmed on the identity image: row 0's red channel steps
//! `0, 4, 8, 12, ...` (`round(i*255/63)` for `N=64`), and green jumps from
//! `0` to `32` (`round(8*255/63)`) exactly at the start of row 1 — i.e.
//! green increments once every `N=64` pixels, exactly as the formula
//! predicts. [`decode_hald`] implements that.
//!
//! # `interp`, `clut`
//!
//! Same as [`crate::lut3d`] — nearest and trilinear only — and the same
//! fix applies: `interp=tetrahedral` (this crate's own declared default,
//! matching the reference's), `pyramid` and `prism` used to silently run
//! trilinear on every unconfigured call; `create` now rejects them by
//! name instead (see `lut3d.rs`'s doc for the measured evidence this is a
//! real, large divergence, not a rounding difference). Concretely: a bare
//! `haldclut` now errors where it used to silently run trilinear — pass
//! `interp=trilinear` (or `nearest`) explicitly to get a working filter
//! today. `clut=first` (process the CLUT only once) has the same shape:
//! parsed fine, but this filter always re-decodes the second input's
//! current frame every event with no caching and no error, i.e. silently
//! always behaves like `clut=all`. `create` now rejects `clut=first` too,
//! rather than accepting a request it cannot honour — implementing the
//! actual caching (state that survives across `on_event` calls,
//! invalidated on... what, exactly, is itself an open question the
//! reference's own docs don't answer) is real work this pass does not
//! attempt.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::lut3d::Cube3d;
use crate::sample;

const PADS: &[Pad] = &[
    Pad {
        name: "main",
        media_type: MediaType::Video,
    },
    Pad {
        name: "clut",
        media_type: MediaType::Video,
    },
];
const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "haldclut",
    description: "Adjust colors using a Hald CLUT",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "haldclut", help = "Adjust colors using a Hald CLUT")]
pub(crate) struct Opts {
    #[opt(name = "clut", help = "when to process CLUT (only `all` is implemented)", unit = "clut_mode", consts = crate::common::CLUT_CONSTS, default = 1, range = 0..=1, flags(video, filtering))]
    pub clut: i32,
    #[opt(name = "interp", help = "select interpolation mode", unit = "lut3d_interp", consts = crate::common::LUT3D_INTERP_CONSTS, default = 2, range = 0..=4, flags(video, filtering))]
    pub interp: i32,
    #[opt(name = "eof_action", help = "action to take when encountering EOF from secondary input", default = "repeat".to_owned(), flags(video, filtering))]
    pub eof_action: String,
    #[opt(
        name = "shortest",
        help = "force termination when the shortest input terminates",
        default = false,
        flags(video, filtering)
    )]
    pub shortest: bool,
    #[opt(
        name = "repeatlast",
        help = "extend last frame of secondary streams beyond EOF",
        default = true,
        flags(video, filtering)
    )]
    pub repeatlast: bool,
    #[opt(name = "ts_sync_mode", help = "how strictly to sync streams based on secondary input timestamps", default = "default".to_owned(), flags(video, filtering))]
    pub ts_sync_mode: String,
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

/// Decode a Hald CLUT image into a [`Cube3d`], per this module's doc.
///
/// # Errors
/// A clean error if `frame` is not RGB, not addressable, not square, or
/// its side is not a perfect cube (`level^3`).
pub(crate) fn decode_hald(frame: &Frame) -> std::result::Result<Cube3d, String> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = frame.data
    else {
        return Err("haldclut: CLUT input is not a video frame".to_owned());
    };
    if !format.is_rgb() || !sample::is_addressable(format) {
        return Err("haldclut: CLUT input must be an RGB format".to_owned());
    }
    if width != height {
        return Err("haldclut: CLUT image must be square".to_owned());
    }
    let level = (f64::from(width)).cbrt().round() as u32;
    if level == 0 || level.saturating_pow(3) != width {
        return Err(format!("haldclut: {width} is not a perfect cube side"));
    }
    let n = (level as usize).saturating_mul(level as usize);
    let (Some(cr), Some(cg), Some(cb)) = (
        sample::component(format, 0),
        sample::component(format, 1),
        sample::component(format, 2),
    ) else {
        return Err("haldclut: CLUT format has no RGB components".to_owned());
    };
    let (Some(pr), Some(pg), Some(pb)) = (
        frame.plane(cr.plane as usize),
        frame.plane(cg.plane as usize),
        frame.plane(cb.plane as usize),
    ) else {
        return Err("haldclut: CLUT frame has no planes".to_owned());
    };
    let big_endian = format.is_big_endian();
    let (max_r, max_g, max_b) = (
        f64::from(sample::max_value(cr)),
        f64::from(sample::max_value(cg)),
        f64::from(sample::max_value(cb)),
    );
    let total = n.saturating_mul(n).saturating_mul(n);
    let mut data = vec![[0.0f64; 3]; total];
    let w = width as usize;
    for idx in 0..total {
        let x = idx % w;
        let y = idx.checked_div(w).unwrap_or(0);
        let (Some(rr), Some(rg), Some(rb)) = (pr.row(y), pg.row(y), pb.row(y)) else {
            continue;
        };
        let v_r = f64::from(sample::read(rr, x, cr, big_endian)) / max_r;
        let v_g = f64::from(sample::read(rg, x, cg, big_endian)) / max_g;
        let v_b = f64::from(sample::read(rb, x, cb, big_endian)) / max_b;
        if let Some(slot) = data.get_mut(idx) {
            *slot = [v_r, v_g, v_b];
        }
    }
    Ok(Cube3d::from_samples(n, data))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interp {
    Nearest,
    Trilinear,
}

#[derive(Debug)]
pub(crate) struct HaldClut {
    interp: Interp,
    fs_opts: FrameSyncOpts,
}

impl FrameSyncFilter for HaldClut {
    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let (Some(main), Some(clut_frame)) = (event.take(0), event.get(1)) else {
            return Ok(FrameOut::None);
        };
        let Ok(cube) = decode_hald(clut_frame) else {
            return Ok(FrameOut::One(main));
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = main.data
        else {
            return Ok(FrameOut::One(main));
        };
        if !format.is_rgb() || !sample::is_addressable(format) {
            return Ok(FrameOut::One(main));
        }
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        let (Some(cr), Some(cg), Some(cb)) = (
            sample::component(format, 0),
            sample::component(format, 1),
            sample::component(format, 2),
        ) else {
            return Ok(FrameOut::One(main));
        };
        let (max_r, max_g, max_b) = (
            f64::from(sample::max_value(cr)),
            f64::from(sample::max_value(cg)),
            f64::from(sample::max_value(cb)),
        );
        let (Some(pr), Some(pg), Some(pb)) = (
            main.plane(cr.plane as usize),
            main.plane(cg.plane as usize),
            main.plane(cb.plane as usize),
        ) else {
            return Ok(FrameOut::One(main));
        };
        // Same reasoning as `lut3d`: a packed format shares one plane
        // across R/G/B, so all output planes are borrowed disjointly at
        // once via `planes_mut` rather than three separate `plane_mut`
        // calls.
        let mut out_planes = out.planes_mut();
        let w = pr
            .row_bytes()
            .checked_div(usize::from(cr.step.max(1)))
            .unwrap_or(0);
        for y in 0..pr.rows() {
            let (Some(rr), Some(rg), Some(rb)) = (pr.row(y), pg.row(y), pb.row(y)) else {
                continue;
            };
            for x in 0..w {
                let r = f64::from(sample::read(rr, x, cr, big_endian)) / max_r;
                let g = f64::from(sample::read(rg, x, cg, big_endian)) / max_g;
                let b = f64::from(sample::read(rb, x, cb, big_endian)) / max_b;
                let out_v = match self.interp {
                    Interp::Nearest => cube.sample_nearest(r, g, b),
                    Interp::Trilinear => cube.sample_trilinear(r, g, b),
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to [0, 1] before scaling, so the product is in [0, max] and max fits in u16 by construction; truncation (not rounding) is the measured reference behaviour, see vaco_filter_lut::lut3d's module doc"
                )]
                let to_u16 = |v: f64, max: f64| v.clamp(0.0, 1.0).mul_add(max, 0.0) as u16;
                if let Some(row) = out_planes
                    .get_mut(cr.plane as usize)
                    .and_then(|p| p.row_mut(y))
                {
                    sample::write(row, x, cr, big_endian, to_u16(out_v[0], max_r));
                }
                if let Some(row) = out_planes
                    .get_mut(cg.plane as usize)
                    .and_then(|p| p.row_mut(y))
                {
                    sample::write(row, x, cg, big_endian, to_u16(out_v[1], max_g));
                }
                if let Some(row) = out_planes
                    .get_mut(cb.plane as usize)
                    .and_then(|p| p.row_mut(y))
                {
                    sample::write(row, x, cb, big_endian, to_u16(out_v[2], max_b));
                }
            }
        }
        drop(out_planes);
        out.pts = main.pts;
        out.time_base = main.time_base;
        out.duration = main.duration;
        out.sample_aspect_ratio = main.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }

    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        self.fs_opts
    }
}

/// # Errors
/// A named error for `tetrahedral`/`pyramid`/`prism` (`2..=4`) — see the
/// module doc for why these are rejected rather than silently run as
/// `trilinear`.
fn interp_from_opt(v: i32) -> std::result::Result<Interp, String> {
    match v {
        0 => Ok(Interp::Nearest),
        1 => Ok(Interp::Trilinear),
        2 => Err(
            "haldclut: interp=tetrahedral is not implemented (this is the reference's own \
             default; pass interp=trilinear or interp=nearest explicitly — see this module's \
             doc)"
                .to_owned(),
        ),
        3 => Err("haldclut: interp=pyramid is not implemented — see this module's doc".to_owned()),
        4 => Err("haldclut: interp=prism is not implemented — see this module's doc".to_owned()),
        other => Err(format!("haldclut: interp={other} is out of range (0..=4)")),
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let interp = interp_from_opt(opts.interp)?;
    if opts.clut == 0 {
        return Err(
            "haldclut: clut=first is not implemented (this filter always re-decodes the CLUT \
             input every event, i.e. always behaves like clut=all; pass clut=all explicitly \
             — see this module's doc)"
                .to_owned(),
        );
    }
    let eof_action = vaco_filter_framesync::EofAction::from_name(&opts.eof_action)
        .ok_or_else(|| format!("haldclut: bad `eof_action` `{}`", opts.eof_action))?;
    let ts_sync = vaco_filter_framesync::TsSyncMode::from_name(&opts.ts_sync_mode)
        .ok_or_else(|| format!("haldclut: bad `ts_sync_mode` `{}`", opts.ts_sync_mode))?;
    let filter = HaldClut {
        interp,
        fs_opts: FrameSyncOpts {
            eof_action,
            shortest: opts.shortest,
            repeatlast: opts.repeatlast,
            ts_sync,
        },
    };
    let rgb_set = FormatSet::video_list(common::formats_where(|f| {
        f.is_rgb() && sample::is_addressable(f)
    }));
    let formats = NodeFormats {
        inputs: vec![rgb_set.clone(), rgb_set.clone()],
        outputs: vec![rgb_set],
        ties: Tie::all_pads(1, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Synced::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    /// A level-2 identity Hald CLUT: `N = 4`, image side `= 2^3 = 8`.
    fn identity_hald(level: u32) -> Frame {
        let count = level * level;
        let side = level.saturating_pow(3);
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, side, side).unwrap();
        let comp = sample::component(PixFmt::Rgb24, 0).unwrap();
        let cg = sample::component(PixFmt::Rgb24, 1).unwrap();
        let cb = sample::component(PixFmt::Rgb24, 2).unwrap();
        let total = (count * count * count) as usize;
        let row_width = side as usize;
        let scale = |i: u32| -> u16 {
            if count <= 1 {
                0
            } else {
                ((f64::from(i) * 255.0) / f64::from(count - 1)).round() as u16
            }
        };
        for idx in 0..total {
            let red_idx = (idx as u32) % count;
            let green_idx = (idx as u32).checked_div(count).unwrap_or(0) % count;
            let blue_idx = (idx as u32)
                .checked_div(count.saturating_mul(count))
                .unwrap_or(0);
            let px = idx % row_width;
            let py = idx.checked_div(row_width).unwrap_or(0);
            let mut plane = frame.plane_mut(0).unwrap();
            if let Some(row) = plane.row_mut(py) {
                sample::write(row, px, comp, false, scale(red_idx));
                sample::write(row, px, cg, false, scale(green_idx));
                sample::write(row, px, cb, false, scale(blue_idx));
            }
        }
        frame
    }

    #[test]
    fn decodes_the_documented_level_and_size() {
        let hald = identity_hald(2);
        let cube = decode_hald(&hald).unwrap();
        assert_eq!(cube.size, 4); // N = level^2 = 4
    }

    #[test]
    fn identity_hald_is_a_no_op_under_trilinear() {
        // Independent oracle: an identity Hald CLUT, once decoded, must be
        // the identity 3D LUT — checked via the same `Cube3d` machinery
        // `lut3d`'s own identity test uses, on data decoded from pixels
        // rather than hand-written.
        let hald = identity_hald(4);
        let cube = decode_hald(&hald).unwrap();
        for &(r, g, b) in &[(0.0, 0.0, 0.0), (1.0, 1.0, 1.0), (0.4, 0.6, 0.2)] {
            let out = cube.sample_trilinear(r, g, b);
            assert!((out[0] - r).abs() < 0.02, "r {out:?} vs {r}");
            assert!((out[1] - g).abs() < 0.02, "g {out:?} vs {g}");
            assert!((out[2] - b).abs() < 0.02, "b {out:?} vs {b}");
        }
    }

    #[test]
    fn non_cube_side_is_a_clean_error() {
        let mut budget = Budget::new(Limits::strict());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 100, 100).unwrap();
        assert!(decode_hald(&frame).is_err());
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=haldclut`): every named `clut`/`interp`
    /// constant must parse, not just the bare integer.
    #[test]
    fn named_option_values_parse() {
        for (name, expected) in [("first", 0), ("all", 1)] {
            let opts = Opts::parse(Some(&format!("clut={name}"))).unwrap();
            assert_eq!(opts.clut, expected, "clut={name}");
        }
        for (name, expected) in [
            ("nearest", 0),
            ("trilinear", 1),
            ("tetrahedral", 2),
            ("pyramid", 3),
            ("prism", 4),
        ] {
            let opts = Opts::parse(Some(&format!("interp={name}"))).unwrap();
            assert_eq!(opts.interp, expected, "interp={name}");
        }
    }

    /// Same shape as `lut3d`'s equivalent test: `tetrahedral` (the
    /// reference's own default), `pyramid` and `prism` used to silently
    /// run `trilinear`. `interp_from_opt` now rejects each by name.
    #[test]
    fn unimplemented_interp_values_are_a_named_error_not_a_silent_substitution() {
        for v in [2, 3, 4] {
            let err = interp_from_opt(v).unwrap_err();
            assert!(
                err.contains("haldclut") && err.contains("not implemented"),
                "interp={v}: unexpected error text: {err}"
            );
        }
    }

    #[test]
    fn implemented_interp_values_still_create() {
        assert_eq!(interp_from_opt(0), Ok(Interp::Nearest));
        assert_eq!(interp_from_opt(1), Ok(Interp::Trilinear));
    }

    /// `clut=first` used to parse fine and silently behave like `clut=all`
    /// (no caching implemented). `create` now rejects it explicitly.
    /// `Opts::parse` reads `req.args` (the raw string) directly, so a
    /// bare `args: Some(...)` -- no `arguments` slice needed -- exercises
    /// the real `create` path.
    #[test]
    fn clut_first_is_a_named_error_not_a_silent_substitution() {
        let req = Instantiate {
            name: "haldclut",
            instance: "haldclut",
            args: Some("clut=first:interp=trilinear"),
            arguments: &[],
        };
        let err = create(&req).unwrap_err();
        assert!(
            err.contains("haldclut")
                && err.contains("clut=first")
                && err.contains("not implemented"),
            "unexpected error text: {err}"
        );
    }

    #[test]
    fn clut_all_still_creates() {
        let req = Instantiate {
            name: "haldclut",
            instance: "haldclut",
            args: Some("clut=all:interp=trilinear"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
