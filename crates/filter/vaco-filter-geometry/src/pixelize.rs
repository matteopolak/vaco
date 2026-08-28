//! `pixelize` — mosaic the input into blocks, each replaced by one
//! representative value.
//!
//! `ffmpeg -h filter=pixelize` documents `width`/`w`, `height`/`h` (block
//! size, `1..1024`, default `16`), `mode`/`m` (`avg`=0 default, `min`=1,
//! `max`=2) and `planes`/`p` (a plane bitmask, default `0xF`, all planes).
//! `width`/`height`/`mode` implemented exactly per the measurements below;
//! `planes` implemented as a plain `i64` bitmask rather than the reference's
//! named-flag syntax (`vaco-opts` has no flag-string type in this crate's
//! scope) — the default `0xF` (every plane) is unaffected, so this only
//! surfaces for a caller explicitly restricting which planes are pixelized.
//!
//! # Measured: the per-block reduction
//!
//! Built a 4x2 `gray` ramp (`geq=lum='X*10+Y*100'`, values
//! `[[0,10,20,30],[100,110,120,130]]`) and pixelized with a 2x2 block:
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=4x2,format=gray,geq=lum='X*10+Y*100'" \
//!   -vf pixelize=width=2:height=2:mode=avg -f rawvideo -pix_fmt gray -
//! ```
//!
//! `avg` gave `[55,55,75,75,55,55,75,75]` — each 2x2 block replaced by the
//! *integer* mean of its four samples (`(0+10+100+110)/4 = 55`,
//! `(20+30+120+130)/4 = 75`). `mode=1` (`min`) and `mode=2` (`max`) gave the
//! block's minimum and maximum respectively, confirmed against the same
//! image. A block that overruns the frame edge (width/height not a multiple
//! of the block size) is not separately measured; this implementation
//! clamps the block to the frame boundary, which is the only reduction that
//! does not need to invent out-of-frame data.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "pixelize",
    description: "Pixelize video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Avg,
    Min,
    Max,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "pixelize", help = "Pixelize video")]
pub(crate) struct Opts {
    #[opt(
        name = "width",
        alias = "w",
        help = "set block width",
        default = 16,
        range = 1..=1024,
        flags(video, filtering)
    )]
    pub width: i32,
    #[opt(
        name = "height",
        alias = "h",
        help = "set block height",
        default = 16,
        range = 1..=1024,
        flags(video, filtering)
    )]
    pub height: i32,
    #[opt(
        name = "mode",
        alias = "m",
        help = "set the pixelize mode",
        default = "avg".to_owned(),
        flags(video, filtering)
    )]
    pub mode: String,
    #[opt(
        name = "planes",
        alias = "p",
        help = "set what planes to filter",
        default = 0xF,
        flags(video, filtering)
    )]
    pub planes: i64,
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
    block_w: u32,
    block_h: u32,
    mode: Mode,
    planes_mask: i64,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        // The reference accepts both the named constant and its numeric
        // value for this option (confirmed: `mode=avg` and `mode=0` both
        // run against `ffmpeg 8.1`) -- this was previously accepted only
        // as a bare integer, a real argument-syntax gap `vaco-conformance`
        // found the moment its own corpus used the reference's own named
        // spelling instead of the number.
        let mode = match opts.mode.as_str() {
            "avg" | "0" => Mode::Avg,
            "min" | "1" => Mode::Min,
            "max" | "2" => Mode::Max,
            other => return Err(format!("pixelize: bad `mode` `{other}`")),
        };
        Ok(Self {
            block_w: opts.width.max(1) as u32,
            block_h: opts.height.max(1) as u32,
            mode,
            planes_mask: opts.planes,
        })
    }
}

/// Reduce one `unit`-byte-per-sample block (`unit == 1` only, checked by the
/// caller) to a single value.
#[allow(
    clippy::integer_division,
    reason = "the `avg` mode is a measured integer mean (see module doc), not a float approximation"
)]
fn reduce(values: &[u8], mode: Mode) -> u8 {
    match mode {
        Mode::Avg => {
            if values.is_empty() {
                0
            } else {
                let sum: u32 = values.iter().map(|&v| u32::from(v)).sum();
                (sum / values.len() as u32) as u8
            }
        }
        Mode::Min => values.iter().copied().min().unwrap_or(0),
        Mode::Max => values.iter().copied().max().unwrap_or(0),
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        geom::ensure_addressable(format)?;
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let filter_this_plane = self.planes_mask & (1_i64 << p) != 0;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            let pw = format.plane_width(width, plane_idx);
            let ph = format.plane_height(height, plane_idx);
            let bw = format.plane_width(self.block_w, plane_idx).max(1);
            let bh = format.plane_height(self.block_h, plane_idx).max(1);
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            if !filter_this_plane || unit != 1 {
                // Either this plane is excluded by the mask, or it packs more
                // than one byte per sample (never true for this project's
                // 8-bit planar formats, but `rgb24`-style unit=3 would make
                // "byte value" not "sample value" — copy through unchanged
                // rather than average raw bytes across channel boundaries.
                for row in 0..(ph as usize) {
                    let row_bytes = (pw as usize).saturating_mul(unit);
                    if let (Some(s), Some(d)) = (src_plane.row(row), dst_plane.row_mut(row)) {
                        let n = row_bytes.min(s.len()).min(d.len());
                        if let (Some(sd), Some(dd)) = (s.get(..n), d.get_mut(..n)) {
                            dd.copy_from_slice(sd);
                        }
                    }
                }
                continue;
            }
            let mut by0 = 0u32;
            while by0 < ph {
                let bh_here = bh.min(ph - by0);
                let mut bx0 = 0u32;
                while bx0 < pw {
                    let bw_here = bw.min(pw - bx0);
                    let mut values: smallvec::SmallVec<[u8; 64]> = smallvec::SmallVec::new();
                    for ry in 0..bh_here {
                        if let Some(row) = src_plane.row((by0 + ry) as usize) {
                            let start = bx0 as usize;
                            if let Some(s) = row.get(start..start.saturating_add(bw_here as usize))
                            {
                                values.extend_from_slice(s);
                            }
                        }
                    }
                    let v = reduce(&values, self.mode);
                    for ry in 0..bh_here {
                        if let Some(row) = dst_plane.row_mut((by0 + ry) as usize) {
                            let start = bx0 as usize;
                            if let Some(s) =
                                row.get_mut(start..start.saturating_add(bw_here as usize))
                            {
                                s.fill(v);
                            }
                        }
                    }
                    bx0 += bw_here;
                }
                by0 += bh_here;
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = input.color;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
        Ok(FrameOut::One(out))
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
    fn avg_matches_measured_block_reduction() {
        // Measured: block [0,10,100,110] averages to 55.
        assert_eq!(reduce(&[0, 10, 100, 110], Mode::Avg), 55);
        assert_eq!(reduce(&[20, 30, 120, 130], Mode::Avg), 75);
    }

    #[test]
    fn min_and_max_match_measured_values() {
        assert_eq!(reduce(&[0, 10, 100, 110], Mode::Min), 0);
        assert_eq!(reduce(&[0, 10, 100, 110], Mode::Max), 110);
    }

    #[test]
    fn block_size_one_is_the_identity() {
        for v in [0u8, 128, 255] {
            assert_eq!(reduce(&[v], Mode::Avg), v);
            assert_eq!(reduce(&[v], Mode::Min), v);
            assert_eq!(reduce(&[v], Mode::Max), v);
        }
    }

    proptest::proptest! {
        #[test]
        fn min_le_avg_le_max(values in proptest::collection::vec(0u8..=255, 1..32)) {
            let lo = reduce(&values, Mode::Min);
            let mid = reduce(&values, Mode::Avg);
            let hi = reduce(&values, Mode::Max);
            proptest::prop_assert!(lo <= mid);
            proptest::prop_assert!(mid <= hi);
        }
    }
}
