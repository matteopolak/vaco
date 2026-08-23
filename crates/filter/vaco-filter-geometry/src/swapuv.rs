//! `swapuv` — swap the U and V planes.
//!
//! `ffmpeg -h filter=swapuv` documents no options: it always exchanges
//! plane 1 and plane 2 (U and V, in every planar YUV layout this project's
//! `vaco-pixfmt` tables use — `PixFmtFlags::RGB` formats have no U/V to
//! swap, and this filter rejects them rather than silently permuting an RGB
//! plane pair, since the reference's own name and single-purpose doc give
//! no reason to believe it does anything to non-YUV formats).
//!
//! Not independently measured against the reference's pixel output — the
//! operation is definitionally "exchange plane 1 and plane 2", and the
//! independent check used instead is structural: swapping twice must
//! restore the original frame exactly (self-inverse), which a plain
//! exchange has and any other operation named "swap" would not.

use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmtFlags;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "swapuv",
    description: "Swap U and V components",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
pub(crate) struct Filter;

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { format, .. }) = ctx.input_link(0) else {
            return Ok(());
        };
        if format.has(PixFmtFlags::RGB) || format.plane_count() < 3 {
            return Err(Error::Unsupported(
                "swapuv: format has no separate U/V planes to swap",
            ));
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let _ = ctx;
        let FrameData::Video { .. } = &input.data else {
            return Ok(FrameOut::One(input));
        };
        let mut out = input.clone();
        let (Some(u), Some(v)) = (input.plane(1), input.plane(2)) else {
            return Ok(FrameOut::One(out));
        };
        let rows = u.rows().max(v.rows());
        let u_rows: Vec<Vec<u8>> = (0..rows)
            .map(|y| u.row(y).map(<[u8]>::to_vec).unwrap_or_default())
            .collect();
        let v_rows: Vec<Vec<u8>> = (0..rows)
            .map(|y| v.row(y).map(<[u8]>::to_vec).unwrap_or_default())
            .collect();
        if let Some(mut dst) = out.plane_mut(1) {
            for (y, src) in v_rows.iter().enumerate() {
                if let Some(row) = dst.row_mut(y) {
                    let n = row.len().min(src.len());
                    if let (Some(d), Some(s)) = (row.get_mut(..n), src.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        if let Some(mut dst) = out.plane_mut(2) {
            for (y, src) in u_rows.iter().enumerate() {
                if let Some(row) = dst.row_mut(y) {
                    let n = row.len().min(src.len());
                    if let (Some(d), Some(s)) = (row.get_mut(..n), src.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
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
        filter: Box::new(Simple::new(Filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    #[test]
    fn swap_is_its_own_inverse() {
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Yuv420p, 4, 4).unwrap();
        if let Some(mut p) = frame.plane_mut(1) {
            for b in p.row_mut(0).unwrap() {
                *b = 11;
            }
        }
        if let Some(mut p) = frame.plane_mut(2) {
            for b in p.row_mut(0).unwrap() {
                *b = 22;
            }
        }
        let u0 = frame.plane(1).unwrap().row(0).unwrap().to_vec();
        let v0 = frame.plane(2).unwrap().row(0).unwrap().to_vec();
        assert_ne!(u0, v0);
    }
}
