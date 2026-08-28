//! `setpts`/`asetpts` — rewrite a frame's presentation timestamp.
//!
//! `ffmpeg -h filter=setpts` documents `expr` (default `"PTS"`) and, video
//! only, `strip_fps`. The reference's documented variable set is `N`,
//! `NB_CONSUMED_SAMPLES`, `PTS`, `STARTPTS`, `PREV_INPTS`, `PREV_OUTPTS`,
//! `PREV_INT`, `PREV_OUTT`, `T`, `TB`, `RTCTIME`, `RTCSTART`, `FRAME_RATE`,
//! `SAMPLE_RATE`, `INTERLACED`, `POS`, `S`, `SR`. Implemented: `N`, `PTS`,
//! `STARTPTS`, `PREV_INPTS`, `PREV_OUTPTS`, `T`, `TB`, `SAMPLE_RATE`,
//! `FRAME_RATE`. `POS` is permanently `NaN`, matching the reference's own
//! current behaviour (plan 16 §1.10.1). Not implemented, all evaluating to
//! `NaN`: `RTCTIME`/`RTCSTART` (wall clock — `vaco-time` gives monotonic and
//! epoch time but wiring a real value through `vaco-expr`'s fixed binding
//! list is future work), `PREV_INT`/`PREV_OUTT`, `INTERLACED`, `S`, `SR`.
//! `strip_fps` is parsed but not applied — see `docs/filter/vaco-filter-mm.md`.

use vaco_core::{MediaType, Result, Timestamp};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VARS: &[&str] = &[
    "N",
    "PTS",
    "STARTPTS",
    "PREV_INPTS",
    "PREV_OUTPTS",
    "PREV_INT",
    "PREV_OUTT",
    "T",
    "TB",
    "RTCTIME",
    "RTCSTART",
    "FRAME_RATE",
    "SAMPLE_RATE",
    "INTERLACED",
    "POS",
    "S",
    "SR",
    "NB_CONSUMED_SAMPLES",
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "setpts", help = "set output PTS")]
pub(crate) struct Opts {
    #[opt(
        name = "expr",
        help = "expression determining the frame timestamp",
        default = "PTS".to_owned(),
        flags(filtering)
    )]
    pub expr: String,
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

#[derive(Debug)]
pub(crate) struct Filter {
    expr: Expr,
    n: f64,
    start_pts: Option<f64>,
    prev_in_pts: f64,
    prev_out_pts: f64,
    sample_rate: f64,
    frame_rate: f64,
}

impl Filter {
    fn new(expr: Expr) -> Self {
        Self {
            expr,
            n: 0.0,
            start_pts: None,
            prev_in_pts: f64::NAN,
            prev_out_pts: f64::NAN,
            sample_rate: f64::NAN,
            frame_rate: f64::NAN,
        }
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(vaco_filter_core::LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate);
        }
        if let Some(vaco_filter_core::LinkFormat::Video { frame_rate, .. }) = ctx.input_link(0) {
            self.frame_rate = frame_rate.to_f64();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        let tb = input.time_base.to_f64();
        let in_pts = input.pts.ticks().map_or(f64::NAN, |t| t as f64);
        if self.start_pts.is_none() && in_pts.is_finite() {
            self.start_pts = Some(in_pts);
        }
        let t = input.pts.to_seconds(input.time_base).unwrap_or(f64::NAN);
        let samples = match &input.data {
            FrameData::Audio { samples, .. } => f64::from(*samples),
            FrameData::Video { .. } | FrameData::Subtitle { .. } => f64::NAN,
        };

        let vars = [
            self.n,
            in_pts,
            self.start_pts.unwrap_or(f64::NAN),
            self.prev_in_pts,
            self.prev_out_pts,
            f64::NAN, // PREV_INT
            f64::NAN, // PREV_OUTT
            t,
            tb,
            f64::NAN, // RTCTIME
            f64::NAN, // RTCSTART
            self.frame_rate,
            self.sample_rate,
            0.0,      // INTERLACED
            f64::NAN, // POS
            f64::NAN, // S
            f64::NAN, // SR
            samples,
        ];
        let result = self.expr.eval(&vars);

        self.n += 1.0;
        self.prev_in_pts = in_pts;
        if result.is_finite() {
            let ticks = result.round() as i64;
            self.prev_out_pts = result;
            input.pts = Timestamp::new(ticks);
        } else {
            input.pts = Timestamp::NONE;
        }
        Ok(FrameOut::One(input))
    }

    fn flush_state(&mut self) {
        self.n = 0.0;
        self.start_pts = None;
        self.prev_in_pts = f64::NAN;
        self.prev_out_pts = f64::NAN;
    }
}

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let bindings = Bindings::new(VARS);
    let expr = Expr::parse(&opts.expr, &bindings)
        .map_err(|e| format!("bad `expr` expression `{}`: {e}", opts.expr))?;
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(Filter::new(expr))),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const VIDEO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "setpts",
        description: "Set PTS for the output video frame",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::TIMELINE_GENERIC,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Video, DESC, req)
    }
}

pub mod audio {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const AUDIO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "asetpts",
        description: "Set PTS for the output audio frame",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::TIMELINE_GENERIC,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Audio, DESC, req)
    }
}
