//! `superequalizer` — an 18-band graphic equalizer.
//!
//! `ffmpeg -h filter=superequalizer` (2026-08-23) lists exactly `1b`
//! through `18b`, one **linear** gain multiplier per band (range 0 to 20,
//! default 1 — not a dB value, unlike every other filter in this crate).
//! The band centre frequencies (65, 92, 131, 185, 262, 370, 523, 740, 1047,
//! 1480, 2093, 2960, 4186, 5920, 8372, 11840, 16744, 20000 Hz) come straight
//! from each option's help text.
//!
//! The reference implements this with an FFT-domain filter bank; this crate
//! builds it from eighteen cascaded [`vaco_filter_adsp::biquad::peaking`] sections
//! instead — a structural approximation, not a claim of matching the
//! reference's magnitude response band-for-band. `gain_db = 20*log10(gain)`
//! converts the linear knob so that the documented default (`1` on every
//! band) is exactly the cookbook's 0 dB, i.e. identity — verified below. `Q`
//! is fixed at the ratio between adjacent centre frequencies is roughly
//! constant (~1.41x, half an octave); `Q = 1/(r - 1/r)` for that ratio,
//! measured for this crate rather than probed from the reference, is what
//! keeps neighbouring bands from over- or under-lapping.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use vaco_filter_adsp::biquad::{self as biquad, Coeffs, State, WidthType};

pub const DESC: FilterDesc = FilterDesc {
    name: "superequalizer",
    description: "apply 18 band equalization filter",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// Band centre frequencies, in ascending order, from the reference's own
/// option help text (`ffmpeg -h filter=superequalizer`).
const CENTERS_HZ: [f64; 18] = [
    65.0, 92.0, 131.0, 185.0, 262.0, 370.0, 523.0, 740.0, 1047.0, 1480.0, 2093.0, 2960.0, 4186.0,
    5920.0, 8372.0, 11840.0, 16744.0, 20000.0,
];

/// `Q = 1 / (r - 1/r)` for a ~half-octave (`r = sqrt(2)`) band ratio, the
/// spacing `CENTERS_HZ` uses almost throughout. `r - 1/r` at `r = sqrt(2)`
/// is exactly `1/sqrt(2)`, so `Q = sqrt(2)`.
const BAND_Q: f64 = std::f64::consts::SQRT_2;

#[derive(Debug, Clone, Copy)]
struct StageDesign {
    f0: f64,
    gain_db: f64,
}

#[derive(Debug, Clone)]
struct SuperEqualizer {
    designs: [StageDesign; 18],
    stages: Vec<[(Coeffs, State); 18]>,
}

impl FrameFilter for SuperEqualizer {
    fn configure(&mut self, ctx: &mut vaco_filter_core::FilterContext<'_>) -> Result<()> {
        if let Some(vaco_filter_core::LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let fs = f64::from(*sample_rate);
            let n = layout.channels.max(1) as usize;
            let template: [(Coeffs, State); 18] = std::array::from_fn(|i| {
                let d = self.designs.get(i).copied().unwrap_or(StageDesign {
                    f0: 0.0,
                    gain_db: 0.0,
                });
                (
                    biquad::peaking(fs, d.f0, WidthType::QFactor, BAND_Q, d.gain_db),
                    State::default(),
                )
            });
            self.stages = vec![template; n];
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for (i, ch) in channels.iter_mut().enumerate() {
            let Some(stage) = self.stages.get_mut(i) else {
                continue;
            };
            for s in ch.iter_mut() {
                let mut v = *s;
                for (coeffs, state) in stage.iter_mut() {
                    v = state.process(coeffs, v);
                }
                *s = v;
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
        for stage in &mut self.stages {
            for (_, state) in stage.iter_mut() {
                *state = State::default();
            }
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let mut designs = [StageDesign {
        f0: 0.0,
        gain_db: 0.0,
    }; 18];
    for (i, f0) in CENTERS_HZ.into_iter().enumerate() {
        let key = format!("{}b", i + 1);
        let gain_linear = common::f64_opt(req, &[key.as_str()], 1.0).max(0.0);
        // `log10(0)` is `-inf`, not `NaN`, and every downstream `Coeffs`
        // build clamps a non-finite result to identity — but a silent
        // "band 3 is now a black hole" is worse than an explicit floor.
        let gain_linear = gain_linear.max(1e-6);
        let Some(slot) = designs.get_mut(i) else {
            continue;
        };
        *slot = StageDesign {
            f0,
            gain_db: 20.0 * gain_linear.log10(),
        };
    }
    let filter = SuperEqualizer {
        designs,
        stages: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gains_are_identity() {
        // Every band at its documented default (linear gain 1, i.e. 0 dB
        // once converted) must leave the response flat at that band's
        // centre frequency — the peaking cookbook formula's own zero-gain
        // identity property, checked at the frequency this filter actually
        // designs for.
        for f0 in CENTERS_HZ {
            let c = biquad::peaking(48_000.0, f0, WidthType::QFactor, BAND_Q, 0.0);
            assert!(
                c.response_db(2.0 * std::f64::consts::PI * f0 / 48_000.0)
                    .abs()
                    < 1e-6,
                "band at {f0} Hz is not flat at 0 dB gain"
            );
        }
    }
}
