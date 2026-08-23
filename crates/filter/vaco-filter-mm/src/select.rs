//! `select`/`aselect` — pick frames by expression.
//!
//! `ffmpeg -h filter=select` documents `expr`/`e` (default `"1"`, always
//! true) and `outputs`/`n` (default 1). The reference's full documented
//! variable set for `select` includes several video-only fields
//! (`pict_type`, `interlace_type`, `key`, `scene`) this crate has no signal
//! for yet; implemented here: `n`, `selected_n`, `prev_selected_n`, `pts`,
//! `t`, `tb`, `start_pts`, `start_t`, `pos` (permanently `NaN`, matching the
//! reference's own current behaviour per plan 16 §1.10.1).
//!
//! With `outputs=1` (by far the common case) a frame passes when `expr`
//! evaluates non-zero and is dropped otherwise — exercised directly.
//! `outputs>1` routes each frame to output `round(expr) - 1`, clamped to the
//! valid pad range; this is a structural reading of "select among outputs",
//! not measured against the reference's own multi-output semantics.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

const VARS: &[&str] = &[
    "n",
    "selected_n",
    "prev_selected_n",
    "pts",
    "t",
    "tb",
    "start_pts",
    "start_t",
    "pos",
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "select", help = "select frames to pass in output")]
pub(crate) struct Opts {
    #[opt(
        name = "expr",
        alias = "e",
        help = "expression to use for selecting frames",
        default = "1".to_owned(),
        flags(filtering)
    )]
    pub expr: String,

    #[opt(
        name = "outputs",
        alias = "n",
        help = "number of outputs",
        default = 1,
        range = 1..=4096,
        flags(filtering)
    )]
    pub outputs: i32,
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
    outputs: usize,
    n: f64,
    selected_n: f64,
    prev_selected_n: f64,
    start_pts: Option<f64>,
    start_t: Option<f64>,
}

impl vaco_filter_core::Filter for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<vaco_filter_core::Activity> {
        use vaco_filter_core::Activity;

        if (0..self.outputs).any(|p| !ctx.output_has_room(p)) {
            return Ok(if ctx.output_closed(0) {
                Activity::Eof
            } else {
                Activity::Blocked
            });
        }
        let Some(input) = ctx.take_input(0) else {
            if ctx.input_at_eof(0) {
                ctx.close_all_outputs();
                return Ok(Activity::Eof);
            }
            ctx.forward_wanted();
            return Ok(Activity::NeedInput);
        };

        let pts = input.pts.ticks().map_or(f64::NAN, |t| t as f64);
        let t = input.pts.to_seconds(input.time_base).unwrap_or(f64::NAN);
        if self.start_pts.is_none() && pts.is_finite() {
            self.start_pts = Some(pts);
        }
        if self.start_t.is_none() && t.is_finite() {
            self.start_t = Some(t);
        }
        let vars = [
            self.n,
            self.selected_n,
            self.prev_selected_n,
            pts,
            t,
            input.time_base.to_f64(),
            self.start_pts.unwrap_or(f64::NAN),
            self.start_t.unwrap_or(f64::NAN),
            f64::NAN,
        ];
        let result = self.expr.eval(&vars);
        self.n += 1.0;

        if self.outputs == 1 {
            if result != 0.0 {
                self.prev_selected_n = self.selected_n;
                self.selected_n += 1.0;
                ctx.push_output(0, input)?;
            }
        } else if result.is_finite() {
            let idx = (result.round() as i64 - 1)
                .clamp(0, i64::try_from(self.outputs).unwrap_or(i64::MAX) - 1);
            self.prev_selected_n = self.selected_n;
            self.selected_n += 1.0;
            ctx.push_output(usize::try_from(idx).unwrap_or(0), input)?;
        }
        Ok(Activity::Progressed)
    }

    fn flush(&mut self) {
        self.n = 0.0;
        self.selected_n = 0.0;
        self.prev_selected_n = 0.0;
        self.start_pts = None;
        self.start_t = None;
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
    let outputs = usize::try_from(opts.outputs.max(1)).unwrap_or(1);
    let output_pads =
        pads::of(media, outputs).ok_or_else(|| "select: too many outputs".to_owned())?;
    let filter = Filter {
        expr,
        outputs,
        n: 0.0,
        selected_n: 0.0,
        prev_selected_n: 0.0,
        start_pts: None,
        start_t: None,
    };
    Ok(Instance {
        desc: FilterDesc {
            outputs: output_pads,
            ..desc
        },
        formats: NodeFormats {
            inputs: vec![vaco_filter_core::negotiate::FormatSet::default()],
            outputs: (0..outputs)
                .map(|_| vaco_filter_core::negotiate::FormatSet::default())
                .collect(),
            ties: {
                let mut pads_list: SmallVec<[(vaco_filter_core::link::Direction, u32); 8]> =
                    SmallVec::new();
                pads_list.push((vaco_filter_core::link::Direction::Input, 0));
                for i in 0..outputs {
                    pads_list.push((vaco_filter_core::link::Direction::Output, i as u32));
                }
                vaco_filter_core::negotiate::Tie::all_pads(1, outputs, media)
                    .into_iter()
                    .map(|mut tie| {
                        tie.pads = pads_list.iter().copied().collect();
                        tie
                    })
                    .collect()
            },
            label: req.instance.to_owned(),
        },
        filter: Box::new(filter),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const VIDEO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "select",
        description: "Select video frames to pass in output",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
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
        name: "aselect",
        description: "Select audio frames to pass in output",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Audio, DESC, req)
    }
}
