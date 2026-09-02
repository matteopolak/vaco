//! `sierpinski` — the Sierpinski carpet (`type=carpet`, the default) or
//! triangle (`type=triangle`) fractal, in `rgb0`.
//!
//! `ffmpeg -h filter=sierpinski` documents `size`/`s`, `rate`/`r`, `seed`
//! (triangle mode's chaos-game RNG), `jump` (default 100) and `type` (0 =
//! `carpet`, default; 1 = `triangle`).
//!
//! # The carpet: closed form, membership test
//!
//! Probed at 27×27 = 3³ (`ffmpeg -f lavfi -i
//! sierpinski=size=27x27:type=carpet -f rawvideo -pix_fmt rgb0 -frames:v 1
//! -`), rendered as ASCII in this crate's own probe log: the classic
//! Sierpinski carpet, exactly. The textbook membership test — a pixel is a
//! **hole** (background, `(0,0,0,0)`) iff at some level its base-3 digit of
//! `x` and of `y` are *both* `1`, otherwise it is **filled**
//! (`(255,255,255,255)`) — reproduces the probe exactly, including at the
//! non-power-of-3 default size (the test does not require `w`/`h` to be a
//! power of 3; it is evaluated per absolute pixel coordinate). This is a
//! closed-form definition independent of the reference (Sierpinski's own
//! 1916 construction), not a transcription of anything reference-specific.
//!
//! # What is not reproduced: the zoom animation
//!
//! A second probe (3 consecutive frames at the same size) shows the
//! reference's carpet is **not** static — it changes frame to frame, almost
//! certainly a zoom/pan driven by `jump`. This crate could not pin that
//! animation's exact parametrisation in the time available, so **only frame
//! composition is exact for a static view of the carpet (equivalent to the
//! reference's frame 0)**; every output frame here renders the same static
//! carpet rather than animating. See `docs/filter/vaco-filter-source.md`.
//!
//! # The triangle: chaos game, not calibrated
//!
//! `type=triangle` is implemented as the standard chaos-game construction
//! (repeatedly jump halfway toward a randomly chosen triangle vertex,
//! `jump` times per pixel drawn, seeded by `seed`) using this crate's own
//! [`crate::rng`] — see that module's doc for why seeded generators here do
//! not reproduce the reference's bit stream. **Algorithmically faithful,
//! not bit-exact.**

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptEnum, OptionsExt as _};
use vaco_pixfmt::PixFmt;

use crate::rng::{SplitMix64, resolve_seed};
use vaco_filter_graph::registry::{Instance, Instantiate};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "sierpinski_type", base = "int")]
pub enum FractalType {
    #[opt_const(name = "carpet", help = "sierpinski carpet")]
    #[default]
    Carpet,
    #[opt_const(name = "triangle", help = "sierpinski triangle")]
    Triangle,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "sierpinski", help = "render a Sierpinski fractal")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set frame size", default = (640, 480), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set frame rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "seed", help = "set the seed", default = -1_i64, range = -1..=0xFFFF_FFFF_i64, flags(filtering))]
    pub seed: i64,
    #[opt(name = "jump", help = "set the jump", default = 100, range = 1..=10000, flags(filtering))]
    pub jump: i32,
    #[opt(name = "type", help = "set fractal type", unit = "sierpinski_type", default = FractalType::Carpet, default_repr = "carpet", flags(filtering))]
    pub kind: FractalType,
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
    name: "sierpinski",
    description: "Render a Sierpinski fractal",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

/// The carpet membership test: `true` for a filled pixel.
pub(crate) fn carpet_filled(mut x: u32, mut y: u32) -> bool {
    while x > 0 || y > 0 {
        if x % 3 == 1 && y % 3 == 1 {
            return false;
        }
        x /= 3;
        y /= 3;
    }
    true
}

/// Renders one static triangle via the chaos game: `jump` warm-up jumps,
/// then one plotted point per remaining iteration, `iterations` total.
fn render_triangle(width: u32, height: u32, jump: u32, seed: u64) -> Vec<bool> {
    let mut filled = vec![false; (width as usize) * (height as usize)];
    if width == 0 || height == 0 {
        return filled;
    }
    let w = f64::from(width);
    let h = f64::from(height);
    let vertices = [(w / 2.0, 0.0), (0.0, h - 1.0), (w - 1.0, h - 1.0)];
    let mut rng = SplitMix64::new(seed);
    let mut p = (w / 2.0, h / 2.0);
    #[allow(
        clippy::integer_division,
        reason = "a quarter of the pixel count is a deliberate, generous iteration budget"
    )]
    let iterations = u64::from(width) * u64::from(height) / 4 + u64::from(jump);
    for i in 0..iterations {
        let v = vertices
            .get(rng.next_below(3))
            .copied()
            .unwrap_or_else(|| vertices.first().copied().unwrap_or((0.0, 0.0)));
        p = (f64::midpoint(p.0, v.0), f64::midpoint(p.1, v.1));
        if i >= u64::from(jump) {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "p is clamped into the frame just below"
            )]
            let (x, y) = (
                p.0.round().clamp(0.0, w - 1.0) as u32,
                p.1.round().clamp(0.0, h - 1.0) as u32,
            );
            if let Some(slot) = filled.get_mut(y as usize * width as usize + x as usize) {
                *slot = true;
            }
        }
    }
    filled
}

#[derive(Debug)]
enum Pattern {
    Carpet,
    Triangle(Vec<bool>),
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    pattern: Pattern,
    frame_rate: Rational,
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
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Rgb0, self.width, self.height)?;
        if let Some(mut plane) = frame.plane_mut(0) {
            for row_idx in 0..plane.rows() {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "plane.rows() == height, which fits in u32"
                )]
                let yy = row_idx as u32;
                if let Some(row) = plane.row_mut(row_idx) {
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "x < width, which fits in u32"
                        )]
                        let xx = x as u32;
                        let filled = match &self.pattern {
                            Pattern::Carpet => carpet_filled(xx, yy),
                            Pattern::Triangle(map) => map
                                .get(yy as usize * self.width as usize + xx as usize)
                                .copied()
                                .unwrap_or(false),
                        };
                        let v = if filled { 255 } else { 0 };
                        if let [r, g, b, a] = px {
                            *r = v;
                            *g = v;
                            *b = v;
                            *a = v;
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.duration = vaco_core::Duration(1);
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

/// Same defensive purpose as `life`'s `MAX_CELLS`: the chaos game's `filled`
/// buffer is a plain `Vec<bool>`, not a `vaco_frame` plane, so nothing else
/// bounds it. Found by this crate's own fuzz target trying `size=911111x91111`.
const MAX_TRIANGLE_CELLS: u64 = 1 << 26;

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    let rate = opts.rate.0;
    let jump = u32::try_from(opts.jump.max(1)).unwrap_or(100);
    let pattern = match opts.kind {
        FractalType::Carpet => Pattern::Carpet,
        FractalType::Triangle => {
            let cells = u64::from(width) * u64::from(height);
            if cells > MAX_TRIANGLE_CELLS {
                return Err(format!(
                    "sierpinski: size {width}x{height} ({cells} cells) exceeds the {MAX_TRIANGLE_CELLS}-cell limit for type=triangle"
                ));
            }
            let seed = resolve_seed(opts.seed, 0x5A17_C0DE);
            Pattern::Triangle(render_triangle(width, height, jump, seed))
        }
    };
    let source = Source {
        width,
        height,
        pattern,
        frame_rate: rate,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgb0)),
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
    fn carpet_matches_the_measured_reference_pattern() {
        // Transcribed from the module doc's 27x27 probe: '#' = filled.
        const ROWS: [&str; 9] = [
            "#########",
            "#.##.##.#",
            "#########",
            "###...###",
            "#.#...#.#",
            "###...###",
            "#########",
            "#.##.##.#",
            "#########",
        ];
        for (y, row) in ROWS.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                let expect = ch == '#';
                assert_eq!(carpet_filled(x as u32, y as u32), expect, "({x},{y})");
            }
        }
    }

    /// Independent oracle: the carpet has self-similar structure at every
    /// scale — the pattern in the top-left 9x9 block must recur, scaled by
    /// 3, as the top-left 27x27 block's own top-left 9x9-of-27 quadrant
    /// (this is what "fractal" means, not a re-statement of the digit test).
    #[test]
    #[allow(
        clippy::integer_division,
        reason = "computing which 9x9 macro-block a coordinate falls in is exactly floor(x/9)"
    )]
    fn the_carpet_is_self_similar_across_a_factor_of_three() {
        for y in 0..27u32 {
            for x in 0..27u32 {
                // Removing the middle 9x9 block of a 27x27 tile, and scaling
                // by 3, must reproduce the 9x9 carpet's own hole pattern.
                if x / 9 == 1 && y / 9 == 1 {
                    continue;
                }
                let scaled = carpet_filled(x % 9, y % 9);
                assert_eq!(carpet_filled(x, y), scaled, "({x},{y})");
            }
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "sierpinski",
            instance: "sierpinski",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn triangle_mode_is_reproducible_for_a_fixed_seed() {
        let a = render_triangle(64, 64, 10, 42);
        let b = render_triangle(64, 64, 10, 42);
        assert_eq!(a, b);
        assert!(a.iter().any(|&f| f), "chaos game must fill something");
    }
}
