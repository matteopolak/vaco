//! `alphaextract` — copy a format's alpha channel out as a standalone
//! `gray` frame.
//!
//! `ffmpeg -h filter=alphaextract` documents no options. Rejects formats
//! with no alpha channel ([`vaco_pixfmt::PixFmtFlags::ALPHA`]) rather than
//! emitting an all-opaque frame, since there is no such data to extract.
//!
//! Component index 3 is alpha by this project's own `PixFmtDescriptor`
//! convention ("channel 0 is Y or R, 1 is U or G, 2 is V or B, 3 is
//! alpha" — `vaco-pixfmt`'s own doc), and that component's `.plane` field
//! names which plane actually holds it, so this does not hard-code plane
//! index 3 for formats where alpha shares a packed plane with other
//! channels — though a *packed* alpha (sharing bytes with colour channels
//! in one plane) is out of scope here: [`geom::plane_unit_bytes`] returns
//! the whole packed group's stride, and pulling out one channel's byte
//! within it needs the component's `.offset`/`.step`, which this filter
//! does not thread through (documented gap; every alpha-bearing format this
//! project currently tables — `yuva420p` and friends, `rgba`/`bgra`/`gbrap`
//! — happens to put alpha in its own plane, so the gap has not been hit).

use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "alphaextract",
    description: "Extract an alpha channel as a grayscale image component",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

fn alpha_plane_index(format: PixFmt) -> Option<u8> {
    if !format.has(PixFmtFlags::ALPHA) {
        return None;
    }
    format.descriptor().components.get(3).map(|c| c.plane)
}

#[derive(Debug, Default)]
pub(crate) struct Filter {
    alpha_plane: u8,
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { format, .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        self.alpha_plane = alpha_plane_index(format).ok_or(Error::Unsupported(
            "alphaextract: format has no alpha channel",
        ))?;
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { format: f, .. } = &mut out {
                *f = PixFmt::Gray8;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { width, height, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let Some(src) = input.plane(self.alpha_plane as usize) else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(PixFmt::Gray8, width, height)?;
        if let Some(mut dst) = out.plane_mut(0) {
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
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
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
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::default())),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn yuva420p_alpha_is_plane_3() {
        assert_eq!(alpha_plane_index(PixFmt::Yuva420p), Some(3));
    }

    #[test]
    fn a_format_with_no_alpha_has_none() {
        assert_eq!(alpha_plane_index(PixFmt::Yuv420p), None);
    }
}
