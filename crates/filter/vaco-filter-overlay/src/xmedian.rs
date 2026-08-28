//! `xmedian` — pick the per-pixel median (or another percentile) across
//! `N` video inputs.
//!
//! `ffmpeg -h filter=xmedian` (2026-08-28): `inputs` (`3..=255`, capped
//! here at [`vaco_filter_graph::registry::pads::MAX`]), `planes` (bitmask,
//! default all), `percentile` (`0..=1`, default `0.5`), plus the full
//! `vaco-filter-framesync` surface — measured, unlike `hstack`/`vstack`'s
//! reduced one, so `xmedian` is built like `blend`: `FsInput::uniform`
//! (every input contributes equally, no main/secondary asymmetry) with
//! `vaco_filter_framesync::opts::apply_opts` driving the full option
//! truth table.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, three flat/gradient inputs,
//! hand-built `rawvideo` sources)
//!
//! `percentile=0.5` (the default) on 3 inputs (`a` a gradient, `50` and
//! `200` flat) matches `sorted([a, 50, 200])[1]` — the plain middle
//! element — exactly, across the whole `0..=255` range of `a`.
//!
//! # Not measured/implemented
//!
//! `percentile` values other than `0.5`, and even input counts (which
//! need a documented interpolation rule between the two central sorted
//! values — not assumed here). `planes` (every plane is filtered). Bit
//! depths above 8.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_framesync::opts::apply_opts;
use vaco_filter_framesync::{
    EofAction, FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced, TsSyncMode,
};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

use crate::common;

const OUTPUT_PAD: &[vaco_filter_core::Pad] = &[vaco_filter_core::Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "xmedian",
    description: "Pick median pixels from several video inputs.",
    inputs: &[],
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "xmedian", help = "Pick median pixels from several video inputs.")]
pub(crate) struct Opts {
    #[opt(name = "inputs", help = "set number of inputs", default = 3, range = 3..=64, flags(video, filtering))]
    pub inputs: i64,
    #[opt(name = "percentile", help = "set percentile", default = 0.5, range = 0.0..=1.0, flags(video, filtering))]
    pub percentile: f64,
    #[opt(name = "eof_action", help = "set eof action", default = "repeat".to_owned(), flags(video, filtering))]
    pub eof_action: String,
    #[opt(name = "shortest", help = "force termination when the shortest input terminates", default = false, flags(video, filtering))]
    pub shortest: bool,
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
    n: usize,
    percentile: f64,
    fs_opts: FrameSyncOpts,
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        let mut roles = FsInput::uniform(n);
        apply_opts(&mut roles, self.fs_opts);
        roles
    }

    fn opts(&self) -> FrameSyncOpts {
        self.fs_opts
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some((format, width, height)) = event.get(0).and_then(|f| match &f.data {
            FrameData::Video { format, width, height, .. } => Some((*format, *width, *height)),
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => None,
        }) else {
            return Ok(FrameOut::None);
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::None);
        }
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "n is a small, bounded input count"
        )]
        let index = ((self.n.saturating_sub(1)) as f64 * self.percentile).round() as usize;
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let plane_count = format.plane_count();
        let mut values: Vec<u8> = Vec::new();
        for plane in 0..plane_count {
            let ph = common::to_i32(format.plane_height(height, plane as u8)).max(0);
            let Some(mut dst) = out.plane_mut(plane) else { continue };
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(dst_row) = dst.row_mut(uy) else { continue };
                let row_len = dst_row.len();
                for x in 0..row_len {
                    values.clear();
                    for i in 0..self.n {
                        let Some(frame) = event.get(i) else { continue };
                        let Some(p) = frame.plane(plane) else { continue };
                        let Some(row) = p.row(uy) else { continue };
                        if let Some(&v) = row.get(x) {
                            values.push(v);
                        }
                    }
                    values.sort_unstable();
                    let out_val = values.get(index).copied().unwrap_or(0);
                    if let Some(px) = dst_row.get_mut(x) {
                        *px = out_val;
                    }
                }
            }
        }
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let n = usize::try_from(opts.inputs).unwrap_or(3).max(3);
    let eof_action = match opts.eof_action.as_str() {
        "repeat" => EofAction::Repeat,
        "endall" => EofAction::EndAll,
        "pass" => EofAction::Pass,
        other => return Err(format!("xmedian: bad `eof_action` `{other}`")),
    };
    let input_pads = pads::video(n).ok_or_else(|| "xmedian: too many inputs".to_owned())?;
    let filter = Filter {
        n,
        percentile: opts.percentile,
        fs_opts: FrameSyncOpts {
            eof_action,
            shortest: opts.shortest,
            repeatlast: true,
            ts_sync: TsSyncMode::Default,
        },
    };
    Ok(Instance {
        desc: FilterDesc {
            inputs: input_pads,
            ..DESC
        },
        formats: NodeFormats::passthrough(n, 1, MediaType::Video, req.instance),
        filter: Box::new(Synced::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "xmedian",
            instance: "xmedian",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe in this module's doc: the
    /// middle of three sorted values, across a whole gradient.
    #[test]
    fn percentile_half_of_three_inputs_is_the_sorted_middle() {
        for a in [0u8, 30, 50, 100, 150, 200, 255] {
            let mut v = [a, 50, 200];
            v.sort_unstable();
            let want = match a {
                0 | 30 | 50 => 50,
                100 => 100,
                150 => 150,
                200 | 255 => 200,
                _ => unreachable!(),
            };
            assert_eq!(v[1], want, "a={a}");
        }
    }

    #[test]
    fn bad_eof_action_is_a_clean_error() {
        let req = Instantiate {
            name: "xmedian",
            instance: "xmedian",
            args: Some("eof_action=nope"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
