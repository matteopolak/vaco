//! `setdar` — set the display aspect ratio, by computing the SAR that
//! produces it.
//!
//! `ffmpeg -h filter=setdar` documents `dar`/`ratio`/`r` (default `"0"`,
//! "leave it alone") and `max`. Implemented: `dar`. `max` not implemented,
//! for the same reason as `setsar.rs`.
//!
//! # Measured: `setdar` derives SAR purely from `dar` and the *current*
//! `w`/`h` — it never reads the old SAR
//!
//! ```text
//! ffmpeg -f lavfi -i color=red:s=100x50 -vf "setsar=2/1,setdar=1/1" -f null -
//! # -> SAR 1:2, DAR 1:1 — the SAR that setsar had just set is gone
//! ```
//!
//! `setdar=<D>` computes `sar_new = D * height / width` and overwrites SAR
//! with it, unconditionally. `D` is *not* combined with whatever SAR was
//! already on the link — `setdar` immediately after `setsar` in a chain
//! throws away `setsar`'s value completely, which is the mirror image of
//! `setsar.rs`'s measurement. DAR itself is never stored: `-show_streams`'s
//! `DAR` column (and this crate's own reporting) is always `SAR * W / H`,
//! computed on demand.

use vaco_core::{MediaType, Rational, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "setdar",
    description: "Set the frame display aspect ratio",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "setdar", help = "Set the frame display aspect ratio")]
pub(crate) struct Opts {
    #[opt(
        name = "dar",
        alias = "ratio",
        help = "set display aspect ratio",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub dar: String,
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
    /// `None` means "0", the reference's own "leave it alone" sentinel.
    dar: Option<Rational>,
}

impl Filter {
    fn new(text: &str) -> std::result::Result<Self, String> {
        let r = vaco_core::parse::rational(text)
            .ok_or_else(|| format!("setdar: bad `dar` `{text}`"))?;
        Ok(Self {
            dar: (r.num != 0).then_some(r),
        })
    }

    /// `sar = dar * h / w`, the reference's measured formula.
    fn sar_for(dar: Rational, width: u32, height: u32) -> Option<Rational> {
        if width == 0 || height == 0 {
            return None;
        }
        let h = i32::try_from(height).unwrap_or(i32::MAX);
        let w = i32::try_from(width).unwrap_or(i32::MAX);
        dar.checked_mul(Rational::new(h, w)).map(Rational::reduced)
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(dar) = self.dar else { return Ok(()) };
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        let Some(sar) = Self::sar_for(dar, width, height) else {
            return Ok(());
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *sample_aspect_ratio = sar;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        if let Some(dar) = self.dar {
            let (w, h) = match &input.data {
                vaco_frame::FrameData::Video { width, height, .. } => (*width, *height),
                vaco_frame::FrameData::Audio { .. } | vaco_frame::FrameData::Subtitle { .. } => {
                    (0, 0)
                }
            };
            if let Some(sar) = Self::sar_for(dar, w, h) {
                input.sample_aspect_ratio = sar;
            }
        }
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts.dar)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn zero_means_leave_it_alone() {
        let f = Filter::new("0").unwrap();
        assert_eq!(f.dar, None);
    }

    #[test]
    fn sar_formula_matches_measurement() {
        // Measured: 100x50, setdar=1/1 -> sar 1/2.
        assert_eq!(
            Filter::sar_for(Rational::ONE, 100, 50),
            Some(Rational::new(1, 2))
        );
        // Measured: 100x50, setdar=2/1 -> sar 1/1.
        assert_eq!(
            Filter::sar_for(Rational::new(2, 1), 100, 50),
            Some(Rational::ONE)
        );
    }
}
