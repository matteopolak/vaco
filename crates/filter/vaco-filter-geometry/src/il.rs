//! `il` — deinterleave or interleave a frame's fields, independently per
//! luma/chroma/alpha plane group.
//!
//! `ffmpeg -h filter=il` documents `luma_mode`/`l`, `chroma_mode`/`c`,
//! `alpha_mode`/`a` (`none`=0 default, `interleave`=1, `deinterleave`=2)
//! and `luma_swap`/`ls`, `chroma_swap`/`cs`, `alpha_swap`/`as` (booleans,
//! default `false`).
//!
//! # Measured: the row permutation and its inverse
//!
//! Built a 1x6 `gray` column `[0,10,20,30,40,50]` and ran
//! `il=luma_mode=deinterleave`, then chained `,il=luma_mode=interleave` on
//! the result:
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=1x6,format=gray,geq=lum='Y*10'" \
//!   -vf "il=luma_mode=deinterleave" -f rawvideo -pix_fmt gray -
//! # -> [0,20,40,10,30,50]
//! ffmpeg -f lavfi -i "color=black:s=1x6,format=gray,geq=lum='Y*10'" \
//!   -vf "il=luma_mode=deinterleave,il=luma_mode=interleave" \
//!   -f rawvideo -pix_fmt gray -
//! # -> [0,10,20,30,40,50], the exact original
//! ```
//!
//! So `deinterleave` puts the even rows (`0,2,4,...`) first, then the odd
//! rows (`1,3,5,...`), each half in its original order — the standard
//! "planar field storage" layout — and `interleave` is confirmed to be its
//! exact inverse by the round-trip. `luma_swap`/`chroma_swap`/`alpha_swap`
//! (which half comes first) were not independently measured; implemented
//! as swapping the two halves' roles before applying the mode, the only
//! reading consistent with the option's name.
//!
//! A plane whose height is odd is floored to the nearest even number for
//! the split; the final unpaired row is left untouched, since there is no
//! reference behaviour measured for it.

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
    name: "il",
    description: "Deinterleave or interleave fields",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    None,
    Interleave,
    Deinterleave,
}

impl Mode {
    const fn from_opt(v: i32) -> Self {
        match v {
            1 => Self::Interleave,
            2 => Self::Deinterleave,
            _ => Self::None,
        }
    }
}

/// `ffmpeg -h filter=il`'s own named constants for `luma_mode`/
/// `chroma_mode`/`alpha_mode` -- two names per non-zero value
/// (`interleave`/`i`, `deinterleave`/`d`), which is why this is a
/// hand-written `consts` list on a plain `i32` field rather than
/// `#[derive(OptEnum)]` (that derive emits exactly one name per variant).
/// A real command line using either spelling used to fail to parse
/// against this crate outright.
const IL_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "none",
        help: "",
        unit: "il_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "interleave",
        help: "",
        unit: "il_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "i",
        help: "",
        unit: "il_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "deinterleave",
        help: "",
        unit: "il_mode",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "d",
        help: "",
        unit: "il_mode",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "il", help = "Deinterleave or interleave fields")]
pub(crate) struct Opts {
    #[opt(name = "luma_mode", alias = "l", help = "select luma mode", unit = "il_mode", consts = IL_MODE_CONSTS, default = 0, range = 0..=2, flags(video, filtering))]
    pub luma_mode: i32,
    #[opt(name = "chroma_mode", alias = "c", help = "select chroma mode", unit = "il_mode", consts = IL_MODE_CONSTS, default = 0, range = 0..=2, flags(video, filtering))]
    pub chroma_mode: i32,
    #[opt(name = "alpha_mode", alias = "a", help = "select alpha mode", unit = "il_mode", consts = IL_MODE_CONSTS, default = 0, range = 0..=2, flags(video, filtering))]
    pub alpha_mode: i32,
    #[opt(
        name = "luma_swap",
        alias = "ls",
        help = "swap luma fields",
        default = false,
        flags(video, filtering)
    )]
    pub luma_swap: bool,
    #[opt(
        name = "chroma_swap",
        alias = "cs",
        help = "swap chroma fields",
        default = false,
        flags(video, filtering)
    )]
    pub chroma_swap: bool,
    #[opt(
        name = "alpha_swap",
        alias = "as",
        help = "swap alpha fields",
        default = false,
        flags(video, filtering)
    )]
    pub alpha_swap: bool,
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

/// Row permutation for one plane: `dst[y] = src[perm(y)]`, `None` for the
/// odd leftover row when `h` is odd (copied through unchanged by the
/// caller).
#[allow(
    clippy::integer_division,
    reason = "splitting a plane into two field halves is a whole-row count, not a lossy approximation"
)]
fn perm(mode: Mode, swap: bool, h: u32, y: u32) -> u32 {
    let half = h / 2;
    match mode {
        Mode::None => y,
        Mode::Deinterleave => {
            // dst first half = even src rows, second half = odd src rows
            // (or swapped).
            let (first_step, second_step) = if swap { (1, 0) } else { (0, 1) };
            if y < half {
                y * 2 + first_step
            } else {
                (y - half) * 2 + second_step
            }
        }
        Mode::Interleave => {
            let (first_step, second_step) = if swap { (1, 0) } else { (0, 1) };
            if y % 2 == first_step {
                y / 2
            } else if y % 2 == second_step {
                half + y / 2
            } else {
                y
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    luma: (Mode, bool),
    chroma: (Mode, bool),
    alpha: (Mode, bool),
}

impl Filter {
    pub(crate) const fn new(opts: &Opts) -> Self {
        Self {
            luma: (Mode::from_opt(opts.luma_mode), opts.luma_swap),
            chroma: (Mode::from_opt(opts.chroma_mode), opts.chroma_swap),
            alpha: (Mode::from_opt(opts.alpha_mode), opts.alpha_swap),
        }
    }

    fn mode_for(&self, plane: usize) -> (Mode, bool) {
        match plane {
            0 => self.luma,
            1 | 2 => self.chroma,
            _ => self.alpha,
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let _ = ctx;
        let FrameData::Video { format, .. } = &input.data else {
            return Ok(FrameOut::One(input));
        };
        let format = *format;
        geom::ensure_addressable(format)?;
        let mut out = input.clone();
        for p in 0..format.plane_count() {
            let (mode, swap) = self.mode_for(p);
            if mode == Mode::None {
                continue;
            }
            let Some(src) = input.plane(p) else { continue };
            let h = src.rows() as u32;
            let even_h = h - h % 2;
            let rows: Vec<Vec<u8>> = (0..h)
                .map(|y| {
                    if y >= even_h {
                        src.row(y as usize).map(<[u8]>::to_vec).unwrap_or_default()
                    } else {
                        let sy = perm(mode, swap, even_h, y);
                        src.row(sy as usize).map(<[u8]>::to_vec).unwrap_or_default()
                    }
                })
                .collect();
            if let Some(mut dst) = out.plane_mut(p) {
                for (y, row_data) in rows.iter().enumerate() {
                    if let Some(row) = dst.row_mut(y) {
                        let n = row.len().min(row_data.len());
                        if let (Some(d), Some(s)) = (row.get_mut(..n), row_data.get(..n)) {
                            d.copy_from_slice(s);
                        }
                    }
                }
            }
        }
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn deinterleave_matches_measured_permutation() {
        let src = [0u8, 10, 20, 30, 40, 50];
        let out: Vec<u8> = (0..6)
            .map(|y| src[perm(Mode::Deinterleave, false, 6, y) as usize])
            .collect();
        assert_eq!(out, vec![0, 20, 40, 10, 30, 50]);
    }

    #[test]
    fn interleave_is_the_measured_inverse_of_deinterleave() {
        let src = [0u8, 10, 20, 30, 40, 50];
        let deinterleaved: Vec<u8> = (0..6)
            .map(|y| src[perm(Mode::Deinterleave, false, 6, y) as usize])
            .collect();
        let restored: Vec<u8> = (0..6)
            .map(|y| deinterleaved[perm(Mode::Interleave, false, 6, y) as usize])
            .collect();
        assert_eq!(restored, src.to_vec());
    }

    #[test]
    fn none_mode_is_the_identity() {
        for y in 0..6 {
            assert_eq!(perm(Mode::None, false, 6, y), y);
        }
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=il`): both names for the non-zero values
    /// (`interleave`/`i`, `deinterleave`/`d`) must parse, not just the
    /// bare integers.
    #[test]
    fn named_mode_values_parse() {
        for (name, expected) in [
            ("none", 0),
            ("interleave", 1),
            ("i", 1),
            ("deinterleave", 2),
            ("d", 2),
        ] {
            let opts = Opts::parse(Some(&format!("luma_mode={name}"))).unwrap();
            assert_eq!(opts.luma_mode, expected, "luma_mode={name}");
        }
    }
}
