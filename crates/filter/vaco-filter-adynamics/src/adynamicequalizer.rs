//! `adynamicequalizer` — apply dynamic equalisation of input audio.
//!
//! `ffmpeg -h filter=adynamicequalizer` (2026-08-27): `threshold` (0 to 100,
//! default 0), `dfrequency`/`dqfactor` (detection filter centre/Q, default
//! 1000/1), `tfrequency`/`tqfactor` (target filter centre/Q, default
//! 1000/1), `attack`/`release` (0.01 to 2000 ms, default 20/200), `ratio` (0
//! to 30, default 1), `makeup` (0 to 1000 dB, default 0), `range` (1 to
//! 2000, default 50 — the maximum gain change in dB), `mode` (`listen`=-1,
//! `cutbelow`=0 default, `cutabove`=1, `boostbelow`=2, `boostabove`=3),
//! `dftype` (`bandpass`=0 default, `lowpass`=1, `highpass`=2, `peak`=3),
//! `tftype` (`bell`=0 default, `lowshelf`=1, `highshelf`=2).
//!
//! # Shape, not a measured match
//!
//! This is a real dynamic equaliser — a detector filter feeds an envelope,
//! the envelope's level relative to `threshold` drives a gain via `ratio`,
//! and that gain modulates a *target* filter's own gain in real time — built
//! from [`vaco_filter_adsp::biquad`] (the same RBJ cookbook `vaco-filter-aeq`
//! uses) and this crate's own [`crate::engine::Envelope`]. What is **not**
//! measured against the reference is `threshold`'s exact unit (`0-100`
//! strongly suggests a percentage of full scale, so it is read here as
//! `db(threshold / 100)`, which makes the documented default of `0` read as
//! "effectively never triggers" — a defensible reading of the option's
//! default, not a probed one) and the reference's own gain-computer formula
//! for `ratio`/`range`. The biquad coefficients are recomputed every sample
//! rather than once per block, which is correct but slower than the
//! reference almost certainly is — a performance gap, not a correctness one.
//!
//! `mode=listen` bypasses the target filter and outputs the detector signal
//! directly, matching the documented purpose of a "listen" mode in every
//! dynamic-EQ plugin that has one (tune the detector by ear before trusting
//! its effect on the target band).

use vaco_core::{MediaType, Result};
use vaco_filter_adsp::biquad::{self as biquad, Coeffs, State, WidthType};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::engine::Envelope;

pub const DESC: FilterDesc = FilterDesc {
    name: "adynamicequalizer",
    description: "apply dynamic equalization of input audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Listen,
    CutBelow,
    CutAbove,
    BoostBelow,
    BoostAbove,
}

impl Mode {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "listen" | "-1" => Self::Listen,
            "cutabove" | "1" => Self::CutAbove,
            "boostbelow" | "2" => Self::BoostBelow,
            "boostabove" | "3" => Self::BoostAbove,
            _ => Self::CutBelow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectType {
    Bandpass,
    Lowpass,
    Highpass,
    Peak,
}

impl DetectType {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "lowpass" | "1" => Self::Lowpass,
            "highpass" | "2" => Self::Highpass,
            "peak" | "3" => Self::Peak,
            _ => Self::Bandpass,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetType {
    Bell,
    Lowshelf,
    Highshelf,
}

impl TargetType {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "lowshelf" | "1" => Self::Lowshelf,
            "highshelf" | "2" => Self::Highshelf,
            _ => Self::Bell,
        }
    }
}

#[derive(Debug, Clone)]
struct ChannelState {
    detect: State,
    target: State,
    envelope: Envelope,
}

#[derive(Debug, Clone)]
struct DynamicEq {
    threshold_db: f64,
    dfrequency: f64,
    dqfactor: f64,
    tfrequency: f64,
    tqfactor: f64,
    attack_ms: f64,
    release_ms: f64,
    ratio: f64,
    makeup: f64,
    range: f64,
    mode: Mode,
    dftype: DetectType,
    tftype: TargetType,
    sample_rate: f64,
    detect_coeffs: Coeffs,
    states: Vec<ChannelState>,
}

impl DynamicEq {
    fn build_detect_coeffs(&self) -> Coeffs {
        match self.dftype {
            DetectType::Bandpass => biquad::bandpass(
                self.sample_rate,
                self.dfrequency,
                WidthType::QFactor,
                self.dqfactor,
                false,
            ),
            DetectType::Lowpass => biquad::lowpass(
                self.sample_rate,
                self.dfrequency,
                WidthType::QFactor,
                self.dqfactor,
            ),
            DetectType::Highpass => biquad::highpass(
                self.sample_rate,
                self.dfrequency,
                WidthType::QFactor,
                self.dqfactor,
            ),
            DetectType::Peak => Coeffs::identity(),
        }
    }
}

fn target_coeffs(
    tftype: TargetType,
    sample_rate: f64,
    tfrequency: f64,
    tqfactor: f64,
    gain_db: f64,
) -> Coeffs {
    match tftype {
        TargetType::Bell => biquad::peaking(
            sample_rate,
            tfrequency,
            WidthType::QFactor,
            tqfactor,
            gain_db,
        ),
        TargetType::Lowshelf => biquad::lowshelf(
            sample_rate,
            tfrequency,
            WidthType::QFactor,
            tqfactor,
            gain_db,
        ),
        TargetType::Highshelf => biquad::highshelf(
            sample_rate,
            tfrequency,
            WidthType::QFactor,
            tqfactor,
            gain_db,
        ),
    }
}

/// The gain (dB) the target filter should apply right now, given the
/// detector's current level. See this module's doc for what is and is not
/// measured about this formula. A free function (not a `DynamicEq` method)
/// so it can be called while `self.states` is mutably borrowed in
/// [`DynamicEq::filter_frame`]'s per-sample loop.
fn gain_db(
    mode: Mode,
    ratio: f64,
    makeup: f64,
    range: f64,
    threshold_db: f64,
    level_db: f64,
) -> f64 {
    let ratio = if ratio.is_finite() && ratio > 0.0 {
        ratio
    } else {
        1.0
    };
    let reduction = |overshoot: f64| (overshoot * (1.0 - 1.0 / ratio)).max(0.0);
    let g = match mode {
        Mode::Listen => 0.0,
        Mode::CutBelow => -reduction(threshold_db - level_db),
        Mode::CutAbove => -reduction(level_db - threshold_db),
        Mode::BoostBelow => reduction(threshold_db - level_db),
        Mode::BoostAbove => reduction(level_db - threshold_db),
    };
    (g + makeup).clamp(-range, range)
}

impl FrameFilter for DynamicEq {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
            self.detect_coeffs = self.build_detect_coeffs();
            let n = layout.channels.max(1) as usize;
            self.states = vec![
                ChannelState {
                    detect: State::default(),
                    target: State::default(),
                    envelope: Envelope::default(),
                };
                n
            ];
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.states.len() != channels.len() {
            self.states = vec![
                ChannelState {
                    detect: State::default(),
                    target: State::default(),
                    envelope: Envelope::default(),
                };
                channels.len()
            ];
        }
        let attack = Envelope::coeff(self.attack_ms, self.sample_rate);
        let release = Envelope::coeff(self.release_ms, self.sample_rate);
        let (dftype, mode, ratio, makeup, range, threshold_db) = (
            self.dftype,
            self.mode,
            self.ratio,
            self.makeup,
            self.range,
            self.threshold_db,
        );
        let (tftype, sample_rate, tfrequency, tqfactor) = (
            self.tftype,
            self.sample_rate,
            self.tfrequency,
            self.tqfactor,
        );
        let detect_coeffs = self.detect_coeffs;
        for (ch, st) in channels.iter_mut().zip(self.states.iter_mut()) {
            for s in ch.iter_mut() {
                let detected = match dftype {
                    DetectType::Peak => *s,
                    _ => st.detect.process(&detect_coeffs, *s),
                };
                if mode == Mode::Listen {
                    *s = detected;
                    continue;
                }
                let level = st.envelope.step(detected.abs(), attack, release);
                let level_db = common::db(level);
                let gain = gain_db(mode, ratio, makeup, range, threshold_db, level_db);
                let coeffs = target_coeffs(tftype, sample_rate, tfrequency, tqfactor, gain);
                *s = st.target.process(&coeffs, *s);
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
        for st in &mut self.states {
            st.detect = State::default();
            st.target = State::default();
            st.envelope = Envelope::default();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let threshold_pct = common::f64_opt(req, &["threshold"], 0.0);
    let filter = DynamicEq {
        threshold_db: common::db(threshold_pct / 100.0),
        dfrequency: common::f64_opt(req, &["dfrequency"], 1000.0),
        dqfactor: common::f64_opt(req, &["dqfactor"], 1.0),
        tfrequency: common::f64_opt(req, &["tfrequency"], 1000.0),
        tqfactor: common::f64_opt(req, &["tqfactor"], 1.0),
        attack_ms: common::f64_opt(req, &["attack"], 20.0),
        release_ms: common::f64_opt(req, &["release"], 200.0),
        ratio: common::f64_opt(req, &["ratio"], 1.0),
        makeup: common::f64_opt(req, &["makeup"], 0.0),
        range: common::f64_opt(req, &["range"], 50.0).max(1.0),
        mode: req
            .named("mode")
            .map_or(Mode::CutBelow, |v| Mode::parse(&v)),
        dftype: req
            .named("dftype")
            .map_or(DetectType::Bandpass, |v| DetectType::parse(&v)),
        tftype: req
            .named("tftype")
            .map_or(TargetType::Bell, |v| TargetType::parse(&v)),
        sample_rate: 48_000.0,
        detect_coeffs: Coeffs::identity(),
        states: Vec::new(),
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

    fn eq() -> DynamicEq {
        DynamicEq {
            threshold_db: -20.0,
            dfrequency: 1000.0,
            dqfactor: 1.0,
            tfrequency: 1000.0,
            tqfactor: 1.0,
            attack_ms: 20.0,
            release_ms: 200.0,
            ratio: 4.0,
            makeup: 0.0,
            range: 50.0,
            mode: Mode::CutBelow,
            dftype: DetectType::Bandpass,
            tftype: TargetType::Bell,
            sample_rate: 48_000.0,
            detect_coeffs: Coeffs::identity(),
            states: Vec::new(),
        }
    }

    /// Reads the free [`gain_db`] function's arguments off this fixture's
    /// scalar fields — [`gain_db`] takes plain arguments rather than
    /// `&DynamicEq` so it can be called while `self.states` is mutably
    /// borrowed in [`DynamicEq::filter_frame`]; the tests go through this
    /// helper so a fixture (with `..eq()`-style tweaks) reads naturally.
    fn eq_gain_db(e: &DynamicEq, level_db: f64) -> f64 {
        gain_db(e.mode, e.ratio, e.makeup, e.range, e.threshold_db, level_db)
    }

    /// `ratio = 1` must be a gain-computer identity, exactly as
    /// `crate::engine::Curve`'s own test of the same property — checked
    /// independently here since [`gain_db`] is a different formula
    /// (bidirectional, four modes) built for a different purpose.
    #[test]
    fn ratio_one_is_identity_in_every_mode() {
        let mut e = eq();
        e.ratio = 1.0;
        for mode in [
            Mode::CutBelow,
            Mode::CutAbove,
            Mode::BoostBelow,
            Mode::BoostAbove,
        ] {
            e.mode = mode;
            for level in [-60.0, -20.0, 0.0, 10.0] {
                assert!(
                    eq_gain_db(&e, level).abs() < 1e-9,
                    "{mode:?} at {level}: {}",
                    eq_gain_db(&e, level)
                );
            }
        }
    }

    /// `cutbelow` must never boost: gain is always `<= makeup`.
    #[test]
    fn cutbelow_never_boosts() {
        let e = eq();
        for level in [-100.0, -50.0, -20.0, 0.0, 20.0] {
            assert!(
                eq_gain_db(&e, level) <= 1e-9,
                "level {level}: {}",
                eq_gain_db(&e, level)
            );
        }
    }

    /// `boostbelow` must never cut.
    #[test]
    fn boostbelow_never_cuts() {
        let mut e = eq();
        e.mode = Mode::BoostBelow;
        for level in [-100.0, -50.0, -20.0, 0.0, 20.0] {
            assert!(
                eq_gain_db(&e, level) >= -1e-9,
                "level {level}: {}",
                eq_gain_db(&e, level)
            );
        }
    }

    /// `range` clamps the final gain regardless of how large `ratio` or the
    /// overshoot is.
    #[test]
    fn range_clamps_the_gain() {
        let mut e = eq();
        e.ratio = 30.0;
        e.range = 6.0;
        let g = eq_gain_db(&e, -200.0);
        assert!(g >= -6.0 - 1e-9, "{g}");
    }

    #[test]
    fn mode_and_type_parsing_covers_names_and_codes() {
        assert_eq!(Mode::parse("listen"), Mode::parse("-1"));
        assert_eq!(Mode::parse("cutabove"), Mode::parse("1"));
        assert_eq!(DetectType::parse("peak"), DetectType::parse("3"));
        assert_eq!(TargetType::parse("highshelf"), TargetType::parse("2"));
    }
}
