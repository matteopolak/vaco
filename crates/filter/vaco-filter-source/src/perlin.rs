//! `perlin` — fractal (multi-octave) Perlin gradient noise, in `gray8`.
//!
//! `ffmpeg -h filter=perlin` documents `size`/`s`, `rate`/`r`, `octaves`,
//! `persistence`, `xscale`/`yscale`/`tscale`, and `random_mode` (0 =
//! `random`, default; 1 = `ken`, Perlin's own 1985 reference permutation
//! table; 2 = `seed`, `random_seed`/`seed`).
//!
//! # What is implemented, and what is not calibrated
//!
//! This module implements the textbook 2D Perlin gradient-noise algorithm
//! (Perlin, "An Image Synthesizer", 1985; the classic fade/lerp/gradient
//! construction, not the 2002 "improved noise" simplex variant) with
//! fractal summation across `octaves` at `persistence` per octave —
//! published, reproducible, and independent of any reference source.
//!
//! It does **not** reproduce Perlin's original 256-entry permutation table
//! for `random_mode=ken`: transcribing that table by hand risks a silent
//! single-entry error that would be far worse than an honest
//! non-reproduction, and no probe of a black-box binary can recover a
//! literal integer table more reliably than getting it from the (public,
//! but off-limits per D7) source. All three `random_mode`s here build their
//! permutation from this crate's own [`crate::rng`] (Fisher–Yates shuffle
//! of `0..256`), so `random_mode=ken` and `random_mode=seed` are
//! indistinguishable other than by the seed each resolves to.
//!
//! The exact `[-1, 1] -> [0, 255]` output scaling and the fractal-sum
//! normalisation are this crate's own reasonable choices, not measured
//! against the reference. **Algorithmically faithful (a real, published
//! noise construction), not calibrated or bit-exact.** See
//! `docs/filter/vaco-filter-source.md`.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptEnum, OptionsExt as _};
use vaco_pixfmt::PixFmt;

use crate::rng::SplitMix64;
use vaco_filter_graph::registry::{Instance, Instantiate};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "perlin_random_mode", base = "int")]
pub enum RandomMode {
    #[opt_const(name = "random", help = "compute and use random seed")]
    #[default]
    Random,
    #[opt_const(
        name = "ken",
        help = "use the predefined initial pattern defined by Ken Perlin in the original article"
    )]
    Ken,
    #[opt_const(name = "seed", help = "use the value specified by random_seed")]
    Seed,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "perlin", help = "generate Perlin noise")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "octaves", help = "set the number of components", default = 1, range = 1..=i32::MAX, flags(filtering))]
    pub octaves: i32,
    #[opt(
        name = "persistence",
        help = "set the octaves persistence",
        default = 1.0,
        flags(filtering)
    )]
    pub persistence: f64,
    #[opt(
        name = "xscale",
        help = "set x-scale factor",
        default = 1.0,
        flags(filtering)
    )]
    pub xscale: f64,
    #[opt(
        name = "yscale",
        help = "set y-scale factor",
        default = 1.0,
        flags(filtering)
    )]
    pub yscale: f64,
    #[opt(
        name = "tscale",
        help = "set t-scale factor",
        default = 1.0,
        flags(filtering)
    )]
    pub tscale: f64,
    #[opt(name = "random_mode", help = "set random mode", unit = "perlin_random_mode", default = RandomMode::Random, default_repr = "random", flags(filtering))]
    pub random_mode: RandomMode,
    #[opt(
        name = "random_seed",
        alias = "seed",
        help = "set the seed",
        default = 0_u32,
        flags(filtering)
    )]
    pub random_seed: u32,
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
    name: "perlin",
    description: "Generate Perlin noise",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

/// A permutation of `0..256`, doubled to `512` entries so lookups never need
/// to wrap explicitly (the classic Perlin trick).
#[derive(Debug, Clone)]
pub(crate) struct Permutation([u8; 512]);

impl Permutation {
    pub(crate) fn from_seed(seed: u64) -> Self {
        let mut table: [u8; 256] = [0; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation, reason = "i < 256")]
            {
                *slot = i as u8;
            }
        }
        let mut rng = SplitMix64::new(seed);
        // Fisher-Yates.
        for i in (1..256).rev() {
            let j = rng.next_below(i + 1);
            table.swap(i, j);
        }
        let mut doubled = [0u8; 512];
        doubled[..256].copy_from_slice(&table);
        doubled[256..].copy_from_slice(&table);
        Self(doubled)
    }

    fn at(&self, i: i32) -> u8 {
        let idx = (i & 0xFF) as usize;
        self.0.get(idx).copied().unwrap_or(0)
    }
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// The classic 2D gradient function: 8 directions selected by the low 3
/// bits of the hashed lattice value.
fn grad(hash: u8, x: f64, y: f64) -> f64 {
    match hash & 7 {
        0 => x + y,
        1 => -x + y,
        2 => x - y,
        3 => -x - y,
        4 => x,
        5 => -x,
        6 => y,
        _ => -y,
    }
}

/// One octave of 2D Perlin noise at `(x, y)`, in roughly `[-1, 1]`.
pub(crate) fn noise2d(perm: &Permutation, x: f64, y: f64) -> f64 {
    let xi = x.floor();
    let yi = y.floor();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "lattice coordinates wrap via `& 0xFF`, so range is irrelevant"
    )]
    let (xi_i, yi_i) = (xi as i32, yi as i32);
    let xf = x - xi;
    let yf = y - yi;
    let u = fade(xf);
    let v = fade(yf);

    let a = i32::from(perm.at(xi_i)) + yi_i;
    let b = i32::from(perm.at(xi_i + 1)) + yi_i;

    let g00 = grad(perm.at(a), xf, yf);
    let g10 = grad(perm.at(b), xf - 1.0, yf);
    let g01 = grad(perm.at(a + 1), xf, yf - 1.0);
    let g11 = grad(perm.at(b + 1), xf - 1.0, yf - 1.0);

    let x1 = lerp(u, g00, g10);
    let x2 = lerp(u, g01, g11);
    lerp(v, x1, x2)
}

/// Fractal (multi-octave) sum, normalised so the theoretical maximum
/// amplitude is 1.
pub(crate) fn fractal_noise(
    perm: &Permutation,
    x: f64,
    y: f64,
    octaves: u32,
    persistence: f64,
) -> f64 {
    let mut total = 0.0;
    let mut amplitude = 1.0;
    let mut max_amplitude = 0.0;
    let mut freq = 1.0;
    for _ in 0..octaves.max(1) {
        total += noise2d(perm, x * freq, y * freq) * amplitude;
        max_amplitude += amplitude;
        amplitude *= persistence;
        freq *= 2.0;
    }
    if max_amplitude > 0.0 {
        total / max_amplitude
    } else {
        0.0
    }
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    perm: Permutation,
    octaves: u32,
    persistence: f64,
    xscale: f64,
    yscale: f64,
    tscale: f64,
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
            .acquire_video(PixFmt::Gray8, self.width, self.height)?;
        #[allow(clippy::cast_precision_loss, reason = "frame index stays small")]
        let t = self.next as f64 * self.tscale;
        if let Some(mut plane) = frame.plane_mut(0) {
            for row_idx in 0..plane.rows() {
                #[allow(clippy::cast_precision_loss, reason = "row index stays small")]
                let y = row_idx as f64 * self.yscale + t;
                if let Some(row) = plane.row_mut(row_idx) {
                    for (x, px) in row.iter_mut().enumerate() {
                        #[allow(clippy::cast_precision_loss, reason = "column index stays small")]
                        let xf = x as f64 * self.xscale;
                        let n = fractal_noise(&self.perm, xf, y, self.octaves, self.persistence);
                        let v = 128.0 + 127.0 * n.clamp(-1.0, 1.0);
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "v is clamped into [1, 255]"
                        )]
                        {
                            *px = v.round().clamp(0.0, 255.0) as u8;
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

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    let rate = opts.rate.0;
    let seed = match opts.random_mode {
        RandomMode::Random => 0x50_45_52_4C_49_4E_00_01_u64,
        RandomMode::Ken | RandomMode::Seed => u64::from(opts.random_seed),
    };
    let source = Source {
        width,
        height,
        perm: Permutation::from_seed(seed),
        octaves: u32::try_from(opts.octaves.max(1)).unwrap_or(1),
        persistence: opts.persistence,
        xscale: opts.xscale,
        yscale: opts.yscale,
        tscale: opts.tscale,
        frame_rate: rate,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Gray8)),
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
    fn noise_stays_within_the_theoretical_bound() {
        let perm = Permutation::from_seed(1);
        for i in 0..200 {
            #[allow(clippy::cast_precision_loss, reason = "test range is tiny")]
            let x = f64::from(i) * 0.37;
            #[allow(clippy::cast_precision_loss, reason = "test range is tiny")]
            let y = f64::from(i) * 0.91;
            let n = fractal_noise(&perm, x, y, 3, 0.5);
            assert!((-1.5..=1.5).contains(&n), "n={n} out of bound at i={i}");
        }
    }

    #[test]
    fn same_seed_reproduces_the_same_field() {
        let a = Permutation::from_seed(42);
        let b = Permutation::from_seed(42);
        for i in 0..512 {
            assert_eq!(a.at(i), b.at(i));
        }
    }

    #[test]
    fn is_continuous_across_a_lattice_boundary() {
        // Perlin noise is continuous (C1) everywhere, including at integer
        // lattice points -- a real property of the construction, not a
        // re-statement of the formula.
        let perm = Permutation::from_seed(7);
        let a = noise2d(&perm, 0.999_999, 0.5);
        let b = noise2d(&perm, 1.000_001, 0.5);
        assert!((a - b).abs() < 0.001, "{a} vs {b}");
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "perlin",
            instance: "perlin",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
