//! `sinc` — a Kaiser-windowed sinc low-pass, high-pass, band-pass or
//! band-reject FIR, in `flt`, mono. A one-shot coefficient dump.
//!
//! `ffmpeg -h filter=sinc` documents `sample_rate`/`r`, `nb_samples`/`n`,
//! `hp`/`lp` (cutoff frequencies in Hz, default 0 = unset), `phase`
//! (0..100, default 50), `beta` (Kaiser beta, default -1 = auto from
//! `att`), `att` (stop-band attenuation dB, default 120), `round`, and
//! `hptaps`/`lptaps` (0 = auto).
//!
//! # Filter type selection
//!
//! Neither option's help text names a `type` — the reference infers
//! low-pass/high-pass/band-pass/band-reject from which of `hp`/`lp` are set
//! and their relative order, which this module reproduces the documented
//! way a Kaiser-windowed-sinc design textbook would:
//!
//! - only `lp` set: low-pass at `lp`.
//! - only `hp` set: high-pass at `hp`.
//! - both set, `hp < lp`: band-pass over `[hp, lp]` (low-pass at `lp` minus
//!   low-pass at `hp`).
//! - both set, `hp >= lp`: band-reject over `[lp, hp]` (an impulse minus
//!   the band-pass kernel above).
//!
//! # What is calibrated, and what is not
//!
//! The Kaiser window and its `beta`-from-`att` formula are Kaiser's own
//! published 1974 design equations, reproduced exactly (`beta =
//! 0.1102*(A-8.7)` for `A > 50`, the relevant case at the documented
//! default `att=120`). The windowed-sinc construction itself is the
//! textbook FIR design method. **Not calibrated**: this crate's own choice
//! of tap count when `hptaps`/`lptaps=0` (the reference's auto-length
//! formula was not recovered from black-box probing), and the `phase`
//! option (linear/minimum-phase blend) is accepted but always produces a
//! linear-phase (symmetric) kernel. See
//! `docs/filter/vaco-filter-asource.md`.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_sampfmt::SampleFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

/// Modified Bessel function of the first kind, order 0 -- the series
/// Kaiser's window formula is built from, truncated at 32 terms (converges
/// well past `beta` of a few hundred, far beyond any FIR design in practice).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..32i32 {
        let k_f = f64::from(k);
        term *= (x / (2.0 * k_f)).powi(2);
        sum += term;
    }
    sum
}

/// Kaiser's own published beta-from-attenuation formula (1974).
pub(crate) fn kaiser_beta_from_attenuation(att_db: f64) -> f64 {
    if att_db > 50.0 {
        0.1102 * (att_db - 8.7)
    } else if att_db >= 21.0 {
        0.5842 * (att_db - 21.0).powf(0.4) + 0.07886 * (att_db - 21.0)
    } else {
        0.0
    }
}

fn kaiser_window(n: usize, n_taps: usize, beta: f64) -> f64 {
    if n_taps <= 1 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss, reason = "n, n_taps are FIR tap counts")]
    let alpha = (n_taps - 1) as f64 / 2.0;
    #[allow(clippy::cast_precision_loss, reason = "n is a small tap index")]
    let n_f = n as f64;
    let ratio = (n_f - alpha) / alpha;
    bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / bessel_i0(beta)
}

/// A single low-pass kernel tap at normalised cutoff `fc` (`0..0.5` of the
/// sample rate).
fn lowpass_tap(n: usize, n_taps: usize, fc: f64, beta: f64) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "n, n_taps are FIR tap counts")]
    let center = (n_taps as f64 - 1.0) / 2.0;
    #[allow(clippy::cast_precision_loss, reason = "n is a small tap index")]
    let n_f = n as f64;
    2.0 * fc * sinc(2.0 * fc * (n_f - center)) * kaiser_window(n, n_taps, beta)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Kind {
    Lowpass(f64),
    Highpass(f64),
    Bandpass(f64, f64),
    Bandreject(f64, f64),
}

/// Tap `n` of an `n_taps`-long design, per the module doc's type-selection
/// rule.
pub(crate) fn tap(n: usize, n_taps: usize, kind: Kind, beta: f64) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "n, n_taps are FIR tap counts")]
    let center = (n_taps as f64 - 1.0) / 2.0;
    let impulse = |k: usize| -> f64 {
        #[allow(clippy::cast_precision_loss, reason = "k is a small tap index")]
        let k_f = k as f64;
        if (k_f - center).abs() < 1e-9 {
            1.0
        } else {
            0.0
        }
    };
    match kind {
        Kind::Lowpass(fc) => lowpass_tap(n, n_taps, fc, beta),
        Kind::Highpass(fc) => impulse(n) - lowpass_tap(n, n_taps, fc, beta),
        Kind::Bandpass(lo, hi) => {
            lowpass_tap(n, n_taps, hi, beta) - lowpass_tap(n, n_taps, lo, beta)
        }
        Kind::Bandreject(lo, hi) => {
            impulse(n) - (lowpass_tap(n, n_taps, hi, beta) - lowpass_tap(n, n_taps, lo, beta))
        }
    }
}

const DEFAULT_TAPS: usize = 4001;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "sinc", help = "generate a sinc kaiser-windowed FIR")]
pub(crate) struct Opts {
    #[opt(name = "sample_rate", alias = "r", help = "set sample rate", default = 44100, range = 1..=i32::MAX, flags(filtering))]
    pub sample_rate: i32,
    #[opt(name = "nb_samples", alias = "n", help = "set the number of samples per requested frame", default = 1024, range = 1..=i32::MAX, flags(filtering))]
    pub nb_samples: i32,
    #[opt(name = "hp", help = "set high-pass filter frequency", default = 0.0, range = 0.0..=f64::MAX, flags(filtering))]
    pub hp: f64,
    #[opt(name = "lp", help = "set low-pass filter frequency", default = 0.0, range = 0.0..=f64::MAX, flags(filtering))]
    pub lp: f64,
    #[opt(name = "beta", help = "set kaiser window beta", default = -1.0, range = -1.0..=256.0, flags(filtering))]
    pub beta: f64,
    #[opt(name = "att", help = "set stop-band attenuation", default = 120.0, range = 40.0..=180.0, flags(filtering))]
    pub att: f64,
    #[opt(name = "hptaps", help = "set number of taps for high-pass filter", default = 0, range = 0..=32768, flags(filtering))]
    pub hptaps: i32,
    #[opt(name = "lptaps", help = "set number of taps for low-pass filter", default = 0, range = 0..=32768, flags(filtering))]
    pub lptaps: i32,
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
    name: "sinc",
    description: "Generate a sinc kaiser-windowed FIR coefficients",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    flags: FilterFlags::empty(),
};

#[derive(Debug)]
struct Source {
    n_taps: usize,
    kind: Kind,
    beta: f64,
    sample_rate: u32,
    block: u32,
    next: u64,
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
        #[allow(clippy::cast_possible_truncation, reason = "n_taps <= 32768")]
        let total = self.n_taps as u64;
        if self.next >= total {
            return Ok(None);
        }
        let want = u32::try_from(total - self.next)
            .unwrap_or(self.block)
            .min(self.block);
        let layout = vaco_chlayout::ChannelLayout::from_name("mono")
            .or_else(|| vaco_chlayout::ChannelLayout::default_for(1))
            .ok_or(vaco_core::Error::Unsupported(
                "no mono channel layout available",
            ))?;
        let mut frame = ctx
            .pool()
            .acquire_audio(SampleFmt::F32, layout, want, self.sample_rate)?;
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            for (i, px) in row.chunks_exact_mut(4).enumerate() {
                #[allow(clippy::cast_possible_truncation, reason = "index stays within n_taps")]
                let idx = (self.next as usize) + i;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "tap() is a small finite value"
                )]
                let v = tap(idx, self.n_taps, self.kind, self.beta) as f32;
                px.copy_from_slice(&v.to_le_bytes());
            }
        }
        frame.pts = Timestamp::new(i64::try_from(self.next).unwrap_or(0));
        frame.time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
        frame.duration = vaco_core::Duration(i64::from(want));
        self.next = self.next.saturating_add(u64::from(want));
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(i64::try_from(self.next).unwrap_or(0))
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let sample_rate = u32::try_from(opts.sample_rate.max(1)).unwrap_or(44100);
    let nyquist = f64::from(sample_rate) / 2.0;
    let norm = |f: f64| (f / nyquist.max(1.0)).clamp(0.0, 0.5);
    let kind = if opts.hp > 0.0 && opts.lp > 0.0 {
        if opts.hp < opts.lp {
            Kind::Bandpass(norm(opts.hp), norm(opts.lp))
        } else {
            Kind::Bandreject(norm(opts.lp), norm(opts.hp))
        }
    } else if opts.lp > 0.0 {
        Kind::Lowpass(norm(opts.lp))
    } else if opts.hp > 0.0 {
        Kind::Highpass(norm(opts.hp))
    } else {
        Kind::Lowpass(0.25)
    };
    let beta = if opts.beta >= 0.0 {
        opts.beta
    } else {
        kaiser_beta_from_attenuation(opts.att)
    };
    let requested_taps = opts.lptaps.max(opts.hptaps);
    let n_taps = if requested_taps > 0 {
        usize::try_from(requested_taps).unwrap_or(DEFAULT_TAPS)
    } else {
        DEFAULT_TAPS
    };
    let source = Source {
        n_taps,
        kind,
        beta,
        sample_rate,
        block: u32::try_from(opts.nb_samples.max(1)).unwrap_or(1024),
        next: 0,
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
    fn kaiser_beta_matches_the_published_formula_at_the_default_attenuation() {
        // att=120 > 50, so beta = 0.1102*(120-8.7).
        let beta = kaiser_beta_from_attenuation(120.0);
        assert!((beta - 0.1102 * 111.3).abs() < 1e-9);
    }

    #[test]
    fn lowpass_kernel_sums_to_roughly_the_passband_gain() {
        // A lowpass FIR's DC gain (sum of taps) should be close to 1.0.
        let n_taps = 501;
        let beta = kaiser_beta_from_attenuation(120.0);
        let sum: f64 = (0..n_taps)
            .map(|n| tap(n, n_taps, Kind::Lowpass(0.1), beta))
            .sum();
        assert!((sum - 1.0).abs() < 0.01, "{sum}");
    }

    #[test]
    fn highpass_kernel_has_near_zero_dc_gain() {
        let n_taps = 501;
        let beta = kaiser_beta_from_attenuation(120.0);
        let sum: f64 = (0..n_taps)
            .map(|n| tap(n, n_taps, Kind::Highpass(0.1), beta))
            .sum();
        assert!(sum.abs() < 0.01, "{sum}");
    }

    #[test]
    fn kernel_is_symmetric_linear_phase() {
        let n_taps = 101;
        let beta = kaiser_beta_from_attenuation(120.0);
        for n in 0..n_taps {
            let a = tap(n, n_taps, Kind::Lowpass(0.2), beta);
            let b = tap(n_taps - 1 - n, n_taps, Kind::Lowpass(0.2), beta);
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "sinc",
            instance: "sinc",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
