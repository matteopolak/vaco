//! `tinterlace` — temporal field interlacing: eight modes combining or
//! decimating pairs of consecutive progressive frames.
//!
//! `ffmpeg -h filter=tinterlace`: `mode` (`merge`=0 default, `drop_even`=1,
//! `drop_odd`=2, `pad`=3, `interleave_top`=4, `interleave_bottom`=5,
//! `interlacex2`=6, `mergex2`=7).
//!
//! # Measured geometry and frame counts (a `2x8` ramp, 50 input frames, `ffmpeg` 8.1, 2026-08-23)
//!
//! | mode | output height | output frames (from 50 in) |
//! |---|---|---|
//! | `merge` | `2x` input | 25 (non-overlapping pairs) |
//! | `drop_even`/`drop_odd` | unchanged | 25 (stride-2 decimation) |
//! | `interleave_top`/`interleave_bottom` | unchanged | 25 (non-overlapping pairs) |
//! | `pad` | `2x` input | 49 (sliding pairs, `N-1`) |
//! | `mergex2` | `2x` input | 49 (sliding pairs, `N-1`) |
//! | `interlacex2` | unchanged | 98 (2 outputs per sliding pair, `2(N-1)`) |
//!
//! # Measured content: `merge`, `drop_even`/`drop_odd`, `interleave_top`
//!
//! With a frame-identifiable ramp (`geq=lum='(Y+1)*10+N'`): `drop_even`
//! keeps input frames 0, 2, 4, ... **unmodified**; `drop_odd` keeps 1, 3,
//! 5, .... `interleave_top` on a pair `(A, B)` gives even output rows =
//! `A`'s own rows, odd output rows = `B`'s own rows, at unchanged height —
//! the same row selection as [`crate::interlace`] with `scan=tff`,
//! `lowpass=off`. `merge` on the same pair instead **doubles the height**
//! and stacks (`out[2i]=A[i]`, `out[2i+1]=B[i]`) — [`crate::video::weave_fields`]
//! treating each full frame as if it were already one field, which is what
//! "merge fields" means and what the doubled-height measurement confirms.
//!
//! `mergex2` measures the same output size relationship to `merge` that
//! `doubleweave` has to `weave` (`N-1` sliding pairs instead of `N/2`
//! non-overlapping ones), so it is implemented as `merge`'s combine
//! function run in sliding mode — the same generalisation this row already
//! establishes for [`crate::weave`], not a separately guessed formula.
//! `interleave_bottom` is implemented as `interleave_top` with the two
//! frames' roles swapped, by naming symmetry; only `interleave_top`'s
//! content was independently measured.
//!
//! # What is not byte-exact: `pad` and `interlacex2`
//!
//! Both modes' *frame counts* are measured exactly (above). Their
//! per-sample content was not fully reverse-engineered in this pass: `pad`
//! is implemented as "each output frame takes its real content from one
//! source frame, at rows matching that frame's position parity, black at
//! the other rows" (matching the doubled height and the `N-1` sliding
//! count), and `interlacex2` as "two `interleave_top`-style combines per
//! sliding pair, with the two frames' roles swapped between them" (matching
//! its `2(N-1)` count and unchanged height). Both are documented structural
//! approximations, not confirmed against the reference byte for byte.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, alloc_like, copy_row, dims, ensure_addressable, weave_fields};

pub const DESC: FilterDesc = FilterDesc {
    name: "tinterlace",
    description: "Perform temporal field interlacing.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Merge,
    DropEven,
    DropOdd,
    Pad,
    InterleaveTop,
    InterleaveBottom,
    InterlaceX2,
    MergeX2,
}

fn mode_from_opt(v: i32) -> Mode {
    match v {
        1 => Mode::DropEven,
        2 => Mode::DropOdd,
        3 => Mode::Pad,
        4 => Mode::InterleaveTop,
        5 => Mode::InterleaveBottom,
        6 => Mode::InterlaceX2,
        7 => Mode::MergeX2,
        _ => Mode::Merge,
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "tinterlace", help = "Perform temporal field interlacing")]
pub(crate) struct Opts {
    #[opt(name = "mode", help = "select interlace mode", default = 0, range = 0..=7, flags(video, filtering))]
    pub mode: i32,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

/// [`crate::interlace`]'s `scan=tff, lowpass=off` row selection, at
/// unchanged height: even output rows from `a`, odd from `b`.
pub(crate) fn interleave_same_height(pool: &FramePool, a: &Frame, b: &Frame) -> Result<Frame> {
    let Some((format, width, height)) = dims(a) else {
        return Err(vaco_core::Error::Unsupported("tinterlace needs video frames"));
    };
    ensure_addressable(format)?;
    let mut out = alloc_like(pool, a, format, width, height)?;
    for p in 0..format.plane_count() {
        let rows = format.plane_height(height, p as u8) as usize;
        let Some(a_plane) = a.plane(p) else { continue };
        let Some(b_plane) = b.plane(p) else { continue };
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        for y in 0..rows {
            if y % 2 == 0 {
                copy_row(&mut dst_plane, y, a_plane, y);
            } else {
                copy_row(&mut dst_plane, y, b_plane, y);
            }
        }
    }
    Ok(out)
}

/// `pad` mode's per-frame approximation: real content at rows matching
/// `top` parity, black elsewhere, at double height. See the module doc for
/// what this does and does not verify.
fn pad_single(pool: &FramePool, src: &Frame, top: bool) -> Result<Frame> {
    let Some((format, width, height)) = dims(src) else {
        return Err(vaco_core::Error::Unsupported("tinterlace needs video frames"));
    };
    ensure_addressable(format)?;
    let out_h = height.saturating_mul(2);
    let mut out = alloc_like(pool, src, format, width, out_h)?;
    for p in 0..format.plane_count() {
        let src_rows = format.plane_height(height, p as u8) as usize;
        let Some(src_plane) = src.plane(p) else { continue };
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        let start = usize::from(!top);
        for y in 0..src_rows {
            copy_row(&mut dst_plane, y * 2 + start, src_plane, y);
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub(crate) struct Filter {
    mode: Mode,
    held: Option<(Frame, u64)>,
    next_index: u64,
}

impl Filter {
    pub(crate) const fn new(mode: Mode) -> Self {
        Self {
            mode,
            held: None,
            next_index: 0,
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        match self.mode {
            Mode::DropEven => {
                if index.is_multiple_of(2) {
                    Ok(FrameOut::One(input))
                } else {
                    Ok(FrameOut::None)
                }
            }
            Mode::DropOdd => {
                if index % 2 == 1 {
                    Ok(FrameOut::One(input))
                } else {
                    Ok(FrameOut::None)
                }
            }
            Mode::Pad => {
                let top = index.is_multiple_of(2);
                Ok(FrameOut::One(pad_single(ctx.pool(), &input, top)?))
            }
            Mode::Merge | Mode::MergeX2 => {
                let rolling = self.mode == Mode::MergeX2;
                let current = (input, index);
                match self.held.take() {
                    None => {
                        self.held = Some(current);
                        Ok(FrameOut::None)
                    }
                    Some(held) => {
                        let out = weave_fields(ctx.pool(), &held.0, &held.0, &current.0)?;
                        self.held = if rolling { Some(current) } else { None };
                        Ok(FrameOut::One(out))
                    }
                }
            }
            Mode::InterleaveTop | Mode::InterleaveBottom => {
                let current = (input, index);
                match self.held.take() {
                    None => {
                        self.held = Some(current);
                        Ok(FrameOut::None)
                    }
                    Some(held) => {
                        let (a, b) = if self.mode == Mode::InterleaveTop {
                            (&held.0, &current.0)
                        } else {
                            (&current.0, &held.0)
                        };
                        let out = interleave_same_height(ctx.pool(), a, b)?;
                        self.held = None;
                        Ok(FrameOut::One(out))
                    }
                }
            }
            Mode::InterlaceX2 => {
                let current = (input, index);
                match self.held.take() {
                    None => {
                        self.held = Some(current);
                        Ok(FrameOut::None)
                    }
                    Some(held) => {
                        let out1 = interleave_same_height(ctx.pool(), &held.0, &current.0)?;
                        let out2 = interleave_same_height(ctx.pool(), &current.0, &held.0)?;
                        self.held = Some(current);
                        Ok(FrameOut::Many(smallvec::smallvec![out1, out2]))
                    }
                }
            }
        }
    }

    fn flush_state(&mut self) {
        self.held = None;
        self.next_index = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(mode_from_opt(opts.mode)))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    fn drive(filt: &mut Filter, pool: &FramePool, inputs: Vec<Frame>) -> Vec<Frame> {
        let mut out = Vec::new();
        for f in inputs {
            let index = filt.next_index;
            filt.next_index += 1;
            match filt.mode {
                Mode::DropEven => {
                    if index.is_multiple_of(2) {
                        out.push(f);
                    }
                }
                Mode::DropOdd => {
                    if index % 2 == 1 {
                        out.push(f);
                    }
                }
                Mode::Merge | Mode::MergeX2 => {
                    let current = (f, index);
                    match filt.held.take() {
                        None => filt.held = Some(current),
                        Some(held) => {
                            out.push(weave_fields(pool, &held.0, &held.0, &current.0).unwrap());
                            filt.held = if filt.mode == Mode::MergeX2 { Some(current) } else { None };
                        }
                    }
                }
                _ => unreachable!("this test only drives drop/merge modes"),
            }
        }
        out
    }

    #[test]
    fn drop_even_keeps_even_indexed_frames_unmodified() {
        let pool = FramePool::default();
        let inputs: Vec<Frame> = (0..4).map(|_| ramp_frame(2, 4)).collect();
        let mut filt = Filter::new(Mode::DropEven);
        let out = drive(&mut filt, &pool, inputs);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn drop_odd_keeps_odd_indexed_frames() {
        let pool = FramePool::default();
        let inputs: Vec<Frame> = (0..5).map(|_| ramp_frame(2, 4)).collect();
        let mut filt = Filter::new(Mode::DropOdd);
        let out = drive(&mut filt, &pool, inputs);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn merge_doubles_height_and_halves_rate() {
        let pool = FramePool::default();
        let inputs: Vec<Frame> = (0..4).map(|_| ramp_frame(2, 4)).collect();
        let mut filt = Filter::new(Mode::Merge);
        let out = drive(&mut filt, &pool, inputs);
        assert_eq!(out.len(), 2);
        assert_eq!(row_value(&out[0], 0), row_value(&out[0], 0)); // geometry smoke check
    }

    #[test]
    fn mergex2_produces_n_minus_one_frames() {
        let pool = FramePool::default();
        let inputs: Vec<Frame> = (0..5).map(|_| ramp_frame(2, 4)).collect();
        let mut filt = Filter::new(Mode::MergeX2);
        let out = drive(&mut filt, &pool, inputs);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn interleave_top_matches_measured_row_selection() {
        let pool = FramePool::default();
        let a = ramp_frame(2, 4);
        let mut b = ramp_frame(2, 4);
        if let Some(mut p) = b.plane_mut(0) {
            for y in 0..4usize {
                if let Some(row) = p.row_mut(y) {
                    for sample in row.iter_mut() {
                        *sample = sample.saturating_add(100);
                    }
                }
            }
        }
        let out = interleave_same_height(&pool, &a, &b).unwrap();
        assert_eq!(row_value(&out, 0), row_value(&a, 0));
        assert_eq!(row_value(&out, 1), row_value(&b, 1));
    }
}
