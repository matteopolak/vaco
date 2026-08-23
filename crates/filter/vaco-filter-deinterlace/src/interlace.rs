//! `interlace` — convert progressive video into interlaced, by taking
//! alternating rows from pairs of consecutive frames.
//!
//! `ffmpeg -h filter=interlace`: `scan` (`tff`=0 default, `bff`=1),
//! `lowpass` (`off`=0, `linear`=1 default, `complex`=2).
//!
//! # Measured: row selection, geometry and the `linear` lowpass kernel
//!
//! `2x8` gray-ramp probes (`ffmpeg` 8.1, 2026-08-23) established:
//!
//! - Output height equals input height (unlike `weave`, which doubles it —
//!   `interlace`'s two inputs are full progressive frames, not half-height
//!   fields, so it keeps only half of each one's rows rather than stacking
//!   two half-height fields).
//! - Two identical frames in produce that frame back unchanged with
//!   `lowpass=off` — the invariant this row's brief names explicitly.
//! - `scan=tff` takes frame A's even rows and frame B's odd rows;
//!   `scan=bff` takes the other assignment.
//! - `lowpass=linear` is a vertical `[1,2,1]/4` filter with edge-clamped
//!   replication, applied to *each source frame's own full column* before
//!   the row subsampling above — confirmed with single-impulse probes: an
//!   impulse of 100 at an interior row produces `25,50,25` at the three
//!   affected rows (`(1*0 + 2*100 + 1*0)/4` etc. for the neighbours,
//!   `(1*0+2*100+1*0)/4=50` at the centre), and an impulse at row 0 produces
//!   `75` there alone (`(1*100 + 2*100 + 1*0)/4` with the edge clamped to a
//!   repeat of row 0), not `50` (which a naive unclamped filter would give).
//! - `lowpass=complex` is **not** byte-exact here: the same impulse-response
//!   probe gave a centre weight that was not constant near the top of the
//!   frame (`0.88` at row 2, `0.75` from row 4 onward, on the same 16-row
//!   test), meaning it is not a simple space-invariant 3-tap kernel, and the
//!   exact tap structure was not resolved in this pass. `complex` reuses
//!   the `linear` kernel as a documented approximation — see the crate docs.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FrameData, FramePool, PlaneRef};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, alloc_like, copy_row, dims, ensure_addressable};

pub const DESC: FilterDesc = FilterDesc {
    name: "interlace",
    description: "Convert progressive video into interlaced.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lowpass {
    Off,
    Linear,
    /// See the module doc: not byte-exact, falls back to [`Lowpass::Linear`].
    ComplexApprox,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "interlace", help = "Convert progressive video into interlaced")]
pub(crate) struct Opts {
    #[opt(name = "scan", help = "scanning mode", default = 0, range = 0..=1, flags(video, filtering))]
    pub scan: i32,
    #[opt(name = "lowpass", help = "vertical low-pass filter", default = 1, range = 0..=2, flags(video, filtering))]
    pub lowpass: i32,
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

/// The `[1,2,1]/4` vertical filter, edge-clamped, at row `y` of a plane with
/// `rows` total rows.
fn filtered_sample(plane: PlaneRef<'_>, x: usize, y: usize, rows: usize, unit: usize) -> u8 {
    let get = |ry: usize| -> u32 {
        plane
            .row(ry)
            .and_then(|r| r.get(x * unit))
            .copied()
            .map_or(0, u32::from)
    };
    let above = if y == 0 { get(0) } else { get(y - 1) };
    let below = if y.saturating_add(1) >= rows { get(rows.saturating_sub(1)) } else { get(y + 1) };
    let center = get(y);
    let sum = above.saturating_add(center.saturating_mul(2)).saturating_add(below);
    #[allow(clippy::integer_division, reason = "fixed 4-tap normalisation, not a lossy size split")]
    let v = (sum.saturating_add(2)) / 4;
    u8::try_from(v.min(255)).unwrap_or(255)
}

fn copy_row_filtered(dst: &mut vaco_frame::PlaneMut<'_>, dy: usize, src: PlaneRef<'_>, sy: usize, rows: usize, unit: usize) {
    let Some(src_row) = src.row(sy) else { return };
    #[allow(
        clippy::integer_division,
        reason = "row length is always a whole multiple of the sample unit width, never a lossy split"
    )]
    let width_samples = src_row.len() / unit.max(1);
    if let Some(dst_row) = dst.row_mut(dy) {
        for x in 0..width_samples {
            let v = filtered_sample(src, x, sy, rows, unit.max(1));
            if let Some(b) = dst_row.get_mut(x * unit.max(1)) {
                *b = v;
            }
        }
    }
}

fn combine(pool: &FramePool, a: &Frame, b: &Frame, tff: bool, lowpass: Lowpass) -> Result<Frame> {
    let Some((format, width, height)) = dims(a) else {
        return Err(vaco_core::Error::Unsupported("interlace needs video frames"));
    };
    ensure_addressable(format)?;
    let mut out = alloc_like(pool, a, format, width, height)?;
    let use_filter = !matches!(lowpass, Lowpass::Off);
    for p in 0..format.plane_count() {
        let rows = format.plane_height(height, p as u8) as usize;
        let unit = a.plane(p).map_or(1, |pl| {
            let w = format.plane_width(width, p as u8) as usize;
            if w == 0 {
                1
            } else {
                #[allow(
                    clippy::integer_division,
                    reason = "row byte length is always a whole multiple of the pixel width, never a lossy split"
                )]
                {
                    pl.row(0).map_or(w, <[u8]>::len) / w.max(1)
                }
            }
        }).max(1);
        let Some(a_plane) = a.plane(p) else { continue };
        let Some(b_plane) = b.plane(p) else { continue };
        let Some(mut dst_plane) = out.plane_mut(p) else { continue };
        for y in 0..rows {
            let from_a = (y % 2 == 0) == tff;
            if from_a {
                if use_filter {
                    copy_row_filtered(&mut dst_plane, y, a_plane, y, rows, unit);
                } else {
                    copy_row(&mut dst_plane, y, a_plane, y);
                }
            } else if use_filter {
                copy_row_filtered(&mut dst_plane, y, b_plane, y, rows, unit);
            } else {
                copy_row(&mut dst_plane, y, b_plane, y);
            }
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub(crate) struct Filter {
    tff: bool,
    lowpass: Lowpass,
    held: Option<Frame>,
}

impl Filter {
    pub(crate) const fn new(tff: bool, lowpass: Lowpass) -> Self {
        Self {
            tff,
            lowpass,
            held: None,
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let Some(a) = self.held.take() else {
            self.held = Some(input);
            return Ok(FrameOut::None);
        };
        let out = combine(ctx.pool(), &a, &input, self.tff, self.lowpass)?;
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.held = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let lowpass = match opts.lowpass {
        0 => Lowpass::Off,
        2 => Lowpass::ComplexApprox,
        _ => Lowpass::Linear,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts.scan == 0, lowpass))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn identical_frames_with_lowpass_off_reproduce_the_frame() {
        // The invariant the row's brief names explicitly.
        let pool = FramePool::default();
        let a = ramp_frame(2, 8);
        let b = ramp_frame(2, 8);
        let out = combine(&pool, &a, &b, true, Lowpass::Off).unwrap();
        for y in 0..8 {
            assert_eq!(row_value(&out, y), row_value(&a, y), "row {y}");
        }
    }

    #[test]
    fn tff_takes_evens_from_a_and_odds_from_b() {
        let pool = FramePool::default();
        let a = ramp_frame(2, 4);
        let mut b = ramp_frame(2, 4);
        // Distinguish a from b: shift b's ramp by 100.
        if let Some(mut p) = b.plane_mut(0) {
            for y in 0..4usize {
                if let Some(row) = p.row_mut(y) {
                    for sample in row.iter_mut() {
                        *sample = sample.saturating_add(100);
                    }
                }
            }
        }
        let out = combine(&pool, &a, &b, true, Lowpass::Off).unwrap();
        assert_eq!(row_value(&out, 0), 0); // from a
        assert_eq!(row_value(&out, 1), 101); // from b (1+100)
        assert_eq!(row_value(&out, 2), 2); // from a
        assert_eq!(row_value(&out, 3), 103); // from b
    }

    #[test]
    fn linear_lowpass_matches_measured_impulse_response() {
        let pool = FramePool::default();
        let mut a = ramp_frame(1, 10);
        if let Some(mut p) = a.plane_mut(0) {
            for y in 0..10usize {
                if let Some(row) = p.row_mut(y) {
                    row.fill(0);
                }
            }
            if let Some(row) = p.row_mut(4) {
                row.fill(100);
            }
        }
        let b = a.clone();
        let out = combine(&pool, &a, &b, true, Lowpass::Linear).unwrap();
        // Measured: impulse of 100 at row4 -> 25,50,25 at rows 3,4,5.
        assert_eq!(row_value(&out, 3), 25);
        assert_eq!(row_value(&out, 4), 50);
        assert_eq!(row_value(&out, 5), 25);
    }
}
