//! `colorchart` — a 6×4 grid of solid colour patches: the standard
//! (`preset=reference`, default) or skin-tone (`preset=skintones`) colour
//! checker chart, in `gbrp`.
//!
//! `ffmpeg -h filter=colorchart` documents `rate`/`r`, `duration`/`d`,
//! `sar`, `patch_size` (default `"64x64"`) and `preset` (0 = `reference`,
//! default; 1 = `skintones`). Frame size is `6 * patch_w` × `4 * patch_h`.
//!
//! # The patch values (measured, and cross-checked)
//!
//! `preset=reference`'s 24 sRGB triples, probed via `ffmpeg -f lavfi -i
//! colorchart=preset=reference -f rawvideo -pix_fmt gbrp -frames:v 1 -` and
//! sampled at each patch centre, are **exactly** the widely published
//! X-Rite/BabelColor `ColorChecker` average sRGB values (e.g. "Dark Skin" =
//! `(115, 82, 68)`, "White" = `(243, 243, 242)`) — an independent public
//! reference this crate's values were checked against, not merely restated
//! from one probe. `preset=skintones`'s 24 triples have no equally
//! well-known public source this crate could cross-check against, so they
//! are recorded as measured, unverified against a second source.
//!
//! **Exact** for both presets, at the default `patch_size`; the patch grid
//! geometry (each patch a solid `patch_w`×`patch_h` rectangle, 6 columns by
//! 4 rows) was confirmed at the default size only.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptEnum, OptionsExt as _};
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration(-1);
const COLS: u32 = 6;
const ROWS: u32 = 4;

#[rustfmt::skip]
const REFERENCE: [[u8; 3]; 24] = [
    [115, 82, 68], [194, 150, 130], [98, 122, 157], [87, 108, 67], [133, 128, 177], [103, 189, 170],
    [214, 126, 44], [80, 91, 166], [193, 90, 99], [94, 60, 108], [157, 188, 64], [224, 163, 46],
    [56, 61, 150], [70, 148, 73], [175, 54, 60], [231, 199, 31], [187, 86, 149], [8, 133, 161],
    [243, 243, 242], [200, 200, 200], [160, 160, 160], [122, 122, 121], [85, 85, 85], [52, 52, 52],
];

#[rustfmt::skip]
const SKINTONES: [[u8; 3]; 24] = [
    [54, 38, 43], [105, 43, 42], [147, 43, 43], [77, 41, 42], [134, 43, 41], [201, 134, 118],
    [59, 41, 41], [192, 103, 76], [208, 156, 141], [152, 82, 61], [162, 132, 118], [212, 171, 150],
    [205, 91, 31], [164, 100, 55], [204, 136, 95], [178, 142, 116], [210, 152, 108], [217, 167, 131],
    [206, 166, 126], [208, 163, 97], [245, 180, 0], [212, 184, 125], [179, 165, 150], [196, 184, 105],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "colorchart_preset", base = "int")]
pub enum Preset {
    #[opt_const(name = "reference", help = "reference")]
    #[default]
    Reference,
    #[opt_const(name = "skintones", help = "skintones")]
    Skintones,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "colorchart", help = "generate color checker chart")]
pub(crate) struct Opts {
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
    #[opt(name = "patch_size", help = "set the single patch size", default = (64, 64), flags(filtering))]
    pub patch_size: (u32, u32),
    #[opt(name = "preset", help = "set the color checker chart preset", unit = "colorchart_preset", default = Preset::Reference, default_repr = "reference", flags(filtering))]
    pub preset: Preset,
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
    name: "colorchart",
    description: "Generate color checker chart",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

#[allow(
    clippy::integer_division,
    reason = "the patch grid is inherently a floor-division of pixel position by patch size"
)]
fn patch_at(x: u32, y: u32, patch_w: u32, patch_h: u32, table: &[[u8; 3]; 24]) -> [u8; 3] {
    if patch_w == 0 || patch_h == 0 {
        return [0, 0, 0];
    }
    let col = (x / patch_w).min(COLS - 1);
    let row = (y / patch_h).min(ROWS - 1);
    let idx = (row * COLS + col) as usize;
    table.get(idx).copied().unwrap_or([0, 0, 0])
}

#[derive(Debug)]
struct Source {
    patch_w: u32,
    patch_h: u32,
    table: &'static [[u8; 3]; 24],
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
                *width = self.patch_w * COLS;
                *height = self.patch_h * ROWS;
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
        let (w, h) = (self.patch_w * COLS, self.patch_h * ROWS);
        let mut frame = ctx.pool().acquire_video(PixFmt::Gbrp, w, h)?;
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
                            let rgb = patch_at(xx, yy, self.patch_w, self.patch_h, self.table);
                            // Plane order is G, B, R.
                            *px = match plane_idx {
                                0 => rgb[1],
                                1 => rgb[2],
                                _ => rgb[0],
                            };
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.duration = vaco_core::Duration(1);
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
    let (patch_w, patch_h) = opts.patch_size;
    let rate = opts.rate.0;
    let total_frames = if opts.duration.0 < 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * rate.to_f64())
                .round()
                .max(0.0) as u64,
        )
    };
    let table = match opts.preset {
        Preset::Reference => &REFERENCE,
        Preset::Skintones => &SKINTONES,
    };
    let source = Source {
        patch_w,
        patch_h,
        table,
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
                pixel_formats: Some(Constraint::Exact(PixFmt::Gbrp)),
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

    #[test]
    fn reference_preset_matches_the_published_colorchecker_values() {
        assert_eq!(patch_at(0, 0, 64, 64, &REFERENCE), [115, 82, 68]);
        assert_eq!(patch_at(383, 0, 64, 64, &REFERENCE), [103, 189, 170]);
        assert_eq!(patch_at(0, 255, 64, 64, &REFERENCE), [243, 243, 242]);
        assert_eq!(patch_at(383, 255, 64, 64, &REFERENCE), [52, 52, 52]);
    }

    #[test]
    fn skintones_preset_matches_the_measured_reference() {
        assert_eq!(patch_at(0, 0, 64, 64, &SKINTONES), [54, 38, 43]);
        assert_eq!(patch_at(383, 255, 64, 64, &SKINTONES), [196, 184, 105]);
    }

    #[test]
    fn every_pixel_in_a_patch_is_uniform() {
        for y in 0..256u32 {
            for x in [0u32, 30, 63, 64, 127] {
                let a = patch_at(x, y, 64, 64, &REFERENCE);
                let b = patch_at(x - x % 64, y, 64, 64, &REFERENCE);
                assert_eq!(a, b);
            }
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "colorchart",
            instance: "colorchart",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
