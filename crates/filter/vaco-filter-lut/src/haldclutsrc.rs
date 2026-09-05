//! `haldclutsrc` — generate an identity Hald CLUT image.
//!
//! `ffmpeg -h filter=haldclutsrc` documents `level` (2..16, default 6),
//! `rate`/`r` (default 25), `duration`/`d` (default unset, i.e.
//! unlimited — measured: running it with no `-frames:v` cap never
//! terminates on its own, the same "sentinel means infinite, not zero"
//! shape `allrgb`/`allyuv` already use) and `sar` (default 1/1).
//!
//! # Measured: size and pixel formula
//!
//! ```text
//! ffmpeg -f lavfi -i haldclutsrc=level=2 -frames:v 1 -pix_fmt rgb24 -f rawvideo -
//! # -> 8x8 rgb24 (side = level^3 = 8), always rgb24 regardless of
//! #    requested output format (probed via `-f null -`'s own stream line).
//! ```
//!
//! The pixel content matches [`crate::haldclut::decode_hald`]'s documented
//! convention exactly, read in the opposite direction: for `level=2`
//! (`N = level^2 = 4`), pixel index `idx = x + y*side` decodes to
//! `r = idx % N`, `g = (idx / N) % N`, `b = idx / N^2`, and each channel
//! value is `scale(v) = floor(v * 255 / (N - 1))` — confirmed pixel by
//! pixel: index 0 is `(0,0,0)`, index 1 is `(0x55,0,0)` (`round`-and-
//! `floor` agree at `85` here), index 4 is `(0,0x55,0)` (the `g`
//! rollover lands exactly where `r + g*N` predicts).
//!
//! # Corrected: `scale` truncates, it does not round
//!
//! A second probe at `level=3` (`N = 9`, where `1*255/8 = 31.875` and
//! `2*255/8 = 63.75` are not integers) disambiguates: pixel 1 measured
//! `0x1f` (`31`), pixel 2 measured `0x3f` (`63`) — truncation, not
//! rounding (`32`/`64`). The same rule [`crate::lut1d`] and
//! [`crate::lut3d`] measure for the LUT-application side; this module's
//! `scale` implements it the same way.
//!
//! **Exact** at every `level` this crate's tests probe.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration::from_micros(-1);

pub const DESC: FilterDesc = FilterDesc {
    name: "haldclutsrc",
    description: "Provide an identity Hald CLUT",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "haldclutsrc", help = "Provide an identity Hald CLUT")]
pub(crate) struct Opts {
    #[opt(name = "level", help = "set level", default = 6, range = 2..=16, flags(filtering))]
    pub level: i32,
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
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

/// `floor(v * 255 / (n - 1))`, the measured (truncating, not rounding)
/// scale from a `0..n-1` index to an 8-bit sample — see this module's doc.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "v in [0, n-1] and n <= 256*256 by construction (level <= 16, n = level*level <= 256), so the product fits in u16 well before truncation"
)]
fn scale(v: u32, n: u32) -> u16 {
    if n <= 1 {
        return 0;
    }
    ((f64::from(v) * 255.0) / f64::from(n - 1)) as u16
}

/// Fill one `rgb24` row of the `side x side` identity Hald image at row
/// `row_y`, per this module's measured index formula.
fn fill_row(row: &mut [u8], row_y: u32, side: u32, count: u32) {
    for (col_x, px) in (0u32..side).zip(row.chunks_exact_mut(3)) {
        let idx = col_x.saturating_add(row_y.saturating_mul(side));
        let red_idx = idx % count;
        let green_idx = idx.checked_div(count).unwrap_or(0) % count;
        let blue_idx = idx.checked_div(count.saturating_mul(count)).unwrap_or(0);
        if let [pr, pg, pb] = px {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "scale() returns a value in [0, 255] for count <= 256"
            )]
            let to_u8 = |v: u16| v as u8;
            *pr = to_u8(scale(red_idx, count));
            *pg = to_u8(scale(green_idx, count));
            *pb = to_u8(scale(blue_idx, count));
        }
    }
}

#[derive(Debug)]
struct Source {
    side: u32,
    n: u32,
    frame_rate: Rational,
    sar: Rational,
    total_frames: Option<u64>,
    next: i64,
}

fn frame_budget(duration: VDuration, rate: Rational) -> u64 {
    duration
        .to_ticks_rounding(rate.inverse(), vaco_core::Rounding::NearestAwayFromZero)
        .and_then(|frames| u64::try_from(frames.max(0)).ok())
        .unwrap_or(0)
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
                *width = self.side;
                *height = self.side;
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
            .acquire_video(PixFmt::Rgb24, self.side, self.side)?;
        if let Some(mut plane) = frame.plane_mut(0) {
            for y in 0..plane.rows() {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "plane.rows() == self.side, which came from a u32 level <= 16"
                )]
                let yy = y as u32;
                if let Some(row) = plane.row_mut(y) {
                    fill_row(row, yy, self.side, self.n);
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
    #[allow(
        clippy::cast_sign_loss,
        reason = "opts.level's range (2..=16) is enforced by vaco-opts before this runs"
    )]
    let level = opts.level as u32;
    let n = level.saturating_mul(level);
    let side = level.saturating_pow(3);
    let rate = opts.rate.0;
    let total_frames = if opts.duration < VDuration::ZERO {
        None
    } else {
        Some(frame_budget(opts.duration, rate))
    };
    let source = Source {
        side,
        n,
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
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgb24)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn frame_budget_retains_a_large_awkward_clock_duration() {
        let frames = 9_007_199_254_740_993_i64;
        let duration = VDuration::from_ticks(frames, Rational::new(1_001, 30_000))
            .unwrap_or(VDuration::ZERO);
        assert_eq!(frame_budget(duration, Rational::new(30_000, 1_001)), frames as u64);
    }

    fn frame_rows(level: u32) -> Vec<Vec<u8>> {
        let n = level * level;
        let side = level.pow(3);
        (0..side)
            .map(|y| {
                let mut row = vec![0u8; (side * 3) as usize];
                fill_row(&mut row, y, side, n);
                row
            })
            .collect()
    }

    #[test]
    fn matches_measured_reference_points_at_level_2() {
        // Measured: ffmpeg 8.1, haldclutsrc=level=2 (this module's doc).
        let rows = frame_rows(2);
        let px = |x: usize, y: usize| {
            let i = x * 3;
            (rows[y][i], rows[y][i + 1], rows[y][i + 2])
        };
        assert_eq!(px(0, 0), (0, 0, 0));
        assert_eq!(px(1, 0), (0x55, 0, 0));
        assert_eq!(px(2, 0), (0xaa, 0, 0));
        assert_eq!(px(3, 0), (0xff, 0, 0));
        // idx 4 = x=4,y=0 (side=8, so idx 4 is still row 0): g rolls over
        // from 0 to 1 here (idx / N with N=4), matching the R rollover's
        // period exactly.
        assert_eq!(px(4, 0), (0, 0x55, 0));
        // idx 8 = x=0,y=1: g's second step, 2*255/3 = 170 = 0xaa.
        assert_eq!(px(0, 1), (0, 0xaa, 0));
    }

    #[test]
    fn truncates_rather_than_rounds_at_level_3() {
        // Measured: ffmpeg 8.1, haldclutsrc=level=3, pixel 1 is 0x1f (31,
        // not the rounded 32) and pixel 2 is 0x3f (63, not 64).
        let rows = frame_rows(3);
        assert_eq!(rows[0][3], 0x1f);
        assert_eq!(rows[0][6], 0x3f);
    }

    /// The independent oracle: the generated image, reinterpreted through
    /// `haldclut::decode_hald`, must be the identity 3D LUT — the same
    /// property `haldclut`'s own tests check on a hand-built identity
    /// image, now checked on this module's *generated* one instead of a
    /// second hand-transcription of the same formula.
    #[test]
    fn generated_image_decodes_back_to_the_identity_cube() {
        let level = 3;
        let side = level * level * level;
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, side, side).unwrap();
        {
            let n = level * level;
            let mut plane = frame.plane_mut(0).unwrap();
            for y in 0..plane.rows() {
                let row = plane.row_mut(y).unwrap();
                fill_row(row, y as u32, side, n);
            }
        }
        let cube = crate::haldclut::decode_hald(&frame).unwrap();
        for &(r, g, b) in &[(0.0, 0.0, 0.0), (1.0, 1.0, 1.0), (0.4, 0.6, 0.2)] {
            let out = cube.sample_trilinear(r, g, b);
            assert!((out[0] - r).abs() < 0.05, "r {out:?} vs {r}");
            assert!((out[1] - g).abs() < 0.05, "g {out:?} vs {g}");
            assert!((out[2] - b).abs() < 0.05, "b {out:?} vs {b}");
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "haldclutsrc",
            instance: "haldclutsrc",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
