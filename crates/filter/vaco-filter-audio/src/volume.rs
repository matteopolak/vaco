//! `volume` — scale sample amplitude by an expression.
//!
//! `ffmpeg -h filter=volume` documents `volume`, `precision`, `eval`,
//! `replaygain`, `replaygain_preamp` and `replaygain_noclip`. Implemented:
//! `volume` (a `vaco-expr` expression over `n`/`t`/`pts`/`tb`/`sample_rate`,
//! which for the overwhelmingly common case is just a bare number or a
//! `NdB`/`N%` literal — `vaco-expr`'s number grammar already accepts both)
//! and `eval` (`once` computes the gain at configure time; `frame`
//! re-evaluates every frame, which is what makes `volume=...:eval=frame`
//! useful with `enable=` or a time-varying expression).
//!
//! Not implemented: `precision` (arithmetic always happens in the `f64`
//! domain `sample::decode`/`sample::encode` share with every other filter in
//! this crate — see `vaco_resample`'s numeric contract), and the `ReplayGain`
//! side-data options (`vaco-frame`'s `FrameSideData` has no `ReplayGain`
//! variant yet to read from).

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "volume",
    description: "change input volume",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "volume", help = "change input volume")]
pub(crate) struct Opts {
    #[opt(
        name = "volume",
        help = "volume adjustment expression",
        default = "1.0".to_owned(),
        flags(audio, filtering)
    )]
    pub volume: String,

    #[opt(
        name = "eval",
        help = "once or frame",
        default = "once".to_owned(),
        flags(audio, filtering)
    )]
    pub eval: String,
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

const VARS: &[&str] = &["n", "t", "pts", "tb", "sample_rate"];

#[derive(Debug)]
pub(crate) struct Filter {
    expr: Expr,
    per_frame: bool,
    gain: f64,
    frame_index: u64,
    sample_rate: f64,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(VARS);
        let expr = Expr::parse(&opts.volume, &bindings)
            .map_err(|e| format!("volume: bad `volume` expression `{}`: {e}", opts.volume))?;
        Ok(Self {
            expr,
            per_frame: opts.eval == "frame",
            gain: 1.0,
            frame_index: 0,
            sample_rate: 0.0,
        })
    }

    fn eval(&self, n: f64, t: f64, pts: f64, tb: f64) -> f64 {
        self.expr.eval(&[n, t, pts, tb, self.sample_rate])
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(vaco_filter_core::LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate);
        }
        self.gain = self.eval(0.0, 0.0, 0.0, 1.0);
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        if self.per_frame {
            let n = self.frame_index as f64;
            let t = input.pts.to_seconds(input.time_base).unwrap_or(0.0);
            let pts = input.pts.ticks().unwrap_or(0) as f64;
            let tb = input.time_base.to_f64();
            self.gain = self.eval(n, t, pts, tb);
        }
        self.frame_index = self.frame_index.saturating_add(1);

        if (self.gain - 1.0).abs() < f64::EPSILON {
            return Ok(FrameOut::One(input));
        }
        let (_, rate, samples, layout, mut channels) = crate::sample::decode(&input)?;
        for ch in &mut channels {
            for s in ch.iter_mut() {
                *s *= self.gain;
            }
        }
        let vaco_frame::FrameData::Audio { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let _ = samples;
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            format,
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
        self.frame_index = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(vaco_filter_core::Timeline::always())),
    })
}
