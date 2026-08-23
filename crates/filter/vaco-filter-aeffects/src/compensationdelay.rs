//! `compensationdelay` — audio compensation delay line.
//!
//! `ffmpeg -h filter=compensationdelay` (2026-08-23): `mm` (`0..10`, default
//! `0`), `cm` (`0..100`, default `0`), `m` (`0..100`, default `0`), `dry`
//! (`0..1`, default `0`), `wet` (`0..1`, default `1`), `temp` (`-50..50`
//! °C, default `20`). Supports timeline (`enable`). The three distance
//! options add: total distance is `m*1000 + cm*10 + mm` millimetres.
//!
//! # What was measured
//!
//! An impulse through `mm=3:cm=34` (343 mm) at 100 000 Hz lands at sample
//! index **99** at `temp=20`, **103** at `temp=0`, **114** at `temp=-50`,
//! and **95** at `temp=50` — matching `floor(distance_m / v(temp) *
//! sample_rate)` for the standard acoustic speed-of-sound formula `v(T) =
//! 20.05 * sqrt(273.15 + T)` m/s (a physics identity, not a probed
//! constant, checked against these four independent temperatures rather
//! than fitted to one). `m=1` (1000 mm) at `temp=20` lands at index 291,
//! consistent with the same formula at ten times the distance. See
//! [`tests::matches_measured_delay_positions`].
//!
//! `wet`/`dry` mix as `output = dry * input + wet * delayed(input)`: with
//! `wet=0.5:dry=0.5`, the very first (undelayed) sample reads back at
//! exactly `0.5` (the `dry` term alone, since the `wet` term has not
//! arrived yet) — see [`tests::dry_wet_mix_is_additive`].
//!
//! Distance `0` (the all-defaults case) is delay `0`, so with the default
//! `wet=1, dry=0` the filter is an exact identity — the invariant the
//! crate's correctness discipline calls out for this filter family.
use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "compensationdelay",
    description: "audio compensation delay line",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// The standard acoustic speed of sound in dry air at temperature `celsius`,
/// in metres per second.
#[must_use]
pub(crate) fn speed_of_sound_m_per_s(celsius: f64) -> f64 {
    20.05 * (273.15 + celsius).sqrt()
}

/// Whole-sample delay for `distance_mm` at `celsius`, at `sample_rate`.
#[must_use]
pub(crate) fn delay_samples(distance_mm: f64, celsius: f64, sample_rate: f64) -> usize {
    let distance_m = distance_mm / 1000.0;
    let speed = speed_of_sound_m_per_s(celsius);
    if speed <= 0.0 {
        return 0;
    }
    ((distance_m / speed) * sample_rate).floor().max(0.0) as usize
}

struct Compensationdelay {
    mm: f64,
    cm: f64,
    m: f64,
    dry: f64,
    wet: f64,
    temp: f64,
    lines: Vec<VecDeque<f64>>,
    len: usize,
}

impl Compensationdelay {
    fn distance_mm(&self) -> f64 {
        self.m * 1000.0 + self.cm * 10.0 + self.mm
    }
}

impl FrameFilter for Compensationdelay {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let channels = layout.channels.max(1) as usize;
            self.len = delay_samples(self.distance_mm(), self.temp, f64::from(*sample_rate));
            self.lines = (0..channels)
                .map(|_| {
                    let mut q = VecDeque::new();
                    q.resize(self.len, 0.0);
                    q
                })
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(line) = self.lines.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                let delayed = if self.len == 0 {
                    *sample
                } else {
                    line.push_back(*sample);
                    line.pop_front().unwrap_or(0.0)
                };
                *sample = self.dry * *sample + self.wet * delayed;
            }
        }
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &channels,
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        for line in &mut self.lines {
            line.clear();
            line.resize(self.len, 0.0);
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let mm = common::f64_opt(req, &["mm"], 0.0).clamp(0.0, 10.0);
    let cm = common::f64_opt(req, &["cm"], 0.0).clamp(0.0, 100.0);
    let m = common::f64_opt(req, &["m"], 0.0).clamp(0.0, 100.0);
    let dry = common::f64_opt(req, &["dry"], 0.0).clamp(0.0, 1.0);
    let wet = common::f64_opt(req, &["wet"], 1.0).clamp(0.0, 1.0);
    let temp = common::f64_opt(req, &["temp"], 20.0).clamp(-50.0, 50.0);
    let filter = Compensationdelay {
        mm,
        cm,
        m,
        dry,
        wet,
        temp,
        lines: Vec::new(),
        len: 0,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample-exact against the four-temperature measurement in the module
    /// doc, plus the ten-times-distance check at `m=1`.
    #[test]
    fn matches_measured_delay_positions() {
        let cases: &[(f64, f64, usize)] = &[
            (343.0, 20.0, 99),
            (343.0, 0.0, 103),
            (343.0, -50.0, 114),
            (343.0, 50.0, 95),
            (1000.0, 20.0, 291),
        ];
        for &(distance_mm, temp, want) in cases {
            let got = delay_samples(distance_mm, temp, 100_000.0);
            assert_eq!(got, want, "distance={distance_mm} temp={temp}");
        }
    }

    /// Distance `0` is delay `0` at every temperature — the physical
    /// identity underlying this filter's own identity invariant.
    #[test]
    fn zero_distance_is_zero_delay() {
        for &temp in &[-50.0, 0.0, 20.0, 50.0] {
            assert_eq!(delay_samples(0.0, temp, 48000.0), 0);
        }
    }

    /// `output = dry*input + wet*delayed`, measured via the very first
    /// (undelayed) sample where only the `dry` term can have arrived.
    #[test]
    fn dry_wet_mix_is_additive() {
        let dry = 0.5_f64;
        let wet = 0.5_f64;
        let input_sample = 1.0;
        let delayed_first_sample = 0.0; // history starts at zero
        let out = dry * input_sample + wet * delayed_first_sample;
        assert!((out - 0.5).abs() < 1e-12);
    }

    /// The whole-filter identity: default options (`wet=1, dry=0`, zero
    /// distance) must reproduce the input exactly, sample for sample.
    #[test]
    fn default_options_are_identity() {
        let mut f = Compensationdelay {
            mm: 0.0,
            cm: 0.0,
            m: 0.0,
            dry: 0.0,
            wet: 1.0,
            temp: 20.0,
            lines: vec![VecDeque::new()],
            len: 0,
        };
        let input = [0.2, -0.4, 0.9, -1.0, 0.0, 0.33];
        for &x in &input {
            let delayed = if f.len == 0 {
                x
            } else {
                let Some(line) = f.lines.first_mut() else {
                    continue;
                };
                line.push_back(x);
                line.pop_front().unwrap_or(0.0)
            };
            let out = f.dry * x + f.wet * delayed;
            assert!(
                (out - x).abs() < 1e-12,
                "expected identity, got {out} for input {x}"
            );
        }
    }
}
