//! A shared motion-adaptive deinterlace core for `yadif`, `bwdif`,
//! `w3fdif`, `estdif` and `kerndeint`.
//!
//! # Honesty about provenance
//!
//! **This is not a transcription of any of the reference's published
//! kernels.** Reproducing `yadif`'s exact per-pixel formula (its spatial
//! edge-direction search, its multi-term motion-check, its output clamp)
//! with confidence would need either the GPL source (D7 forbids reading
//! it) or a public description precise enough to implement byte-exactly —
//! and the sources this pass could reach (deinterlacing forums, `AviSynth`
//! wiki pages, doxygen struct listings) describe the *shape* of the
//! algorithm (spatial+temporal check, edge-directed interpolation) but not
//! its exact coefficients, and several of those pages are themselves
//! close paraphrases of the GPL source, which is a source this project
//! will not read even indirectly. Rather than risk implementing a
//! half-remembered version of someone else's formula and mislabelling it
//! `Vaco-Provenance: spec`, this is an **original**, independently
//! designed motion-adaptive interpolator: for each row that is not part of
//! the frame's own kept field, blend a temporal candidate (same row,
//! adjacent frames) with a spatial candidate (vertical neighbours, same
//! frame), favouring the temporal one when it is corroborated by both
//! neighbours agreeing.
//!
//! # The invariant this design exists to satisfy
//!
//! The row's brief requires: *"yadif/bwdif on progressive input (both
//! fields from one frame) must reproduce the input exactly."* This is true
//! of this design **by construction**, not by a special case: when `prev`,
//! `cur` and `next` are the same static image, the temporal candidate at
//! every non-kept row equals that row's true value exactly (the average of
//! two identical numbers is that number), the motion score is `0`, and the
//! temporal candidate is used unweighted. See this module's own test.
//! `docs/filter/vaco-filter-deinterlace.md` states plainly that none of
//! `yadif`/`bwdif`/`w3fdif`/`estdif`/`kerndeint` are checked byte-for-byte
//! against the reference binary — only this structural property is.
//!
//! # Limitation: 8-bit planar samples only
//!
//! Like `vaco-filter-vdsp`'s own kernels, this operates on raw bytes and is
//! only correct for one-byte-per-sample planar layouts. A 16-bit path is a
//! mechanical extension (`u16` little-endian reads) left for whoever needs
//! it first, per that crate's own such note.

use vaco_core::{Error, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut};
use vaco_filter_core::FilterContext;
use vaco_frame::{Frame, FrameData, FramePool, PlaneRef};

use crate::video::{alloc_like, copy_row, dims, ensure_addressable, is_tff};

/// Which rows of `cur` are genuine ("kept") for this call: rows whose
/// parity matches `parity_tff` (true = even rows kept).
fn is_kept_row(y: usize, parity_tff: bool) -> bool {
    y.is_multiple_of(2) == parity_tff
}

fn sample(plane: PlaneRef<'_>, x: usize, y: usize) -> Option<u8> {
    plane.row(y)?.get(x).copied()
}

/// The interpolated value for one non-kept sample at `(x, y)` of `cur`,
/// given optional temporal neighbours `prev`/`next` at the same position
/// and same-frame spatial neighbours at `y-1`/`y+1`.
#[allow(
    clippy::too_many_arguments,
    clippy::many_single_char_names,
    reason = "a per-pixel kernel genuinely takes this many operands, named for the pixel-math role they play"
)]
fn blend(
    cur: PlaneRef<'_>,
    prev: Option<PlaneRef<'_>>,
    next: Option<PlaneRef<'_>>,
    x: usize,
    y: usize,
    rows: usize,
) -> u8 {
    let above = if y == 0 { sample(cur, x, 0) } else { sample(cur, x, y - 1) };
    let below = if y.saturating_add(1) >= rows {
        sample(cur, x, rows.saturating_sub(1))
    } else {
        sample(cur, x, y + 1)
    };
    let spatial = match (above, below) {
        (Some(a), Some(b)) => Some((u16::from(a) + u16::from(b)).div_ceil(2)),
        (Some(a), None) => Some(u16::from(a)),
        (None, Some(b)) => Some(u16::from(b)),
        (None, None) => None,
    };
    let p = prev.and_then(|p| sample(p, x, y));
    let n = next.and_then(|p| sample(p, x, y));
    let temporal = match (p, n) {
        (Some(a), Some(b)) => Some((u16::from(a) + u16::from(b)).div_ceil(2)),
        (Some(a), None) => Some(u16::from(a)),
        (None, Some(b)) => Some(u16::from(b)),
        (None, None) => None,
    };
    let motion = match (p, n) {
        (Some(a), Some(b)) => a.abs_diff(b),
        _ => 0,
    };
    let value = match (temporal, spatial) {
        (Some(t), Some(s)) => {
            if motion <= 4 {
                t
            } else {
                (t + s).div_ceil(2)
            }
        }
        (Some(t), None) => t,
        (None, Some(s)) => s,
        (None, None) => 128,
    };
    u8::try_from(value.min(255)).unwrap_or(255)
}

/// Deinterlace one frame: rows matching `parity_tff` are copied from `cur`
/// verbatim (genuine), the others are recomputed via [`blend`] using
/// `prev`/`next` as temporal references where available.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] for a non-addressable pixel format.
pub(crate) fn deinterlace_frame(
    pool: &FramePool,
    prev: Option<&Frame>,
    cur: &Frame,
    next: Option<&Frame>,
    parity_tff: bool,
) -> Result<Frame> {
    let Some((format, width, height)) = dims(cur) else {
        return Err(Error::Unsupported("deinterlacing needs a video frame"));
    };
    ensure_addressable(format)?;
    let mut out = alloc_like(pool, cur, format, width, height)?;
    for p in 0..format.plane_count() {
        let rows = format.plane_height(height, p as u8) as usize;
        let cols = format.plane_width(width, p as u8) as usize;
        let Some(cur_plane) = cur.plane(p) else { continue };
        let prev_plane = prev.and_then(|f| f.plane(p));
        let next_plane = next.and_then(|f| f.plane(p));
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        for y in 0..rows {
            if is_kept_row(y, parity_tff) {
                copy_row(&mut dst_plane, y, cur_plane, y);
                continue;
            }
            let Some(dst_row) = dst_plane.row_mut(y) else {
                continue;
            };
            for x in 0..cols.min(dst_row.len()) {
                let v = blend(cur_plane, prev_plane, next_plane, x, y, rows);
                if let Some(b) = dst_row.get_mut(x) {
                    *b = v;
                }
            }
        }
    }
    Ok(out)
}

/// Shared `Simple`-compatible driver for `yadif`/`bwdif`/`w3fdif`/`estdif`/
/// `kerndeint`: buffers one frame of look-ahead so [`deinterlace_frame`] can
/// see `prev`/`cur`/`next`, always in "one output per input" (`send_frame`)
/// shape.
///
/// # What this does not implement
///
/// The reference's `send_field`/`mode=field` variants (bwdif's own default)
/// output *two* frames per input, one per field, at the field rate. This
/// driver always behaves like `mode=send_frame`/`mode=frame` regardless of
/// the `mode` option's parsed value — a real, documented gap for any caller
/// that asked for the field-rate mode. `parity=auto` is approximated as
/// "whatever the first frame's own `TOP_FIELD_FIRST` flag says", fixed for
/// the whole stream, rather than re-detected per frame.
#[derive(Debug)]
pub(crate) struct Lookahead {
    /// `None` means `parity=auto`: resolved from the first frame seen.
    parity_tff: Option<bool>,
    prev: Option<Frame>,
    cur: Option<Frame>,
}

impl Lookahead {
    pub(crate) const fn new(parity_tff: Option<bool>) -> Self {
        Self {
            parity_tff,
            prev: None,
            cur: None,
        }
    }
}

impl FrameFilter for Lookahead {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if self.cur.is_none() {
            if self.parity_tff.is_none() {
                self.parity_tff = Some(is_tff(&input));
            }
            self.cur = Some(input);
            return Ok(FrameOut::None);
        }
        let Some(cur) = self.cur.take() else {
            return Ok(FrameOut::None);
        };
        let parity = self.parity_tff.unwrap_or(true);
        let out = deinterlace_frame(ctx.pool(), self.prev.as_ref(), &cur, Some(&input), parity)?;
        self.prev = Some(cur);
        self.cur = Some(input);
        Ok(FrameOut::One(out))
    }

    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let Some(cur) = self.cur.take() else {
            return Ok(FrameOut::None);
        };
        let parity = self.parity_tff.unwrap_or(true);
        let out = deinterlace_frame(ctx.pool(), self.prev.as_ref(), &cur, None, parity)?;
        self.prev = None;
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.prev = None;
        self.cur = None;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn a_static_sequence_reproduces_exactly() {
        // The invariant this row's brief names explicitly: three identical
        // frames (a genuinely progressive, unmoving source split into
        // fields) must deinterlace back to themselves exactly.
        let pool = FramePool::default();
        let f = ramp_frame(4, 8);
        let out = deinterlace_frame(&pool, Some(&f), &f, Some(&f), true).unwrap();
        for y in 0..8 {
            assert_eq!(row_value(&out, y), row_value(&f, y), "row {y}");
        }
    }

    #[test]
    fn kept_rows_are_always_copied_verbatim() {
        let pool = FramePool::default();
        let f = ramp_frame(4, 8);
        // No temporal reference at all: kept rows must still be exact.
        let out = deinterlace_frame(&pool, None, &f, None, true).unwrap();
        for y in (0..8).step_by(2) {
            assert_eq!(row_value(&out, y), row_value(&f, y), "kept row {y}");
        }
    }
}
