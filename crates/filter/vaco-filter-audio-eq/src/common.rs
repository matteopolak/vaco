//! Shared option parsing and per-channel application for the biquad family.
//!
//! Every filter in [`crate::engine`]'s family documents more options than this
//! crate implements (`transform`, `precision`, `blocksize`, and their `a`/`r`
//! aliases pick among numerically-different realisations of the *same*
//! transfer function or an execution-speed knob — see `engine::State`'s doc).
//! Rather than declare them on a [`vaco_opts::Options`] struct and reject a
//! filtergraph string that sets one — which is what a strict
//! `set_from_string` would do — every filter here reads only the options it
//! implements straight off [`Instantiate::named`], exactly as
//! `vaco-filter-audio::aformat` does for the reference options it does not
//! implement either. An option this crate does not recognise is silently
//! accepted and ignored, matching that established precedent rather than
//! inventing a new one.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut};
use vaco_filter_core::{FilterContext, LinkFormat, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::Instantiate;

use crate::engine::{self, Coeffs, State, WidthType};

pub(crate) const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// Read a named option as `f64`, trying each alias in order.
pub(crate) fn f64_opt(req: &Instantiate<'_>, keys: &[&str], default: f64) -> f64 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(f) = v.trim().parse::<f64>()
        {
            return f;
        }
    }
    default
}

/// Read a named option as `bool` (`ffmpeg`'s boolean spellings: `1`/`0`,
/// `true`/`false`).
pub(crate) fn bool_opt(req: &Instantiate<'_>, keys: &[&str], default: bool) -> bool {
    for k in keys {
        if let Some(v) = req.named(k) {
            let v = v.trim();
            if v.eq_ignore_ascii_case("true") || v == "1" {
                return true;
            }
            if v.eq_ignore_ascii_case("false") || v == "0" {
                return false;
            }
        }
    }
    default
}

/// Read a named option as `u8`.
pub(crate) fn u8_opt(req: &Instantiate<'_>, keys: &[&str], default: u8) -> u8 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(n) = v.trim().parse::<u8>()
        {
            return n;
        }
    }
    default
}

/// `width_type`/`t`: a name (`h`/`q`/`o`/`s`/`k`) or the reference's numeric
/// encoding (`1..=5` in that same order, probed via `ffmpeg -h`).
pub(crate) fn width_type_opt(req: &Instantiate<'_>) -> WidthType {
    for k in ["width_type", "t"] {
        if let Some(v) = req.named(k) {
            let v = v.trim();
            if let Some(wt) = WidthType::parse(v) {
                return wt;
            }
            match v {
                "1" => return WidthType::Hz,
                "2" => return WidthType::Octave,
                "3" => return WidthType::QFactor,
                "4" => return WidthType::Slope,
                "5" => return WidthType::KHz,
                _ => {}
            }
        }
    }
    WidthType::QFactor
}

/// `channels`/`c`: `"all"` (the default) or a whitespace/`|`-separated list
/// of channel indices. Reference names ("FL", "FR", ...) are not resolved —
/// index selection covers the common `channels=0` / `channels=0 1` cases and
/// is a documented structural gap.
#[derive(Debug, Clone)]
pub(crate) enum ChannelSelect {
    All,
    Indices(Vec<usize>),
}

impl ChannelSelect {
    pub(crate) fn parse(req: &Instantiate<'_>) -> Self {
        let raw = req.named("channels").or_else(|| req.named("c"));
        let Some(raw) = raw else {
            return Self::All;
        };
        let raw = raw.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("all") {
            return Self::All;
        }
        let idx: Vec<usize> = raw
            .split(|c: char| c.is_whitespace() || c == '|' || c == ',')
            .filter_map(|t| t.trim().parse::<usize>().ok())
            .collect();
        if idx.is_empty() {
            Self::All
        } else {
            Self::Indices(idx)
        }
    }

    pub(crate) fn selects(&self, index: usize) -> bool {
        match self {
            Self::All => true,
            Self::Indices(v) => v.contains(&index),
        }
    }
}

/// What coefficients to build, and from what parameters.
///
/// Coefficients cannot be built at `create()` time: the cookbook formulas
/// need the sample rate, which is only known once link negotiation has run.
/// So a filter module builds a `Design` from its options immediately, and
/// [`Biquad::configure`] calls [`Design::build`] once the real rate is
/// available — recomputing it every time `configure` runs, which also covers
/// a format change mid-graph.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Design {
    Lowpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        poles: u8,
    },
    Highpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        poles: u8,
    },
    Bandpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        csg: bool,
    },
    Bandreject {
        f0: f64,
        wt: WidthType,
        width: f64,
    },
    Allpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        order: u8,
    },
    Peaking {
        f0: f64,
        wt: WidthType,
        width: f64,
        gain_db: f64,
    },
    Lowshelf {
        f0: f64,
        wt: WidthType,
        width: f64,
        gain_db: f64,
    },
    Highshelf {
        f0: f64,
        wt: WidthType,
        width: f64,
        gain_db: f64,
    },
    Raw(Coeffs),
}

impl Design {
    pub(crate) fn build(self, fs: f64) -> Coeffs {
        match self {
            Self::Lowpass {
                f0,
                wt,
                width,
                poles,
            } => {
                if poles == 1 {
                    engine::lowpass_one_pole(fs, f0)
                } else {
                    engine::lowpass(fs, f0, wt, width)
                }
            }
            Self::Highpass {
                f0,
                wt,
                width,
                poles,
            } => {
                if poles == 1 {
                    engine::highpass_one_pole(fs, f0)
                } else {
                    engine::highpass(fs, f0, wt, width)
                }
            }
            Self::Bandpass { f0, wt, width, csg } => engine::bandpass(fs, f0, wt, width, csg),
            Self::Bandreject { f0, wt, width } => engine::bandreject(fs, f0, wt, width),
            Self::Allpass {
                f0,
                wt,
                width,
                order,
            } => engine::allpass(fs, f0, wt, width, order),
            Self::Peaking {
                f0,
                wt,
                width,
                gain_db,
            } => engine::peaking(fs, f0, wt, width, gain_db),
            Self::Lowshelf {
                f0,
                wt,
                width,
                gain_db,
            } => engine::lowshelf(fs, f0, wt, width, gain_db),
            Self::Highshelf {
                f0,
                wt,
                width,
                gain_db,
            } => engine::highshelf(fs, f0, wt, width, gain_db),
            Self::Raw(c) => c,
        }
    }
}

/// A `FrameFilter` that runs one biquad section over every selected channel,
/// wet/dry-mixed by `mix`. This is the whole body of every filter in
/// [`crate::engine`]'s family that is not `tiltshelf` (which cascades two of
/// these).
#[derive(Debug, Clone)]
pub(crate) struct Biquad {
    design: Design,
    coeffs: Coeffs,
    pub mix: f64,
    pub select: ChannelSelect,
    states: Vec<State>,
}

impl Biquad {
    pub(crate) fn new(design: Design, mix: f64, select: ChannelSelect) -> Self {
        Self {
            design,
            coeffs: Coeffs::identity(),
            mix,
            select,
            states: Vec::new(),
        }
    }
}

impl FrameFilter for Biquad {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.coeffs = self.design.build(f64::from(*sample_rate));
            self.states = vec![State::default(); layout.channels.max(1) as usize];
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.states.len() != channels.len() {
            self.states = vec![State::default(); channels.len()];
        }
        for (i, ch) in channels.iter_mut().enumerate() {
            if !self.select.selects(i) {
                continue;
            }
            let Some(state) = self.states.get_mut(i) else {
                continue;
            };
            for s in ch.iter_mut() {
                let dry = *s;
                let wet = state.process(&self.coeffs, dry);
                *s = self.mix.mul_add(wet - dry, dry);
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
        for s in &mut self.states {
            *s = State::default();
        }
    }
}

/// `frequency`/`f`, read with a filter-specific default (the reference's
/// default frequency differs per filter — 0 Hz for `equalizer`, 3000 Hz for
/// `highpass`/`bandpass`/`bandreject`/`allpass`/`treble`, 100 Hz for `bass`,
/// 500 Hz for `lowpass` — all probed via `ffmpeg -h filter=<name>`).
pub(crate) fn frequency_opt(req: &Instantiate<'_>, default: f64) -> f64 {
    f64_opt(req, &["frequency", "f"], default)
}

pub(crate) fn width_opt(req: &Instantiate<'_>, default: f64) -> f64 {
    f64_opt(req, &["width", "w"], default)
}

pub(crate) fn gain_opt(req: &Instantiate<'_>, default: f64) -> f64 {
    f64_opt(req, &["gain", "g"], default)
}

pub(crate) fn mix_opt(req: &Instantiate<'_>) -> f64 {
    f64_opt(req, &["mix", "m"], 1.0)
}

pub(crate) fn poles_opt(req: &Instantiate<'_>) -> u8 {
    u8_opt(req, &["poles", "p"], 2)
}
