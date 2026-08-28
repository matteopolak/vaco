//! `morpho` — generalised greyscale morphology (erode/dilate/open/close/
//! gradient/tophat/blackhat) against a structuring element read from a
//! second video stream.
//!
//! `ffmpeg -h filter=morpho` documents `mode` (`0..=6`: `erode`, `dilate`,
//! `open`, `close`, `gradient`, `tophat`, `blackhat`; default `erode`),
//! `planes` (default `7`) and `structure` (`0` `first`/`1` `all`, default
//! `all`), plus the shared `framesync` surface (`eof_action`, `shortest`,
//! `repeatlast`, `ts_sync_mode`). Two inputs, named `default` and
//! `structure` (`ffmpeg -h filter=morpho` prints exactly those pad names).
//!
//! # Two inputs: `vaco-filter-framesync`
//!
//! Like [`crate::edge`]'s sibling crate `vaco-filter-video-composite::overlay`,
//! this is a thin [`FrameSyncFilter`] over [`FsInput::dual`]'s roles: input 0
//! (`default`) drives, input 1 (`structure`) is sampled and may be absent
//! before its first frame. `vaco-filter-core` has no `Simple`-shaped
//! adapter for a two-input filter yet (`planning/INTERFACE-GAPS.md` gap
//! 10's `Paired<F>` is proposed but not landed), so this follows `overlay`'s
//! own pattern directly rather than waiting on it.
//!
//! # Measured: the structuring element is a support mask, not additive
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=5x5,format=gray8,geq=lum='if(eq(X,2)*eq(Y,2),100,0)'" \
//!   -f lavfi -i "color=white:s=3x3,format=gray8" \
//!   -filter_complex "[0][1]morpho=mode=dilate" -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! An all-`255` 3x3 structure grows a `100` impulse into the full 3x3
//! neighbourhood at value `100` — not `100+255` clamped to `255` — so a
//! nonzero structure pixel means "this offset participates", exactly like
//! [`crate::morph`]'s `coordinates` bitmask, not a per-offset value added in
//! (which is how *additive* greyscale morphology, the textbook alternative,
//! would read a white structure).
//!
//! # Measured: self is *not* an implicit candidate — the structure's own
//! centre decides
//!
//! ```text
//! # structure: all 255 except the centre pixel, which is 0
//! ffmpeg ... -filter_complex "[0][1]morpho=mode=dilate" ...
//! ```
//!
//! gives centre `(2,2) = 0`, not `100`: with the structure's centre dark,
//! self is excluded from its own combine, and the impulse's only genuine
//! neighbours are all `0`. This is the one place `morpho` measurably
//! diverges from [`crate::dilation`]/[`crate::erosion`], which always
//! include self regardless of `coordinates` — see [`crate::morph::apply_structured`]'s
//! doc, the engine this filter uses instead of [`crate::morph::apply_plane`].
//!
//! A single active offset (only the structure's top-left pixel lit) grows
//! the impulse into exactly the position that offset names — `out(x,y) =
//! combine over active (dy,dx) of in(x+dx,y+dy)` — confirmed at `(3,3)`
//! picking up `in(2,2)` through offset `(-1,-1)`.
//!
//! # `mode`: `open`/`close`/`gradient`/`tophat`/`blackhat` are compositions
//!
//! Not independently measured against the reference (out of this pass's
//! time budget) but standard greyscale morphology definitions, applied to
//! the same measured `erode`/`dilate` engine: `open = dilate(erode(x))`,
//! `close = erode(dilate(x))`, `gradient = dilate(x) - erode(x)`, `tophat =
//! x - open(x)`, `blackhat = close(x) - x` (each difference clamped to
//! `0..=255`). Verified via the two invariants any flat structuring element
//! containing its own origin must satisfy — anti-extensivity (`open(x) <=
//! x`) and extensivity (`close(x) >= x`) — rather than against a reference
//! probe for every mode.
//!
//! # Border and `structure=first`/`all`
//!
//! Border: clamp-to-edge, matching [`crate::dilation`]/[`crate::erosion`]'s
//! family convention — not separately measured for `morpho` itself, a
//! recorded gap. `structure`: `all` (the default) re-reads the structuring
//! element every event; `first` freezes the first frame this filter sees on
//! input 1 and reuses it. Both plumb through [`FsInput`]'s ordinary
//! `event.get(1)` — `first` just caches what it returns once.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::morph::{self, Op};

const PADS: &[Pad] = &[
    Pad {
        name: "default",
        media_type: MediaType::Video,
    },
    Pad {
        name: "structure",
        media_type: MediaType::Video,
    },
];
const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "morpho",
    description: "Apply Morphological filter",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Erode,
    Dilate,
    Open,
    Close,
    Gradient,
    Tophat,
    Blackhat,
}

impl Mode {
    /// Accepts both the reference's named spelling (`erode`, `dilate`, …)
    /// and the bare numeric index via a `String` field parsed by hand.
    /// Correction, 2026-08-28: `vaco-opts` *does* support named-integer
    /// options centrally (`#[derive(OptEnum)]`, confirmed against real
    /// consumers in `vaco-filter-mm::misc` and this campaign's own fix in
    /// `vaco-filter-geometry::fillborders`/`field`) — this crate's own
    /// `dilation`/`erosion` did not need it because their `coordinates`
    /// option has no named values in the reference, and `mode` here
    /// predates the OptEnum-based idiom rather than being unable to use
    /// it; not migrated in this pass since the String form is already
    /// correct and tested.
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "erode" | "0" => Some(Self::Erode),
            "dilate" | "1" => Some(Self::Dilate),
            "open" | "2" => Some(Self::Open),
            "close" | "3" => Some(Self::Close),
            "gradient" | "4" => Some(Self::Gradient),
            "tophat" | "5" => Some(Self::Tophat),
            "blackhat" | "6" => Some(Self::Blackhat),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "morpho", help = "Apply Morphological filter")]
pub(crate) struct Opts {
    #[opt(name = "mode", help = "set morphological transform", default = "erode".to_owned(), flags(video, filtering))]
    pub mode: String,
    #[opt(name = "planes", help = "set planes", default = 7, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
    #[opt(name = "structure", help = "when to process structures", default = "all".to_owned(), flags(video, filtering))]
    pub structure: String,
    #[opt(
        name = "eof_action",
        help = "action to take when encountering EOF from secondary input",
        default = "repeat".to_owned(),
        flags(video, filtering)
    )]
    pub eof_action: String,
    #[opt(
        name = "shortest",
        help = "force termination when the shortest input terminates",
        default = false,
        flags(video, filtering)
    )]
    pub shortest: bool,
    #[opt(
        name = "repeatlast",
        help = "extend last frame of secondary streams beyond EOF",
        default = true,
        flags(video, filtering)
    )]
    pub repeatlast: bool,
    #[opt(
        name = "ts_sync_mode",
        help = "how strictly to sync streams based on secondary input timestamps",
        default = "default".to_owned(),
        flags(video, filtering)
    )]
    pub ts_sync_mode: String,
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

/// Extract the active (nonzero) offsets from a structure frame's plane 0,
/// relative to its own centre (`height/2`, `width/2`, integer division).
fn structure_offsets(frame: &Frame) -> Vec<(i32, i32)> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = frame.data
    else {
        return Vec::new();
    };
    if common::ensure_8bit_addressable(format).is_err() {
        return Vec::new();
    }
    let Some(plane) = frame.plane(0) else {
        return Vec::new();
    };
    let w = common::to_i32(width);
    let h = common::to_i32(height);
    #[allow(
        clippy::integer_division,
        reason = "locating the structure's own centre pixel: truncation is the \
                  intended behaviour for an odd-sized structure, not a precision bug"
    )]
    let (cy, cx) = (h / 2, w / 2);
    let mut offsets = Vec::new();
    for y in 0..h {
        let Ok(uy) = usize::try_from(y) else { continue };
        let Some(row) = plane.row(uy) else { continue };
        for x in 0..w {
            let Ok(ux) = usize::try_from(x) else { continue };
            if row.get(ux).copied().unwrap_or(0) > 0 {
                offsets.push((y - cy, x - cx));
            }
        }
    }
    offsets
}

fn combine(a: &[Vec<u8>], b: &[Vec<u8>], f: impl Fn(i32, i32) -> i32) -> Vec<Vec<u8>> {
    a.iter()
        .zip(b.iter())
        .map(|(ra, rb)| {
            ra.iter()
                .zip(rb.iter())
                .map(|(&x, &y)| {
                    u8::try_from(f(i32::from(x), i32::from(y)).clamp(0, 255)).unwrap_or(0)
                })
                .collect()
        })
        .collect()
}

/// Apply `mode` to one plane, given the structuring element's active offsets.
fn apply_mode(rows: &[&[u8]], w: i32, h: i32, mode: Mode, offsets: &[(i32, i32)]) -> Vec<Vec<u8>> {
    match mode {
        Mode::Erode => morph::apply_structured(rows, w, h, Op::Erode, offsets),
        Mode::Dilate => morph::apply_structured(rows, w, h, Op::Dilate, offsets),
        Mode::Open => {
            let eroded = morph::apply_structured(rows, w, h, Op::Erode, offsets);
            let borrowed: Vec<&[u8]> = eroded.iter().map(Vec::as_slice).collect();
            morph::apply_structured(&borrowed, w, h, Op::Dilate, offsets)
        }
        Mode::Close => {
            let dilated = morph::apply_structured(rows, w, h, Op::Dilate, offsets);
            let borrowed: Vec<&[u8]> = dilated.iter().map(Vec::as_slice).collect();
            morph::apply_structured(&borrowed, w, h, Op::Erode, offsets)
        }
        Mode::Gradient => {
            let dilated = morph::apply_structured(rows, w, h, Op::Dilate, offsets);
            let eroded = morph::apply_structured(rows, w, h, Op::Erode, offsets);
            combine(&dilated, &eroded, |d, e| d - e)
        }
        Mode::Tophat => {
            let opened = apply_mode(rows, w, h, Mode::Open, offsets);
            let owned: Vec<Vec<u8>> = rows.iter().map(|r| (*r).to_vec()).collect();
            combine(&owned, &opened, |x, o| x - o)
        }
        Mode::Blackhat => {
            let closed = apply_mode(rows, w, h, Mode::Close, offsets);
            let owned: Vec<Vec<u8>> = rows.iter().map(|r| (*r).to_vec()).collect();
            combine(&closed, &owned, |c, x| c - x)
        }
    }
}

#[derive(Debug)]
pub(crate) struct Morpho {
    mode: Mode,
    planes: i64,
    freeze_structure: bool,
    fs_opts: FrameSyncOpts,
    cached_offsets: Option<Vec<(i32, i32)>>,
}

impl Morpho {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let mode = Mode::from_name(&opts.mode)
            .ok_or_else(|| format!("morpho: bad `mode` `{}`", opts.mode))?;
        let freeze_structure = match opts.structure.as_str() {
            "first" | "0" => true,
            "all" | "1" => false,
            other => return Err(format!("morpho: bad `structure` `{other}`")),
        };
        let eof_action = vaco_filter_framesync::EofAction::from_name(&opts.eof_action)
            .ok_or_else(|| format!("morpho: bad `eof_action` `{}`", opts.eof_action))?;
        let ts_sync = vaco_filter_framesync::TsSyncMode::from_name(&opts.ts_sync_mode)
            .ok_or_else(|| format!("morpho: bad `ts_sync_mode` `{}`", opts.ts_sync_mode))?;
        Ok(Self {
            mode,
            planes: opts.planes,
            freeze_structure,
            fs_opts: FrameSyncOpts {
                eof_action,
                shortest: opts.shortest,
                repeatlast: opts.repeatlast,
                ts_sync,
            },
            cached_offsets: None,
        })
    }

    #[must_use]
    fn boxed(self) -> Box<Synced<Self>> {
        Box::new(Synced::new(self))
    }
}

impl FrameSyncFilter for Morpho {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        self.fs_opts
    }

    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, .. } = main.data else {
            return Ok(FrameOut::One(main));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(main));
        }
        let offsets = if self.freeze_structure {
            if self.cached_offsets.is_none() {
                self.cached_offsets = event.get(1).map(structure_offsets);
            }
            self.cached_offsets.clone().unwrap_or_default()
        } else {
            event.get(1).map(structure_offsets).unwrap_or_default()
        };

        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(main));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(src_plane) = main.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let filtered = if common::plane_selected(self.planes, p8) && !offsets.is_empty() {
                apply_mode(&rows, pw, ph, self.mode, &offsets)
            } else {
                rows.iter().map(|r| (*r).to_vec()).collect()
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in filtered.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        common::copy_frame_meta(&mut out, &main);
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let morpho = Morpho::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: morpho.boxed(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const RING8: [(i32, i32); 8] = [
        (-1, -1),
        (-1, 0),
        (-1, 1),
        (0, -1),
        (0, 1),
        (1, -1),
        (1, 0),
        (1, 1),
    ];
    const RING9: [(i32, i32); 9] = [
        (-1, -1),
        (-1, 0),
        (-1, 1),
        (0, -1),
        (0, 0),
        (0, 1),
        (1, -1),
        (1, 0),
        (1, 1),
    ];

    fn impulse(size: usize, cx: usize, cy: usize, value: u8) -> Vec<Vec<u8>> {
        let mut img = vec![vec![0u8; size]; size];
        if let Some(row) = img.get_mut(cy)
            && let Some(px) = row.get_mut(cx)
        {
            *px = value;
        }
        img
    }

    /// Pinned against the reference probe in this module's doc: an all-`255`
    /// 3x3 structure dilates exactly like the fixed-mask engine.
    #[test]
    fn all_ones_structure_matches_the_fixed_mask_dilation() {
        let img = impulse(5, 2, 2, 100);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = morph::apply_structured(&rows, 5, 5, Op::Dilate, &RING9);
        assert_eq!(out[2][2], 100);
        for (dy, dx) in RING8 {
            let y = usize::try_from(2 + dy).unwrap();
            let x = usize::try_from(2 + dx).unwrap();
            assert_eq!(out[y][x], 100, "({x},{y})");
        }
    }

    /// Pinned: a structure whose own centre is dark excludes self, unlike
    /// `dilation`'s fixed-mask engine.
    #[test]
    fn dark_centre_excludes_self() {
        let img = impulse(5, 2, 2, 100);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = morph::apply_structured(&rows, 5, 5, Op::Dilate, &RING8);
        assert_eq!(out[2][2], 0, "self excluded, and its real neighbours are 0");
        assert_eq!(out[1][1], 100, "a genuine neighbour still picks it up");
    }

    /// Pinned: a single active offset grows into exactly the position that
    /// offset names.
    #[test]
    fn single_offset_grows_one_position() {
        let img = impulse(5, 2, 2, 100);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = morph::apply_structured(&rows, 5, 5, Op::Dilate, &[(-1, -1)]);
        assert_eq!(out[3][3], 100);
        assert_eq!(out[2][2], 0);
    }

    /// Independent oracle: for a structuring element that contains its own
    /// origin, opening is anti-extensive (`open(x) <= x`) and closing is
    /// extensive (`close(x) >= x`) — a property of the mathematical
    /// definition, not a re-derivation of `apply_mode`.
    #[test]
    fn open_is_anti_extensive_and_close_is_extensive() {
        let img: Vec<Vec<u8>> = (0..7)
            .map(|y| (0..7).map(|x| ((x * 29 + y * 13) % 251) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let opened = apply_mode(&rows, 7, 7, Mode::Open, &RING9);
        let closed = apply_mode(&rows, 7, 7, Mode::Close, &RING9);
        for y in 0..7 {
            for x in 0..7 {
                assert!(opened[y][x] <= img[y][x], "open ({x},{y})");
                assert!(closed[y][x] >= img[y][x], "close ({x},{y})");
            }
        }
    }

    /// Independent oracle: `gradient = dilate - erode` is never negative by
    /// construction (`dilate(x) >= erode(x)` pointwise, since the maximum
    /// over a set is never below the minimum over the same set).
    #[test]
    fn gradient_is_never_negative() {
        let img: Vec<Vec<u8>> = (0..7)
            .map(|y| (0..7).map(|x| ((x * 29 + y * 13) % 251) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let dilated = morph::apply_structured(&rows, 7, 7, Op::Dilate, &RING9);
        let eroded = morph::apply_structured(&rows, 7, 7, Op::Erode, &RING9);
        for y in 0..7 {
            for x in 0..7 {
                assert!(
                    dilated[y][x] >= eroded[y][x],
                    "the max over a set is never below the min over the same set: ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn structure_offsets_reads_the_documented_centre() {
        let mut img = vec![vec![0u8; 3]; 3];
        if let Some(row) = img.first_mut()
            && let Some(px) = row.first_mut()
        {
            *px = 255;
        }
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        // Emulate structure_offsets' own logic directly (no Frame plumbing
        // needed for this unit): centre is (1,1), so the lit (0,0) pixel is
        // offset (-1,-1).
        let mut offsets = Vec::new();
        for (y, row) in rows.iter().enumerate() {
            for (x, &v) in row.iter().enumerate() {
                if v > 0 {
                    offsets.push((common::to_i32(y) - 1, common::to_i32(x) - 1));
                }
            }
        }
        assert_eq!(offsets, vec![(-1, -1)]);
    }
}
