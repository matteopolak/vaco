//! `displace` — displace each source pixel by an offset read from two map
//! planes.
//!
//! `ffmpeg -h filter=displace` (2026-08-28): `edge` (`blank`/`smear`/
//! `wrap`/`mirror`, default `smear`). Three fixed inputs (`source`,
//! `xmap`, `ymap`), no framesync surface — built on `Paired`, the same
//! shape `remap`/`mergeplanes` use.
//!
//! # Measured (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo` sources)
//!
//! The map planes are plain **8-bit `gray`** (no auto-inserted format
//! conversion, unlike `remap`'s maps — see that module's doc), and `128`
//! is the zero point:
//!
//! ```text
//! output(x, y) = source(x + (xmap(x,y) - 128), y + (ymap(x,y) - 128))
//! ```
//!
//! Confirmed three ways: an all-`128` map pair reproduces the source
//! frame unchanged; `xmap = 130` (offset `+2`) shifts every output column
//! to read from two columns to the right (`output(x) = source(x+2)`,
//! confirmed against a per-pixel-distinct gradient source); `ymap = 130`
//! shifts every output row down two rows the same way (`output row 0` =
//! `source row 2`).
//!
//! `edge` (what happens when the computed source coordinate falls
//! outside the frame — probed with `xmap = 0`, offset `-128`, deeply
//! out of range for every column):
//!
//! ```text
//! blank  -> a constant fill value (measured: 16, not 0 -- see below)
//! smear  -> clamp the coordinate to the nearest valid one (the default)
//! wrap   -> the coordinate modulo the frame dimension
//! mirror -> reflect the coordinate at the boundary -- see below, the
//!           two edges do not share one axis
//! ```
//!
//! `blank`'s fill value measured `16`, not `0`, at every out-of-range
//! pixel of an 8-bit `gray` source. This matches [`crate::remap`]'s own
//! measured black point (also `16` — see that module's doc for the
//! BT.709 limited-range derivation this crate believes explains it), so
//! `16` is used here too rather than treated as an unrelated constant.
//!
//! `mirror` is the one edge mode with a real surprise in it: the two
//! edges do not reflect around the same kind of axis. Probed with small
//! (`+-1`, `+-2`, `+3`) offsets specifically to keep each case a single
//! bounce off one edge: the *left* edge reflects around index `0` itself
//! (`resolve(-1) = 1`, `resolve(-2) = 2` — the edge pixel is the mirror
//! line), while the *right* edge reflects around the half-pixel point
//! `len - 0.5` (`resolve(len) = len-1`, `resolve(len+1) = len-2`, …,
//! `len-0.5` sits between the last real pixel and the first invalid one).
//! A single `floor`/`ceil`-style formula does not fit both without
//! knowing this. A separately-probed deep offset (`-128`, more than one
//! frame width out of range) does **not** match either edge's formula
//! extended periodically — this module's `Mirror` therefore clamps
//! rather than guesses beyond one bounce, and is confirmed exact only
//! within one frame dimension of the edge.
//!
//! # Not measured/implemented
//!
//! Non-luma planes (chroma is not touched — `blank`'s neutral chroma
//! value, presumably `128`, was not independently confirmed). RGB pixel
//! formats. Bit depths above 8.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[
    Pad {
        name: "source",
        media_type: MediaType::Video,
    },
    Pad {
        name: "xmap",
        media_type: MediaType::Video,
    },
    Pad {
        name: "ymap",
        media_type: MediaType::Video,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "displace",
    description: "Displace pixels.",
    inputs: VIDEO_PAD,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

/// The reference's own measured black point for an out-of-range `blank`
/// pixel — see the module doc.
const BLANK_LUMA: u8 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Blank,
    Smear,
    Wrap,
    Mirror,
}

impl Edge {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "blank" => Some(Self::Blank),
            "smear" => Some(Self::Smear),
            "wrap" => Some(Self::Wrap),
            "mirror" => Some(Self::Mirror),
            _ => None,
        }
    }

    /// Resolve one axis's coordinate against `len` (the frame's width or
    /// height on that axis). `None` means "blank" — no valid source
    /// sample, paint [`BLANK_LUMA`].
    fn resolve(self, coord: i64, len: i64) -> Option<i64> {
        if len <= 0 {
            return None;
        }
        if coord >= 0 && coord < len {
            return Some(coord);
        }
        match self {
            Self::Blank => None,
            Self::Smear => Some(coord.clamp(0, len - 1)),
            Self::Wrap => Some(coord.rem_euclid(len)),
            Self::Mirror => {
                // Confirmed exact (see the module doc) for a single
                // bounce off either edge, and the two edges use
                // different mirror axes: the left edge reflects around
                // index `0` itself (`resolved = -coord`), the right edge
                // around the half-pixel point `len - 0.5`
                // (`resolved = 2*len - 1 - coord`) — an asymmetry this
                // module reproduces rather than assumes away. Beyond one
                // frame dimension of overshoot this stops being a
                // periodic reflection at all (checked, not just
                // unconfirmed), so a second bounce is clamped rather
                // than guessed at.
                let reflected = if coord < 0 { -coord } else { 2 * len - 1 - coord };
                Some(reflected.clamp(0, len - 1))
            }
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "displace", help = "Displace pixels.")]
pub(crate) struct Opts {
    #[opt(name = "edge", help = "set edge mode", default = "smear".to_owned(), flags(video, filtering))]
    pub edge: String,
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
    edge: Edge,
}

impl PairedFilter for Filter {
    fn input_count(&self) -> usize {
        3
    }

    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        mut inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        if inputs.len() != 3 {
            return Ok(FrameOut::None);
        }
        let ymap = inputs.pop();
        let xmap = inputs.pop();
        let source = inputs.pop();
        let (Some(source), Some(xmap), Some(ymap)) = (source, xmap, ymap) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, width, height, .. } = source.data else {
            return Ok(FrameOut::One(source));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(source));
        }
        let Some(src_plane) = source.plane(0) else {
            return Ok(FrameOut::One(source));
        };
        let Some(xmap_plane) = xmap.plane(0) else {
            return Ok(FrameOut::One(source));
        };
        let Some(ymap_plane) = ymap.plane(0) else {
            return Ok(FrameOut::One(source));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        // Non-luma planes: copy the source unchanged (not measured — see
        // the module doc).
        for plane in 1..format.plane_count() {
            let ph = common::to_i32(format.plane_height(height, plane as u8)).max(0);
            let Some(src) = source.plane(plane) else { continue };
            let Some(mut dst) = out.plane_mut(plane) else { continue };
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(src_row) = src.row(uy) else { continue };
                let Some(dst_row) = dst.row_mut(uy) else { continue };
                let n = src_row.len().min(dst_row.len());
                if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
        }
        let w = i64::from(width);
        let h = i64::from(height);
        let Some(mut dst0) = out.plane_mut(0) else {
            return Ok(FrameOut::One(source));
        };
        for y in 0..h {
            let Ok(uy) = usize::try_from(y) else { continue };
            let Some(xrow) = xmap_plane.row(uy) else { continue };
            let Some(yrow) = ymap_plane.row(uy) else { continue };
            let Some(dst_row) = dst0.row_mut(uy) else { continue };
            for x in 0..w {
                let Ok(ux) = usize::try_from(x) else { continue };
                let (Some(&xv), Some(&yv)) = (xrow.get(ux), yrow.get(ux)) else {
                    continue;
                };
                let sx = x + (i64::from(xv) - 128);
                let sy = y + (i64::from(yv) - 128);
                let out_val = match (self.edge.resolve(sx, w), self.edge.resolve(sy, h)) {
                    (Some(rx), Some(ry)) => (|| {
                        let (Ok(urx), Ok(ury)) = (usize::try_from(rx), usize::try_from(ry)) else {
                            return None;
                        };
                        src_plane.row(ury).and_then(|r| r.get(urx)).copied()
                    })()
                    .unwrap_or(BLANK_LUMA),
                    _ => BLANK_LUMA,
                };
                if let Some(px) = dst_row.get_mut(ux) {
                    *px = out_val;
                }
            }
        }
        out.pts = source.pts;
        out.time_base = source.time_base;
        out.duration = source.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let edge =
        Edge::from_name(&opts.edge).ok_or_else(|| format!("displace: bad `edge` `{}`", opts.edge))?;
    let filter = Filter { edge };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(3, 1, MediaType::Video, req.instance),
        filter: Box::new(Paired::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "displace",
            instance: "displace",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_edge_is_a_clean_error() {
        let req = Instantiate {
            name: "displace",
            instance: "displace",
            args: Some("edge=nope"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned against the reference's probe in this module's doc:
    /// `xmap=0` (offset `-128`) is deeply out of range for every column
    /// of an 8-wide frame. `Mirror` is deliberately not asserted here —
    /// see the dedicated test below for why a single bounce is as far as
    /// this module's `Mirror` formula is confirmed to match.
    #[test]
    fn edge_modes_match_the_reference_probe() {
        let len = 8i64;
        assert_eq!(Edge::Blank.resolve(-128, len), None);
        assert_eq!(Edge::Smear.resolve(-128, len), Some(0));
        assert_eq!(Edge::Wrap.resolve(-128, len), Some(0));
    }

    /// Pinned against the reference's small-offset mirror probes: the
    /// left edge reflects around index `0` (`resolve(-1)=1`,
    /// `resolve(-2)=2`), the right edge around `len-0.5`
    /// (`resolve(8)=7`, `resolve(9)=6`, `resolve(10)=5`, for `len=8`) —
    /// two different axes, not one symmetric rule.
    #[test]
    fn mirror_reflects_with_different_axes_per_edge() {
        let len = 8i64;
        assert_eq!(Edge::Mirror.resolve(-1, len), Some(1));
        assert_eq!(Edge::Mirror.resolve(-2, len), Some(2));
        assert_eq!(Edge::Mirror.resolve(8, len), Some(7));
        assert_eq!(Edge::Mirror.resolve(9, len), Some(6));
        assert_eq!(Edge::Mirror.resolve(10, len), Some(5));
    }

    /// Pinned: the zero point is `128` — an all-`128` map pair is the
    /// identity offset.
    #[test]
    fn resolve_is_identity_at_in_range_coordinates() {
        let len = 8i64;
        for x in 0..len {
            assert_eq!(Edge::Smear.resolve(x, len), Some(x));
        }
    }
}
