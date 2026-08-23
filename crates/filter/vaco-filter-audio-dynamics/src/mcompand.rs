//! `mcompand` — multiband compress or expand audio dynamic range.
//!
//! `ffmpeg -h filter=mcompand` (2026-08-23) has one option, `args`, whose
//! grammar the reference's texi manual documents as `|`-separated bands,
//! each `attack,decay soft-knee points crossover_freq` — `points` itself a
//! `,`-separated list of `in_db/out_db` pairs (compare [`crate::compand`]'s
//! `points`, which is `|`-separated; `mcompand` reuses `|` for the band
//! separator instead, so the two grammars are not interchangeable). Default:
//! five bands crossing over at 100/400/1600/6400 Hz.
//!
//! Each band is split out with a second-order Butterworth low-pass/high-pass
//! pair at its crossover edges (the cookbook formula, duplicated locally
//! rather than depending on `vaco-filter-audio-eq` — a dozen lines, not
//! worth a cross-crate coupling for), companded with [`crate::compand`]'s
//! transfer-curve machinery, and summed. `soft-knee` is accepted and
//! ignored, matching `compand`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, db, from_db};
use crate::compand::{Point, transfer_db};
use crate::engine::Envelope;

pub const DESC: FilterDesc = FilterDesc {
    name: "mcompand",
    description: "multiband compress or expand dynamic range of audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// A second-order Butterworth section, duplicated from the same Audio EQ
/// Cookbook formula `vaco-filter-audio-eq::engine` uses (`Q = 1/sqrt(2)`
/// fixed, since a crossover has no user-facing `width`/`Q` option).
#[derive(Debug, Clone, Copy, Default)]
struct Biquad2 {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad2 {
    fn lowpass(fs: f64, f0: f64) -> Self {
        Self::build(fs, f0, false)
    }

    fn highpass(fs: f64, f0: f64) -> Self {
        Self::build(fs, f0, true)
    }

    fn build(fs: f64, f0: f64, high: bool) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * f0 / fs;
        if !w0.is_finite() || w0 <= 0.0 || w0 >= std::f64::consts::PI {
            // Above Nyquist / at-or-below DC: pass everything through
            // (lowpass) or block everything (highpass) via the identity /
            // zero section rather than letting `sin`/`cos` degenerate into a
            // non-finite coefficient.
            return if high {
                Self {
                    b0: 0.0,
                    b1: 0.0,
                    b2: 0.0,
                    a1: 0.0,
                    a2: 0.0,
                }
            } else {
                Self {
                    b0: 1.0,
                    b1: 0.0,
                    b2: 0.0,
                    a1: 0.0,
                    a2: 0.0,
                }
            };
        }
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let alpha = w0.sin() / (2.0 * q);
        let cw = w0.cos();
        let (b0, b1, b2) = if high {
            let h = f64::midpoint(1.0, cw);
            (h, -(1.0 + cw), h)
        } else {
            let l = f64::midpoint(1.0, -cw);
            (l, 1.0 - cw, l)
        };
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cw;
        let a2 = 1.0 - alpha;
        if a0 == 0.0 || !a0.is_finite() {
            return Self::default();
        }
        let s = Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        };
        if [s.b0, s.b1, s.b2, s.a1, s.a2].iter().all(|v| v.is_finite()) {
            s
        } else {
            Self::default()
        }
    }

    fn process(&self, state: &mut (f64, f64, f64, f64), x0: f64) -> f64 {
        let (x1, x2, y1, y2) = *state;
        let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
        *state = (x0, x1, y0, y1);
        if y0.is_finite() { y0 } else { 0.0 }
    }
}

#[derive(Debug, Clone)]
struct BandSpec {
    attack_ms: f64,
    decay_ms: f64,
    points: Vec<Point>,
    crossover_hz: f64,
}

fn parse_bands(raw: &str) -> Vec<BandSpec> {
    let mut bands: Vec<BandSpec> = raw
        .split('|')
        .filter_map(|seg| {
            let mut tok = seg.split_whitespace();
            let times = tok.next()?;
            let _knee = tok.next();
            let points_raw = tok.next()?;
            let crossover_hz = tok.next()?.trim().parse::<f64>().ok()?;
            let (attack_s, decay_s) = times.split_once(',').unwrap_or((times, times));
            let points: Vec<Point> = points_raw
                .split(',')
                .filter_map(|p| {
                    let (a, b) = p.split_once('/')?;
                    Some(Point::new(a.trim().parse().ok()?, b.trim().parse().ok()?))
                })
                .collect();
            Some(BandSpec {
                attack_ms: attack_s.trim().parse::<f64>().unwrap_or(0.05) * 1000.0,
                decay_ms: decay_s.trim().parse::<f64>().unwrap_or(0.1) * 1000.0,
                points,
                crossover_hz,
            })
        })
        .collect();
    bands.sort_by(|a, b| a.crossover_hz.total_cmp(&b.crossover_hz));
    if bands.is_empty() {
        bands.push(BandSpec {
            attack_ms: 50.0,
            decay_ms: 100.0,
            points: Vec::new(),
            crossover_hz: 22_000.0,
        });
    }
    bands
}

#[derive(Debug, Clone, Copy, Default)]
struct FilterState {
    lp: (f64, f64, f64, f64),
    hp: (f64, f64, f64, f64),
}

struct Band {
    spec: BandSpec,
    lowpass: Biquad2,
    highpass: Biquad2,
    states: Vec<FilterState>,
    envelopes: Vec<Envelope>,
}

pub(crate) struct MultibandCompand {
    bands: Vec<BandSpec>,
    runtime: Vec<Band>,
    sample_rate: f64,
}

impl FrameFilter for MultibandCompand {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
            let n = layout.channels.max(1) as usize;
            self.runtime = self
                .bands
                .iter()
                .enumerate()
                .map(|(i, spec)| {
                    let low_edge = if i == 0 {
                        0.0
                    } else {
                        self.bands.get(i - 1).map_or(0.0, |b| b.crossover_hz)
                    };
                    let is_last = i + 1 == self.bands.len();
                    Band {
                        spec: spec.clone(),
                        lowpass: if is_last {
                            Biquad2 {
                                b0: 1.0,
                                ..Biquad2::default()
                            }
                        } else {
                            Biquad2::lowpass(self.sample_rate, spec.crossover_hz)
                        },
                        highpass: if low_edge > 0.0 {
                            Biquad2::highpass(self.sample_rate, low_edge)
                        } else {
                            Biquad2 {
                                b0: 1.0,
                                ..Biquad2::default()
                            }
                        },
                        states: vec![FilterState::default(); n],
                        envelopes: vec![Envelope::default(); n],
                    }
                })
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, channels) = crate::sample::decode(&input)?;
        let mut sum: Vec<Vec<f64>> = channels.iter().map(|c| vec![0.0; c.len()]).collect();
        for band in &mut self.runtime {
            if band.states.len() != channels.len() {
                band.states = vec![FilterState::default(); channels.len()];
                band.envelopes = vec![Envelope::default(); channels.len()];
            }
            let attack = Envelope::coeff(band.spec.attack_ms, self.sample_rate);
            let decay = Envelope::coeff(band.spec.decay_ms, self.sample_rate);
            for (ci, ch) in channels.iter().enumerate() {
                let (Some(state), Some(env), Some(out_ch)) = (
                    band.states.get_mut(ci),
                    band.envelopes.get_mut(ci),
                    sum.get_mut(ci),
                ) else {
                    continue;
                };
                for (si, &x) in ch.iter().enumerate() {
                    let filtered = band.highpass.process(&mut state.hp, x);
                    let filtered = band.lowpass.process(&mut state.lp, filtered);
                    let level = env.step(filtered.abs(), attack, decay);
                    let level_db = db(level);
                    let target_db = transfer_db(&band.spec.points, level_db);
                    let gain = from_db(target_db - level_db);
                    if let Some(o) = out_ch.get_mut(si) {
                        *o += filtered * gain;
                    }
                }
            }
        }
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &sum.into(),
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        for band in &mut self.runtime {
            for e in &mut band.envelopes {
                *e = Envelope::default();
            }
            for s in &mut band.states {
                *s = FilterState::default();
            }
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let raw = req.named("args").unwrap_or_else(|| {
        "0.005,0.1 6 -47/-40,-34/-34,-17/-33 100 | 0.003,0.05 6 -47/-40,-34/-34,-17/-33 400 \
         | 0.000625,0.0125 6 -47/-40,-34/-34,-15/-33 1600 \
         | 0.0001,0.025 6 -47/-40,-34/-34,-31/-31,-0/-30 6400 \
         | 0,0.025 6 -38/-31,-28/-28,-0/-25 22000"
            .to_owned()
    });
    let bands = parse_bands(&raw);
    let filter = MultibandCompand {
        bands,
        runtime: Vec::new(),
        sample_rate: 48_000.0,
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

    #[test]
    fn default_args_parse_into_five_bands_ascending() {
        let bands = parse_bands(
            "0.005,0.1 6 -47/-40,-34/-34,-17/-33 100 | 0.003,0.05 6 -47/-40,-34/-34,-17/-33 400 \
             | 0.000625,0.0125 6 -47/-40,-34/-34,-15/-33 1600 \
             | 0.0001,0.025 6 -47/-40,-34/-34,-31/-31,-0/-30 6400 \
             | 0,0.025 6 -38/-31,-28/-28,-0/-25 22000",
        );
        assert_eq!(bands.len(), 5);
        let edges: Vec<f64> = bands.iter().map(|b| b.crossover_hz).collect();
        assert_eq!(edges, vec![100.0, 400.0, 1600.0, 6400.0, 22000.0]);
    }

    #[test]
    fn crossover_coefficients_are_always_finite() {
        for f0 in [0.0, -10.0, 100.0, 24_000.0, 48_000.0] {
            for high in [false, true] {
                let c = Biquad2::build(48_000.0, f0, high);
                assert!(c.b0.is_finite() && c.b1.is_finite() && c.b2.is_finite());
                assert!(c.a1.is_finite() && c.a2.is_finite());
            }
        }
    }
}
