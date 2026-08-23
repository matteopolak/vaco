//! `extractplanes` — split selected planes of a video frame into
//! standalone grayscale frames, one output pad per requested plane.
//!
//! `ffmpeg -h filter=extractplanes` documents one option, `planes`
//! (flags: `y`, `u`, `v`, `r`, `g`, `b`, `a`, default `r`), and a *dynamic*
//! number of output pads — one per bit set.
//!
//! # This is `Fanout`, generalised from `alphaextract`
//!
//! [`crate::alphaextract`] already does exactly this for one fixed
//! channel (alpha, component index 3 by this project's own
//! `PixFmtDescriptor` convention — "channel 0 is Y or R, 1 is U or G, 2 is
//! V or B, 3 is alpha", that module's own doc). This filter is the same
//! per-plane byte copy, generalised to any of the four channel roles and
//! to a *dynamic* output pad count, which is exactly
//! [`vaco_filter_core::adapt::Fanout`]'s shape: one input frame in, a
//! fixed-at-construction N frames out.
//!
//! Only formats where the requested channel already owns a dedicated
//! plane are supported — the same scope `alphaextract` documents as a
//! known gap (a *packed* channel, sharing bytes with others in one plane,
//! needs the component's `offset`/`step` threaded through a stride copy,
//! which neither filter does). Unlike `alphaextract`, this one checks for
//! it explicitly and refuses (`Error::Unsupported`) rather than silently
//! copying the wrong bytes, since a filter requesting `r`+`g`+`b` on a
//! packed `rgb24` frame would otherwise hand back three copies of the same
//! interleaved plane.
//!
//! # Measured: output pad order follows the flag's declared order, not the argument's
//!
//! `extractplanes=planes=v+y+u` on a `yuv444p` source still produces pad 0
//! = Y, pad 1 = U, pad 2 = V — confirmed by comparing each pad's mean
//! sample value against `planes=y`/`=u`/`=v` run separately. The order the
//! flags are written in the option string does not matter; the canonical
//! order `y, u, v, r, g, b, a` (the order the reference's own `-h` output
//! lists them) does.

use smallvec::SmallVec;
use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{Fanout, FanoutFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "extractplanes",
    description: "Extract planes as grayscale frames",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::DYNAMIC_OUTPUTS,
};

/// The `planes` flag vocabulary, in the canonical order the reference's own
/// `-h` output lists them — see this module's doc for why that order, not
/// the argument's, decides output pad order. `component` is this project's
/// `PixFmtDescriptor` index for the channel: 0 = Y/R, 1 = U/G, 2 = V/B,
/// 3 = alpha.
const CHANNELS: &[(&str, u8)] = &[
    ("y", 0),
    ("u", 1),
    ("v", 2),
    ("r", 0),
    ("g", 1),
    ("b", 2),
    ("a", 3),
];

fn parse_planes(text: &str) -> std::result::Result<Vec<u8>, String> {
    let mut set = 0u32;
    for token in text.split('+') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let index = CHANNELS
            .iter()
            .position(|(name, _)| *name == token)
            .ok_or_else(|| format!("extractplanes: unknown plane `{token}`"))?;
        set |= 1 << index;
    }
    if set == 0 {
        return Err("extractplanes: at least one plane must be selected".to_owned());
    }
    let components = CHANNELS
        .iter()
        .enumerate()
        .filter(|(i, _)| set & (1 << i) != 0)
        .map(|(_, &(_, component))| component)
        .collect();
    Ok(components)
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "extractplanes", help = "Extract planes as grayscale frames")]
pub(crate) struct Opts {
    #[opt(
        name = "planes",
        help = "set planes",
        default = "r".to_owned(),
        flags(video, filtering)
    )]
    pub planes: String,
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

/// The physical plane holding `component`, or `None` if this format has no
/// such channel.
fn plane_for_component(format: PixFmt, component: u8) -> Option<u8> {
    format
        .descriptor()
        .components
        .get(component as usize)
        .map(|c| c.plane)
}

/// Whether `plane` in `format` is used by exactly one component — i.e. a
/// genuinely planar channel, not several channels packed into one plane
/// (`rgb24`'s R/G/B, all `.plane == 0`).
fn is_dedicated_plane(format: PixFmt, plane: u8) -> bool {
    format
        .descriptor()
        .components
        .iter()
        .filter(|c| c.plane == plane)
        .count()
        == 1
}

#[derive(Debug)]
pub(crate) struct Filter {
    /// Requested channels, in canonical (output pad) order.
    components: Vec<u8>,
    /// Resolved physical plane per output pad, filled at `configure`.
    planes: Vec<u8>,
}

impl FanoutFilter for Filter {
    fn output_count(&self) -> usize {
        self.components.len()
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { format, .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        let mut planes = Vec::new();
        for &component in &self.components {
            let plane = plane_for_component(format, component).ok_or(Error::Unsupported(
                "extractplanes: format has no such plane",
            ))?;
            if !is_dedicated_plane(format, plane) {
                return Err(Error::Unsupported(
                    "extractplanes: requested channel is packed with another and cannot be extracted alone",
                ));
            }
            planes.push(plane);
        }
        self.planes = planes;
        for pad in 0..self.components.len() {
            if let Some(mut out) = ctx.output_link(pad).cloned() {
                if let LinkFormat::Video { format: f, .. } = &mut out {
                    *f = PixFmt::Gray8;
                }
                ctx.set_output_link(pad, out);
            }
        }
        Ok(())
    }

    fn split_frame(
        &mut self,
        ctx: &mut FilterContext<'_>,
        input: Frame,
    ) -> Result<SmallVec<[Frame; 4]>> {
        let FrameData::Video { width, height, .. } = input.data else {
            return Err(Error::InvalidData("extractplanes: not a video frame"));
        };
        let mut out = SmallVec::new();
        for &plane in &self.planes {
            let mut dst = ctx.pool().acquire_video(PixFmt::Gray8, width, height)?;
            if let Some(src) = input.plane(plane as usize)
                && let Some(mut dst_plane) = dst.plane_mut(0)
            {
                for y in 0..(height as usize) {
                    let Some(src_row) = src.row(y) else { continue };
                    if let Some(dst_row) = dst_plane.row_mut(y) {
                        let n = dst_row.len().min(src_row.len());
                        if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                            d.copy_from_slice(s);
                        }
                    }
                }
            }
            dst.pts = input.pts;
            dst.time_base = input.time_base;
            dst.duration = input.duration;
            dst.sample_aspect_ratio = input.sample_aspect_ratio;
            out.push(dst);
        }
        Ok(out)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let components = parse_planes(&opts.planes)?;
    let n = components.len();
    let output_pads = pads::video(n).ok_or_else(|| "extractplanes: too many planes".to_owned())?;
    let filter = Filter {
        components,
        planes: Vec::new(),
    };
    Ok(Instance {
        desc: FilterDesc {
            outputs: output_pads,
            ..DESC
        },
        formats: NodeFormats::passthrough(1, n, MediaType::Video, req.instance),
        filter: Box::new(Fanout::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn order_is_canonical_not_argument_order() {
        assert_eq!(parse_planes("v+y+u").unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_planes("y+u+v").unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn default_is_r_alone() {
        assert_eq!(parse_planes("r").unwrap(), vec![0]);
    }

    #[test]
    fn an_unknown_flag_is_a_clean_error() {
        assert!(parse_planes("q").is_err());
    }

    #[test]
    fn empty_selection_is_a_clean_error() {
        assert!(parse_planes("").is_err());
    }

    #[test]
    fn yuv420p_y_u_v_are_each_a_dedicated_plane() {
        for component in [0u8, 1, 2] {
            let plane = plane_for_component(PixFmt::Yuv420p, component).unwrap();
            assert!(is_dedicated_plane(PixFmt::Yuv420p, plane));
        }
    }

    #[test]
    fn packed_rgb24_channels_share_one_plane() {
        // R, G and B all live at `.plane == 0` in a packed format, so the
        // dedicated-plane check must refuse extracting any of them alone.
        let plane = plane_for_component(PixFmt::Rgb24, 0).unwrap();
        assert!(!is_dedicated_plane(PixFmt::Rgb24, plane));
    }
}
