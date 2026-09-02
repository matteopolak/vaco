//! `lut1d` — apply a 1D lookup table loaded from a `.cube` file, one column
//! per RGB channel.
//!
//! `ffmpeg -h filter=lut1d` documents `file` (path, required) and `interp`
//! (nearest/linear/cubic/cosine/spline, default **linear** — unlike
//! `lut3d`'s default `tetrahedral`, this crate's two implemented modes
//! (nearest, linear) cover the reference's *default* exactly, not just a
//! fallback).
//!
//! # Measured: RGB only, channel-independent, and the reference truncates
//!
//! ```text
//! ffmpeg -f lavfi -i "color=red:s=2x2,format=yuv420p" -vf lut1d=file=x.cube,format=rgb24 -f rawvideo -
//! # -> auto-inserts a yuv420p->rgb24 conversion before the filter, same as
//! #    lut3d: lut1d requires an RGB pixel format.
//! ```
//!
//! Each output channel reads its **own** column of the table (`table_R` for
//! the red sample, `table_G` for green, `table_B` for blue) — not a single
//! shared curve — confirmed with a 2-row table `[(0,0,0), (0.5,0.5,0.5)]`
//! applied to `0x80808080` (`rgba`): alpha passed through unchanged at
//! `0x80`, and R/G/B all became `0x40`.
//!
//! That last probe also pins down a rounding convention this crate's
//! sibling filters got wrong: `128/255 * 0.5 * 255 = 64.0000000000001...`
//! in exact arithmetic is `64`, an unhelpful probe on its own, so the
//! disambiguating one used a 3-point table
//! (`LUT_1D_SIZE 3`: `(0,0,0)`, `(0.5,0.25,0.75)`, `(1,1,1)`) against input
//! `0x808080`: nearest mode measured `(127, 63, 191)` for source values
//! `(127.5, 63.75, 191.25)`, and linear mode measured `(128, 64, 191)` for
//! source values `(128.0, 64.5, 191.5)`. Every fractional case rounds
//! *down*, including `64.5 -> 64` and `191.5 -> 191`, which rules out both
//! round-half-away-from-zero and round-half-to-even (both would have
//! produced `64`/`192` or `128`/`65`/`192` somewhere in that set) — the
//! reference **truncates** the final float-to-sample conversion rather
//! than rounding it. `haldclutsrc` (this crate's other new filter)
//! independently reproduces the same rule (`level=3`'s pixel 1: `1*255/8 =
//! 31.875` measures as `31`, not `32`). [`lut3d`] and [`haldclut`] shipped
//! with `.round()` here before this measurement; both are corrected to
//! match (see their own doc updates).
//!
//! # Format: `.cube`, `LUT_1D_SIZE` (the same file family [`crate::lut3d`]
//! already parses)
//!
//! `LUT_1D_SIZE N` followed by `N` `"r g b"` rows — the same de facto
//! Adobe/Iridas text format `.cube`'s 3D variant uses, just with the 1D
//! keyword and one value per channel per row instead of a cube. `TITLE`/
//! `DOMAIN_MIN`/`DOMAIN_MAX`/`#` lines are recognised and skipped, matching
//! [`crate::lut3d::Cube3d::parse`]; a non-default domain is not applied
//! (same documented gap).
//!
//! # Interpolation: nearest and linear only
//!
//! `cubic`, `cosine` and `spline` need more surrounding points than a
//! two-point neighbourhood and were out of this crate's time budget;
//! requesting one of them falls back to linear. Unlike `lut3d`, this is a
//! fallback from a *non-default* request only — the reference's own
//! default (`linear`) is implemented exactly.
//!
//! # Format restriction: RGB only
//!
//! Same reasoning as [`crate::lut3d`]: a `.cube` table is defined over
//! R/G/B, so this filter requires `is_rgb()`. Alpha, if present, passes
//! through unchanged (measured above).

use std::path::Path;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "lut1d",
    description: "Adjust colors using a 1D LUT",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "lut1d", help = "Adjust colors using a 1D LUT")]
pub(crate) struct Opts {
    #[opt(name = "file", help = "set 1D LUT file name", default = String::new(), flags(video, filtering))]
    pub file: String,
    #[opt(name = "interp", help = "select interpolation mode", unit = "lut1d_interp", consts = LUT1D_INTERP_CONSTS, default = 1, range = 0..=4, flags(video, filtering))]
    pub interp: i32,
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

/// A parsed `.cube` 1D LUT: `size` RGB triples, one per input level.
#[derive(Debug, Clone)]
pub struct Lut1d {
    size: usize,
    data: Vec<[f64; 3]>,
}

impl Lut1d {
    /// # Errors
    /// A clean error for a missing `LUT_1D_SIZE`, a malformed row, or a row
    /// count that does not match `size`.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut size = None;
        let mut data = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("LUT_1D_SIZE") {
                size = rest.trim().parse::<usize>().ok();
                continue;
            }
            if line.starts_with("TITLE")
                || line.starts_with("DOMAIN_MIN")
                || line.starts_with("DOMAIN_MAX")
                || line.starts_with("LUT_3D_SIZE")
            {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(r), Some(g), Some(b)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(format!("lut1d: malformed row `{line}`"));
            };
            let (Ok(r), Ok(g), Ok(b)) = (r.parse::<f64>(), g.parse::<f64>(), b.parse::<f64>())
            else {
                return Err(format!("lut1d: non-numeric row `{line}`"));
            };
            data.push([r, g, b]);
        }
        let size = size.ok_or_else(|| "lut1d: missing LUT_1D_SIZE".to_owned())?;
        if data.len() != size {
            return Err(format!("lut1d: expected {size} rows, got {}", data.len()));
        }
        Ok(Self { size, data })
    }

    fn at(&self, ch: usize, i: usize) -> f64 {
        let i = i.min(self.size.saturating_sub(1));
        self.data
            .get(i)
            .and_then(|row| row.get(ch))
            .copied()
            .unwrap_or(0.0)
    }

    /// Sample channel `ch`'s own curve at normalised input `v` in `[0, 1]`.
    #[must_use]
    pub(crate) fn sample(&self, ch: usize, v: f64, interp: Interp) -> f64 {
        if self.size < 2 {
            return self.at(ch, 0);
        }
        let scale = (self.size - 1) as f64;
        let x = v.clamp(0.0, 1.0) * scale;
        let i0 = x.floor() as usize;
        match interp {
            Interp::Nearest => self.at(ch, x.round() as usize),
            Interp::Linear => {
                let frac = x - i0 as f64;
                let a = self.at(ch, i0);
                let b = self.at(ch, i0.saturating_add(1));
                a + (b - a) * frac
            }
        }
    }
}

/// `ffmpeg -h filter=lut1d`'s own named constants for `interp`, confirmed
/// directly -- note the non-sequential numbering (`cubic`=2, `cosine`=3).
/// Only `nearest`/`linear` are implemented; unlike `lut3d`/`haldclut` this
/// crate's default (`linear`) is one of them, so only a caller who
/// explicitly asks for `cubic`/`cosine`/`spline` is affected.
/// [`Interp::from_opt`] rejects those three by name instead of silently
/// running `linear`.
const LUT1D_INTERP_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "nearest",
        help: "",
        unit: "lut1d_interp",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "linear",
        help: "",
        unit: "lut1d_interp",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "cubic",
        help: "",
        unit: "lut1d_interp",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "cosine",
        help: "",
        unit: "lut1d_interp",
        value: vaco_opts::ConstValue::Int(3),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "spline",
        help: "",
        unit: "lut1d_interp",
        value: vaco_opts::ConstValue::Int(4),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interp {
    Nearest,
    Linear,
}

impl Interp {
    /// # Errors
    /// A named error for `cubic`/`cosine`/`spline` (`2..=4`) — see the
    /// module doc for why these are rejected rather than silently run as
    /// `linear`.
    fn from_opt(v: i32) -> std::result::Result<Self, String> {
        match v {
            0 => Ok(Self::Nearest),
            1 => Ok(Self::Linear),
            2 => Err("lut1d: interp=cubic is not implemented — see this module's doc".to_owned()),
            3 => Err("lut1d: interp=cosine is not implemented — see this module's doc".to_owned()),
            4 => Err("lut1d: interp=spline is not implemented — see this module's doc".to_owned()),
            other => Err(format!("lut1d: interp={other} is out of range (0..=4)")),
        }
    }
}

/// `v.clamp(0, 1) * max`, truncated toward zero — the reference's own
/// float-to-sample rule, measured in this module's doc.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0, 1] before scaling, so the product is in [0, max] and max fits in u16 by construction; truncation (not rounding) is the measured reference behaviour"
)]
fn to_u16(v: f64, max: f64) -> u16 {
    (v.clamp(0.0, 1.0) * max) as u16
}

#[derive(Debug)]
pub(crate) struct Filter {
    table: Lut1d,
    interp: Interp,
}

impl Filter {
    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if !format.is_rgb() || !sample::is_addressable(format) {
            return;
        }
        let big_endian = format.is_big_endian();
        for ch in 0..3usize {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let Some(mut plane) = input.plane_mut(comp.plane as usize) else {
                continue;
            };
            let max = f64::from(sample::max_value(comp));
            let w = plane
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..plane.rows() {
                let Some(row) = plane.row_mut(y) else {
                    continue;
                };
                for x in 0..w {
                    let v = f64::from(sample::read(row, x, comp, big_endian)) / max;
                    let out = self.table.sample(ch, v, self.interp);
                    sample::write(row, x, comp, big_endian, to_u16(out, max));
                }
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
    let opts = Opts::parse(req.args)?;
    if opts.file.is_empty() {
        return Err("lut1d: `file` is required".to_owned());
    }
    let text = std::fs::read_to_string(Path::new(&opts.file))
        .map_err(|e| format!("lut1d: could not read `{}`: {e}", opts.file))?;
    let table = Lut1d::parse(&text)?;
    let interp = Interp::from_opt(opts.interp)?;
    let set = FormatSet::video_list(common::formats_where(|f| {
        f.is_rgb() && sample::is_addressable(f)
    }));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter { table, interp })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn identity() -> Lut1d {
        Lut1d::parse("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap()
    }

    #[test]
    fn parses_the_documented_row_count() {
        let t = identity();
        assert_eq!(t.size, 2);
    }

    #[test]
    fn wrong_row_count_is_a_clean_error() {
        assert!(Lut1d::parse("LUT_1D_SIZE 2\n0 0 0\n").is_err());
    }

    #[test]
    fn identity_table_is_a_no_op_under_both_interpolations() {
        // Independent oracle: an identity 1D LUT (every point maps to
        // itself) must reproduce its input exactly, at grid points and
        // in between, under either implemented interpolation mode.
        let t = identity();
        for &v in &[0.0, 1.0, 0.3, 0.777] {
            for interp in [Interp::Nearest, Interp::Linear] {
                for ch in 0..3 {
                    let out = t.sample(ch, v, interp);
                    if interp == Interp::Linear {
                        assert!((out - v).abs() < 1e-9, "{interp:?} ch={ch} v={v} out={out}");
                    }
                }
            }
        }
    }

    #[test]
    fn measured_against_the_reference_a_three_point_table() {
        // Measured: ffmpeg 8.1, `lut1d=file=...:interp=nearest|linear` on
        // rgb24 0x808080 through `LUT_1D_SIZE 3`: (0,0,0), (0.5,0.25,0.75),
        // (1,1,1) (this module's doc).
        let t = Lut1d::parse("LUT_1D_SIZE 3\n0 0 0\n0.5 0.25 0.75\n1 1 1\n").unwrap();
        let v = 128.0 / 255.0;
        let nearest: Vec<u16> = (0..3)
            .map(|ch| to_u16(t.sample(ch, v, Interp::Nearest), 255.0))
            .collect();
        assert_eq!(nearest, vec![127, 63, 191]);
        let linear: Vec<u16> = (0..3)
            .map(|ch| to_u16(t.sample(ch, v, Interp::Linear), 255.0))
            .collect();
        assert_eq!(linear, vec![128, 64, 191]);
    }

    #[test]
    fn alpha_passes_through_unchanged() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 128;
            row[1] = 128;
            row[2] = 128;
            row[3] = 128;
        }
        let half = Lut1d::parse("LUT_1D_SIZE 2\n0 0 0\n0.5 0.5 0.5\n").unwrap();
        let f = Filter {
            table: half,
            interp: Interp::Linear,
        };
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[64, 64, 64, 128]);
    }

    #[test]
    fn creatable_requires_a_file() {
        let req = Instantiate {
            name: "lut1d",
            instance: "lut1d",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=lut1d`): every one of the five named `interp`
    /// constants must parse, not just the bare integer -- including the
    /// two whose numbering is not in name order (`cubic`=2, `cosine`=3).
    #[test]
    fn named_interp_values_parse() {
        for (name, expected) in [
            ("nearest", 0),
            ("linear", 1),
            ("cubic", 2),
            ("cosine", 3),
            ("spline", 4),
        ] {
            let opts = Opts::parse(Some(&format!("file=x.cube:interp={name}"))).unwrap();
            assert_eq!(opts.interp, expected, "interp={name}");
        }
    }

    /// `cubic`/`cosine`/`spline` used to silently run `linear` (the
    /// crate's default, unlike `lut3d`, is already one of the two
    /// implemented modes, so only an explicit non-default request is
    /// affected). `Interp::from_opt` now rejects each by name.
    #[test]
    fn unimplemented_interp_values_are_a_named_error_not_a_silent_substitution() {
        for v in [2, 3, 4] {
            let err = Interp::from_opt(v).unwrap_err();
            assert!(
                err.contains("lut1d") && err.contains("not implemented"),
                "interp={v}: unexpected error text: {err}"
            );
        }
    }

    #[test]
    fn implemented_interp_values_still_create() {
        assert_eq!(Interp::from_opt(0), Ok(Interp::Nearest));
        assert_eq!(Interp::from_opt(1), Ok(Interp::Linear));
    }
}
