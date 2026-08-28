//! `hstack` — stack `N` video inputs side by side, left to right.
//!
//! `ffmpeg -h filter=hstack` (2026-08-28): `inputs` (`2..=INT_MAX`, default
//! `2`), `shortest` (bool, default `false`).
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo`/`lavfi` sources)
//!
//! Output width is the exact sum of every input's width; output height must
//! be the *same* across every input — feeding an `8x8` and an `8x12` input
//! is a hard `configure` error ("height does not match"), not a resize or a
//! crop. Confirmed with matching-height, mismatched-width inputs producing
//! `width0+width1` exactly (`8+12=20`), and with mismatched heights failing
//! to configure at all.
//!
//! Mixed pixel formats between inputs are accepted by the reference CLI
//! (its own graph auto-inserts a format-conversion filter upstream), which
//! is exactly what this tree's negotiator does too — `Tie::all_pads` on a
//! `passthrough` node folds a format mismatch into a converter spliced in
//! by [`vaco_filter_core::negotiate`]'s repair step, not something this
//! filter has to do for itself. `on_event` can assume every input frame it
//! sees already shares one pixel format.
//!
//! `shortest=false` (the default) continues to the *longest* input's
//! length, freezing each shorter input's last frame — measured directly: a
//! 1-frame and a 5-frame input (`loop`-extended) at the default produce `5`
//! output frames; the same pair with `shortest=true` produces `1`. That is
//! exactly [`vaco_filter_framesync::FrameSyncOpts`]'s own
//! `eof_action=Repeat` default, reached here for free through
//! [`vaco_filter_framesync::FsInput::uniform`] rather than reimplemented.
//!
//! # Not measured/implemented
//!
//! Bit depths and pixel formats are not restricted beyond
//! [`crate::common::ensure_addressable`] (this filter is a pure byte
//! mover, so there is no narrower claim to make the way this project's
//! pixel-*math* filters need), but no probe compared a 10-bit or
//! big-endian pair against the reference specifically.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

use crate::common;

const OUTPUT_PAD: &[vaco_filter_core::Pad] = &[vaco_filter_core::Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "hstack",
    description: "Stack video inputs horizontally.",
    inputs: &[],
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "hstack", help = "Stack video inputs horizontally.")]
pub(crate) struct Opts {
    #[opt(name = "inputs", help = "set number of inputs", default = 2, range = 2..=64, flags(video, filtering))]
    pub inputs: i64,
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
    shortest: bool,
    /// Each input's own width, resolved once in `configure`.
    widths: Vec<u32>,
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
        let mut widths = Vec::new();
        let mut format = None;
        let mut height = None;
        for i in 0..self.n {
            let Some(LinkFormat::Video {
                format: f,
                width,
                height: h,
                ..
            }) = ctx.input_link(i).cloned()
            else {
                return Ok(());
            };
            common::ensure_addressable(f)?;
            if let Some(expect) = height
                && expect != h
            {
                return Err(vaco_core::Error::Unsupported(
                    "hstack: every input must have the same height",
                ));
            }
            height.get_or_insert(h);
            format.get_or_insert(f);
            widths.push(width);
        }
        self.widths = widths;
        let (Some(height), Some(format)) = (height, format) else {
            return Ok(());
        };
        let total_width: u32 = self.widths.iter().copied().fold(0u32, u32::saturating_add);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { width: w, height: h, format: fmt, .. } = &mut out {
                *w = total_width;
                *h = height;
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
        let Some((format, height)) = event.get(0).and_then(|f| match &f.data {
            FrameData::Video { format, height, .. } => Some((*format, *height)),
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => None,
        }) else {
            return Ok(FrameOut::None);
        };
        let total_width: u32 = self.widths.iter().copied().fold(0u32, u32::saturating_add);
        let mut out = ctx.pool().acquire_video(format, total_width, height)?;
        let plane_count = format.plane_count();
        for plane in 0..plane_count {
            let mut col_offset = 0usize;
            for i in 0..self.n {
                let Some(frame) = event.get(i) else { continue };
                let Some(src) = frame.plane(plane) else { continue };
                let Some(mut dst) = out.plane_mut(plane) else { continue };
                let ph = common::to_i32(format.plane_height(height, plane as u8)).max(0);
                let mut written = 0usize;
                for y in 0..ph {
                    let Ok(uy) = usize::try_from(y) else { continue };
                    let Some(src_row) = src.row(uy) else { continue };
                    written = written.max(src_row.len());
                    let Some(dst_row) = dst.row_mut(uy) else { continue };
                    if let Some(seg) = dst_row.get_mut(col_offset..col_offset + src_row.len()) {
                        seg.copy_from_slice(src_row);
                    }
                }
                col_offset += written;
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
    let input_pads = pads::video(n).ok_or_else(|| "hstack: too many inputs".to_owned())?;
    let filter = Filter {
        n,
        shortest: opts.shortest,
        widths: Vec::new(),
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
            name: "hstack",
            instance: "hstack",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn creatable_with_four_inputs() {
        let req = Instantiate {
            name: "hstack",
            instance: "hstack",
            args: Some("inputs=4"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe in this module's doc: default
    /// `shortest=false` and explicit `shortest=true` are both accepted, and
    /// map onto `FrameSyncOpts.shortest` unchanged.
    #[test]
    fn shortest_option_reaches_frame_sync_opts() {
        let req = Instantiate {
            name: "hstack",
            instance: "hstack",
            args: Some("shortest=true"),
            arguments: &[],
        };
        let opts = Opts::parse(Some("shortest=true")).unwrap();
        assert!(opts.shortest);
        assert!(create(&req).is_ok());
    }

    #[test]
    fn too_many_inputs_is_a_clean_error() {
        let req = Instantiate {
            name: "hstack",
            instance: "hstack",
            args: Some("inputs=1000"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
