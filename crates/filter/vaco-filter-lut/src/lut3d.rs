//! `lut3d` — apply a 3D lookup table loaded from a `.cube` file.
//!
//! `ffmpeg -h filter=lut3d` documents `file` (path, required), `clut`
//! (first/all, default all — governs a *video* CLUT input this filter does
//! not have; parsed and otherwise unused), and `interp` (nearest/
//! trilinear/tetrahedral/pyramid/prism, default tetrahedral).
//!
//! # Measured: output format and identity round-trip
//!
//! ```text
//! ffmpeg -f lavfi -i "color=0x123456,format=rgb24" -vf lut3d=file=identity.cube -f rawvideo -
//! # -> byte-identical output for a size-2 identity cube (every corner
//! #    maps to itself): 0x12 0x34 0x56 unchanged. Format stays rgb24.
//! ```
//!
//! # Format: `.cube` (the de facto Adobe/Iridas text format)
//!
//! `LUT_3D_SIZE N` followed by `N^3` `"r g b"` rows, red fastest-varying —
//! a documented, publicly specified text format (not reference-internal
//! behaviour), so this parser is written directly against that
//! specification rather than probed. `DOMAIN_MIN`/`DOMAIN_MAX` and `TITLE`
//! lines, and `#`-prefixed comments, are recognised and skipped; a
//! non-default domain is not applied (documented gap — every `.cube` this
//! crate has tested uses the default `0..1` domain).
//!
//! # Interpolation: trilinear and nearest only, and `tetrahedral` — the
//! reference's own default — is now a hard error
//!
//! `tetrahedral`, `pyramid` and `prism` need a different geometric
//! decomposition of the enclosing cube than trilinear and are out of this
//! crate's time budget to implement. This used to fall back to trilinear
//! rather than erroring, reasoned as "the visual difference is usually
//! small and a LUT this crate cannot open is a worse failure than one it
//! approximates" — which means **every unconfigured `lut3d` call silently
//! ran the wrong interpolation**, since `tetrahedral` is the reference's
//! own default. Verified concretely: the same 2-level `.cube` and the
//! same `0x808080` input pixel give `0x69` under `trilinear` and `0x26`
//! under `tetrahedral` on real `ffmpeg 8.1` — a large, real divergence,
//! not a rounding difference. A silent substitution that lands on every
//! default invocation is the worse failure, not the smaller one: `create`
//! now rejects `tetrahedral`/`pyramid`/`prism` by name instead. This is a
//! real, deliberate behaviour change — a bare `lut3d=file=…` now errors
//! where it used to silently run trilinear — not a refinement that
//! preserves the old default's usability; pass `interp=trilinear` (or
//! `nearest`) explicitly to get a working filter today.
//!
//! # Format restriction: RGB only
//!
//! A `.cube` table is defined over R/G/B; this filter requires
//! `is_rgb()`, forcing an upstream conversion for a YUV input (mirroring
//! [`crate::swapuv`]'s family-restriction pattern). Alpha, if present,
//! passes through unchanged.
//!
//! # Corrected: the reference truncates the final sample, it does not round
//!
//! This filter shipped calling `.round()` when converting an interpolated
//! `[0, 1]` value back to an integer sample. A follow-up probe while
//! building [`crate::lut1d`] (see that module's doc for the full
//! measurement) found the opposite: a size-2 cube worth `0.5*(r, g, b)`
//! applied to `0x808080` under `interp=nearest` measures `0x7f7f7f`
//! (`127, 127, 127`), not `0x80808080`'s rounded `128` — the nearest grid
//! corner's value `0.5` scales to exactly `127.5`, and the reference kept
//! the `127`. Confirmed independently by `lut1d` (a different code path,
//! same rule) and by `haldclutsrc`'s pixel generation. `to_u16` here (and
//! in [`crate::haldclut`]) now truncates rather than rounds; see
//! `truncates_rather_than_rounds_the_final_sample` for the regression
//! test this measurement produced.
//!
//! # Attempted and abandoned: `.3dl`/`.dat`/`.m3d`
//!
//! `planning/16-filters.md`'s row for this crate also names `.3dl`/`.dat`/
//! `.m3d` parsers. `.3dl` (Autodesk Lustre's format) was probed twice: a
//! two-point mesh header (`0 1023`) followed by 8 `"r g b"` rows for a
//! 2-level cube, and the same 8 rows with no mesh header at all. Both
//! produced `Parsed_lut3d_0: Unexpected EOF` rather than a result — the
//! reference expects substantially more input than either guess supplied,
//! which means its mesh-size detection does not work the way either probe
//! assumed, and a third guess would be exactly the failure mode
//! `planning/AGENT-CONSTRAINTS.md` warns against (a formula that matches
//! one probe and is silently wrong elsewhere — see that file's
//! `fillborders=reflect` example). Rather than ship a `.3dl` reader tested
//! against zero successful reference round-trips, this crate leaves it
//! unimplemented: `Cube3d::parse` only understands `.cube`. `.dat`/`.m3d`
//! were not attempted at all, for the same reason one level up — no
//! working `.3dl` parse to generalise from.

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
    name: "lut3d",
    description: "Adjust colors using a 3D LUT",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "lut3d", help = "Adjust colors using a 3D LUT")]
pub(crate) struct Opts {
    #[opt(name = "file", help = "set 3D LUT file name", default = String::new(), flags(video, filtering))]
    pub file: String,
    #[opt(name = "clut", help = "when to process CLUT (not applicable; parsed only)", unit = "clut_mode", consts = crate::common::CLUT_CONSTS, default = 1, range = 0..=1, flags(video, filtering))]
    pub clut: i32,
    #[opt(name = "interp", help = "select interpolation mode", unit = "lut3d_interp", consts = crate::common::LUT3D_INTERP_CONSTS, default = 2, range = 0..=4, flags(video, filtering))]
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

/// A parsed `.cube` 3D LUT: `size^3` RGB triples, red fastest-varying.
#[derive(Debug, Clone)]
pub struct Cube3d {
    pub(crate) size: usize,
    data: Vec<[f64; 3]>,
}

impl Cube3d {
    /// Build directly from decoded samples, red fastest-varying — the
    /// shape [`crate::haldclut`] decodes a Hald image into.
    pub(crate) fn from_samples(size: usize, data: Vec<[f64; 3]>) -> Self {
        Self { size, data }
    }

    /// # Errors
    /// A clean error for a missing `LUT_3D_SIZE`, a malformed row, or a row
    /// count that does not match `size^3`.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut size = None;
        let mut data = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
                size = rest.trim().parse::<usize>().ok();
                continue;
            }
            if line.starts_with("TITLE")
                || line.starts_with("DOMAIN_MIN")
                || line.starts_with("DOMAIN_MAX")
                || line.starts_with("LUT_1D_SIZE")
            {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(r), Some(g), Some(b)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(format!("lut3d: malformed row `{line}`"));
            };
            let (Ok(r), Ok(g), Ok(b)) = (r.parse::<f64>(), g.parse::<f64>(), b.parse::<f64>())
            else {
                return Err(format!("lut3d: non-numeric row `{line}`"));
            };
            data.push([r, g, b]);
        }
        let size = size.ok_or_else(|| "lut3d: missing LUT_3D_SIZE".to_owned())?;
        let expected = size.saturating_mul(size).saturating_mul(size);
        if data.len() != expected {
            return Err(format!(
                "lut3d: expected {expected} rows for size {size}, got {}",
                data.len()
            ));
        }
        Ok(Self { size, data })
    }

    fn at(&self, r: usize, g: usize, b: usize) -> [f64; 3] {
        let r = r.min(self.size.saturating_sub(1));
        let g = g.min(self.size.saturating_sub(1));
        let b = b.min(self.size.saturating_sub(1));
        let idx = r
            .saturating_add(g.saturating_mul(self.size))
            .saturating_add(b.saturating_mul(self.size).saturating_mul(self.size));
        self.data.get(idx).copied().unwrap_or([0.0, 0.0, 0.0])
    }

    /// Trilinear interpolation at normalised `(r, g, b)` in `[0, 1]`.
    #[must_use]
    pub(crate) fn sample_trilinear(&self, r: f64, g: f64, b: f64) -> [f64; 3] {
        if self.size < 2 {
            return self.at(0, 0, 0);
        }
        let scale = (self.size - 1) as f64;
        let (rf, gf, bf) = (
            r.clamp(0.0, 1.0) * scale,
            g.clamp(0.0, 1.0) * scale,
            b.clamp(0.0, 1.0) * scale,
        );
        let (r0, g0, b0) = (
            rf.floor() as usize,
            gf.floor() as usize,
            bf.floor() as usize,
        );
        let (rd, gd, bd) = (rf - r0 as f64, gf - g0 as f64, bf - b0 as f64);
        let c000 = self.at(r0, g0, b0);
        let c100 = self.at(r0 + 1, g0, b0);
        let c010 = self.at(r0, g0 + 1, b0);
        let c110 = self.at(r0 + 1, g0 + 1, b0);
        let c001 = self.at(r0, g0, b0 + 1);
        let c101 = self.at(r0 + 1, g0, b0 + 1);
        let c011 = self.at(r0, g0 + 1, b0 + 1);
        let c111 = self.at(r0 + 1, g0 + 1, b0 + 1);
        let at = |a: [f64; 3], i: usize| a.get(i).copied().unwrap_or(0.0);
        core::array::from_fn(|i| {
            let c00 = at(c000, i) * (1.0 - rd) + at(c100, i) * rd;
            let c10 = at(c010, i) * (1.0 - rd) + at(c110, i) * rd;
            let c01 = at(c001, i) * (1.0 - rd) + at(c101, i) * rd;
            let c11 = at(c011, i) * (1.0 - rd) + at(c111, i) * rd;
            let c0 = c00 * (1.0 - gd) + c10 * gd;
            let c1 = c01 * (1.0 - gd) + c11 * gd;
            c0 * (1.0 - bd) + c1 * bd
        })
    }

    #[must_use]
    pub(crate) fn sample_nearest(&self, r: f64, g: f64, b: f64) -> [f64; 3] {
        if self.size < 2 {
            return self.at(0, 0, 0);
        }
        let scale = (self.size - 1) as f64;
        let round = |v: f64| (v.clamp(0.0, 1.0) * scale).round() as usize;
        self.at(round(r), round(g), round(b))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interp {
    Nearest,
    Trilinear,
}

impl Interp {
    /// # Errors
    /// A named error for `tetrahedral`/`pyramid`/`prism` (`2..=4`) — see
    /// the module doc for why these are rejected rather than silently
    /// run as `trilinear`.
    fn from_opt(v: i32) -> std::result::Result<Self, String> {
        match v {
            0 => Ok(Self::Nearest),
            1 => Ok(Self::Trilinear),
            2 => Err(
                "lut3d: interp=tetrahedral is not implemented (this is the reference's own \
                 default; pass interp=trilinear or interp=nearest explicitly — see this \
                 module's doc)"
                    .to_owned(),
            ),
            3 => Err("lut3d: interp=pyramid is not implemented — see this module's doc".to_owned()),
            4 => Err("lut3d: interp=prism is not implemented — see this module's doc".to_owned()),
            other => Err(format!("lut3d: interp={other} is out of range (0..=4)")),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    cube: Cube3d,
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
        let Some((cr, cg, cb)) = (|| {
            Some((
                sample::component(format, 0)?,
                sample::component(format, 1)?,
                sample::component(format, 2)?,
            ))
        })() else {
            return;
        };
        let (Some(pr), Some(pg), Some(pb)) = (
            input.plane(cr.plane as usize),
            input.plane(cg.plane as usize),
            input.plane(cb.plane as usize),
        ) else {
            return;
        };
        let max_r = f64::from(sample::max_value(cr));
        let max_g = f64::from(sample::max_value(cg));
        let max_b = f64::from(sample::max_value(cb));
        let rows = pr.rows();
        let w = pr
            .row_bytes()
            .checked_div(usize::from(cr.step.max(1)))
            .unwrap_or(0);
        // Snapshot the source before any plane is (potentially) aliased
        // with another for a packed format where R/G/B share bytes.
        let src: Vec<Vec<(u16, u16, u16)>> = (0..rows)
            .map(|y| {
                let (Some(rr), Some(rg), Some(rb)) = (pr.row(y), pg.row(y), pb.row(y)) else {
                    return Vec::new();
                };
                (0..w)
                    .map(|x| {
                        (
                            sample::read(rr, x, cr, big_endian),
                            sample::read(rg, x, cg, big_endian),
                            sample::read(rb, x, cb, big_endian),
                        )
                    })
                    .collect()
            })
            .collect();
        // A packed format (`rgb24`) has all three components in the same
        // plane, so `Frame::planes_mut` (all planes disjoint at once) is
        // used rather than three separate `plane_mut` calls, which would
        // borrow the same plane mutably more than once when packed.
        let mut planes = input.planes_mut();
        for y in 0..rows {
            let Some(row_src) = src.get(y) else { continue };
            for x in 0..w {
                let Some(&(vr, vg, vb)) = row_src.get(x) else {
                    continue;
                };
                let (r, g, b) = (
                    f64::from(vr) / max_r,
                    f64::from(vg) / max_g,
                    f64::from(vb) / max_b,
                );
                let out = match self.interp {
                    Interp::Nearest => self.cube.sample_nearest(r, g, b),
                    Interp::Trilinear => self.cube.sample_trilinear(r, g, b),
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to [0, 1] before scaling, so the product is in [0, max] and max fits in u16 by construction; truncation (not rounding) is the measured reference behaviour, see this module's doc"
                )]
                let to_u16 = |v: f64, max: f64| v.clamp(0.0, 1.0).mul_add(max, 0.0) as u16;
                if let Some(row) = planes.get_mut(cr.plane as usize).and_then(|p| p.row_mut(y)) {
                    sample::write(row, x, cr, big_endian, to_u16(out[0], max_r));
                }
                if let Some(row) = planes.get_mut(cg.plane as usize).and_then(|p| p.row_mut(y)) {
                    sample::write(row, x, cg, big_endian, to_u16(out[1], max_g));
                }
                if let Some(row) = planes.get_mut(cb.plane as usize).and_then(|p| p.row_mut(y)) {
                    sample::write(row, x, cb, big_endian, to_u16(out[2], max_b));
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
        return Err("lut3d: `file` is required".to_owned());
    }
    let text = std::fs::read_to_string(Path::new(&opts.file))
        .map_err(|e| format!("lut3d: could not read `{}`: {e}", opts.file))?;
    let cube = Cube3d::parse(&text)?;
    let interp = Interp::from_opt(opts.interp)?;
    let set = FormatSet::video_list(common::formats_where(|f| {
        f.is_rgb() && sample::is_addressable(f)
    }));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter { cube, interp })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn identity_cube() -> Cube3d {
        Cube3d::parse(
            "LUT_3D_SIZE 2\n\
             0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n",
        )
        .unwrap()
    }

    #[test]
    fn parses_the_documented_row_count() {
        let cube = identity_cube();
        assert_eq!(cube.size, 2);
        assert_eq!(cube.data.len(), 8);
    }

    #[test]
    fn wrong_row_count_is_a_clean_error() {
        assert!(Cube3d::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }

    #[test]
    fn identity_cube_trilinear_is_a_no_op() {
        // Independent oracle: an identity LUT (every corner maps to
        // itself) must reproduce its input exactly under trilinear
        // interpolation, for any interior point too.
        let cube = identity_cube();
        for &(r, g, b) in &[
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (0.3, 0.7, 0.5),
            (0.123, 0.456, 0.789),
        ] {
            let out = cube.sample_trilinear(r, g, b);
            assert!((out[0] - r).abs() < 1e-9, "r: {out:?} vs {r}");
            assert!((out[1] - g).abs() < 1e-9, "g: {out:?} vs {g}");
            assert!((out[2] - b).abs() < 1e-9, "b: {out:?} vs {b}");
        }
    }

    #[test]
    fn measured_against_the_reference_a_hand_built_2x2x2_cube() {
        // Measured: ffmpeg 8.1, `lut3d=file=identity.cube` on rgb24
        // 0x123456 reproduces 0x123456 exactly (this module's doc).
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 0x12;
            row[1] = 0x34;
            row[2] = 0x56;
        }
        let f = Filter {
            cube: identity_cube(),
            interp: Interp::Trilinear,
        };
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[0x12, 0x34, 0x56]);
    }

    #[test]
    fn truncates_rather_than_rounds_the_final_sample() {
        // Measured: ffmpeg 8.1, a size-2 cube worth exactly half the input
        // (every corner but (0,0,0) scaled by 0.5) applied to rgb24
        // 0x808080 under interp=nearest measures 0x7f7f7f, not the
        // 0x80808080-rounded 0x80: the nearest corner's value 0.5 scales to
        // exactly 127.5, and the reference keeps 127 (this module's doc).
        let cube = Cube3d::parse(
            "LUT_3D_SIZE 2\n\
             0 0 0\n0.5 0 0\n0 0.5 0\n0.5 0.5 0\n\
             0 0 0.5\n0.5 0 0.5\n0 0.5 0.5\n0.5 0.5 0.5\n",
        )
        .unwrap();
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 0x80;
            row[1] = 0x80;
            row[2] = 0x80;
        }
        let f = Filter {
            cube,
            interp: Interp::Nearest,
        };
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[0x7f, 0x7f, 0x7f]);
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=lut3d`): every named `clut`/`interp` constant
    /// must parse, not just the bare integer.
    #[test]
    fn named_option_values_parse() {
        for (name, expected) in [("first", 0), ("all", 1)] {
            let opts = Opts::parse(Some(&format!("file=x.cube:clut={name}"))).unwrap();
            assert_eq!(opts.clut, expected, "clut={name}");
        }
        for (name, expected) in [
            ("nearest", 0),
            ("trilinear", 1),
            ("tetrahedral", 2),
            ("pyramid", 3),
            ("prism", 4),
        ] {
            let opts = Opts::parse(Some(&format!("file=x.cube:interp={name}"))).unwrap();
            assert_eq!(opts.interp, expected, "interp={name}");
        }
    }

    /// `tetrahedral` (the reference's own default), `pyramid` and `prism`
    /// used to silently run `trilinear` -- accepted, wrong, no error, on
    /// every unconfigured call since `tetrahedral` is the default.
    /// `Interp::from_opt` now rejects each by name instead.
    #[test]
    fn unimplemented_interp_values_are_a_named_error_not_a_silent_substitution() {
        for v in [2, 3, 4] {
            let err = Interp::from_opt(v).unwrap_err();
            assert!(
                err.contains("lut3d") && err.contains("not implemented"),
                "interp={v}: unexpected error text: {err}"
            );
        }
    }

    #[test]
    fn implemented_interp_values_still_create() {
        assert_eq!(Interp::from_opt(0), Ok(Interp::Nearest));
        assert_eq!(Interp::from_opt(1), Ok(Interp::Trilinear));
    }
}
