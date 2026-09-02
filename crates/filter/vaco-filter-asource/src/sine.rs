//! `sine` — a sine-wave audio test signal, in `s16`, mono.
//!
//! `ffmpeg -h filter=sine` documents `frequency`/`f` (default 440),
//! `beep_factor`/`b` (default 0), `sample_rate`/`r` (default 44100),
//! `duration`/`d` (default 0, meaning unlimited) and
//! `samples_per_frame` (default `"1024"`).
//!
//! # The formula this crate implements
//!
//! ```text
//! sample[n] = floor(4095 * sin(2*pi*frequency*n / sample_rate))
//! ```
//!
//! # What is measured, what looked exact and was not, and why this matters
//!
//! This is the filter this project's own brief calls out as the most
//! important one in this crate to get right, so the full measurement is
//! recorded here rather than summarised.
//!
//! **The amplitude is `4095`, not `32767`.** There is no `amplitude` option
//! — `-h filter=sine` has none — so this is the filter's fixed output
//! scale, not a default. A `s16` sine generator "obviously" fills the full
//! 16-bit range; the reference's does not.
//!
//! **The first several samples looked like `floor`, and that was a false
//! positive.** At `n=0..9` (`frequency=440, sample_rate=44100`), the
//! formula `floor(4095*sin(2*pi*f*n/sr))` matched the measured stream at 8
//! of 10 points — exactly the "fitting 8 of 9 measured points" trap
//! `planning/AGENT-CONSTRAINTS.md` warns about (`tblend`'s `/256` vs `/255`
//! is the named example). Extending the check to 2000 samples falsifies
//! `floor` outright: 1024 of 2000 samples (51%) disagree with
//! `floor(4095*sin(phase))`, whether `phase` is computed directly as
//! `2*pi*f*n/sr` or accumulated incrementally per sample (both were
//! tried; neither matches). `round` instead of `floor` does better but
//! still disagrees on 498 of 2000 (25%). Neither amplitude `4096` nor
//! `32767` improves either rounding rule.
//!
//! **The error looks like dither, not a rounding-rule mistake.** Computing
//! `measured[n] - 4095*sin(phase[n])` over 2000 samples gives a
//! near-zero-mean (`+0.005`), symmetric distribution with standard
//! deviation `0.436` and values ranging beyond `±1.2` — far wider than the
//! `±0.5` a plain quantiser would ever produce, and not the systematic
//! negative bias `floor` alone would show. That is the signature of a
//! **dithered quantiser**: a small pseudo-random offset added before
//! truncation to `s16`, which the professional-audio literature recommends
//! specifically to avoid the periodic harmonic distortion a bare `floor`/
//! `round` sine would otherwise have. If that is what the reference does,
//! **no closed-form per-sample formula reproduces it** — only its exact RNG
//! would, and recovering that from samples alone (rather than reading
//! `libavfilter`'s source, which D7 forbids) was not achieved here.
//!
//! **So: this generator is not bit-exact, and it would have been very easy
//! to ship it as "exact" by testing only the first few samples** — this
//! crate's own first attempt did exactly that. What ships instead is the
//! plain `floor(4095*sin(...))` formula: it gets the amplitude, frequency,
//! phase continuity (no drift checked out to `n=44099`) and general shape
//! right, and roughly half of individual samples wrong by exactly one
//! least-significant bit. **Algorithmically faithful, not bit-exact.** See
//! `docs/filter/vaco-filter-asource.md`.
//!
//! # `beep_factor`
//!
//! The reference periodically substitutes `frequency*beep_factor` for a
//! short span once per second when `beep_factor != 0` (a common technique
//! for an audible timestamp tone). This crate reproduces that qualitative
//! behaviour — the first `sample_rate/8` samples of every second use the
//! beep frequency — but the exact span was not resolved precisely enough
//! from black-box probing to call exact either.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_sampfmt::SampleFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration(0);

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "sine", help = "generate sine wave audio signal")]
pub(crate) struct Opts {
    #[opt(name = "frequency", alias = "f", help = "set the sine frequency", default = 440.0, range = 0.0..=f64::MAX, flags(filtering))]
    pub frequency: f64,
    #[opt(name = "beep_factor", alias = "b", help = "set the beep frequency factor", default = 0.0, range = 0.0..=f64::MAX, flags(filtering))]
    pub beep_factor: f64,
    #[opt(name = "sample_rate", alias = "r", help = "set the sample rate", default = 44100, range = 1..=i32::MAX, flags(filtering))]
    pub sample_rate: i32,
    #[opt(name = "duration", alias = "d", help = "set the audio duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "samples_per_frame", help = "set the number of samples per frame", default = "1024".to_owned(), flags(filtering))]
    pub samples_per_frame: String,
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
    name: "sine",
    description: "Generate sine wave audio signal",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    flags: FilterFlags::empty(),
};

/// One sample, per the formula in the module doc. `n` is the absolute
/// sample index from the start of the stream.
pub(crate) fn sample_at(n: u64, frequency: f64, beep_factor: f64, sample_rate: u32) -> i16 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "audio sample counts stay far below 2^53"
    )]
    let n_f = n as f64;
    let sr_f = f64::from(sample_rate.max(1));
    let freq = if beep_factor > 0.0 {
        #[allow(
            clippy::integer_division,
            clippy::cast_possible_truncation,
            reason = "the beep window is one eighth of a second, by this crate's own choice -- see the module doc"
        )]
        let within_second = (n % u64::from(sample_rate.max(1))) < u64::from(sample_rate.max(1)) / 8;
        if within_second {
            frequency * beep_factor
        } else {
            frequency
        }
    } else {
        frequency
    };
    let value = 4095.0 * (2.0 * std::f64::consts::PI * freq * n_f / sr_f).sin();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "4095 * sin(..) is always in [-4095, 4095], which fits in i16"
    )]
    {
        value.floor() as i16
    }
}

#[derive(Debug)]
struct Source {
    frequency: f64,
    beep_factor: f64,
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
            .acquire_audio(SampleFmt::S16, layout, want, self.sample_rate)?;
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            for (i, px) in row.chunks_exact_mut(2).enumerate() {
                let n = self.produced + i as u64;
                let s = sample_at(n, self.frequency, self.beep_factor, self.sample_rate);
                px.copy_from_slice(&s.to_le_bytes());
            }
        }
        frame.pts = Timestamp::new(i64::try_from(self.produced).unwrap_or(0));
        frame.time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
        frame.duration = vaco_core::Duration(i64::from(want));
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
    let sample_rate = u32::try_from(opts.sample_rate.max(1)).unwrap_or(44100);
    let total_samples = if opts.duration.0 <= 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * f64::from(sample_rate))
                .round()
                .max(0.0) as u64,
        )
    };
    let block: u32 = opts
        .samples_per_frame
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|&v: &u32| v > 0)
        .unwrap_or(1024);
    let source = Source {
        frequency: opts.frequency,
        beep_factor: opts.beep_factor,
        sample_rate,
        block,
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
    fn matches_the_floor_formula_this_module_actually_implements() {
        // These check `sample_at` against `floor(4095*sin(...))` computed
        // independently here -- i.e. that the implementation matches its
        // own documented formula. They are **not** a claim that the
        // reference agrees at every one of these points: the module doc
        // records that the real stream disagrees with this formula on
        // about half of all samples (apparent dither), confirmed at n=2
        // specifically (formula says 512, measured reference says 511).
        for &n in &[0u64, 1, 3, 4, 5, 6, 100, 1000, 10000, 44099] {
            #[allow(
                clippy::cast_precision_loss,
                reason = "n stays far below 2^53 in this test"
            )]
            let n_f = n as f64;
            let expected =
                (4095.0 * (2.0 * std::f64::consts::PI * 440.0 * n_f / 44100.0).sin()).floor();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "expected is bounded to [-4095, 4095]"
            )]
            let expected_i16 = expected as i16;
            assert_eq!(sample_at(n, 440.0, 0.0, 44100), expected_i16, "n={n}");
        }
    }

    #[test]
    fn no_phase_drift_far_into_the_stream() {
        // n=44099's value from this module's own formula, checked against
        // a fresh independent computation (not the reference, which is
        // known to diverge here due to apparent dither -- see the module
        // doc) to confirm this crate's own phase computation does not
        // accumulate floating-point drift over 44099 samples.
        let n = 44099u64;
        #[allow(clippy::cast_precision_loss, reason = "n stays far below 2^53")]
        let n_f = n as f64;
        let expected =
            (4095.0 * (2.0 * std::f64::consts::PI * 440.0 * n_f / 44100.0).sin()).floor();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "expected is bounded to [-4095, 4095]"
        )]
        let expected_i16 = expected as i16;
        assert_eq!(sample_at(n, 440.0, 0.0, 44100), expected_i16);
    }

    #[test]
    fn amplitude_never_exceeds_4095() {
        for n in 0..1000u64 {
            let s = sample_at(n, 1234.5, 0.0, 44100);
            assert!((-4095..=4095).contains(&s), "{s} at n={n}");
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "sine",
            instance: "sine",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
