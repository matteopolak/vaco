//! `anoisesrc` — coloured noise, in `dbl`, mono.
//!
//! `ffmpeg -h filter=anoisesrc` documents `sample_rate`/`r`,
//! `amplitude`/`a` (default 1), `duration`/`d`, `color`/`colour`/`c` (0..5:
//! `white`, `pink`, `brown`, `blue`, `violet`, `velvet`), `seed`/`s`,
//! `nb_samples`/`n` and `density` (default 0.05, `velvet` noise's impulse
//! rate).
//!
//! # What is measured, and what is not
//!
//! Probing `color=white` at `amplitude=1` gives samples in `[-1, 1]` with
//! standard deviation `~0.578` — matching a **uniform** distribution's
//! `1/sqrt(3) = 0.577`, not a Gaussian's — so white noise here is
//! `amplitude * uniform(-1, 1)`. That is this crate's actual measured
//! finding; everything below it is a published noise-colouring technique
//! applied on top of this crate's own RNG (see [`crate::rng`]), which does
//! **not** reproduce the reference's bit stream. So: **`color=white`'s
//! distribution shape is measured and matched; no colour's specific sample
//! sequence is bit-exact**, since that would require the reference's exact
//! RNG.
//!
//! - `pink`: Paul Kellet's "economy" three-pole pink noise filter — a
//!   widely published set of one-pole coefficients approximating a -3dB/oct
//!   spectrum from white noise.
//! - `brown` (red): leaky integration of white noise (published; the
//!   textbook way to get a -6dB/oct spectrum), normalised to stay in range.
//! - `blue`: first-difference of white noise (+3dB/oct).
//! - `violet`: second-difference of white noise (+6dB/oct).
//! - `velvet`: sparse random-sign impulses at the requested `density`
//!   (impulses per sample) — the published Velvet Noise construction.
//!
//! **Algorithmically faithful for every colour; bit-exact for none** (RNG
//! divergence). See `docs/filter/vaco-filter-asource.md`.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptEnum, OptionsExt as _};
use vaco_sampfmt::SampleFmt;

use crate::rng::{SplitMix64, resolve_seed};
use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration::from_micros(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "anoisesrc_color", base = "int")]
pub enum NoiseColor {
    #[opt_const(name = "white", help = "white noise")]
    #[default]
    White,
    #[opt_const(name = "pink", help = "pink noise")]
    Pink,
    #[opt_const(name = "brown", help = "brown noise")]
    Brown,
    #[opt_const(name = "blue", help = "blue noise")]
    Blue,
    #[opt_const(name = "violet", help = "violet noise")]
    Violet,
    #[opt_const(name = "velvet", help = "velvet noise")]
    Velvet,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "anoisesrc", help = "generate a noise audio signal")]
pub(crate) struct Opts {
    #[opt(name = "sample_rate", alias = "r", help = "set sample rate", default = 48000, range = 15..=i32::MAX, flags(filtering))]
    pub sample_rate: i32,
    #[opt(name = "amplitude", alias = "a", help = "set amplitude", default = 1.0, range = 0.0..=1.0, flags(filtering))]
    pub amplitude: f64,
    #[opt(name = "duration", alias = "d", help = "set duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "color", alias = "colour", help = "set noise color", unit = "anoisesrc_color", default = NoiseColor::White, default_repr = "white", flags(filtering))]
    pub color: NoiseColor,
    #[opt(name = "seed", alias = "s", help = "set random seed", default = -1_i64, flags(filtering))]
    pub seed: i64,
    #[opt(name = "nb_samples", alias = "n", help = "set the number of samples per requested frame", default = 1024, range = 1..=i32::MAX, flags(filtering))]
    pub nb_samples: i32,
    #[opt(name = "density", help = "set density", default = 0.05, range = 0.0..=1.0, flags(filtering))]
    pub density: f64,
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
    name: "anoisesrc",
    description: "Generate a noise audio signal",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    flags: FilterFlags::empty(),
};

/// Per-colour filter state, carried across `produce` calls so the coloured
/// spectrum is continuous across frame boundaries.
#[derive(Debug, Default)]
struct ColorState {
    pink: [f64; 3],
    brown: f64,
    prev1: f64,
    prev2: f64,
}

fn next_sample(
    color: NoiseColor,
    amplitude: f64,
    density: f64,
    rng: &mut SplitMix64,
    state: &mut ColorState,
) -> f64 {
    let white = rng.next_bipolar();
    match color {
        NoiseColor::White => amplitude * white,
        NoiseColor::Pink => {
            // Paul Kellet's economy pink noise filter (published).
            state.pink[0] = 0.997 * state.pink[0] + 0.029_591_2 * white;
            state.pink[1] = 0.985 * state.pink[1] + 0.032_534_5 * white;
            state.pink[2] = 0.950 * state.pink[2] + 0.048_79 * white;
            let sum = state.pink[0] + state.pink[1] + state.pink[2] + white * 0.1848;
            (amplitude * sum / 2.0).clamp(-amplitude, amplitude)
        }
        NoiseColor::Brown => {
            state.brown = (state.brown + white * 0.02).clamp(-1.0, 1.0);
            amplitude * state.brown
        }
        NoiseColor::Blue => {
            let diff = white - state.prev1;
            state.prev1 = white;
            (amplitude * diff / 2.0).clamp(-amplitude, amplitude)
        }
        NoiseColor::Violet => {
            let diff1 = white - state.prev1;
            let diff2 = diff1 - state.prev2;
            state.prev2 = diff1;
            state.prev1 = white;
            (amplitude * diff2 / 4.0).clamp(-amplitude, amplitude)
        }
        NoiseColor::Velvet => {
            if rng.next_bipolar().abs() < density {
                if white >= 0.0 { amplitude } else { -amplitude }
            } else {
                0.0
            }
        }
    }
}

#[derive(Debug)]
struct Source {
    color: NoiseColor,
    amplitude: f64,
    density: f64,
    rng: SplitMix64,
    state: ColorState,
    sample_rate: u32,
    block: u32,
    total_samples: Option<u64>,
    produced: u64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                sample_rate,
                time_base,
                ..
            } = &mut out
            {
                *sample_rate = self.sample_rate;
                *time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_samples.is_some_and(|n| self.produced >= n) {
            return Ok(None);
        }
        let want = self.total_samples.map_or(self.block, |n| {
            u32::try_from(n - self.produced)
                .unwrap_or(self.block)
                .min(self.block)
        });
        let layout = vaco_chlayout::ChannelLayout::from_name("mono")
            .or_else(|| vaco_chlayout::ChannelLayout::default_for(1))
            .ok_or(vaco_core::Error::Unsupported(
                "no mono channel layout available",
            ))?;
        let mut frame = ctx
            .pool()
            .acquire_audio(SampleFmt::F64, layout, want, self.sample_rate)?;
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            for px in row.chunks_exact_mut(8) {
                let s = next_sample(
                    self.color,
                    self.amplitude,
                    self.density,
                    &mut self.rng,
                    &mut self.state,
                );
                px.copy_from_slice(&s.to_le_bytes());
            }
        }
        frame.pts = Timestamp::new(i64::try_from(self.produced).unwrap_or(0));
        frame.time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
        frame.set_duration_ticks(i64::from(want));
        self.produced = self.produced.saturating_add(u64::from(want));
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(i64::try_from(self.produced).unwrap_or(0))
    }

    fn flush_state(&mut self) {
        self.produced = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let sample_rate = u32::try_from(opts.sample_rate.max(15)).unwrap_or(48000);
    let total_samples = if opts.duration.as_micros() <= 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * f64::from(sample_rate))
                .round()
                .max(0.0) as u64,
        )
    };
    let seed = resolve_seed(opts.seed, 0x00A0_157E);
    let source = Source {
        color: opts.color,
        amplitude: opts.amplitude,
        density: opts.density,
        rng: SplitMix64::new(seed),
        state: ColorState::default(),
        sample_rate,
        block: u32::try_from(opts.nb_samples.max(1)).unwrap_or(1024),
        total_samples,
        produced: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::default()],
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
    fn white_noise_stays_within_amplitude() {
        let mut rng = SplitMix64::new(1);
        let mut state = ColorState::default();
        for _ in 0..10_000 {
            let s = next_sample(NoiseColor::White, 0.5, 0.05, &mut rng, &mut state);
            assert!((-0.5..=0.5).contains(&s), "{s}");
        }
    }

    #[test]
    fn every_color_stays_within_amplitude() {
        for color in [
            NoiseColor::White,
            NoiseColor::Pink,
            NoiseColor::Brown,
            NoiseColor::Blue,
            NoiseColor::Violet,
            NoiseColor::Velvet,
        ] {
            let mut rng = SplitMix64::new(7);
            let mut state = ColorState::default();
            for _ in 0..10_000 {
                let s = next_sample(color, 1.0, 0.05, &mut rng, &mut state);
                assert!((-1.0..=1.0).contains(&s), "{color:?}: {s}");
            }
        }
    }

    #[test]
    fn same_seed_reproduces_the_same_sequence() {
        let mut rng_a = SplitMix64::new(42);
        let mut rng_b = SplitMix64::new(42);
        let mut state_a = ColorState::default();
        let mut state_b = ColorState::default();
        for _ in 0..100 {
            let a = next_sample(NoiseColor::Pink, 1.0, 0.05, &mut rng_a, &mut state_a);
            let b = next_sample(NoiseColor::Pink, 1.0, 0.05, &mut rng_b, &mut state_b);
            assert!((a - b).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "anoisesrc",
            instance: "anoisesrc",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
