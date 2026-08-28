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
//! pair at its crossover edges — [`vaco_filter_adsp::biquad`]'s cookbook
//! design at `Q = 1/sqrt(2)` fixed, since a crossover has no user-facing
//! `width`/`Q` option — companded with [`crate::compand`]'s transfer-curve
//! machinery, and summed. `soft-knee` is accepted and ignored, matching
//! `compand`.
//!
//! This module used to carry its own duplicate of the cookbook Butterworth
//! formula (on the theory that depending on `vaco-filter-aeq` for "a
//! dozen lines" was not worth a cross-crate coupling). `vaco-filter-adsp`
//! now exists as the shared home those lines belong in regardless of size
//! (D19), so [`crossover_lowpass`] and [`crossover_highpass`] build on it
//! directly. The one piece kept local is the out-of-range crossover
//! frequency guard: at or below DC, or at or above Nyquist, the crossover
//! has nothing to split, so the guard substitutes the identity (lowpass) or
//! zero (highpass) section directly rather than asking the cookbook formula
//! for a design point outside its intended domain — `vaco-filter-adsp`'s
//! own `lowpass`/`highpass` do not special-case this range (their contract
//! is "coefficients stay finite", not "physically sensible above
//! Nyquist" — see their tests), so reproducing this crate's prior exact
//! behaviour at those edges means keeping the guard here rather than
//! pushing it down.

use vaco_core::{MediaType, Result};
use vaco_filter_adsp::biquad::{Coeffs, State, WidthType, highpass, lowpass};
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

/// Fixed Butterworth `Q` for a crossover section (no user-facing `width`).
const CROSSOVER_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// A crossover's low-pass edge, or the identity section (pass everything
/// through) if `f0` is at/below DC or at/above Nyquist — see the module doc.
fn crossover_lowpass(fs: f64, f0: f64) -> Coeffs {
    if !f0.is_finite() || f0 <= 0.0 || f0 >= fs / 2.0 {
        return Coeffs::identity();
    }
    lowpass(fs, f0, WidthType::QFactor, CROSSOVER_Q)
}

/// A crossover's high-pass edge, or the zero section (block everything) if
/// `f0` is at/below DC or at/above Nyquist — see the module doc.
fn crossover_highpass(fs: f64, f0: f64) -> Coeffs {
    if !f0.is_finite() || f0 <= 0.0 || f0 >= fs / 2.0 {
        return Coeffs {
            b0: 0.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        };
    }
    highpass(fs, f0, WidthType::QFactor, CROSSOVER_Q)
}

/// `state.process(coeffs, x)`, clamped to `0.0` on a non-finite result — the
/// same per-sample safety net this module had before the move to
/// `vaco_filter_adsp::biquad::State`, which does not clamp on its own.
fn process_clamped(state: &mut State, coeffs: &Coeffs, x: f64) -> f64 {
    let y = state.process(coeffs, x);
    if y.is_finite() { y } else { 0.0 }
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
    lp: State,
    hp: State,
}

struct Band {
    spec: BandSpec,
    lowpass: Coeffs,
    highpass: Coeffs,
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
                            Coeffs::identity()
                        } else {
                            crossover_lowpass(self.sample_rate, spec.crossover_hz)
                        },
                        highpass: if low_edge > 0.0 {
                            crossover_highpass(self.sample_rate, low_edge)
                        } else {
                            Coeffs::identity()
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
                    let filtered = process_clamped(&mut state.hp, &band.highpass, x);
                    let filtered = process_clamped(&mut state.lp, &band.lowpass, filtered);
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
            for c in [
                crossover_lowpass(48_000.0, f0),
                crossover_highpass(48_000.0, f0),
            ] {
                assert!(c.b0.is_finite() && c.b1.is_finite() && c.b2.is_finite());
                assert!(c.a1.is_finite() && c.a2.is_finite());
            }
        }
    }

    /// The edge case the module doc calls out explicitly: a crossover at or
    /// above Nyquist must not be handed to the cookbook formula at all — the
    /// low-pass side must pass everything through and the high-pass side
    /// must block everything, exactly as before the move to
    /// `vaco_filter_adsp::biquad`.
    #[test]
    fn out_of_range_crossover_uses_the_identity_and_zero_sections() {
        let fs = 48_000.0;
        for f0 in [0.0, -10.0, fs / 2.0, fs] {
            assert_eq!(crossover_lowpass(fs, f0), Coeffs::identity());
            assert_eq!(
                crossover_highpass(fs, f0),
                Coeffs {
                    b0: 0.0,
                    b1: 0.0,
                    b2: 0.0,
                    a1: 0.0,
                    a2: 0.0,
                }
            );
        }
    }
}
