//! `afdelaysrc` — a sinc fractional-delay FIR, in `flt`. A one-shot
//! coefficient dump, replicated across every channel of `channel_layout`
//! (default `stereo`).
//!
//! `ffmpeg -h filter=afdelaysrc` documents `delay`/`d` (default 0),
//! `sample_rate`/`r`, `nb_samples`/`n`, `taps`/`t` (default 0, meaning
//! "auto") and `channel_layout`/`c` (default `"stereo"`).
//!
//! # What is measured, and what is not
//!
//! Probing `delay=2.5` (`ffmpeg -f lavfi -i afdelaysrc=delay=2.5 -f f32le
//! -`) with `taps=0` gives 21 taps peaked equally at index 2 and 3 —
//! exactly where a `0.5`-fractional delay's sinc peak should straddle. The
//! near-peak values match a **plain, unwindowed** `sinc(n - delay)` to
//! within 0.2% (e.g. index 2/3: sinc predicts `0.6366`, measured is
//! `0.6354`); an attempt at a Blackman-windowed version *centred on the
//! whole array* was checked against these same numbers first and was
//! wrong by more than an order of magnitude at the very indices that
//! should be near the peak (`~0.03` predicted vs `0.64` measured) — a
//! concrete instance of the "second implementation must be wrong
//! differently" trap this project's `AGENT-CONSTRAINTS.md` warns about:
//! that windowed version *looked* principled and was falsified by the
//! very data it was supposed to explain.
//!
//! Further from the peak, the measured taper decays a little faster than
//! plain `sinc` does (e.g. index 8: sinc predicts `-0.058`, measured is
//! `-0.044`, a ratio of `0.76` vs `~1.0` near the peak) — so there is some
//! real windowing, just not the naive centred-Blackman guess, and this
//! crate did not recover its exact shape from black-box probing in the
//! time available. This module uses **plain, unwindowed `sinc(n - delay)`**
//! rather than ship a second wrong guess: **exact near the peak, increasingly
//! approximate away from it**, and this crate's own auto-`taps` heuristic
//! (`2*(ceil(delay)+10)+1`) rather than the reference's (measured: `delay=0`
//! gives exactly 1 tap, `delay=2.5` gives 21 — no single simple formula
//! from two points was attempted). See
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

/// Tap `n` of an `n_taps`-long fractional-delay FIR targeting `delay`.
/// Plain, unwindowed sinc -- see the module doc for why. `n_taps` is kept
/// in the signature (unused) so a future windowed version is a one-line
/// change here rather than a signature change at every call site.
pub(crate) fn tap(n: usize, _n_taps: usize, delay: f64) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "n is a small tap index")]
    let n_f = n as f64;
    sinc(n_f - delay)
}

fn auto_taps(delay: f64) -> usize {
    let margin = delay.ceil().max(0.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "margin is a small, non-negative delay-derived count"
    )]
    let margin_i = margin as usize;
    2 * (margin_i + 10) + 1
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "afdelaysrc", help = "generate a Fractional delay FIR")]
pub(crate) struct Opts {
    #[opt(name = "delay", alias = "d", help = "set fractional delay", default = 0.0, range = 0.0..=32767.0, flags(filtering))]
    pub delay: f64,
    #[opt(name = "sample_rate", alias = "r", help = "set sample rate", default = 44100, range = 1..=i32::MAX, flags(filtering))]
    pub sample_rate: i32,
    #[opt(name = "nb_samples", alias = "n", help = "set the number of samples per requested frame", default = 1024, range = 1..=i32::MAX, flags(filtering))]
    pub nb_samples: i32,
    #[opt(name = "taps", alias = "t", help = "set number of taps for delay filter", default = 0, range = 0..=32768, flags(filtering))]
    pub taps: i32,
    #[opt(name = "channel_layout", alias = "c", help = "set channel layout", default = "stereo".to_owned(), flags(filtering))]
    pub channel_layout: String,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

pub const DESC: FilterDesc = FilterDesc {
    name: "afdelaysrc",
    description: "Generate a Fractional delay FIR coefficients",
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
    delay: f64,
    layout: vaco_chlayout::ChannelLayout,
    sample_rate: u32,
    block: u32,
    next: u64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                sample_rate,
                layout,
                time_base,
                ..
            } = &mut out
            {
                *sample_rate = self.sample_rate;
                *layout = self.layout.clone();
                *time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        #[allow(clippy::cast_possible_truncation, reason = "n_taps <= 32768 * 21 + 1, well within u64")]
        let total = self.n_taps as u64;
        if self.next >= total {
            return Ok(None);
        }
        let want = u32::try_from(total - self.next).unwrap_or(self.block).min(self.block);
        let mut frame = ctx.pool().acquire_audio(
            SampleFmt::F32,
            self.layout.clone(),
            want,
            self.sample_rate,
        )?;
        for ch in 0..frame.plane_count() {
            if let Some(mut plane) = frame.plane_mut(ch)
                && let Some(row) = plane.row_mut(0)
            {
                for (i, px) in row.chunks_exact_mut(4).enumerate() {
                    #[allow(clippy::cast_possible_truncation, reason = "index stays within n_taps")]
                    let idx = (self.next as usize) + i;
                    #[allow(clippy::cast_possible_truncation, reason = "tap() is a small finite value")]
                    let v = tap(idx, self.n_taps, self.delay) as f32;
                    px.copy_from_slice(&v.to_le_bytes());
                }
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
    let n_taps = if opts.taps > 0 {
        usize::try_from(opts.taps).unwrap_or(1)
    } else {
        auto_taps(opts.delay)
    };
    let layout = vaco_chlayout::ChannelLayout::from_name(&opts.channel_layout)
        .ok_or_else(|| format!("afdelaysrc: bad channel_layout `{}`", opts.channel_layout))?;
    let source = Source {
        n_taps,
        delay: opts.delay,
        layout,
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
    fn zero_delay_peaks_at_the_first_tap() {
        assert!((tap(0, 21, 0.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn half_integer_delay_peaks_symmetrically_between_two_taps() {
        // delay=2.5: the sinc peak straddles indices 2 and 3 equally --
        // matching the module doc's measured reference shape.
        let a = tap(2, 21, 2.5);
        let b = tap(3, 21, 2.5);
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        assert!(a > tap(1, 21, 2.5));
        assert!(a > tap(4, 21, 2.5));
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "afdelaysrc",
            instance: "afdelaysrc",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
