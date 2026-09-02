//! `vstack` — stack `N` video inputs top to bottom. The same shape as
//! [`crate::hstack`], rotated.
//!
//! `ffmpeg -h filter=vstack` (2026-08-28): `inputs` (`2..=INT_MAX`, default
//! `2`), `shortest` (bool, default `false`) — the same two options,
//! measured to behave identically to `hstack`'s (`ffmpeg`'s own help text
//! literally shares one `"(h|v)stack AVOptions"` block between the two).
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo`/`lavfi` sources)
//!
//! Output height is the exact sum of every input's height; output width
//! must be the same across every input, or the reference refuses to
//! configure — the exact rotation of `hstack`'s own measured rule. See
//! that module's doc for the `shortest`/mixed-format details, which apply
//! unchanged.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

use crate::common;

const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "vstack",
    description: "Stack video inputs vertically.",
    inputs: &[],
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "vstack", help = "Stack video inputs vertically.")]
pub(crate) struct Opts {
    #[opt(name = "inputs", help = "set number of inputs", default = 2, range = 2..=64, flags(video, filtering))]
    pub inputs: i64,
    #[opt(
        name = "shortest",
        help = "force termination when the shortest input terminates",
        default = false,
        flags(video, filtering)
    )]
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
    shortest: bool,
    /// Each input's own height, resolved once in `configure`.
    heights: Vec<u32>,
}

impl FrameSyncFilter for Filter {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::uniform(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts {
            shortest: self.shortest,
            ..FrameSyncOpts::default()
        }
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let mut heights = Vec::new();
        let mut format = None;
        let mut width = None;
        for i in 0..self.n {
            let Some(LinkFormat::Video {
                format: f,
                width: w,
                height,
                ..
            }) = ctx.input_link(i).cloned()
            else {
                return Ok(());
            };
            common::ensure_addressable(f)?;
            if let Some(expect) = width
                && expect != w
            {
                return Err(vaco_core::Error::Unsupported(
                    "vstack: every input must have the same width",
                ));
            }
            width.get_or_insert(w);
            format.get_or_insert(f);
            heights.push(height);
        }
        self.heights = heights;
        let (Some(width), Some(format)) = (width, format) else {
            return Ok(());
        };
        let total_height: u32 = self.heights.iter().copied().fold(0u32, u32::saturating_add);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width: w,
                height: h,
                format: fmt,
                ..
            } = &mut out
            {
                *w = width;
                *h = total_height;
                *fmt = format;
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
        let Some((format, width)) = event.get(0).and_then(|f| match &f.data {
            FrameData::Video { format, width, .. } => Some((*format, *width)),
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => None,
        }) else {
            return Ok(FrameOut::None);
        };
        let total_height: u32 = self.heights.iter().copied().fold(0u32, u32::saturating_add);
        let mut out = ctx.pool().acquire_video(format, width, total_height)?;
        let plane_count = format.plane_count();
        for plane in 0..plane_count {
            let mut row_offset = 0usize;
            for i in 0..self.n {
                let Some(frame) = event.get(i) else { continue };
                let Some(src) = frame.plane(plane) else {
                    continue;
                };
                let Some(mut dst) = out.plane_mut(plane) else {
                    continue;
                };
                let input_height = self.heights.get(i).copied().unwrap_or(0);
                let ph = common::to_i32(format.plane_height(input_height, plane as u8)).max(0);
                for y in 0..ph {
                    let Ok(uy) = usize::try_from(y) else { continue };
                    let Some(src_row) = src.row(uy) else { continue };
                    let Some(dst_row) = dst.row_mut(row_offset + uy) else {
                        continue;
                    };
                    let n = dst_row.len().min(src_row.len());
                    if let (Some(dst_slice), Some(src_slice)) =
                        (dst_row.get_mut(..n), src_row.get(..n))
                    {
                        dst_slice.copy_from_slice(src_slice);
                    }
                }
                row_offset += usize::try_from(ph).unwrap_or(0);
            }
        }
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let n = usize::try_from(opts.inputs).unwrap_or(2).max(2);
    let input_pads = pads::video(n).ok_or_else(|| "vstack: too many inputs".to_owned())?;
    let filter = Filter {
        n,
        shortest: opts.shortest,
        heights: Vec::new(),
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
            name: "vstack",
            instance: "vstack",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn too_many_inputs_is_a_clean_error() {
        let req = Instantiate {
            name: "vstack",
            instance: "vstack",
            args: Some("inputs=1000"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
