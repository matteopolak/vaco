//! `transpose` — swap width and height, optionally with a flip, i.e. a
//! 90°-multiple rotation.
//!
//! `ffmpeg -h filter=transpose` documents `dir` (`cclock_flip`=0 default,
//! `clock`=1, `cclock`=2, `clock_flip`=3) and `passthrough`
//! (`none`=0 default, `landscape`=1, `portrait`=2). Both implemented.
//!
//! # Measured: what each `dir` actually computes
//!
//! Built a 2-wide×3-tall `gray` image with `geq=lum='Y*10+X'` (so byte
//! `row*10+col` identifies its own origin) and ran all four directions:
//!
//! ```text
//! ffmpeg -f lavfi -i color=black:s=2x3 \
//!   -vf "format=gray,geq=lum='Y*10+X',transpose=dir=clock" \
//!   -f rawvideo -pix_fmt gray -
//! ```
//!
//! Writing `O[r][c]` for the original pixel at row `r`, column `c`, and `T`
//! for the plain matrix transpose (`T[r][c] = O[c][r]`, which swaps width and
//! height with no reversal), the four directions are exactly:
//!
//! | `dir` | value | measured formula | equals |
//! |---|---:|---|---|
//! | `cclock_flip` (default) | 0 | `N[r][c] = O[c][r]` | `T` itself |
//! | `clock` | 1 | `N[r][c] = O[H-1-c][r]` | `hflip(T)` |
//! | `cclock` | 2 | `N[r][c] = O[c][W-1-r]` | `vflip(T)` |
//! | `clock_flip` | 3 | (rows of `clock`'s output, reversed) | `vflip(hflip(T))` |
//!
//! `H`/`W` are the *input's* height/width. This is not the naming a reader
//! would guess from "cclock" alone (a plain transpose is not a rotation at
//! all — it is a rotation *composed with* a flip along the diagonal), which
//! is exactly why it was measured rather than assumed. `dir=cclock_flip`
//! being the plain transpose, and the *default*, was the surprise: a first
//! guess would default to `clock` or `cclock` as "the identity-ish one".
//!
//! # Subsampling: symmetric formats only
//!
//! Transposing swaps a plane's own width and height. For a format whose
//! chroma subsampling is symmetric (4:4:4, 4:2:0, gray, RGB — `log2_chroma_w
//! == log2_chroma_h`) the chroma plane's post-transpose dimensions are still
//! consistent with `PixFmt::plane_layout` for the swapped frame size. For an
//! *asymmetric* format (4:2:2 and siblings, `log2_chroma_w != log2_chroma_h`)
//! they are not: the chroma plane would need a different subsampling factor
//! after the swap than the pixel format declares. Rather than silently
//! produce a mismatched buffer, this filter refuses asymmetric formats with
//! [`vaco_core::Error::Unsupported`] — the reference converts around this
//! internally; a caller here should insert `format=yuv420p` (or similar)
//! before `transpose` when starting from 4:2:2.

use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "transpose",
    description: "Transpose input video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dir {
    CclockFlip,
    Clock,
    Cclock,
    ClockFlip,
}

impl Dir {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "0" | "cclock_flip" => Ok(Self::CclockFlip),
            "1" | "clock" => Ok(Self::Clock),
            "2" | "cclock" => Ok(Self::Cclock),
            "3" | "clock_flip" => Ok(Self::ClockFlip),
            other => Err(format!("transpose: bad `dir` `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Passthrough {
    None,
    Landscape,
    Portrait,
}

impl Passthrough {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "0" | "none" => Ok(Self::None),
            "1" | "landscape" => Ok(Self::Landscape),
            "2" | "portrait" => Ok(Self::Portrait),
            other => Err(format!("transpose: bad `passthrough` `{other}`")),
        }
    }

    fn skip(self, w: u32, h: u32) -> bool {
        match self {
            Self::None => false,
            Self::Landscape => w >= h,
            Self::Portrait => h >= w,
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "transpose", help = "Transpose input video")]
pub(crate) struct Opts {
    #[opt(
        name = "dir",
        help = "set transpose direction",
        default = "cclock_flip".to_owned(),
        flags(video, filtering)
    )]
    pub dir: String,
    #[opt(
        name = "passthrough",
        help = "do not apply transposition if the input matches the specified geometry",
        default = "none".to_owned(),
        flags(video, filtering)
    )]
    pub passthrough: String,
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
    dir: Dir,
    passthrough: Passthrough,
}

impl Filter {
    pub(crate) const fn new(dir: Dir, passthrough: Passthrough) -> Self {
        Self { dir, passthrough }
    }
}

/// Where in the input the output pixel at (new row `nr`, new col `nc`) comes
/// from, for an input of `in_w` × `in_h`. See the module doc's measured table.
fn source_of(dir: Dir, in_w: u32, in_h: u32, nr: u32, nc: u32) -> (u32, u32) {
    // Returns `(old_col, old_row)`. Plain transpose T[r][c] = O[c][r]: new
    // row `nr` reads old column `nr` (new row index selects the old *column*),
    // new col `nc` reads old row `nc`.
    match dir {
        Dir::CclockFlip => (nr, nc),
        Dir::Clock => (nr, in_h.saturating_sub(1).saturating_sub(nc)),
        Dir::Cclock => (in_w.saturating_sub(1).saturating_sub(nr), nc),
        Dir::ClockFlip => (
            in_w.saturating_sub(1).saturating_sub(nr),
            in_h.saturating_sub(1).saturating_sub(nc),
        ),
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format,
            width,
            height,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        geom::ensure_addressable(format)?;
        let (sw, sh) = format.log2_chroma();
        if sw != sh {
            return Err(Error::Unsupported(
                "transpose: asymmetric chroma subsampling (e.g. 4:2:2) is not supported; \
                 convert to a symmetric format first",
            ));
        }
        if self.passthrough.skip(width, height) {
            return Ok(());
        }
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width: w,
                height: h,
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                std::mem::swap(w, h);
                // Not separately measured: inverting SAR is what keeps a
                // square-pixel source displaying at the swapped aspect ratio
                // rather than stretched. If the reference does something
                // else for an anamorphic (non-1:1 SAR) input, this is the
                // divergence to check first.
                *sample_aspect_ratio = sample_aspect_ratio.inverse();
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

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
        if self.passthrough.skip(width, height) {
            return Ok(FrameOut::One(input));
        }
        let mut out = ctx.pool().acquire_video(format, height, width)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            let plane_w = format.plane_width(width, plane_idx);
            let plane_h = format.plane_height(height, plane_idx);
            let new_w = format.plane_width(height, plane_idx);
            let new_h = format.plane_height(width, plane_idx);
            if unit == 1 {
                for nr in 0..new_h {
                    let Some(dst_row) = dst_plane.row_mut(nr as usize) else {
                        continue;
                    };
                    let (source_col, reverse_rows) = match self.dir {
                        Dir::CclockFlip => (nr, false),
                        Dir::Clock => (nr, true),
                        Dir::Cclock | Dir::ClockFlip => {
                            (
                                plane_w.saturating_sub(1).saturating_sub(nr),
                                matches!(self.dir, Dir::ClockFlip),
                            )
                        }
                    };
                    for nc in 0..new_w {
                        let sr = if reverse_rows {
                            plane_h.saturating_sub(1).saturating_sub(nc)
                        } else {
                            nc
                        };
                        let Some(src_row) = src_plane.row(sr as usize) else {
                            continue;
                        };
                        let Some(&value) = src_row.get(source_col as usize) else {
                            continue;
                        };
                        if let Some(dst) = dst_row.get_mut(nc as usize) {
                            *dst = value;
                        }
                    }
                }
                continue;
            }
            for nr in 0..new_h {
                let Some(dst_row) = dst_plane.row_mut(nr as usize) else {
                    continue;
                };
                for nc in 0..new_w {
                    let (sc, sr) = source_of(self.dir, plane_w, plane_h, nr, nc);
                    let Some(src_row) = src_plane.row(sr as usize) else {
                        continue;
                    };
                    let src_start = (sc as usize).saturating_mul(unit);
                    let Some(src_px) = src_row.get(src_start..src_start.saturating_add(unit))
                    else {
                        continue;
                    };
                    let dst_start = (nc as usize).saturating_mul(unit);
                    if let Some(dst_px) = dst_row.get_mut(dst_start..dst_start.saturating_add(unit))
                    {
                        dst_px.copy_from_slice(src_px);
                    }
                }
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = input.color;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio.inverse();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let dir = Dir::parse(&opts.dir)?;
    let passthrough = Passthrough::parse(&opts.passthrough)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(dir, passthrough))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    // Measured against ffmpeg 8.1 with a 2x3 `gray` image where byte
    // `row*10+col` identifies its own origin; see the module doc.
    const ORIGINAL: [[u32; 2]; 3] = [[0, 1], [10, 11], [20, 21]];

    fn apply(dir: Dir) -> Vec<Vec<u32>> {
        let (w, h) = (2u32, 3u32);
        let new_w = h;
        let new_h = w;
        (0..new_h)
            .map(|nr| {
                (0..new_w)
                    .map(|nc| {
                        let (sc, sr) = source_of(dir, w, h, nr, nc);
                        ORIGINAL[sr as usize][sc as usize]
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn cclock_flip_is_the_plain_transpose() {
        assert_eq!(
            apply(Dir::CclockFlip),
            vec![vec![0, 10, 20], vec![1, 11, 21]]
        );
    }

    #[test]
    fn clock_matches_measured_output() {
        assert_eq!(apply(Dir::Clock), vec![vec![20, 10, 0], vec![21, 11, 1]]);
    }

    #[test]
    fn cclock_matches_measured_output() {
        assert_eq!(apply(Dir::Cclock), vec![vec![1, 11, 21], vec![0, 10, 20]]);
    }

    #[test]
    fn clock_flip_matches_measured_output() {
        assert_eq!(
            apply(Dir::ClockFlip),
            vec![vec![21, 11, 1], vec![20, 10, 0]]
        );
    }

    #[test]
    fn passthrough_landscape_skips_a_wide_image() {
        assert!(Passthrough::Landscape.skip(16, 8));
        assert!(!Passthrough::Landscape.skip(8, 16));
    }
}
