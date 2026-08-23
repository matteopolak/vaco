//! `setparams` — force field order, colour range and colour signalling
//! together, in one filter.
//!
//! `ffmpeg -h filter=setparams` documents `field_mode`, `range`,
//! `color_primaries`, `color_trc`, `colorspace`, `chroma_location` and
//! `alpha_mode`, each `auto` (default, "leave alone") plus a per-field
//! vocabulary. Implemented: `field_mode` (identical semantics to
//! `setfield`'s `mode`), `range` (identical to `setrange`'s `range`),
//! `color_primaries`, `color_trc`, `colorspace` and `chroma_location` —
//! the last four resolved through `vaco_color`'s own `from_name` on each
//! enum, which is already the reference's naming (D17: e.g. `colorspace=rgb`
//! prints back as `gbr`). Not implemented: `alpha_mode` —
//! `vaco_frame`/`vaco_color` have no field to write it into yet.
//!
//! This is functionally `setfield` + `setrange` + four more colour-tag
//! setters fused into one filter; see those two modules for the field-order
//! and range semantics, which are identical here.

use vaco_color::{
    ChromaLocation, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameFlags};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "setparams",
    description: "Force field, or color property for the output video frame",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

fn parse_field(s: &str) -> std::result::Result<Option<(bool, bool)>, String> {
    // `Some((interlaced, tff))`; `None` is "auto" (leave alone).
    match s {
        "-1" | "auto" => Ok(None),
        "0" | "bff" => Ok(Some((true, false))),
        "1" | "tff" => Ok(Some((true, true))),
        "2" | "prog" => Ok(Some((false, false))),
        other => Err(format!("setparams: bad `field_mode` `{other}`")),
    }
}

fn parse_range(s: &str) -> std::result::Result<Option<ColorRange>, String> {
    match s {
        "-1" | "auto" => Ok(None),
        "0" | "unspecified" | "unknown" => Ok(Some(ColorRange::Unspecified)),
        "1" | "limited" | "tv" | "mpeg" => Ok(Some(ColorRange::Limited)),
        "2" | "full" | "pc" | "jpeg" => Ok(Some(ColorRange::Full)),
        other => Err(format!("setparams: bad `range` `{other}`")),
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "setparams",
    help = "Force field, or color property for the output video frame"
)]
pub(crate) struct Opts {
    #[opt(name = "field_mode", help = "select interlace mode", default = "auto".to_owned(), flags(video, filtering))]
    pub field_mode: String,
    #[opt(name = "range", help = "select color range", default = "auto".to_owned(), flags(video, filtering))]
    pub range: String,
    #[opt(name = "color_primaries", help = "select color primaries", default = "auto".to_owned(), flags(video, filtering))]
    pub color_primaries: String,
    #[opt(name = "color_trc", help = "select color transfer", default = "auto".to_owned(), flags(video, filtering))]
    pub color_trc: String,
    #[opt(name = "colorspace", help = "select colorspace", default = "auto".to_owned(), flags(video, filtering))]
    pub colorspace: String,
    #[opt(name = "chroma_location", help = "select chroma sample location", default = "auto".to_owned(), flags(video, filtering))]
    pub chroma_location: String,
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

#[derive(Debug, Default)]
pub(crate) struct Filter {
    field: Option<(bool, bool)>,
    range: Option<ColorRange>,
    primaries: Option<ColorPrimaries>,
    trc: Option<TransferCharacteristic>,
    matrix: Option<MatrixCoefficients>,
    chroma_location: Option<ChromaLocation>,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let named = |s: &str| !matches!(s, "-1" | "auto");
        Ok(Self {
            field: parse_field(&opts.field_mode)?,
            range: parse_range(&opts.range)?,
            primaries: named(&opts.color_primaries)
                .then(|| ColorPrimaries::from_name(&opts.color_primaries))
                .flatten(),
            trc: named(&opts.color_trc)
                .then(|| TransferCharacteristic::from_name(&opts.color_trc))
                .flatten(),
            matrix: named(&opts.colorspace)
                .then(|| MatrixCoefficients::from_name(&opts.colorspace))
                .flatten(),
            chroma_location: named(&opts.chroma_location)
                .then(|| ChromaLocation::from_name(&opts.chroma_location))
                .flatten(),
        })
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        if let Some((interlaced, tff)) = self.field {
            input.flags.set(FrameFlags::INTERLACED, interlaced);
            input
                .flags
                .set(FrameFlags::TOP_FIELD_FIRST, interlaced && tff);
        }
        if let Some(range) = self.range {
            input.color.range = range;
        }
        if let Some(p) = self.primaries {
            input.color.primaries = p;
        }
        if let Some(t) = self.trc {
            input.color.transfer = t;
        }
        if let Some(m) = self.matrix {
            input.color.matrix = m;
        }
        if let Some(c) = self.chroma_location {
            input.color.chroma_location = c;
        }
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
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
    fn auto_everywhere_leaves_everything_none() {
        let f = Filter::new(&Opts::default()).unwrap();
        assert_eq!(f.field, None);
        assert_eq!(f.range, None);
        assert_eq!(f.primaries, None);
    }

    #[test]
    fn tff_sets_interlaced_and_top_field_first() {
        let opts = Opts {
            field_mode: "tff".to_owned(),
            ..Opts::default()
        };
        let f = Filter::new(&opts).unwrap();
        assert_eq!(f.field, Some((true, true)));
    }

    #[test]
    fn colorspace_reuses_the_references_own_naming() {
        let opts = Opts {
            colorspace: "bt709".to_owned(),
            ..Opts::default()
        };
        let f = Filter::new(&opts).unwrap();
        assert_eq!(f.matrix, Some(MatrixCoefficients::Bt709));
    }
}
