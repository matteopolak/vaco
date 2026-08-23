//! `varblur` — box blur whose radius is read per-pixel from a second video
//! stream.
//!
//! `ffmpeg -h filter=varblur` documents `min_r` (`0..=254`, default `0`),
//! `max_r` (`1..=255`, default `8`) and `planes` (default `15`), plus the
//! shared `framesync` surface. Two inputs, named `default` and `radius`.
//!
//! # Two inputs: `vaco-filter-framesync`
//!
//! Same shape as [`crate`]'s sibling filters that gained a second input this
//! pass — a thin [`FrameSyncFilter`] over [`FsInput::dual`]'s roles (input 0
//! drives, input 1 is sampled), following
//! `vaco-filter-video-composite::overlay`'s pattern rather than the
//! not-yet-landed `Paired<F>` adapter
//! (`planning/INTERFACE-GAPS.md` gap 10).
//!
//! # Structural, not framecrc-verified
//!
//! Measured (`ffmpeg 8.1`, `yuv420p`, a single-column impulse against a
//! *constant* radius-map value): even a radius map that reads `0`
//! everywhere (which, with `min_r=0`, should mean "no blur, identity")
//! measurably spreads the impulse over two adjacent output columns at equal
//! weight, rather than leaving it untouched — refuting the simplest
//! reading of the option ("`min_r`/`max_r` linearly rescale the control
//! plane's raw byte value to a per-pixel integer radius, applied as an
//! ordinary box average") as an *exact* description, though it is clearly
//! close to whatever the reference actually does. `gray8` inputs to this
//! same probe produced an all-zero output outright (the reference's
//! `varblur` appears not to support that pixel format at all, unlike every
//! other filter in this crate); this crate's own 8-bit-only scope does not
//! otherwise restrict which 8-bit format is accepted, so nothing here
//! reproduces that gap deliberately, it is simply unexplained.
//!
//! This ships the straightforward reading — `radius(x,y) = round(min_r +
//! (max_r - min_r) * ctrl(x,y) / 255)`, then an ordinary clamp-bordered,
//! truncating box average ([`common::box_pass`], [`crate::avgblur`]'s
//! convention) at that radius — verified via the invariants below, and
//! documented here as not reconciled with the two anomalies above rather
//! than silently presented as exact.
//!
//! # Verified: a constant main field is a fixed point at any radius
//!
//! The average of a constant is that constant, independent of window size —
//! true regardless of how the per-pixel radius is actually computed.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const PADS: &[Pad] = &[
    Pad {
        name: "default",
        media_type: MediaType::Video,
    },
    Pad {
        name: "radius",
        media_type: MediaType::Video,
    },
];
const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "varblur",
    description: "Apply Variable Blur filter",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "varblur", help = "Apply Variable Blur filter")]
pub(crate) struct Opts {
    #[opt(name = "min_r", help = "set min blur radius", default = 0, range = 0..=254, flags(video, filtering))]
    pub min_r: i32,
    #[opt(name = "max_r", help = "set max blur radius", default = 8, range = 1..=255, flags(video, filtering))]
    pub max_r: i32,
    #[opt(name = "planes", help = "set planes to filter", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
    #[opt(
        name = "eof_action",
        help = "action to take when encountering EOF from secondary input",
        default = "repeat".to_owned(),
        flags(video, filtering)
    )]
    pub eof_action: String,
    #[opt(
        name = "shortest",
        help = "force termination when the shortest input terminates",
        default = false,
        flags(video, filtering)
    )]
    pub shortest: bool,
    #[opt(
        name = "repeatlast",
        help = "extend last frame of secondary streams beyond EOF",
        default = true,
        flags(video, filtering)
    )]
    pub repeatlast: bool,
    #[opt(
        name = "ts_sync_mode",
        help = "how strictly to sync streams based on secondary input timestamps",
        default = "default".to_owned(),
        flags(video, filtering)
    )]
    pub ts_sync_mode: String,
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

/// Per-pixel radius from the control plane: linear rescale of its byte value
/// into `[min_r, max_r]`.
fn radius_at(ctrl_rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32, min_r: i32, max_r: i32) -> i32 {
    let v = f64::from(common::sample_clamped(ctrl_rows, x, y, w, h));
    let r = f64::from(min_r) + (f64::from(max_r) - f64::from(min_r)) * v / 255.0;
    r.round() as i32
}

fn blur_plane(
    rows: &[&[u8]],
    ctrl_rows: &[&[u8]],
    w: i32,
    h: i32,
    ctrl_w: i32,
    ctrl_h: i32,
    min_r: i32,
    max_r: i32,
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            // The control plane may be a different size; sample it at the
            // same relative position, clamped.
            #[allow(
                clippy::integer_division,
                reason = "rescaling a pixel coordinate into a differently-sized \
                          control plane: truncation is the intended nearest-below \
                          mapping, not a precision bug"
            )]
            let (cx, cy) = (
                if w > 0 {
                    x * ctrl_w.max(1) / w.max(1)
                } else {
                    0
                },
                if h > 0 {
                    y * ctrl_h.max(1) / h.max(1)
                } else {
                    0
                },
            );
            let r = radius_at(ctrl_rows, cx, cy, ctrl_w, ctrl_h, min_r, max_r).max(0);
            if r == 0 {
                row.push(common::sample_clamped(rows, x, y, w, h));
                continue;
            }
            let count = i64::from(2 * r + 1) * i64::from(2 * r + 1);
            let mut sum: i64 = 0;
            for dy in -r..=r {
                for dx in -r..=r {
                    sum += i64::from(common::sample_clamped(rows, x + dx, y + dy, w, h));
                }
            }
            let avg = if count > 0 {
                #[allow(clippy::integer_division, reason = "computing an average")]
                {
                    sum / count
                }
            } else {
                sum
            };
            row.push(u8::try_from(avg.clamp(0, 255)).unwrap_or(255));
        }
        out.push(row);
    }
    out
}

#[derive(Debug)]
pub(crate) struct VarBlur {
    min_r: i32,
    max_r: i32,
    planes: i64,
    fs_opts: FrameSyncOpts,
}

impl VarBlur {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let eof_action = vaco_filter_framesync::EofAction::from_name(&opts.eof_action)
            .ok_or_else(|| format!("varblur: bad `eof_action` `{}`", opts.eof_action))?;
        let ts_sync = vaco_filter_framesync::TsSyncMode::from_name(&opts.ts_sync_mode)
            .ok_or_else(|| format!("varblur: bad `ts_sync_mode` `{}`", opts.ts_sync_mode))?;
        Ok(Self {
            min_r: opts.min_r,
            max_r: opts.max_r,
            planes: opts.planes,
            fs_opts: FrameSyncOpts {
                eof_action,
                shortest: opts.shortest,
                repeatlast: opts.repeatlast,
                ts_sync,
            },
        })
    }

    #[must_use]
    fn boxed(self) -> Box<Synced<Self>> {
        Box::new(Synced::new(self))
    }
}

impl FrameSyncFilter for VarBlur {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        self.fs_opts
    }

    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, .. } = main.data else {
            return Ok(FrameOut::One(main));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(main));
        }
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(main));
        };
        let ctrl_frame = event.get(1);
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(src_plane) = main.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let filtered = if common::plane_selected(self.planes, p8) {
                if let Some(ctrl) = ctrl_frame
                    && let FrameData::Video {
                        width: cw,
                        height: ch,
                        ..
                    } = ctrl.data
                    && let Some(ctrl_plane) = ctrl.plane(0)
                {
                    let ctrl_w = common::to_i32(cw);
                    let ctrl_h = common::to_i32(ch);
                    let ctrl_rows = common::collect_rows(ctrl_plane, ctrl_h.max(0) as usize);
                    blur_plane(
                        &rows, &ctrl_rows, pw, ph, ctrl_w, ctrl_h, self.min_r, self.max_r,
                    )
                } else {
                    rows.iter().map(|r| (*r).to_vec()).collect()
                }
            } else {
                rows.iter().map(|r| (*r).to_vec()).collect()
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in filtered.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        common::copy_frame_meta(&mut out, &main);
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = VarBlur::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: filter.boxed(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn radius_zero_is_identity_at_that_pixel() {
        let ctrl_row: &[u8] = &[0];
        let ctrl_rows: [&[u8]; 1] = [ctrl_row];
        let row0: &[u8] = &[1, 2, 3];
        let rows: [&[u8]; 1] = [row0];
        let out = blur_plane(&rows, &ctrl_rows, 3, 1, 1, 1, 0, 8);
        assert_eq!(out[0], vec![1, 2, 3]);
    }

    /// Independent oracle: a constant main field is a fixed point of the
    /// average regardless of the per-pixel radius the control plane picks.
    #[test]
    fn a_constant_main_field_is_always_a_fixed_point() {
        let ctrl_owned = vec![vec![200u8; 7]; 7];
        let ctrl_rows: Vec<&[u8]> = ctrl_owned.iter().map(Vec::as_slice).collect();
        let rows_owned = vec![vec![50u8; 7]; 7];
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let out = blur_plane(&rows, &ctrl_rows, 7, 7, 7, 7, 0, 8);
        for row in out {
            for v in row {
                assert_eq!(v, 50);
            }
        }
    }

    #[test]
    fn radius_at_rescales_linearly() {
        assert_eq!(radius_at(&[&[0]], 0, 0, 1, 1, 0, 8), 0);
        assert_eq!(radius_at(&[&[255]], 0, 0, 1, 1, 0, 8), 8);
    }
}
