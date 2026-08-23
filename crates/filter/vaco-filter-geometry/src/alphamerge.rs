//! `alphamerge` — copy the second input's own plane 0 (its luma, for any
//! greyscale or YUV source) into the first input's alpha channel.
//!
//! `ffmpeg -h filter=alphamerge` documents no options of its own, plus the
//! shared `vaco-filter-framesync` surface (`eof_action`/`shortest`/
//! `repeatlast`/`ts_sync_mode`).
//!
//! # This is `vaco-filter-framesync`'s `Synced`, not `Paired`
//!
//! Measured: `alphamerge`'s `-h` output carries the framesync option
//! section **verbatim** — identical to `overlay`'s — unlike `framepack`'s
//! and `mergeplanes`', which carry no such section at all (see
//! [`vaco_filter_core::adapt::Paired`]'s own doc for that measurement).
//! So this filter is a `Synced` consumer exactly like
//! `vaco-filter-video-composite`'s `overlay`: input 0 (`main`) drives,
//! input 1 (`alpha`) is sampled — the trait's own dual-input default,
//! unchanged. It is registered here, in the crate the plan's row assigns
//! it to, but it is not a `Paired`/`Fanout` consumer at all; it is the
//! third measured data point (with `overlay`) that the framesync option
//! surface, not the crate a filter happens to live in, is what decides
//! which adapter a multi-input filter wants.
//!
//! # Scope: which formats this filter adds alpha to
//!
//! The reference accepts `alphamerge` on essentially any input and lets
//! its own format negotiation pick a compatible alpha-carrying pixel
//! format — measured: a `yuv420p` main stays planar (`yuva420p`), but an
//! `rgb24`/`gbrp` main is converted to **packed** `argb`, not `gbrap`,
//! because `argb` is the format the reference's `alphamerge` actually
//! declares support for. Reproducing that exact conversion choice needs
//! this crate's own negotiation model to express "prefers a packed
//! alpha-only format," which none of this crate's other filters do and
//! which is a larger design question than one filter's scope. This
//! implementation instead adds alpha **in place** — every supported input
//! format keeps its own layout and gains one more plane — which is
//! correct and lossless for the formats it does support (`yuv420p`,
//! `yuv422p`, `yuv444p`, their `10le` variants, and `gbrp`/`gbrp10le`) and
//! refuses (`Error::Unsupported`) anything else rather than attempting a
//! packed conversion this crate has not measured.

use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, Synced};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const INPUT_PADS: &[Pad] = &[
    Pad {
        name: "main",
        media_type: MediaType::Video,
    },
    Pad {
        name: "alpha",
        media_type: MediaType::Video,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "alphamerge",
    description: "Copy the luma value of the second input into the alpha channel of the first input",
    inputs: INPUT_PADS,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

/// `format`'s alpha-added counterpart, for the formats this filter
/// supports — see this module's doc for the scope cut.
fn alpha_variant(format: PixFmt) -> Option<PixFmt> {
    Some(match format {
        PixFmt::Yuv420p => PixFmt::Yuva420p,
        PixFmt::Yuv422p => PixFmt::Yuva422p,
        PixFmt::Yuv444p => PixFmt::Yuva444p,
        PixFmt::Yuv420p10le => PixFmt::Yuva420p10le,
        PixFmt::Yuv422p10le => PixFmt::Yuva422p10le,
        PixFmt::Yuv444p10le => PixFmt::Yuva444p10le,
        PixFmt::Gbrp => PixFmt::Gbrap,
        PixFmt::Gbrp10le => PixFmt::Gbrap10le,
        _ => return None,
    })
}

/// Copy every plane `main` already has into a fresh `out_format` frame,
/// leaving the new (alpha) plane for [`copy_alpha`] to fill. `out_format`
/// is always `main_fmt` plus exactly one more plane (see
/// [`alpha_variant`]), so this is a plain per-plane byte copy, not a
/// colour conversion.
fn reformat(pool: &FramePool, main: &Frame, out_format: PixFmt, main_fmt: PixFmt) -> Result<Frame> {
    let FrameData::Video { width, height, .. } = main.data else {
        return Err(Error::InvalidData("alphamerge: main input is not video"));
    };
    let mut out = pool.acquire_video(out_format, width, height)?;
    let planes = main_fmt.plane_count().min(out_format.plane_count());
    for p in 0..planes {
        let Some(src) = main.plane(p) else { continue };
        let Some(mut dst) = out.plane_mut(p) else {
            continue;
        };
        for y in 0..(height as usize) {
            let Some(src_row) = src.row(y) else { continue };
            if let Some(dst_row) = dst.row_mut(y) {
                let n = dst_row.len().min(src_row.len());
                if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
        }
    }
    out.pts = main.pts;
    out.time_base = main.time_base;
    out.duration = main.duration;
    out.color = main.color;
    out.sample_aspect_ratio = main.sample_aspect_ratio;
    Ok(out)
}

/// Copy `alpha`'s plane 0 into `out`'s last plane (the one [`alpha_variant`]
/// added).
#[allow(
    clippy::unnecessary_wraps,
    reason = "kept Result-shaped alongside reformat, which is genuinely fallible, for a matching call site"
)]
fn copy_alpha(alpha: &Frame, out: &mut Frame, out_format: PixFmt) -> Result<()> {
    let FrameData::Video { height, .. } = out.data else {
        return Ok(());
    };
    let alpha_plane = out_format.plane_count().saturating_sub(1);
    let Some(src) = alpha.plane(0) else {
        return Ok(());
    };
    let Some(mut dst) = out.plane_mut(alpha_plane) else {
        return Ok(());
    };
    for y in 0..(height as usize) {
        let Some(src_row) = src.row(y) else { continue };
        if let Some(dst_row) = dst.row_mut(y) {
            let n = dst_row.len().min(src_row.len());
            if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                d.copy_from_slice(s);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(crate) struct Alphamerge {
    /// Resolved once at `configure`. `None` until then.
    out_format: Option<PixFmt>,
    main_format: Option<PixFmt>,
}

impl FrameSyncFilter for Alphamerge {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format: main_format,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        let out_format = alpha_variant(main_format).ok_or(Error::Unsupported(
            "alphamerge: main input's format has no alpha-added variant this filter supports",
        ))?;
        self.main_format = Some(main_format);
        self.out_format = Some(out_format);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { format: f, .. } = &mut out {
                *f = out_format;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let (Some(out_format), Some(main_format)) = (self.out_format, self.main_format) else {
            return Ok(FrameOut::One(main));
        };
        let FrameData::Video { width, height, .. } = main.data else {
            return Ok(FrameOut::One(main));
        };
        let Some(alpha_frame) = event.get(1) else {
            // No secondary frame at this event: pass the main frame through,
            // reformatted but with whatever the pool handed the new plane
            // (typically zero-filled) rather than a real alpha value.
            let mut out = reformat(ctx.pool(), &main, out_format, main_format)?;
            out.pts = event.timestamp();
            out.time_base = event.time_base();
            return Ok(FrameOut::One(out));
        };
        let FrameData::Video {
            width: aw,
            height: ah,
            ..
        } = alpha_frame.data
        else {
            return Ok(FrameOut::One(main));
        };
        if aw != width || ah != height {
            return Err(Error::Unsupported(
                "alphamerge: main and alpha inputs must be the same size",
            ));
        }
        let mut out = reformat(ctx.pool(), &main, out_format, main_format)?;
        copy_alpha(alpha_frame, &mut out, out_format)?;
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "the FilterRegistry::create signature this crate dispatches through always returns Result"
)]
pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let _ = req;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Synced::new(Alphamerge::default())),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn every_supported_format_gains_exactly_one_plane() {
        for fmt in [
            PixFmt::Yuv420p,
            PixFmt::Yuv422p,
            PixFmt::Yuv444p,
            PixFmt::Yuv420p10le,
            PixFmt::Yuv422p10le,
            PixFmt::Yuv444p10le,
            PixFmt::Gbrp,
            PixFmt::Gbrp10le,
        ] {
            let out = alpha_variant(fmt).unwrap();
            assert_eq!(
                out.plane_count(),
                fmt.plane_count() + 1,
                "{fmt:?} -> {out:?}"
            );
        }
    }

    #[test]
    fn an_unsupported_format_is_a_clean_refusal() {
        assert_eq!(alpha_variant(PixFmt::Rgb24), None);
    }

    #[test]
    fn reformat_carries_every_existing_plane_and_copy_alpha_fills_the_new_one() {
        let pool = FramePool::default();
        let mut yuv = pool.acquire_video(PixFmt::Yuv420p, 2, 2).unwrap();
        for p in 0..3 {
            if let Some(mut plane) = yuv.plane_mut(p) {
                plane.fill(0x10 + p as u8);
            }
        }
        let mut alpha_src = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        if let Some(mut plane) = alpha_src.plane_mut(0) {
            plane.fill(0x77);
        }

        let mut out = reformat(&pool, &yuv, PixFmt::Yuva420p, PixFmt::Yuv420p).unwrap();
        assert_eq!(
            out.plane(0)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(0x10)
        );
        assert_eq!(
            out.plane(1)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(0x11)
        );
        assert_eq!(
            out.plane(2)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(0x12)
        );

        copy_alpha(&alpha_src, &mut out, PixFmt::Yuva420p).unwrap();
        assert_eq!(
            out.plane(3)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(0x77)
        );
    }
}
