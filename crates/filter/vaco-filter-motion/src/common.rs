//! Shared 8-bit plane helpers, a small fork of the same predicate every
//! byte-level filter crate in this tree carries independently (see
//! `vaco-filter-artistic::common`'s own doc for why this is not shared).
//!
//! Also holds the grid motion-search, median and translation-warp pieces
//! [`deshake`](crate::deshake) and [`stabdetect`](crate::stabdetect)/
//! [`stabtransform`](crate::stabtransform) share: `deshake` is single-pass
//! causal stabilisation, the `stabdetect`/`stabtransform` pair is the same
//! underlying motion estimate and warp with the two-pass, file-mediated
//! shape `planning/16-filters.md` §4.2's row calls for (`vidstabdetect`/
//! `vidstabtransform`-equivalent — our own transform-file format, not
//! `.trf` compatible, per that row's own note; see `stabdetect`'s doc for
//! why). Extracted here rather than duplicated, per this project's own
//! "grep for the concept before writing it" rule applied *within* one
//! crate, not just across crates.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;
use vaco_pixfmt::PixFmtFlags;
use vaco_filter_vdsp::affine::{AffineMap, bilinear_sample};

/// Reject formats this crate's byte-level, 8-bit-only pixel math cannot
/// address.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] naming which property is the problem.
pub(crate) fn ensure_8bit_addressable(format: PixFmt) -> Result<()> {
    if format.has(PixFmtFlags::HW_ACCEL) {
        return Err(Error::Unsupported("cannot address a hardware surface"));
    }
    if format.has(PixFmtFlags::BITSTREAM) {
        return Err(Error::Unsupported("cannot address a sub-byte-packed format"));
    }
    if format.has(PixFmtFlags::PALETTE) {
        return Err(Error::Unsupported("cannot address a palette format without its side table"));
    }
    if format.max_depth() != 8 {
        return Err(Error::Unsupported("vaco-filter-motion only filters 8-bit samples"));
    }
    Ok(())
}

#[must_use]
pub(crate) fn to_i32<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}

/// Median of `v`, sorting it in place. `0.0` for an empty slice.
pub(crate) fn median(v: &mut [i32]) -> f64 {
    v.sort_unstable();
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    #[allow(clippy::integer_division, reason = "middle index of a sorted slice, truncation is the intended behaviour")]
    let mid = n / 2;
    if n % 2 == 1 {
        v.get(mid).copied().map_or(0.0, f64::from)
    } else {
        let prev = mid.checked_sub(1);
        let (Some(&a), Some(&b)) = (prev.and_then(|i| v.get(i)), v.get(mid)) else {
            return 0.0;
        };
        f64::from(a).midpoint(f64::from(b))
    }
}

/// Median motion vector (`cur` relative to `prev`) over a `3x3` grid of
/// block searches on plane 0. `(0.0, 0.0)` if the frame is too small for
/// even one in-bounds search, or if no block found a match.
pub(crate) fn estimate_motion(prev: &Frame, cur: &Frame, width: u32, height: u32, range: i32) -> (f64, f64) {
    let (Some(p0), Some(c0)) = (prev.plane(0), cur.plane(0)) else {
        return (0.0, 0.0);
    };
    let w = width as usize;
    let h = height as usize;
    #[allow(clippy::integer_division, reason = "block size in pixels, truncation is the intended behaviour")]
    let bw = 32usize.min((w / 4).max(4));
    #[allow(clippy::integer_division, reason = "block size in pixels, truncation is the intended behaviour")]
    let bh = 32usize.min((h / 4).max(4));
    #[allow(clippy::cast_sign_loss, reason = "range is >= 1 by construction at every call site")]
    let range_usize = range.max(1) as usize;
    let margin_x = range_usize.max(bw);
    let margin_y = range_usize.max(bh);
    let two_margin_bw = margin_x.saturating_mul(2).saturating_add(bw);
    let two_margin_bh = margin_y.saturating_mul(2).saturating_add(bh);
    if w <= two_margin_bw || h <= two_margin_bh {
        return (0.0, 0.0);
    }
    let usable_w = w - two_margin_bw;
    let usable_h = h - two_margin_bh;
    let mut dxs: Vec<i32> = Vec::new();
    let mut dys: Vec<i32> = Vec::new();
    for r in 0..3usize {
        for c in 0..3usize {
            #[allow(clippy::integer_division, reason = "grid position in pixels over a fixed 3x3 layout, truncation is the intended behaviour")]
            let bx = margin_x + usable_w * c / 2;
            #[allow(clippy::integer_division, reason = "grid position in pixels over a fixed 3x3 layout, truncation is the intended behaviour")]
            let by = margin_y + usable_h * r / 2;
            let m = vaco_filter_vdsp::motion::search_block(c0, p0, bx, by, bw, bh, range.max(1));
            if m.cost != u32::MAX {
                dxs.push(m.dx);
                dys.push(m.dy);
            }
        }
    }
    if dxs.is_empty() {
        return (0.0, 0.0);
    }
    // `search_block(cur, prev, ...)`'s vector points from the current
    // block's position to where that content was found *in the previous*
    // frame, so the content's actual displacement from prev to cur is the
    // negation of that vector.
    (-median(&mut dxs), -median(&mut dys))
}

/// How an uncovered border pixel (one the warp maps outside the source
/// frame) is filled. Two of the reference's four `edge` modes — see
/// [`deshake`](crate::deshake)'s own doc for which two, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeMode {
    Blank,
    Original,
}

impl EdgeMode {
    pub(crate) fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "0" | "blank" => Ok(Self::Blank),
            "1" | "original" | "2" | "clamp" | "3" | "mirror" => Ok(Self::Original),
            other => Err(format!("bad edge/crop mode `{other}`")),
        }
    }
}

/// Warp every plane of `frame` by the pure-translation correction `corr`
/// (in plane-0/luma pixel units, scaled per plane by that plane's own
/// subsampling ratio), sampling with [`bilinear_sample`] and filling
/// uncovered border pixels per `edge`.
pub(crate) fn warp_translate(
    pool: &FramePool,
    frame: &Frame,
    format: PixFmt,
    width: u32,
    height: u32,
    corr: (f64, f64),
    edge: EdgeMode,
) -> Option<Frame> {
    if width == 0 || height == 0 {
        return None;
    }
    let mut out = pool.acquire_video(format, width, height).ok()?;
    for p in 0..format.plane_count() {
        let p8 = to_i32(p) as u8;
        let pw = format.plane_width(width, p8);
        let ph = format.plane_height(height, p8);
        let scale_x = f64::from(pw) / f64::from(width);
        let scale_y = f64::from(ph) / f64::from(height);
        let map = AffineMap::translation(corr.0 * scale_x, corr.1 * scale_y);
        let (Some(src), Some(mut dst)) = (frame.plane(p), out.plane_mut(p)) else {
            continue;
        };
        let dst_w = to_i32(pw).max(0);
        let dst_h = to_i32(ph).max(0);
        for y in 0..dst_h {
            let Ok(uy) = usize::try_from(y) else { continue };
            for x in 0..dst_w {
                let Ok(ux) = usize::try_from(x) else { continue };
                let (sx, sy) = map.apply(f64::from(x), f64::from(y));
                let sampled = bilinear_sample(src, sx, sy);
                let value = sampled.or_else(|| match edge {
                    EdgeMode::Blank => Some(0),
                    EdgeMode::Original => src.row(uy).and_then(|r| r.get(ux)).copied(),
                });
                if let (Some(v), Some(row)) = (value, dst.row_mut(uy))
                    && let Some(cell) = row.get_mut(ux)
                {
                    *cell = v;
                }
            }
        }
    }
    Some(out)
}
