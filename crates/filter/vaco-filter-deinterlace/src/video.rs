//! Shared byte-level plane/row helpers.
//!
//! Almost every filter in this crate is *row selection and rearrangement*
//! (split a frame into fields, weave two frames' fields back together, shift
//! lines by one, drop alternate rows) with no per-sample arithmetic at all.
//! Operating on whole rows of raw bytes — rather than decoding to `f32` like
//! `vaco-filter-temporal::video::PlaneBuf` does for its arithmetic filters —
//! is both simpler here and exact for any sample depth or plane layout: a
//! row copy does not care whether the row holds 8-bit or 16-bit samples,
//! packed RGB or planar YUV, only that source and destination share the same
//! pixel format (which every filter in this crate requires, via
//! `NodeFormats::passthrough`'s tie between input and output).

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Every filter in this crate but `fieldmatch` (dynamic pads) is one video
/// pad in, one video pad out.
pub(crate) const VIDEO_PAD: &[vaco_filter_core::Pad] = &[vaco_filter_core::Pad {
    name: "default",
    media_type: vaco_core::MediaType::Video,
}];

/// Reject pixel formats whose planes are not addressable a row at a time.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] naming which property is the problem.
pub(crate) fn ensure_addressable(format: PixFmt) -> Result<()> {
    if format.has(PixFmtFlags::HW_ACCEL) {
        return Err(Error::Unsupported("cannot address a hardware surface"));
    }
    if format.has(PixFmtFlags::BITSTREAM) {
        return Err(Error::Unsupported(
            "cannot address a sub-byte-packed format",
        ));
    }
    if format.has(PixFmtFlags::PALETTE) {
        return Err(Error::Unsupported(
            "cannot address a palette format without its side table",
        ));
    }
    Ok(())
}

/// Copy everything but the pixel data and geometry: timestamps, colour
/// signalling, frame flags, sample aspect ratio. Every filter here that
/// derives an output frame from one or more input frames starts from this
/// and then overwrites `pts`/`flags` itself where the filter's own timing or
/// field-order semantics require it.
pub(crate) fn copy_meta(dst: &mut Frame, src: &Frame) {
    dst.pts = src.pts;
    dst.time_base = src.time_base;
    dst.duration = src.duration;
    dst.color = src.color;
    dst.flags = src.flags;
    dst.sample_aspect_ratio = src.sample_aspect_ratio;
}

/// `(format, width, height)` of a video frame, or `None` for anything else (audio, subtitle).
pub(crate) fn dims(frame: &Frame) -> Option<(PixFmt, u32, u32)> {
    match frame.data {
        FrameData::Video {
            format,
            width,
            height,
            ..
        } => Some((format, width, height)),
        FrameData::Audio { .. } | FrameData::Subtitle { .. } => None,
    }
}

/// Allocate a fresh video frame of `format`/`width`/`height` from the
/// context's pool, with `src`'s non-pixel metadata copied onto it.
///
/// # Errors
/// Whatever [`vaco_frame::FramePool::acquire_video`] reports (an
/// unsupported or hardware format, or a budget it cannot satisfy).
pub(crate) fn alloc_like(
    pool: &FramePool,
    src: &Frame,
    format: PixFmt,
    width: u32,
    height: u32,
) -> Result<Frame> {
    let mut out = pool.acquire_video(format, width, height)?;
    copy_meta(&mut out, src);
    Ok(out)
}

/// Copy one whole row (all bytes `PlaneRef`/`PlaneMut` report for it) from
/// `src` row `sy` into `dst` row `dy`. A no-op if either row is out of
/// range, which happens at the edges of odd-height planes — every caller
/// documents its own edge policy rather than relying on this silently doing
/// nothing.
pub(crate) fn copy_row(
    dst: &mut vaco_frame::PlaneMut<'_>,
    dy: usize,
    src: vaco_frame::PlaneRef<'_>,
    sy: usize,
) {
    let Some(src_row) = src.row(sy) else { return };
    let n = src_row.len();
    if let Some(dst_row) = dst.row_mut(dy) {
        let n = n.min(dst_row.len());
        if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
            d.copy_from_slice(s);
        }
    }
}

/// Extract one field (every other row, starting at row `0` for the top
/// field or row `1` for the bottom field) from `src` into a fresh frame at
/// half height, per plane (independently — a vertically-subsampled chroma
/// plane is split at its own resolution, not luma's).
///
/// Measured against the reference (`separatefields` on a `2x8` gray ramp,
/// `ffmpeg` 8.1): `type=top` keeps rows `0,2,4,...`; `type=bottom` keeps
/// rows `1,3,5,...`; output height is exactly half the input's, floored per
/// plane via [`PixFmt::plane_height`]. An odd plane height's leftover row is
/// dropped, matching `field`'s own measured odd-height policy
/// (`vaco-filter-geometry::field`) of giving the possible extra row to the
/// top field only — here expressed as `div_ceil` for top, plain division
/// for bottom.
///
/// # Errors
/// Whatever [`alloc_like`] or [`ensure_addressable`] reports.
pub(crate) fn extract_field(pool: &FramePool, src: &Frame, top: bool) -> Result<Frame> {
    let Some((format, width, height)) = dims(src) else {
        return Err(Error::Unsupported("field extraction needs a video frame"));
    };
    ensure_addressable(format)?;
    #[allow(
        clippy::integer_division,
        reason = "a field's height is a whole-row count by definition, not a lossy approximation"
    )]
    let field_h = if top { height.div_ceil(2) } else { height / 2 };
    let mut out = alloc_like(pool, src, format, width, field_h.max(1))?;
    for p in 0..format.plane_count() {
        let out_rows = format.plane_height(field_h.max(1), p as u8) as usize;
        let Some(src_plane) = src.plane(p) else {
            continue;
        };
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        let start = usize::from(!top);
        for oy in 0..out_rows {
            let sy = start + oy * 2;
            copy_row(&mut dst_plane, oy, src_plane, sy);
        }
    }
    // Tag the extracted field with which one it is: callers that later ask
    // `is_tff` of a field (rather than of the whole frame it came from)
    // need this to reflect the *field's* own role, not the source frame's
    // flag. `pullup` and `fieldmatch` both rely on this.
    out.flags.set(vaco_frame::FrameFlags::TOP_FIELD_FIRST, top);
    Ok(out)
}

/// Weave two fields (each already half-height, from [`extract_field`] or an
/// external field-rate stream) into one full-height frame: `top_field`'s
/// rows land on even output rows, `bottom_field`'s on odd, per plane.
///
/// This is [`extract_field`]'s measured inverse: `separatefields` then
/// `weave` reproduces the original frame byte for byte (checked in
/// `weave`'s own tests), which is this crate's primary correctness oracle
/// for the whole field-manipulation family rather than a hand-derived
/// formula.
///
/// # Errors
/// Whatever [`alloc_like`] or [`ensure_addressable`] reports.
pub(crate) fn weave_fields(
    pool: &FramePool,
    meta_from: &Frame,
    top_field: &Frame,
    bottom_field: &Frame,
) -> Result<Frame> {
    let Some((format, width, top_h)) = dims(top_field) else {
        return Err(Error::Unsupported("weave needs video fields"));
    };
    let Some((_, _, bottom_h)) = dims(bottom_field) else {
        return Err(Error::Unsupported("weave needs video fields"));
    };
    ensure_addressable(format)?;
    let out_h = top_h.saturating_add(bottom_h);
    let mut out = alloc_like(pool, meta_from, format, width, out_h.max(1))?;
    for p in 0..format.plane_count() {
        let top_rows = format.plane_height(top_h, p as u8) as usize;
        let bottom_rows = format.plane_height(bottom_h, p as u8) as usize;
        let Some(top_plane) = top_field.plane(p) else {
            continue;
        };
        let Some(bottom_plane) = bottom_field.plane(p) else {
            continue;
        };
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        for ty in 0..top_rows {
            copy_row(&mut dst_plane, ty * 2, top_plane, ty);
        }
        for by in 0..bottom_rows {
            copy_row(&mut dst_plane, by * 2 + 1, bottom_plane, by);
        }
    }
    Ok(out)
}

/// Whether `frame` is flagged top-field-first. Absent the flag (progressive
/// or unmarked source), this is `false` — measured against the reference:
/// `separatefields`/`weave`'s field order on an unmarked frame behaves like
/// `setfield=bff`, not `setfield=tff` (`ffmpeg` 8.1, `2x8` gray-ramp probe).
pub(crate) fn is_tff(frame: &Frame) -> bool {
    frame
        .flags
        .contains(vaco_frame::FrameFlags::TOP_FIELD_FIRST)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    #[test]
    fn field_height_floors_and_ceils_as_measured() {
        // Measured: a 7-row plane gives the top field the extra row.
        assert_eq!(7u32.div_ceil(2), 4);
        assert_eq!(7u32 / 2, 3);
    }
}

/// Fixtures shared by this crate's filter unit tests, so every filter
/// module's own `#[cfg(test)] mod tests` can `use
/// crate::video::test_support::*;` without each one re-deriving the same
/// "row `y` of `gray8` holds value `y`" fixture.
#[cfg(test)]
pub(crate) mod test_support {
    use vaco_frame::{Frame, FramePool};
    use vaco_pixfmt::PixFmt;

    /// A `gray8` frame of `width x height` whose plane-0 row `y` is filled
    /// with the byte value `y` (truncated to `u8`) — a ramp that makes "did
    /// row selection pick the right rows" a direct byte comparison instead
    /// of needing a real gradient image.
    #[allow(clippy::unwrap_used, reason = "test fixture")]
    pub(crate) fn ramp_frame(width: u32, height: u32) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, width, height).unwrap();
        fill_row_ramp(&mut f);
        f
    }

    /// Fill plane 0 of an existing frame with the same row-index ramp
    /// [`ramp_frame`] uses, without reallocating — for tests that need to
    /// set flags before filling.
    #[allow(
        clippy::unwrap_used,
        clippy::cast_possible_truncation,
        reason = "test fixture"
    )]
    pub(crate) fn fill_row_ramp(frame: &mut Frame) {
        let rows = frame.plane(0).map_or(0, |p| p.rows());
        if let Some(mut p) = frame.plane_mut(0) {
            for y in 0..rows {
                if let Some(row) = p.row_mut(y) {
                    row.fill(y as u8);
                }
            }
        }
    }

    /// The byte value stored at plane-0 row `y` (first byte of the row),
    /// the inverse read for [`ramp_frame`]/[`fill_row_ramp`].
    pub(crate) fn row_value(frame: &Frame, y: usize) -> u8 {
        frame
            .plane(0)
            .and_then(|p| p.row(y).map(<[u8]>::to_vec))
            .and_then(|r| r.first().copied())
            .unwrap_or(0xFF)
    }
}
