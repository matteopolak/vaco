//! Original motion-adaptive deinterlace core for `yadif`, `bwdif`, `w3fdif`,
//! `estdif`, and `kerndeint`.
//!
//! The reference kernels are GPL and lack a sufficiently precise public
//! description, so this is not a transcription. For each missing row it
//! blends a temporal candidate—three readings of the kept field through
//! [`kept_field_estimate_rows`]—with a same-frame vertical-neighbor candidate,
//! favoring temporal information when adjacent temporal readings agree.
//!
//! Reading `prev` and `next` at the missing row samples the discarded field
//! at other times and can reproduce the artifact instead of estimating the
//! kept field. On a zero-vertical-variation fixture, that earlier approach
//! changed the comb score from 730112 at input to 746224 at output.
//! [`kept_field_estimate_rows`] instead asks the same kept-field interpolation
//! question of every frame before temporal averaging.
//!
//! When `prev`, `cur`, and `next` are the same static image, every estimate
//! agrees, the motion score is zero, and the progressive input is reproduced
//! exactly wherever two spatial neighbors exist. A non-kept top or bottom
//! edge has only one neighbor and therefore carries a bounded one-row
//! limitation, covered explicitly by tests.
//!
//! These filters are checked for that structural property, not byte equality
//! with the reference; see `docs/filter/vaco-filter-deinterlace.md`.
//! The implementation supports only one-byte-per-sample planar layouts.

use vaco_core::{Error, Result};
use vaco_filter_core::FilterContext;
use vaco_filter_core::adapt::{FrameFilter, FrameOut};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel, ops};

use crate::video::{alloc_like, copy_row, dims, ensure_addressable, is_tff};

/// Which rows of `cur` are genuine ("kept") for this call: rows whose
/// parity matches `parity_tff` (true = even rows kept).
fn is_kept_row(y: usize, parity_tff: bool) -> bool {
    y.is_multiple_of(2) == parity_tff
}

/// A time-independent estimate of the *kept field's own* value at `x`, from
/// one pair of neighbouring rows. The caller hoists row lookup out of the
/// pixel loop; this avoids repeating checked stride arithmetic for every
/// sample in a row.
///
/// # Why this is the building block
///
/// Every plane this crate hands to [`blend_rows`] — `prev`, `cur` and `next`
/// alike — shares one field-order convention for the whole stream (see
/// [`Lookahead`]'s own doc), so row `y` is genuinely sampled at *every one*
/// of them, but always at the *other* field's time, never the kept field's.
/// Reading `prev`/`next` at row `y` directly therefore does not estimate
/// the value this call needs to invent; it recovers the *other*, discarded
/// field's own already-known value at a different time, which is not a
/// stand-in for the kept field's row `y` at all. This estimator instead
/// asks the same interpolation question of `prev`/`cur`/`next` alike —
/// "what would this frame's kept field show at row `y`?" — so its answers
/// from three different frames are directly comparable and can be averaged
/// or motion-gated as three readings of the *same* underlying signal at
/// three points in time.
#[inline(always)]
#[allow(
    clippy::inline_always,
    reason = "this helper is called for every output pixel in the hot path"
)]
fn kept_field_estimate_rows(
    above_row: Option<&[u8]>,
    below_row: Option<&[u8]>,
    x: usize,
) -> Option<u16> {
    let above = above_row.and_then(|row| row.get(x).copied());
    let below = below_row.and_then(|row| row.get(x).copied());
    match (above, below) {
        (Some(a), Some(b)) => Some((u16::from(a) + u16::from(b)).div_ceil(2)),
        (Some(a), None) => Some(u16::from(a)),
        (None, Some(b)) => Some(u16::from(b)),
        (None, None) => None,
    }
}

#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::indexing_slicing,
    reason = "the caller validates row lengths and x is below cols; direct indexing avoids per-pixel bounds checks"
)]
fn kept_field_estimate_full(above_row: &[u8], below_row: &[u8], x: usize) -> u16 {
    (u16::from(above_row[x]) + u16::from(below_row[x])).div_ceil(2)
}

/// The interpolated value for one non-kept sample at `(x, y)` of `cur`,
/// given optional temporal neighbours `prev`/`next` and same-frame spatial
/// neighbours at `y-1`/`y+1` — all read via [`kept_field_estimate_rows`], so
/// every candidate this blends targets the same instant (the kept field's
/// own, at `cur`'s time) rather than mixing in a different field's time.
#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::too_many_arguments,
    clippy::many_single_char_names,
    reason = "a per-pixel kernel genuinely takes this many operands, named for the pixel-math role they play"
)]
fn blend_rows(
    cur_above: Option<&[u8]>,
    cur_below: Option<&[u8]>,
    prev_above: Option<&[u8]>,
    prev_below: Option<&[u8]>,
    next_above: Option<&[u8]>,
    next_below: Option<&[u8]>,
    x: usize,
) -> u8 {
    let spatial = kept_field_estimate_rows(cur_above, cur_below, x);
    let p = kept_field_estimate_rows(prev_above, prev_below, x);
    let n = kept_field_estimate_rows(next_above, next_below, x);
    // A one-sided reading (only `prev` or only `next` available, at the
    // first/last frame of a sequence) cannot be corroborated against
    // anything and is not itself time-neutral the way the average of two
    // symmetric readings is (see `kept_field_estimate_rows`'s doc) — blending
    // it in unconditionally would reintroduce a fixed time-offset bias at
    // exactly the frames with no partner to cancel it. Only a `Some`/`Some`
    // pair becomes a temporal candidate; a lone reading falls through to
    // the spatial-only case below instead.
    let temporal = match (p, n) {
        (Some(a), Some(b)) => Some((a + b).div_ceil(2)),
        _ => None,
    };
    let motion = match (p, n) {
        (Some(a), Some(b)) => a.abs_diff(b),
        _ => 0,
    };
    let value = match (temporal, spatial) {
        // Both candidates already estimate the *same* instant (see
        // `kept_field_estimate_rows`'s doc), so low motion averages three
        // readings of one signal (a noise reduction) and high motion
        // drops the temporal one (avoiding ghosting from a scene change)
        // rather than ever blending in a different field's time.
        (Some(t), Some(s)) => {
            if motion <= 4 {
                (t + s).div_ceil(2)
            } else {
                s
            }
        }
        (Some(t), None) => t,
        (None, Some(s)) => s,
        (None, None) => 128,
    };
    u8::try_from(value.min(255)).unwrap_or(255)
}

/// Interior-row variant of [`blend_rows`]. All six row views and the source
/// samples are known to exist, so the per-pixel loop can use direct indexing
/// without carrying `Option` state or repeating bounds checks.
#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::many_single_char_names,
    reason = "this is the fully populated interior-row kernel"
)]
fn blend_full_rows(
    cur_above: &[u8],
    cur_below: &[u8],
    prev_above: &[u8],
    prev_below: &[u8],
    next_above: &[u8],
    next_below: &[u8],
    x: usize,
) -> u8 {
    let spatial = kept_field_estimate_full(cur_above, cur_below, x);
    let p = kept_field_estimate_full(prev_above, prev_below, x);
    let n = kept_field_estimate_full(next_above, next_below, x);
    let temporal = (p + n).div_ceil(2);
    let value = if p.abs_diff(n) <= 4 {
        (temporal + spatial).div_ceil(2)
    } else {
        spatial
    };
    u8::try_from(value.min(255)).unwrap_or(255)
}

/// Vector form of [`blend_full_rows`]. The motion gate is a lane mask, so the
/// scalar branchy decision becomes one unconditional candidate calculation
/// followed by a native select. The scalar tail remains in the caller.
#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "the dispatched body must inline for target features; n is the native lane count and full bounds every chunk"
)]
fn blend_full_rows_simd<S: Lanes>(
    simd: S,
    cur_above: &[u8],
    cur_below: &[u8],
    prev_above: &[u8],
    prev_below: &[u8],
    next_above: &[u8],
    next_below: &[u8],
    dst: &mut [u8],
) {
    let n = <S::u8s as SimdBase<S>>::N;
    let full = (dst.len() / n) * n;
    let threshold = <S::u8s as SimdBase<S>>::splat(simd, 4);
    let mut x = 0;
    while x < full {
        let ca = <S::u8s as SimdBase<S>>::from_slice(simd, &cur_above[x..x + n]);
        let cb = <S::u8s as SimdBase<S>>::from_slice(simd, &cur_below[x..x + n]);
        let pa = <S::u8s as SimdBase<S>>::from_slice(simd, &prev_above[x..x + n]);
        let pb = <S::u8s as SimdBase<S>>::from_slice(simd, &prev_below[x..x + n]);
        let na = <S::u8s as SimdBase<S>>::from_slice(simd, &next_above[x..x + n]);
        let nb = <S::u8s as SimdBase<S>>::from_slice(simd, &next_below[x..x + n]);
        let spatial = ops::simd::rounded_avg_u8::<S, S::u8s>(ca, cb);
        let prev = ops::simd::rounded_avg_u8::<S, S::u8s>(pa, pb);
        let next = ops::simd::rounded_avg_u8::<S, S::u8s>(na, nb);
        let temporal = ops::simd::rounded_avg_u8::<S, S::u8s>(prev, next);
        let blended = ops::simd::rounded_avg_u8::<S, S::u8s>(temporal, spatial);
        let motion = ops::simd::abs_diff_u8::<S, S::u8s>(prev, next);
        let selected = ops::simd::select_u8::<S>(motion.simd_le(threshold), blended, spatial);
        selected.store_slice(&mut dst[x..x + n]);
        x += n;
    }
    for x in full..dst.len() {
        dst[x] = blend_full_rows(
            cur_above, cur_below, prev_above, prev_below, next_above, next_below, x,
        );
    }
}

fn blend_full_rows_dispatch(
    caps: Caps,
    cur_above: &[u8],
    cur_below: &[u8],
    prev_above: &[u8],
    prev_below: &[u8],
    next_above: &[u8],
    next_below: &[u8],
    dst: &mut [u8],
) {
    dispatch_kernel!(caps, s => blend_full_rows_simd(
        s,
        cur_above,
        cur_below,
        prev_above,
        prev_below,
        next_above,
        next_below,
        dst
    ));
}

/// Deinterlace one frame: rows matching `parity_tff` are copied from `cur`
/// verbatim (genuine), the others are recomputed via [`blend_rows`] using
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
    deinterlace_frame_with_caps(pool, prev, cur, next, parity_tff, Caps::detect())
}

fn deinterlace_frame_with_caps(
    pool: &FramePool,
    prev: Option<&Frame>,
    cur: &Frame,
    next: Option<&Frame>,
    parity_tff: bool,
    caps: Caps,
) -> Result<Frame> {
    let Some((format, width, height)) = dims(cur) else {
        return Err(Error::Unsupported("deinterlacing needs a video frame"));
    };
    ensure_addressable(format)?;
    let mut out = alloc_like(pool, cur, format, width, height)?;
    for p in 0..format.plane_count() {
        let rows = format.plane_height(height, p as u8) as usize;
        let cols = format.plane_width(width, p as u8) as usize;
        let Some(cur_plane) = cur.plane(p) else {
            continue;
        };
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
            // `row()` performs checked stride arithmetic and range slicing.
            // Resolve each neighbour once per output row instead of once per
            // pixel; the six row views are reused by the whole inner loop.
            let above = y.checked_sub(1);
            let below = y.checked_add(1).filter(|&by| by < rows);
            let cur_above = above.and_then(|ay| cur_plane.row(ay));
            let cur_below = below.and_then(|by| cur_plane.row(by));
            let prev_above = prev_plane.and_then(|plane| above.and_then(|ay| plane.row(ay)));
            let prev_below = prev_plane.and_then(|plane| below.and_then(|by| plane.row(by)));
            let next_above = next_plane.and_then(|plane| above.and_then(|ay| plane.row(ay)));
            let next_below = next_plane.and_then(|plane| below.and_then(|by| plane.row(by)));
            let full_rows = match (
                cur_above, cur_below, prev_above, prev_below, next_above, next_below,
            ) {
                (
                    Some(cur_above),
                    Some(cur_below),
                    Some(prev_above),
                    Some(prev_below),
                    Some(next_above),
                    Some(next_below),
                ) if cur_above.len() >= cols
                    && cur_below.len() >= cols
                    && prev_above.len() >= cols
                    && prev_below.len() >= cols
                    && next_above.len() >= cols
                    && next_below.len() >= cols =>
                {
                    Some((
                        cur_above, cur_below, prev_above, prev_below, next_above, next_below,
                    ))
                }
                _ => None,
            };
            if let Some((cur_above, cur_below, prev_above, prev_below, next_above, next_below)) =
                full_rows
            {
                blend_full_rows_dispatch(
                    caps,
                    cur_above,
                    cur_below,
                    prev_above,
                    prev_below,
                    next_above,
                    next_below,
                    dst_row.get_mut(..cols).unwrap_or(&mut []),
                );
            } else {
                for (x, dst) in dst_row.iter_mut().take(cols).enumerate() {
                    *dst = blend_rows(
                        cur_above, cur_below, prev_above, prev_below, next_above, next_below, x,
                    );
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
    use vaco_frame::{FramePool, PlaneRef};

    fn reference_sample(plane: PlaneRef<'_>, x: usize, y: usize) -> Option<u8> {
        plane.row(y)?.get(x).copied()
    }

    fn reference_estimate(plane: PlaneRef<'_>, x: usize, y: usize, rows: usize) -> Option<u16> {
        let above = y
            .checked_sub(1)
            .and_then(|ay| reference_sample(plane, x, ay));
        let below = if y.saturating_add(1) < rows {
            reference_sample(plane, x, y + 1)
        } else {
            None
        };
        match (above, below) {
            (Some(a), Some(b)) => Some((u16::from(a) + u16::from(b)).div_ceil(2)),
            (Some(a), None) => Some(u16::from(a)),
            (None, Some(b)) => Some(u16::from(b)),
            (None, None) => None,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::many_single_char_names,
        reason = "test-only reference preserves the former per-pixel call shape"
    )]
    fn reference_blend(
        cur: PlaneRef<'_>,
        prev: Option<PlaneRef<'_>>,
        next: Option<PlaneRef<'_>>,
        x: usize,
        y: usize,
        rows: usize,
    ) -> u8 {
        let spatial = reference_estimate(cur, x, y, rows);
        let p = prev.and_then(|plane| reference_estimate(plane, x, y, rows));
        let n = next.and_then(|plane| reference_estimate(plane, x, y, rows));
        let temporal = match (p, n) {
            (Some(a), Some(b)) => Some((a + b).div_ceil(2)),
            _ => None,
        };
        let motion = match (p, n) {
            (Some(a), Some(b)) => a.abs_diff(b),
            _ => 0,
        };
        let value = match (temporal, spatial) {
            (Some(t), Some(s)) if motion <= 4 => (t + s).div_ceil(2),
            (Some(_), Some(s)) => s,
            (Some(t), None) => t,
            (None, Some(s)) => s,
            (None, None) => 128,
        };
        u8::try_from(value.min(255)).unwrap_or(255)
    }

    fn structured_frame(pool: &FramePool, seed: u8) -> Frame {
        let mut frame = pool
            .acquire_video(vaco_pixfmt::PixFmt::Gray8, 11, 9)
            .unwrap();
        let mut plane = frame.plane_mut(0).unwrap();
        for y in 0..plane.rows() {
            let row = plane.row_mut(y).unwrap();
            for (x, sample) in row.iter_mut().enumerate() {
                *sample = seed
                    .wrapping_add((x as u8).wrapping_mul(19))
                    .wrapping_add((y as u8).wrapping_mul(37));
            }
        }
        frame
    }

    #[test]
    fn row_hoisting_preserves_the_previous_per_pixel_kernel() {
        let pool = FramePool::default();
        let previous = structured_frame(&pool, 3);
        let current = structured_frame(&pool, 71);
        let following = structured_frame(&pool, 149);
        let cur_plane = current.plane(0).unwrap();
        let previous_plane = previous.plane(0).unwrap();
        let following_plane = following.plane(0).unwrap();
        let rows = cur_plane.rows();

        for y in 0..rows {
            let cur_above = y.checked_sub(1).and_then(|ay| cur_plane.row(ay));
            let cur_below = y
                .checked_add(1)
                .filter(|&by| by < rows)
                .and_then(|by| cur_plane.row(by));
            let previous_above = y.checked_sub(1).and_then(|ay| previous_plane.row(ay));
            let previous_below = y
                .checked_add(1)
                .filter(|&by| by < rows)
                .and_then(|by| previous_plane.row(by));
            let following_above = y.checked_sub(1).and_then(|ay| following_plane.row(ay));
            let following_below = y
                .checked_add(1)
                .filter(|&by| by < rows)
                .and_then(|by| following_plane.row(by));

            for x in 0..11 {
                let expected = reference_blend(
                    cur_plane,
                    Some(previous_plane),
                    Some(following_plane),
                    x,
                    y,
                    rows,
                );
                let actual = blend_rows(
                    cur_above,
                    cur_below,
                    previous_above,
                    previous_below,
                    following_above,
                    following_below,
                    x,
                );
                assert_eq!(actual, expected, "x={x}, y={y}");
                if let (
                    Some(cur_above),
                    Some(cur_below),
                    Some(previous_above),
                    Some(previous_below),
                    Some(following_above),
                    Some(following_below),
                ) = (
                    cur_above,
                    cur_below,
                    previous_above,
                    previous_below,
                    following_above,
                    following_below,
                ) {
                    let actual_full = blend_full_rows(
                        cur_above,
                        cur_below,
                        previous_above,
                        previous_below,
                        following_above,
                        following_below,
                        x,
                    );
                    assert_eq!(actual_full, expected, "full x={x}, y={y}");
                }
            }
        }
    }

    #[test]
    fn row_kernel_fallback_preserves_one_sided_temporal_edges() {
        let pool = FramePool::default();
        let previous = structured_frame(&pool, 3);
        let current = structured_frame(&pool, 71);
        let following = structured_frame(&pool, 149);
        let cur_plane = current.plane(0).unwrap();
        let previous_plane = previous.plane(0).unwrap();
        let following_plane = following.plane(0).unwrap();
        let rows = cur_plane.rows();
        let variants: &[(Option<PlaneRef<'_>>, Option<PlaneRef<'_>>)] = &[
            (None, None),
            (Some(previous_plane), None),
            (None, Some(following_plane)),
            (Some(previous_plane), Some(following_plane)),
        ];

        for &y in &[0, rows - 1] {
            let cur_above = y.checked_sub(1).and_then(|ay| cur_plane.row(ay));
            let cur_below = y
                .checked_add(1)
                .filter(|&by| by < rows)
                .and_then(|by| cur_plane.row(by));
            for &(prev, next) in variants {
                let prev_above =
                    prev.and_then(|plane| y.checked_sub(1).and_then(|ay| plane.row(ay)));
                let prev_below = prev.and_then(|plane| {
                    y.checked_add(1)
                        .filter(|&by| by < rows)
                        .and_then(|by| plane.row(by))
                });
                let next_above =
                    next.and_then(|plane| y.checked_sub(1).and_then(|ay| plane.row(ay)));
                let next_below = next.and_then(|plane| {
                    y.checked_add(1)
                        .filter(|&by| by < rows)
                        .and_then(|by| plane.row(by))
                });
                for x in 0..11 {
                    let expected = reference_blend(cur_plane, prev, next, x, y, rows);
                    let actual = blend_rows(
                        cur_above, cur_below, prev_above, prev_below, next_above, next_below, x,
                    );
                    assert_eq!(
                        actual, expected,
                        "x={x}, y={y}, prev={prev:?}, next={next:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn dispatched_interior_kernel_matches_scalar_reference() {
        const WIDTH: usize = 67;
        let row = |seed: u8, scale: u8| {
            (0..WIDTH)
                .map(|x| {
                    let x = u8::try_from(x % 256).unwrap();
                    seed.wrapping_add(x.wrapping_mul(scale))
                })
                .collect::<Vec<_>>()
        };
        let cur_above = row(7, 3);
        let cur_below = row(41, 5);
        let prev_above = row(19, 7);
        let prev_below = row(83, 11);
        let next_above = prev_above
            .iter()
            .enumerate()
            .map(|(x, &v)| v.wrapping_add(if x.is_multiple_of(2) { 2 } else { 32 }))
            .collect::<Vec<_>>();
        let next_below = prev_below
            .iter()
            .enumerate()
            .map(|(x, &v)| v.wrapping_add(if x.is_multiple_of(2) { 2 } else { 32 }))
            .collect::<Vec<_>>();
        let mut dispatched = vec![0; WIDTH];
        let mut scalar = vec![0; WIDTH];
        blend_full_rows_dispatch(
            Caps::detect(),
            &cur_above,
            &cur_below,
            &prev_above,
            &prev_below,
            &next_above,
            &next_below,
            &mut dispatched,
        );
        for (x, dst) in scalar.iter_mut().enumerate() {
            *dst = blend_full_rows(
                &cur_above,
                &cur_below,
                &prev_above,
                &prev_below,
                &next_above,
                &next_below,
                x,
            );
        }
        assert_eq!(dispatched, scalar);
    }

    #[test]
    fn short_neighbour_rows_use_the_safe_fallback() {
        let above = [17_u8, 23];
        let below: [u8; 0] = [];
        assert_eq!(
            kept_field_estimate_rows(Some(&above), Some(&below), 0),
            Some(17)
        );
        assert_eq!(
            kept_field_estimate_rows(Some(&above), Some(&below), 1),
            Some(23)
        );
        assert_eq!(
            kept_field_estimate_rows(Some(&above), Some(&below), 2),
            None
        );
        assert_eq!(
            blend_rows(Some(&above), Some(&below), None, None, None, None, 1),
            23
        );
    }

    #[test]
    fn a_static_sequence_reproduces_exactly() {
        // The invariant this row's brief names explicitly: three identical
        // frames (a genuinely progressive, unmoving source split into
        // fields) must deinterlace back to themselves exactly — everywhere
        // a spatial estimate has two same-frame neighbours to average.
        //
        // The one place this cannot hold, and provably cannot for *any*
        // single-neighbour spatial estimator: a non-kept row at the very
        // edge of the frame (here, row 7, the last row of an 8-row plane)
        // has only one same-parity neighbour (row 6) to interpolate from,
        // so on a source whose true value genuinely varies row-to-row (this
        // fixture is a ramp, one unit per row, chosen to expose exactly
        // this), the one-sided estimate is off by the local slope — here,
        // by exactly 1. That is a real, bounded, structural edge limitation
        // (the same shape as this crate's other documented border
        // policies, e.g. `extract_field`'s), not a defect: it is checked
        // explicitly below rather than silently excluded.
        let pool = FramePool::default();
        let f = ramp_frame(4, 8);
        let out = deinterlace_frame(&pool, Some(&f), &f, Some(&f), true).unwrap();
        for y in 0..7 {
            assert_eq!(row_value(&out, y), row_value(&f, y), "row {y}");
        }
        let edge_diff = i32::from(row_value(&out, 7)).abs_diff(i32::from(row_value(&f, 7)));
        assert_eq!(
            edge_diff, 1,
            "the bottom-edge one-sided estimate's error should be exactly the ramp's own slope"
        );
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

/// Black-box comparison of the generic engine with `yadif`, `bwdif`,
/// `w3fdif`, and `estdif` on moving interlaced content.
///
/// `testsrc2` could not isolate combing: after `tinterlace=4`, its progressive
/// and interlaced comb scores were 332712 and 333132 because intrinsic spatial
/// detail dominated the metric. The actual fixture is a flat-per-row,
/// horizontally scrolling ramp (`geq=lum='mod(X*4+N*8,256)'`) passed through
/// `tinterlace=4`. Its progressive comb score is exactly zero, so the raised
/// score measures only alternating-row temporal splicing.
///
/// The 64x48, eight-frame fixture is generated for each run. This engine
/// reduces comb scores from hundreds of thousands to single-digit per-frame
/// byte residuals. Y/U/V PSNR is compared with each reference filter using a
/// conservative floor, while measured values are printed for inspection.
/// See `docs/filter/vaco-filter-deinterlace.md` for the recorded scope.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    reason = "test code shelling out to a real ffmpeg on a small fixed-size 4:2:0 fixture"
)]
mod oracle {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use vaco_pixfmt::PixFmt;

    const W: u32 = 64;
    const H: u32 = 48;
    const FRAMES: usize = 8;

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg").arg("-version").output().is_ok()
    }

    /// Runs `ffmpeg`, feeding `stdin_bytes` if given. `None` only for a
    /// *launch* failure (binary missing); once launched, a non-zero exit
    /// prints the command and stderr and returns `None` too, but by then
    /// the caller has already confirmed the binary is present, so callers
    /// treat a `None` here as a hard failure, not a skip.
    fn run_ffmpeg(args: &[&str], stdin_bytes: Option<&[u8]>) -> Option<Vec<u8>> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(args)
            .stdin(if stdin_bytes.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().ok()?;
        if let Some(bytes) = stdin_bytes {
            child.stdin.take()?.write_all(bytes).ok()?;
        }
        let out = child.wait_with_output().ok()?;
        if !out.status.success() {
            eprintln!(
                "ffmpeg {args:?} exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            return None;
        }
        Some(out.stdout)
    }

    fn frame_byte_len(w: u32, h: u32) -> usize {
        let luma = (w * h) as usize;
        let chroma = ((w / 2) * (h / 2)) as usize;
        luma + 2 * chroma
    }

    fn frame_from_yuv420p(pool: &FramePool, w: u32, h: u32, bytes: &[u8]) -> Frame {
        let format = PixFmt::Yuv420p;
        let mut f = pool.acquire_video(format, w, h).unwrap();
        let mut offset = 0usize;
        for p in 0..format.plane_count() {
            let p = p as u8;
            let rows = format.plane_height(h, p) as usize;
            let cols = format.plane_width(w, p) as usize;
            let mut plane = f.plane_mut(p as usize).unwrap();
            for y in 0..rows {
                let src = &bytes[offset..offset + cols];
                if let Some(row) = plane.row_mut(y) {
                    let n = cols.min(row.len());
                    row[..n].copy_from_slice(&src[..n]);
                }
                offset += cols;
            }
        }
        f
    }

    fn plane_bytes(frame: &Frame, format: PixFmt, w: u32, h: u32, p: u8) -> Vec<u8> {
        let rows = format.plane_height(h, p) as usize;
        let cols = format.plane_width(w, p) as usize;
        let plane = frame.plane(p as usize).unwrap();
        let mut out = Vec::new();
        for y in 0..rows {
            if let Some(row) = plane.row(y) {
                out.extend_from_slice(&row[..cols.min(row.len())]);
            }
        }
        out
    }

    fn psnr_u8(a: &[u8], b: &[u8]) -> f64 {
        let n = a.len().min(b.len());
        if n == 0 {
            return f64::INFINITY;
        }
        let mse: f64 = a[..n]
            .iter()
            .zip(&b[..n])
            .map(|(&x, &y)| {
                let d = f64::from(x) - f64::from(y);
                d * d
            })
            .sum::<f64>()
            / n as f64;
        if mse == 0.0 {
            return f64::INFINITY;
        }
        20.0 * 255.0f64.log10() - 10.0 * mse.log10()
    }

    /// `comb_score` (see `vaco_filter_vdsp`) summed over every plane of a
    /// frame, so a whole-frame "how combed is this" number can be compared
    /// before and after deinterlacing.
    fn frame_comb_score(frame: &Frame, format: PixFmt) -> u64 {
        (0..format.plane_count())
            .filter_map(|p| frame.plane(p))
            .map(vaco_filter_vdsp::comb_score)
            .sum()
    }

    #[test]
    fn measured_against_real_ffmpeg_deinterlacers() {
        if !ffmpeg_available() {
            eprintln!("skipping measured_against_real_ffmpeg_deinterlacers: ffmpeg not on PATH");
            return;
        }
        let format = PixFmt::Yuv420p;
        let size = format!("{W}x{H}");
        // A flat-per-row, continuously horizontally-scrolling ramp: zero
        // progressive comb score by construction, so `tinterlace`'s
        // alternating-row time splice is the *only* source of comb score
        // in the interlaced version — see this module's doc for why
        // `testsrc2` could not be used for this measurement.
        let interlaced = run_ffmpeg(
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=size={size}:rate=50:c=black"),
                "-vf",
                r"geq=lum='mod(X*4+N*8\,256)':cb=128:cr=128,tinterlace=4",
                "-frames:v",
                &FRAMES.to_string(),
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "-",
            ],
            None,
        )
        .expect("ffmpeg is on PATH; generating the fixture must succeed");
        let fbytes = frame_byte_len(W, H);
        assert_eq!(
            interlaced.len(),
            fbytes * FRAMES,
            "fixture is not the expected size; ffmpeg's own geq/tinterlace behaviour changed"
        );

        let pool = FramePool::default();
        let frames: Vec<Frame> = (0..FRAMES)
            .map(|i| frame_from_yuv420p(&pool, W, H, &interlaced[i * fbytes..(i + 1) * fbytes]))
            .collect();

        // tinterlace=4 (interleave_top) is top-field-first by construction.
        let ours: Vec<Frame> = (0..frames.len())
            .map(|i| {
                let prev = i.checked_sub(1).map(|j| &frames[j]);
                let next = frames.get(i + 1);
                deinterlace_frame(&pool, prev, &frames[i], next, true).unwrap()
            })
            .collect();

        let input_comb: u64 = frames.iter().map(|f| frame_comb_score(f, format)).sum();
        let our_comb: u64 = ours.iter().map(|f| frame_comb_score(f, format)).sum();
        // Structural claim: real deinterlacing happened, not a pass-through
        // or a random-looking substitute. Checked once, on the sum across
        // all six frames, then individually below so one lucky frame
        // cannot hide a broken one.
        assert!(
            our_comb * 10 < input_comb,
            "deinterlaced output is not markedly less combed than the raw interlaced input \
             (input comb={input_comb}, ours comb={our_comb}): this is a structural defect, not a rounding one"
        );
        for (i, (inp, out)) in frames.iter().zip(&ours).enumerate() {
            let ic = frame_comb_score(inp, format);
            let oc = frame_comb_score(out, format);
            assert!(
                oc * 4 < ic,
                "frame {i}: comb score did not drop convincingly (input={ic}, ours={oc})"
            );
        }

        // This crate's `Lookahead` only ever implements the "one output
        // frame per input frame" shape (see its own doc). `bwdif`,
        // `w3fdif` and `estdif` default to the reference's *field-rate*
        // mode (two outputs per input) — a different, already-documented
        // gap, not something this comparison should trip over — so each
        // is pinned to its own frame-rate mode option here for an
        // apples-to-apples comparison. `yadif`'s default already is
        // frame-rate.
        for (name, vf) in [
            ("yadif", "yadif=mode=send_frame"),
            ("bwdif", "bwdif=mode=send_frame"),
            ("w3fdif", "w3fdif=mode=frame"),
            ("estdif", "estdif=mode=frame"),
        ] {
            let reference = run_ffmpeg(
                &[
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-s",
                    &size,
                    "-r",
                    "25",
                    "-i",
                    "-",
                    "-vf",
                    vf,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "rawvideo",
                    "-",
                ],
                Some(&interlaced),
            )
            .unwrap_or_else(|| {
                panic!("ffmpeg -vf {vf} failed on a fixture ffmpeg itself just produced")
            });
            assert_eq!(
                reference.len(),
                fbytes * FRAMES,
                "{name}: reference output is not the expected size"
            );
            let ref_frames: Vec<Frame> = (0..FRAMES)
                .map(|i| frame_from_yuv420p(&pool, W, H, &reference[i * fbytes..(i + 1) * fbytes]))
                .collect();

            for (plane_idx, plane_name) in [(0u8, "Y"), (1, "U"), (2, "V")] {
                let mut sum_psnr = 0.0;
                for (out, refer) in ours.iter().zip(&ref_frames) {
                    let a = plane_bytes(out, format, W, H, plane_idx);
                    let b = plane_bytes(refer, format, W, H, plane_idx);
                    let p = psnr_u8(&a, &b);
                    sum_psnr += p;
                    // Per D6/705779d, an individual frame's plane must not
                    // be wildly worse than the average: a single wrecked
                    // frame or plane hiding behind a healthy mean is
                    // exactly the "structured deviation" the ruling calls
                    // a bug, so it is checked here rather than only on the
                    // averaged number below.
                    assert!(
                        p.is_infinite() || p > 12.0,
                        "{name}/{plane_name}: one frame's PSNR ({p:.1} dB) is far below the \
                         rest — looks structural, not a general algorithm disagreement"
                    );
                }
                let mean = sum_psnr / FRAMES as f64;
                eprintln!("{name}/{plane_name}: mean PSNR vs real ffmpeg = {mean:.2} dB");
                assert!(
                    mean > 18.0,
                    "{name}/{plane_name}: mean PSNR against real ffmpeg is only {mean:.2} dB, \
                     too low to call this the same picture"
                );
            }
        }
    }

    /// Same measurement, on busy, realistic content (`testsrc2`) rather
    /// than the clean synthetic ramp above.
    ///
    /// # Why this exists in addition to the ramp fixture
    ///
    /// The ramp above is a genuine, discriminating test — but it is also
    /// *unambiguous* motion (perfectly linear, perfectly flat spatially),
    /// which is exactly the case where any competent temporal interpolator
    /// converges on the same answer (measured: all four real filters and
    /// this crate agreed byte-for-byte on it). That is real evidence this
    /// crate's core reconstruction is mathematically sound, but it is not
    /// evidence about disagreement on genuinely detailed content, where
    /// the reference's own undocumented edge-direction heuristics (see
    /// this module's top doc) and this crate's simpler original design
    /// can and do pick different answers. This test measures that case
    /// honestly instead of leaving it assumed.
    ///
    /// # Measured result
    ///
    /// Y/U/V PSNR against real `yadif` on `testsrc2`, measured: 24.01 dB
    /// (Y), 27.83 dB (U), 28.14 dB (V); comb score 689384 -> 251126 (a
    /// 63.6% reduction). The assertions below use floors well under these
    /// figures rather than pinning to them exactly, per this project's own
    /// rule against hard-coding a number that invites a future
    /// tolerance-widening rather than a real look; the real numbers are
    /// printed on every run via `--nocapture`. Consistent with "two
    /// reasonable but different deinterlacers", not with either side being
    /// broken. The
    /// comb-score check confirms this crate's own output is still a real
    /// deinterlace on this content, not merely plausible-looking noise:
    /// `testsrc2`'s own detail keeps the comb score away from zero even
    /// after a correct deinterlace (see the ramp fixture's own doc for why
    /// `testsrc2` cannot be used for the *zero-baseline* comb assertion),
    /// so this checks a substantial relative reduction instead of a near-
    /// zero absolute one.
    #[test]
    fn measured_against_real_yadif_on_busy_content() {
        if !ffmpeg_available() {
            eprintln!("skipping measured_against_real_yadif_on_busy_content: ffmpeg not on PATH");
            return;
        }
        let format = PixFmt::Yuv420p;
        let size = format!("{W}x{H}");
        let interlaced = run_ffmpeg(
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={size}:rate=50"),
                "-vf",
                "tinterlace=4",
                "-frames:v",
                &FRAMES.to_string(),
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "-",
            ],
            None,
        )
        .expect("ffmpeg is on PATH; generating the fixture must succeed");
        let fbytes = frame_byte_len(W, H);
        assert_eq!(interlaced.len(), fbytes * FRAMES, "unexpected fixture size");

        let pool = FramePool::default();
        let frames: Vec<Frame> = (0..FRAMES)
            .map(|i| frame_from_yuv420p(&pool, W, H, &interlaced[i * fbytes..(i + 1) * fbytes]))
            .collect();
        let ours: Vec<Frame> = (0..frames.len())
            .map(|i| {
                let prev = i.checked_sub(1).map(|j| &frames[j]);
                let next = frames.get(i + 1);
                deinterlace_frame(&pool, prev, &frames[i], next, true).unwrap()
            })
            .collect();

        let input_comb: u64 = frames.iter().map(|f| frame_comb_score(f, format)).sum();
        let our_comb: u64 = ours.iter().map(|f| frame_comb_score(f, format)).sum();
        eprintln!("busy content: input comb={input_comb}, ours comb={our_comb}");
        assert!(
            our_comb * 2 < input_comb,
            "on busy content, deinterlacing should still at least halve the comb score \
             (input={input_comb}, ours={our_comb})"
        );

        let reference = run_ffmpeg(
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                &size,
                "-r",
                "25",
                "-i",
                "-",
                "-vf",
                "yadif=mode=send_frame",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "-",
            ],
            Some(&interlaced),
        )
        .expect("ffmpeg -vf yadif failed on a fixture ffmpeg itself just produced");
        assert_eq!(
            reference.len(),
            fbytes * FRAMES,
            "reference output is not the expected size"
        );
        let ref_frames: Vec<Frame> = (0..FRAMES)
            .map(|i| frame_from_yuv420p(&pool, W, H, &reference[i * fbytes..(i + 1) * fbytes]))
            .collect();

        for (plane_idx, plane_name) in [(0u8, "Y"), (1, "U"), (2, "V")] {
            let mut sum_psnr = 0.0;
            for (out, refer) in ours.iter().zip(&ref_frames) {
                let a = plane_bytes(out, format, W, H, plane_idx);
                let b = plane_bytes(refer, format, W, H, plane_idx);
                sum_psnr += psnr_u8(&a, &b);
            }
            let mean = sum_psnr / FRAMES as f64;
            eprintln!("busy content yadif/{plane_name}: mean PSNR vs real ffmpeg = {mean:.2} dB");
            assert!(
                mean > 15.0,
                "busy content yadif/{plane_name}: mean PSNR against real ffmpeg is only \
                 {mean:.2} dB, too low to call this the same picture"
            );
        }
    }
}
