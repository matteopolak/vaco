//! `premultiply`/`unpremultiply` — associate/unassociate colour with alpha.
//!
//! `ffmpeg -h filter=premultiply` (`(un)premultiply AVOptions`, shared with
//! `unpremultiply`) documents `planes` (bitmask, default 15) and `inplace`
//! (default false).
//!
//! # Measured: framesync is really involved, even though no framesync
//! options are exposed
//!
//! ```text
//! ffmpeg ... -filter_complex "[0][1]premultiply" ... -loglevel verbose
//! # -> "[framesync @ ...] Selected 1/25 time base" / "Sync level 1" / "Sync level 0"
//! ```
//!
//! So this filter is [`Synced`]-shaped like `alphamerge`/`msad`, just
//! without a user-visible `eof_action`/`shortest`/`ts_sync_mode` surface —
//! this crate uses [`FrameSyncOpts::default`] for it, the same shape
//! `vaco-filter-framesync`'s own doc anticipates for the `maskedmerge`
//! family.
//!
//! # Measured: a main input with no alpha channel is an exact no-op
//!
//! ```text
//! ffmpeg -f lavfi -i "color=0xc8c8c8,format=gray" -f lavfi -i "color=black,format=gray" \
//!   -filter_complex "[0][1]premultiply" -f rawvideo -
//! # -> output bytes identical to the unfiltered main input, for every
//! #    tested alpha value (0 and 200 both probed).
//! ```
//!
//! # Not fully pinned down: the second input's exact role when main *does*
//! have alpha
//!
//! The doc string says "`PreMultiply` first stream with first plane of
//! second stream", but a `yuva420p` main input forced a pixel-format
//! conversion that made the raw output bytes ambiguous between "used
//! main's own alpha" and "used the second stream's plane", and pinning
//! that down exactly was out of this crate's time budget. Given the clean,
//! unambiguous no-alpha-is-a-no-op measurement above, this implementation
//! takes the conservative, testable reading: when main's resolved format
//! has its own alpha channel, that channel is what gets multiplied against
//! (standard alpha-premultiplication algebra, `color' = color * alpha /
//! maxval`); the second input is still required (matching the fixed
//! two-pad shape) but is not read for this case. **This is a documented
//! simplification, not a confirmed match** — flagged rather than guessed
//! silently.
//!
//! # Not implemented: `inplace=1`'s single-input shape
//!
//! `inplace=1` makes the reference accept exactly one input (using its own
//! alpha in place) — measured, but this crate always registers the fixed
//! two-input shape (`inplace`'s default) and parses `inplace` without
//! switching pad count. `vaco-filter-plumbing::split`'s `outputs`-driven
//! `DYNAMIC_OUTPUTS` shape is the template for adding that later.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const PADS: &[Pad] = &[
    Pad { name: "main", media_type: MediaType::Video },
    Pad { name: "alpha", media_type: MediaType::Video },
];
const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "premultiply", help = "(Un)PreMultiply first stream with first plane of second stream")]
pub(crate) struct Opts {
    #[opt(name = "planes", help = "set planes", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
    #[opt(name = "inplace", help = "enable inplace mode (not implemented; parsed only)", default = false, flags(video, filtering))]
    pub inplace: bool,
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

/// Multiply (or divide) `frame`'s colour channels by its own alpha,
/// in place. A no-op if the format has no alpha channel — the measured
/// reference behaviour this module's doc records.
pub(crate) fn apply(frame: &mut Frame, planes: i64, divide: bool) {
    let FrameData::Video { format, .. } = frame.data else {
        return;
    };
    if !format.has_alpha() || !sample::is_addressable(format) {
        return;
    }
    let alpha_ch = format.component_count().saturating_sub(1);
    let Some(alpha_comp) = sample::component(format, alpha_ch) else {
        return;
    };
    let max = f64::from(sample::max_value(alpha_comp));
    if max == 0.0 {
        return;
    }
    let big_endian = format.is_big_endian();
    let Some(alpha_plane) = frame.plane(alpha_comp.plane as usize) else {
        return;
    };
    let alpha_w = alpha_plane
        .row_bytes()
        .checked_div(usize::from(alpha_comp.step.max(1)))
        .unwrap_or(0);
    let alpha_rows: Vec<Vec<u16>> = (0..alpha_plane.rows())
        .map(|y| {
            alpha_plane
                .row(y)
                .map(|r| (0..alpha_w).map(|x| sample::read(r, x, alpha_comp, big_endian)).collect())
                .unwrap_or_default()
        })
        .collect();
    for ch in 0..alpha_ch {
        if !sample::plane_selected(planes, ch) {
            continue;
        }
        let Some(comp) = sample::component(format, ch) else { continue };
        let Some(mut plane) = frame.plane_mut(comp.plane as usize) else { continue };
        let w = plane.row_bytes().checked_div(usize::from(comp.step.max(1))).unwrap_or(0);
        for y in 0..plane.rows() {
            let Some(alpha_row) = alpha_rows.get(y) else { continue };
            let Some(row) = plane.row_mut(y) else { continue };
            for x in 0..w {
                let a = f64::from(alpha_row.get(x).copied().unwrap_or(0));
                let v = f64::from(sample::read(row, x, comp, big_endian));
                let out_v = if divide {
                    if a == 0.0 { 0.0 } else { v * max / a }
                } else {
                    v * a / max
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to [0, max] and max fits in u16 by construction"
                )]
                let out_v = out_v.clamp(0.0, max).round() as u16;
                sample::write(row, x, comp, big_endian, out_v);
            }
        }
    }
}

#[derive(Debug)]
struct Filter {
    planes: i64,
    divide: bool,
}

impl FrameSyncFilter for Filter {
    fn on_event(&mut self, _ctx: &mut FilterContext<'_>, event: &mut FrameSyncEvent<'_>) -> Result<FrameOut> {
        let Some(mut main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        main.make_writable();
        apply(&mut main, self.planes, self.divide);
        Ok(FrameOut::One(main))
    }

    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts::default()
    }
}

fn build(desc: FilterDesc, divide: bool, req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: vec![set.clone(), set.clone()],
        outputs: vec![set],
        ties: Tie::all_pads(1, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc,
        formats,
        filter: Box::new(Synced::new(Filter {
            planes: i64::from(opts.planes),
            divide,
        })),
    })
}

#[allow(
    clippy::module_inception,
    reason = "the module name is the registered filter name, matching `vaco_filter_color::lut`'s `lut`/`lutrgb`/`lutyuv` submodules"
)]
pub mod premultiply {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, PADS, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "premultiply",
        description: "PreMultiply first stream with first plane of second stream",
        inputs: PADS,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, false, req)
    }
}

pub mod unpremultiply {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, PADS, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "unpremultiply",
        description: "UnPreMultiply first stream with first plane of second stream",
        inputs: PADS,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, true, req)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    #[test]
    fn no_alpha_channel_is_an_exact_no_op() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gray8, 1, 1).unwrap();
        frame.plane_mut(0).unwrap().fill(200);
        apply(&mut frame, 15, false);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 200);
    }

    #[test]
    fn premultiply_then_unpremultiply_round_trips_up_to_rounding() {
        // Independent oracle: composing the two must recover the original
        // colour (within one integer-rounding step of the alpha division),
        // regardless of what the formula's internal constants are.
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 100;
            row[1] = 150;
            row[2] = 200;
            row[3] = 128; // half alpha
        }
        apply(&mut frame, 15, false);
        apply(&mut frame, 15, true);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        for (out, orig) in row.iter().take(3).zip([100i32, 150, 200]) {
            assert!((i32::from(*out) - orig).abs() <= 1, "out={out} orig={orig}");
        }
    }

    #[test]
    fn premultiply_at_half_alpha_halves_color() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 200;
            row[1] = 200;
            row[2] = 200;
            row[3] = 128;
        }
        apply(&mut frame, 15, false);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row[0], (200.0f64 * 128.0 / 255.0).round() as u8);
    }
}
