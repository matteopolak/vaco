//! `select`/`aselect` — pick frames by expression.
//!
//! `ffmpeg -h filter=select` documents `expr`/`e` (default `"1"`, always
//! true) and `outputs`/`n` (default 1). The reference's documentation
//! (`filters.texi`) names the full variable set: `n`, `selected_n`,
//! `prev_selected_n`, `TB`, `pts`, `t`, `start_pts`, `start_t`, `pict_type`,
//! `interlace_type`, `key`, `pos` and `scene`. `pict_type`/`interlace_type`/
//! `key` still have no signal in this framework (no coded-picture-type or
//! field-order metadata reaches a filter); `pos` stays permanently `NaN`,
//! matching the reference's own current behaviour per plan 16 §1.10.1.
//! `scene` is implemented — see below.
//!
//! # Output routing: `ceil(val)-1`, not `round(val)-1`
//!
//! The reference's own documentation states the rule exactly: "if the
//! evaluation result is negative or NaN, the frame is sent to the first
//! output; otherwise it is sent to the output with index `ceil(val)-1`".
//! Measured directly against `ffmpeg 8.1` because a plausible-looking
//! `round(val)-1` reading (this module's previous implementation) agrees
//! with `ceil` on integers and disagrees on everything else — the
//! distinguishing case:
//!
//! ```text
//! select=outputs=3:expr='1.2'   # ceil(1.2)-1 = 1  -> second output (measured)
//!                                # round(1.2)-1 = 0 -> first output (what this
//!                                #   module used to compute — wrong)
//! select=outputs=3:expr='2.0'   # ceil(2.0)-1 = 1  -> second output (measured,
//!                                #   matches round too, so this case alone
//!                                #   would not have caught the bug)
//! select=outputs=3:expr='-0.5'  # negative -> first output (measured)
//! ```
//! Reproduced with `ffmpeg -filter_complex
//! "select=outputs=3:expr='1.2'[a][b][c]"` and three `-map`ped `rawvideo`
//! outputs: the frame lands in the second output file, never the first.
//!
//! # `scene`
//!
//! The reference's own `scene` value ("a value between 0 and 1 which
//! quantifies the difference between the current and the previous frame",
//! `filters.texi`) is computed by an internal metric this project cannot
//! read (D7). This crate uses [`vaco_filter_vdsp::normalised_sad`] — the
//! same 0.0..=1.0 frame-difference fraction `vaco-filter-temporal`'s
//! `freezedetect` already treats as its scene-difference signal — as a
//! structural stand-in, per this row's brief ("extend vdsp rather than
//! duplicating"; there is nothing to extend, since `normalised_sad` already
//! is the shared kernel). **Not verified bit-exact against the reference's
//! `scene` values** — only the *shape* (0 for identical frames, positive and
//! bounded by 1 for a full-frame change) is exercised. `scene` is audio-side
//! `NaN` (no luma plane to diff two audio frames on).
//!
//! With `outputs=1` (by far the common case) a frame passes when `expr`
//! evaluates non-zero and is dropped otherwise — exercised directly.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
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
    "scene",
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
    /// The previous frame, kept only to compute `scene` (video). `None` for
    /// `aselect`, and for the first frame of any stream.
    prev_frame: Option<Frame>,
    video: bool,
}

impl Filter {
    /// `scene`: [`vaco_filter_vdsp::normalised_sad`] between this frame's
    /// luma plane and the previous one's — see this module's doc for why
    /// this is a structural stand-in, not the reference's own metric.
    fn scene_value(&self, frame: &Frame) -> f64 {
        if !self.video {
            return f64::NAN;
        }
        let Some(prev) = &self.prev_frame else {
            return f64::NAN;
        };
        match (frame.plane(0), prev.plane(0)) {
            (Some(a), Some(b)) => vaco_filter_vdsp::normalised_sad(a, b),
            _ => f64::NAN,
        }
    }
}

/// `outputs>1` routing: reference documentation (`filters.texi`,
/// `select`/`aselect`) states it exactly — "if the evaluation result is
/// negative or NaN, the frame is sent to the first output; otherwise it is
/// sent to the output with index `ceil(val)-1`". Measured directly against
/// `ffmpeg 8.1`; see this module's doc comment for the distinguishing probe
/// against the `round(val)-1` reading this used to compute.
fn route_output(result: f64, outputs: usize) -> usize {
    let idx = if result.is_finite() && result >= 0.0 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "routing index, clamped to the pad range immediately below"
        )]
        let ceiled = result.ceil() as i64;
        ceiled.saturating_sub(1)
    } else {
        0
    }
    .clamp(0, i64::try_from(outputs).unwrap_or(i64::MAX) - 1);
    usize::try_from(idx).unwrap_or(0)
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
        let scene = self.scene_value(&input);
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
            scene,
        ];
        let result = self.expr.eval(&vars);
        self.n += 1.0;
        if self.video {
            self.prev_frame = Some(input.clone());
        }

        if self.outputs == 1 {
            if result != 0.0 {
                self.prev_selected_n = self.selected_n;
                self.selected_n += 1.0;
                ctx.push_output(0, input)?;
            }
        } else {
            let idx = route_output(result, self.outputs);
            self.prev_selected_n = self.selected_n;
            self.selected_n += 1.0;
            ctx.push_output(idx, input)?;
        }
        Ok(Activity::Progressed)
    }

    fn flush(&mut self) {
        self.n = 0.0;
        self.selected_n = 0.0;
        self.prev_frame = None;
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
        prev_frame: None,
        video: media == MediaType::Video,
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::float_cmp,
    reason = "test code; the float_cmp case asserts normalised_sad is exactly \
              0.0 on identical planes, which is the property under test"
)]
mod tests {
    use super::*;
    use vaco_filter_core::mock::gray_frame;

    /// The distinguishing case: `ceil(1.2)-1 = 1` and `round(1.2)-1 = 0`
    /// disagree, so this alone would have caught this module's previous
    /// `round`-based routing. Measured against `ffmpeg 8.1` (see this
    /// module's doc comment): `select=outputs=3:expr='1.2'` lands the frame
    /// in the *second* mapped output, never the first.
    #[test]
    fn routes_by_ceil_not_round() {
        assert_eq!(route_output(1.2, 3), 1);
    }

    /// A case where `ceil` and `round` happen to agree, kept as a second,
    /// independent confirmation rather than the whole story.
    #[test]
    fn routes_an_exact_integer_the_same_under_either_reading() {
        assert_eq!(route_output(2.0, 3), 1);
    }

    #[test]
    fn negative_result_goes_to_the_first_output() {
        assert_eq!(route_output(-0.5, 3), 0);
    }

    #[test]
    fn nan_result_goes_to_the_first_output() {
        assert_eq!(route_output(f64::NAN, 3), 0);
    }

    #[test]
    fn routing_never_exceeds_the_pad_range() {
        assert_eq!(route_output(1000.0, 3), 2);
    }

    fn scene_filter() -> Filter {
        let bindings = Bindings::new(VARS);
        Filter {
            expr: Expr::parse("scene", &bindings).unwrap(),
            outputs: 1,
            n: 0.0,
            selected_n: 0.0,
            prev_selected_n: 0.0,
            start_pts: None,
            start_t: None,
            prev_frame: None,
            video: true,
        }
    }

    /// `scene` has no predecessor to diff against on the first frame of a
    /// stream, so it must be `NaN` — not a silent `0`, which would read as
    /// "no change" rather than "no signal yet".
    #[test]
    fn scene_is_nan_with_no_previous_frame() {
        let filter = scene_filter();
        let f = gray_frame(4, 4, 0, 0x40);
        assert!(filter.scene_value(&f).is_nan());
    }

    #[test]
    fn scene_is_zero_for_two_identical_frames() {
        let mut filter = scene_filter();
        filter.prev_frame = Some(gray_frame(4, 4, 0, 0x40));
        let f = gray_frame(4, 4, 1, 0x40);
        assert_eq!(filter.scene_value(&f), 0.0);
    }

    #[test]
    fn scene_is_positive_and_bounded_for_two_different_frames() {
        let mut filter = scene_filter();
        filter.prev_frame = Some(gray_frame(4, 4, 0, 0x00));
        let f = gray_frame(4, 4, 1, 0xff);
        let value = filter.scene_value(&f);
        assert!(value > 0.0 && value <= 1.0);
    }

    #[test]
    fn scene_is_nan_for_audio() {
        let mut filter = scene_filter();
        filter.video = false;
        filter.prev_frame = Some(gray_frame(4, 4, 0, 0x40));
        let f = gray_frame(4, 4, 1, 0x40);
        assert!(filter.scene_value(&f).is_nan());
    }
}
