//! `tiltshelf` — apply a tilt shelf filter.
//!
//! Shares its option schema with [`crate::treble`]/[`crate::highshelf`]
//! (`ffmpeg -h filter=tiltshelf` prints the same `treble/high/tiltshelf`
//! class, probed 2026-08-23) but is a different transfer function: a tilt
//! EQ, not a shelf. Built as a cascade of a low shelf cutting `-gain/2` and
//! a high shelf boosting `+gain/2` — see `vaco_filter_adsp::biquad::tilt` for why that
//! construction is a genuine tilt (0 dB at the pivot, `-gain/2`/`+gain/2` at
//! DC/Nyquist) and its numeric verification.
//!
//! Defaults match `treble`/`highshelf`: `frequency`/`f` 3000 Hz,
//! `width_type`/`t` `q`, `width`/`w` 0.5, `gain`/`g` 0 dB.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, ChannelSelect};
use vaco_filter_adsp::biquad::{self as biquad, Coeffs, State, WidthType};

pub const DESC: FilterDesc = FilterDesc {
    name: "tiltshelf",
    description: "apply a tilt shelf filter",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone)]
struct TiltShelf {
    f0: f64,
    wt: WidthType,
    width: f64,
    gain_db: f64,
    mix: f64,
    select: ChannelSelect,
    low: Coeffs,
    high: Coeffs,
    low_states: Vec<State>,
    high_states: Vec<State>,
}

impl FrameFilter for TiltShelf {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let (low, high) = biquad::tilt(
                f64::from(*sample_rate),
                self.f0,
                self.wt,
                self.width,
                self.gain_db,
            );
            self.low = low;
            self.high = high;
            let n = layout.channels.max(1) as usize;
            self.low_states = vec![State::default(); n];
            self.high_states = vec![State::default(); n];
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.low_states.len() != channels.len() {
            self.low_states = vec![State::default(); channels.len()];
            self.high_states = vec![State::default(); channels.len()];
        }
        for (i, ch) in channels.iter_mut().enumerate() {
            if !self.select.selects(i) {
                continue;
            }
            let (Some(ls), Some(hs)) = (self.low_states.get_mut(i), self.high_states.get_mut(i))
            else {
                continue;
            };
            for s in ch.iter_mut() {
                let dry = *s;
                let mid = ls.process(&self.low, dry);
                let wet = hs.process(&self.high, mid);
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
        for s in &mut self.low_states {
            *s = State::default();
        }
        for s in &mut self.high_states {
            *s = State::default();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = TiltShelf {
        f0: common::frequency_opt(req, 3000.0),
        wt: common::width_type_opt(req),
        width: common::width_opt(req, 0.5),
        gain_db: common::gain_opt(req, 0.0),
        mix: common::mix_opt(req),
        select: ChannelSelect::parse(req),
        low: Coeffs::identity(),
        high: Coeffs::identity(),
        low_states: Vec::new(),
        high_states: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}
