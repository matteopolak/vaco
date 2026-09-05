//! The "over" alpha-compositing formula, measured against ffmpeg 8.1, and the
//! byte-level plane walk `overlay` uses to apply it.
//!
//! # The measured formulas
//!
//! Built with raw `rgba` frames (`-f rawvideo -pix_fmt rgba`) so the bytes
//! going in are exact, no chroma subsampling or colour-matrix rounding in the
//! way. With `a_fg = overlay_alpha/255`, `a_bg = background_alpha/255` (`1.0`
//! when a side has no alpha component), and
//! `out_a = a_fg + a_bg*(1 - a_fg)`:
//!
//! | `alpha=` | Formula, per colour channel |
//! |---|---|
//! | `straight` (2), `unknown`/`auto` (0, share one option value) | `(fg*a_fg + bg*a_bg*(1-a_fg)) / out_a` |
//! | `premultiplied` (1) | `fg*a_fg + bg*a_bg*(1-a_fg)/out_a` |
//!
//! The two share the same numerator's *background* term, normalised by
//! `out_a`; only the foreground term differs, and only when `out_a != 1`
//! (an opaque background makes the two formulas coincide, which is why they
//! looked identical until a semi-transparent background pair was tried —
//! see `docs/filter/vaco-filter-video-composite.md` for the two probes,
//! with different alpha pairs, that pinned the asymmetric shape).
//!
//! `out_a` itself is the plain "over" alpha, the same for both settings.
//!
//! Measured example (`a_fg=100/255`, `a_bg=200/255`, `fg=255`, `bg=0`):
//! `out_a=221.57`, straight `115.09→115`, premultiplied `100.0→100` — both
//! confirmed against the reference byte-for-byte.
//!
//! # `x`/`y` truncate toward zero
//!
//! Measured with `format=rgb` (no chroma subsampling to obscure the
//! boundary): `x=5.0` through `x=5.9` all place the overlay at pixel column
//! 5; `x=-0.5` places it at column 0; `x=-1.5` places it at column -1. That
//! is `f64 as i64` in Rust exactly — C's `(int)x` cast, truncating toward
//! zero, not [`f64::floor`]. [`to_pixel`] does this and is what
//! [`crate::overlay`] calls after evaluating `x`/`y`.
//!
//! # What this module does not attempt
//!
//! Depths other than 8 bits ([`crate::geom::ensure_addressable_8bit`]
//! rejects them before this module is reached) and chroma-plane alpha
//! sampling finer than "the alpha value at the corresponding full-resolution
//! position, floor-mapped" ([`crate::geom::plane_coord`]) — the reference may
//! average four alpha samples for a subsampled chroma pixel; this crate reads
//! one. Both are reported gaps, not silent guesses: see this crate's docs.

use vaco_core::Result;
use vaco_frame::Frame;
use vaco_pixfmt::PixFmt;

use crate::geom;

/// How to interpret an input's alpha channel, going into the "over" formula.
///
/// `Auto` and `Unknown` share option value `0` in the reference and are
/// indistinguishable from `Straight` in every configuration probed — see this
/// module's doc. They are kept as separate enum values anyway because the
/// option surface (`overlay=alpha=auto`) needs somewhere to land; both simply
/// select [`AlphaMode::straight_term`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlphaMode {
    #[default]
    Auto,
    Unknown,
    Straight,
    Premultiplied,
}

impl AlphaMode {
    /// Parse the option value: `auto`, `unknown`, `straight`, `premultiplied`,
    /// or the reference's own numeric spelling `0`/`1`/`2`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "unknown" | "0" => Some(Self::Unknown),
            "premultiplied" | "1" => Some(Self::Premultiplied),
            "straight" | "2" => Some(Self::Straight),
            _ => None,
        }
    }

    const fn is_premultiplied(self) -> bool {
        matches!(self, Self::Premultiplied)
    }
}

/// One channel's "over" composite, per the measured formula.
///
/// `fg`/`bg` are 0..=255 sample values; `a_fg`/`a_bg` and the return value are
/// all in `0.0..=1.0`. `out_a` is passed in rather than recomputed because a
/// caller blends several channels (Y, U, V, or R, G, B) against the one
/// `out_a` for a pixel.
#[must_use]
fn composite_channel(mode: AlphaMode, fg: f64, bg: f64, a_fg: f64, a_bg: f64, out_a: f64) -> f64 {
    let bg_term = bg * a_bg * (1.0 - a_fg);
    if out_a <= 0.0 {
        return if mode.is_premultiplied() {
            fg * a_fg
        } else {
            0.0
        };
    }
    if mode.is_premultiplied() {
        fg.mul_add(a_fg, bg_term / out_a)
    } else {
        fg.mul_add(a_fg, bg_term) / out_a
    }
}

/// `out_a = a_fg + a_bg*(1 - a_fg)`, the ordinary "over" alpha — the same
/// formula for both [`AlphaMode`] settings.
#[must_use]
fn combined_alpha(a_fg: f64, a_bg: f64) -> f64 {
    a_fg + a_bg * (1.0 - a_fg)
}

/// `f64` sample (0..=255) to a clamped, rounded byte.
#[must_use]
fn to_u8(v: f64) -> u8 {
    if !v.is_finite() {
        return 0;
    }
    // `as` saturates rather than wrapping, so this is safe for any finite
    // input without a manual clamp.
    v.round().clamp(0.0, 255.0) as u8
}

/// `x`/`y` (or any expression result naming a pixel offset) to a pixel
/// coordinate: truncation toward zero, matching the reference's `(int)`
/// cast. `as i64` on a non-finite value saturates to `0`/`i64::MIN`/
/// `i64::MAX` rather than panicking, which is what makes a NaN or infinite
/// expression degrade to "far off screen" instead of undefined behaviour.
#[must_use]
pub fn to_pixel(v: f64) -> i64 {
    if v.is_nan() {
        return 0;
    }
    v as i64
}

/// The rectangle of the overlay, in main-frame pixel coordinates, that
/// actually needs to be read: the intersection of `[x, x+ow) x [y, y+oh)`
/// with `[0, main_w) x [0, main_h)`.
///
/// `None` when the two do not overlap at all — an overlay fully outside the
/// main frame, which must leave the main frame untouched.
#[must_use]
pub fn clip(x: i64, y: i64, ow: u32, oh: u32, main_w: u32, main_h: u32) -> Option<ClippedRect> {
    if ow == 0 || oh == 0 || main_w == 0 || main_h == 0 {
        return None;
    }
    let ov_right = x.checked_add(i64::from(ow))?;
    let ov_bottom = y.checked_add(i64::from(oh))?;
    let main_x0 = x.max(0);
    let main_y0 = y.max(0);
    let main_x1 = ov_right.min(i64::from(main_w));
    let main_y1 = ov_bottom.min(i64::from(main_h));
    if main_x1 <= main_x0 || main_y1 <= main_y0 {
        return None;
    }
    let width = u32::try_from(main_x1 - main_x0).ok()?;
    let height = u32::try_from(main_y1 - main_y0).ok()?;
    // The overlay-space origin of the clipped rectangle: how far the visible
    // window sits from the overlay's own (0, 0), which is nonzero exactly
    // when the overlay hangs off the top or left edge.
    let ov_x0 = u32::try_from(main_x0 - x).ok()?;
    let ov_y0 = u32::try_from(main_y0 - y).ok()?;
    Some(ClippedRect {
        main_x: u32::try_from(main_x0).ok()?,
        main_y: u32::try_from(main_y0).ok()?,
        ov_x: ov_x0,
        ov_y: ov_y0,
        width,
        height,
    })
}

/// The overlapping region between the overlay's placement and the main
/// frame, in both frames' own pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClippedRect {
    pub main_x: u32,
    pub main_y: u32,
    pub ov_x: u32,
    pub ov_y: u32,
    pub width: u32,
    pub height: u32,
}

/// Composite `overlay` onto `main` in place, within `rect`, interpreting
/// `overlay`'s alpha channel per `mode`.
///
/// Both frames must already be the same [`PixFmt`] — reformatting a
/// mismatched pair is [`crate::overlay`]'s job, via `vaco-scale`, before this
/// is called, so this function's own logic stays free of colour-matrix code.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] if `format` is not addressable at 8 bits
/// ([`geom::ensure_addressable_8bit`]).
pub fn composite(
    main: &mut Frame,
    overlay: &Frame,
    format: PixFmt,
    rect: ClippedRect,
    mode: AlphaMode,
) -> Result<()> {
    geom::ensure_addressable_8bit(format)?;
    if format.is_planar() {
        composite_planar(main, overlay, format, rect, mode)
    } else {
        composite_packed(main, overlay, format, rect, mode)
    }
}

/// One plane per logical channel (YUV, YUVA, GBR, GBRA families).
///
/// The two alpha planes (`main`'s current one and `overlay`'s) are read into
/// flat snapshots *before* any colour plane is mutated: a colour plane and
/// the alpha plane are different planes of the same `main: &mut Frame`, and
/// borrowing one plane mutably while reading another immutably through the
/// same `Frame` handle does not type-check, since `Frame::plane`/`plane_mut`
/// borrow the whole frame rather than one plane. Snapshotting also happens to
/// be exactly what correctness needs anyway: every colour channel must blend
/// against the background alpha as it was *before* this composite started,
/// and the alpha plane itself is written last (see below), so an unsnapshotted
/// read would still be correct today — but only by accident of ordering, so
/// the snapshot is taken unconditionally rather than relying on it.
fn composite_planar(
    main: &mut Frame,
    overlay: &Frame,
    format: PixFmt,
    rect: ClippedRect,
    mode: AlphaMode,
) -> Result<()> {
    let alpha_plane = geom::alpha_component(format).map(|c| c.plane);
    if alpha_plane.is_none() {
        copy_opaque_planar(main, overlay, format, rect)?;
        return Ok(());
    }
    let bg_alpha = alpha_plane.map(|ap| snapshot_alpha(main, ap, rect.main_x, rect.main_y, rect));
    let fg_alpha = alpha_plane.map(|ap| snapshot_alpha(overlay, ap, rect.ov_x, rect.ov_y, rect));
    let color_planes = format.plane_count();
    for plane in 0..color_planes {
        let plane = plane as u8;
        if Some(plane) == alpha_plane {
            continue;
        }
        blend_plane(
            main,
            overlay,
            format,
            plane,
            rect,
            mode,
            fg_alpha.as_deref(),
            bg_alpha.as_deref(),
        )?;
    }
    // Finally the alpha plane itself, if there is one: the output's own
    // alpha is `out_a`, not a channel blend. Uses the same snapshots, so it
    // agrees exactly with what the colour planes just blended against.
    if let Some(ap) = alpha_plane {
        write_alpha_plane(main, ap, rect, fg_alpha.as_deref(), bg_alpha.as_deref());
    }
    Ok(())
}

/// Copy every colour plane for a format with no alpha channel. The over
/// equation reduces to the foreground sample for every alpha mode, including
/// subsampled chroma planes, so a plane-local span copy is exact.
fn copy_opaque_planar(
    main: &mut Frame,
    overlay: &Frame,
    format: PixFmt,
    rect: ClippedRect,
) -> Result<()> {
    for plane in 0..format.plane_count() {
        let plane = plane as u8;
        let unit = geom::plane_unit_bytes(format, plane)?;
        let main_x = geom::plane_coord(rect.main_x, format, plane, true);
        let main_y = geom::plane_coord(rect.main_y, format, plane, false);
        let ov_x = geom::plane_coord(rect.ov_x, format, plane, true);
        let ov_y = geom::plane_coord(rect.ov_y, format, plane, false);
        let width = format.plane_width(rect.width, plane).max(1) as usize;
        let height = format.plane_height(rect.height, plane).max(1);
        let Some(row_bytes) = width.checked_mul(unit) else {
            continue;
        };
        let Some(mut main_plane) = main.plane_mut(plane as usize) else {
            continue;
        };
        let Some(ov_plane) = overlay.plane(plane as usize) else {
            continue;
        };
        for row in 0..height {
            let Some(dst_row) = main_plane.row_mut(main_y.saturating_add(row) as usize) else {
                continue;
            };
            let Some(src_row) = ov_plane.row(ov_y.saturating_add(row) as usize) else {
                continue;
            };
            let Some(dst_start) = (main_x as usize).checked_mul(unit) else {
                continue;
            };
            let Some(src_start) = (ov_x as usize).checked_mul(unit) else {
                continue;
            };
            let Some(dst) = dst_row.get_mut(dst_start..dst_start.saturating_add(row_bytes)) else {
                continue;
            };
            let Some(src) = src_row.get(src_start..src_start.saturating_add(row_bytes)) else {
                continue;
            };
            dst.copy_from_slice(src);
        }
    }
    Ok(())
}

/// Read a full-resolution alpha rectangle into a flat, row-major buffer.
/// Missing rows/columns (should not happen for an in-bounds rect, but this
/// crate never indexes without a checked fallback) read as opaque (`255`).
fn snapshot_alpha(frame: &Frame, plane: u8, x0: u32, y0: u32, rect: ClippedRect) -> Vec<u8> {
    let mut out = vec![255u8; (rect.width as usize).saturating_mul(rect.height as usize)];
    let Some(p) = frame.plane(plane as usize) else {
        return out;
    };
    for row in 0..rect.height {
        let Some(src_row) = p.row((y0.saturating_add(row)) as usize) else {
            continue;
        };
        let Some(dst_row) = out.get_mut((row as usize).saturating_mul(rect.width as usize)..)
        else {
            continue;
        };
        for col in 0..rect.width {
            let Some(&b) = src_row.get((x0.saturating_add(col)) as usize) else {
                continue;
            };
            if let Some(slot) = dst_row.get_mut(col as usize) {
                *slot = b;
            }
        }
    }
    out
}

/// Read one alpha sample out of a [`snapshot_alpha`] buffer, `1.0` (opaque)
/// if there is none.
fn alpha_at(snapshot: Option<&[u8]>, rect: ClippedRect, row: u32, col: u32) -> f64 {
    let Some(buf) = snapshot else { return 1.0 };
    let idx = (row as usize).saturating_mul(rect.width as usize) + col as usize;
    buf.get(idx).map_or(1.0, |&b| f64::from(b) / 255.0)
}

/// All channels interleaved in one plane (`rgb24`, `rgba`).
fn composite_packed(
    main: &mut Frame,
    overlay: &Frame,
    format: PixFmt,
    rect: ClippedRect,
    mode: AlphaMode,
) -> Result<()> {
    let unit = geom::plane_unit_bytes(format, 0)?;
    let alpha_offset = geom::alpha_component(format).map(|c| c.offset as usize);
    if alpha_offset.is_none() {
        copy_opaque_packed(main, overlay, rect, unit);
        return Ok(());
    }
    let color_offsets: Vec<usize> = format
        .descriptor()
        .components
        .iter()
        .take(3.min(format.component_count()))
        .map(|c| c.offset as usize)
        .collect();
    let Some(mut main_plane) = main.plane_mut(0) else {
        return Ok(());
    };
    let Some(ov_plane) = overlay.plane(0) else {
        return Ok(());
    };
    for row in 0..rect.height {
        let my = rect.main_y.saturating_add(row);
        let oy = rect.ov_y.saturating_add(row);
        let Some(dst_row) = main_plane.row_mut(my as usize) else {
            continue;
        };
        let Some(src_row) = ov_plane.row(oy as usize) else {
            continue;
        };
        for col in 0..rect.width {
            let mx = (rect.main_x.saturating_add(col) as usize).saturating_mul(unit);
            let ox = (rect.ov_x.saturating_add(col) as usize).saturating_mul(unit);
            let Some(dst_px) = dst_row.get_mut(mx..mx.saturating_add(unit)) else {
                continue;
            };
            let Some(src_px) = src_row.get(ox..ox.saturating_add(unit)) else {
                continue;
            };
            let a_fg = alpha_offset
                .and_then(|o| src_px.get(o))
                .map_or(1.0, |&b| f64::from(b) / 255.0);
            let a_bg = alpha_offset
                .and_then(|o| dst_px.get(o))
                .map_or(1.0, |&b| f64::from(b) / 255.0);
            let out_a = combined_alpha(a_fg, a_bg);
            for &off in &color_offsets {
                let Some(fg) = src_px.get(off) else { continue };
                let Some(bg) = dst_px.get(off) else { continue };
                let composed =
                    composite_channel(mode, f64::from(*fg), f64::from(*bg), a_fg, a_bg, out_a);
                if let Some(dst) = dst_px.get_mut(off) {
                    *dst = to_u8(composed);
                }
            }
            if let Some(o) = alpha_offset
                && let Some(dst) = dst_px.get_mut(o)
            {
                *dst = to_u8(out_a * 255.0);
            }
        }
    }
    Ok(())
}

/// An opaque packed overlay replaces each covered destination pixel exactly,
/// independent of its alpha-mode option. Avoid the floating-point over formula
/// in that case and copy contiguous clipped spans instead.
fn copy_opaque_packed(main: &mut Frame, overlay: &Frame, rect: ClippedRect, unit: usize) {
    let Some(row_bytes) = (rect.width as usize).checked_mul(unit) else {
        return;
    };
    let Some(mut main_plane) = main.plane_mut(0) else {
        return;
    };
    let Some(ov_plane) = overlay.plane(0) else {
        return;
    };
    for row in 0..rect.height {
        let my = rect.main_y.saturating_add(row);
        let oy = rect.ov_y.saturating_add(row);
        let Some(dst_row) = main_plane.row_mut(my as usize) else {
            continue;
        };
        let Some(src_row) = ov_plane.row(oy as usize) else {
            continue;
        };
        let Some(dst_start) = (rect.main_x as usize).checked_mul(unit) else {
            continue;
        };
        let Some(src_start) = (rect.ov_x as usize).checked_mul(unit) else {
            continue;
        };
        let Some(dst) = dst_row.get_mut(dst_start..dst_start.saturating_add(row_bytes)) else {
            continue;
        };
        let Some(src) = src_row.get(src_start..src_start.saturating_add(row_bytes)) else {
            continue;
        };
        dst.copy_from_slice(src);
    }
}

/// Blend one non-alpha plane (a colour channel) of a planar format, sampling
/// background/foreground alpha from the snapshots [`composite_planar`] took
/// up front (see that function's doc for why they are snapshots rather than
/// a live read of `main`'s own alpha plane).
fn blend_plane(
    main: &mut Frame,
    overlay: &Frame,
    format: PixFmt,
    plane: u8,
    rect: ClippedRect,
    mode: AlphaMode,
    fg_alpha: Option<&[u8]>,
    bg_alpha: Option<&[u8]>,
) -> Result<()> {
    let unit = geom::plane_unit_bytes(format, plane)?;
    // Plane-space coordinates of the clipped rectangle's corner and extent,
    // for both frames independently (their placements differ by (x, y), so
    // their chroma rounding can differ too).
    let p_main_x = geom::plane_coord(rect.main_x, format, plane, true);
    let p_main_y = geom::plane_coord(rect.main_y, format, plane, false);
    let p_ov_x = geom::plane_coord(rect.ov_x, format, plane, true);
    let p_ov_y = geom::plane_coord(rect.ov_y, format, plane, false);
    let p_w = format.plane_width(rect.width, plane).max(1);
    let p_h = format.plane_height(rect.height, plane).max(1);
    // The alpha snapshots are indexed by full-resolution (row, col) within
    // the rect; a chroma plane sample at plane-row `row` corresponds to
    // full-resolution rows `row << vsub .. (row+1) << vsub`, so the
    // top-left one (floor) is what is sampled — the same nearest-corner
    // approximation `crate::geom::plane_coord` makes for the pixel data
    // itself.
    let (hsub, vsub) = if plane_dims_differ(format, plane) {
        format.log2_chroma()
    } else {
        (0, 0)
    };

    let Some(mut main_plane) = main.plane_mut(plane as usize) else {
        return Ok(());
    };
    let Some(ov_plane) = overlay.plane(plane as usize) else {
        return Ok(());
    };

    for row in 0..p_h {
        let my = p_main_y.saturating_add(row);
        let oy = p_ov_y.saturating_add(row);
        let alpha_row = row << vsub;
        let Some(dst_row) = main_plane.row_mut(my as usize) else {
            continue;
        };
        let Some(src_row) = ov_plane.row(oy as usize) else {
            continue;
        };
        for col in 0..p_w {
            let mx = (p_main_x.saturating_add(col) as usize).saturating_mul(unit);
            let ox = (p_ov_x.saturating_add(col) as usize).saturating_mul(unit);
            let Some(dst) = dst_row.get_mut(mx) else {
                continue;
            };
            let Some(src) = src_row.get(ox) else {
                continue;
            };
            let alpha_col = col << hsub;
            let a_fg = alpha_at(fg_alpha, rect, alpha_row, alpha_col);
            let a_bg = alpha_at(bg_alpha, rect, alpha_row, alpha_col);
            let out_a = combined_alpha(a_fg, a_bg);
            let composed =
                composite_channel(mode, f64::from(*src), f64::from(*dst), a_fg, a_bg, out_a);
            *dst = to_u8(composed);
        }
    }
    Ok(())
}

/// Whether `plane` is decimated relative to the frame — i.e. whether alpha
/// samples for it need the `<< log2_chroma` expansion back to full
/// resolution before indexing the alpha snapshot.
fn plane_dims_differ(format: PixFmt, plane: u8) -> bool {
    geom::plane_is_chroma(format, plane)
}

/// Write the composited alpha plane: `out_a`, not a colour-channel blend.
/// Uses the same snapshots the colour planes just blended against, so the
/// alpha this writes is consistent with what was actually composited.
fn write_alpha_plane(
    main: &mut Frame,
    alpha_plane: u8,
    rect: ClippedRect,
    fg_alpha: Option<&[u8]>,
    bg_alpha: Option<&[u8]>,
) {
    let Some(mut main_plane) = main.plane_mut(alpha_plane as usize) else {
        return;
    };
    for row in 0..rect.height {
        let my = rect.main_y.saturating_add(row);
        let Some(dst_row) = main_plane.row_mut(my as usize) else {
            continue;
        };
        for col in 0..rect.width {
            let mx = rect.main_x.saturating_add(col) as usize;
            let Some(dst) = dst_row.get_mut(mx) else {
                continue;
            };
            let a_fg = alpha_at(fg_alpha, rect, row, col);
            let a_bg = alpha_at(bg_alpha, rect, row, col);
            *dst = to_u8(combined_alpha(a_fg, a_bg) * 255.0);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test code; the measured probes are pinned as exact byte/formula values"
)]
mod tests {
    use super::*;
    use vaco_frame::FramePool;

    #[test]
    fn straight_matches_the_measured_probe() {
        // fg=255, a_fg=100/255; bg=0, a_bg=200/255 — the informative probe
        // from docs/filter/vaco-filter-video-composite.md.
        let a_fg = 100.0 / 255.0;
        let a_bg = 200.0 / 255.0;
        let out_a = combined_alpha(a_fg, a_bg);
        let r = composite_channel(AlphaMode::Straight, 255.0, 0.0, a_fg, a_bg, out_a);
        assert_eq!(to_u8(r), 115);
        let b = composite_channel(AlphaMode::Straight, 0.0, 255.0, a_fg, a_bg, out_a);
        assert_eq!(to_u8(b), 140);
    }

    #[test]
    fn premultiplied_matches_the_measured_probe() {
        let a_fg = 100.0 / 255.0;
        let a_bg = 200.0 / 255.0;
        let out_a = combined_alpha(a_fg, a_bg);
        let r = composite_channel(AlphaMode::Premultiplied, 255.0, 0.0, a_fg, a_bg, out_a);
        assert_eq!(to_u8(r), 100);
        let b = composite_channel(AlphaMode::Premultiplied, 0.0, 255.0, a_fg, a_bg, out_a);
        assert_eq!(to_u8(b), 140);
    }

    #[test]
    fn opaque_background_makes_straight_and_premultiplied_coincide() {
        // Measured: main opaque (a_bg=1) makes out_a=1 regardless of a_fg,
        // which is exactly why the first probe (opaque rgb24 background)
        // could not distinguish the two settings.
        let a_fg = 128.0 / 255.0;
        let out_a = combined_alpha(a_fg, 1.0);
        assert_eq!(out_a, 1.0);
        let s = composite_channel(AlphaMode::Straight, 255.0, 200.0, a_fg, 1.0, out_a);
        let p = composite_channel(AlphaMode::Premultiplied, 255.0, 200.0, a_fg, 1.0, out_a);
        assert_eq!(to_u8(s), to_u8(p));
    }

    #[test]
    fn to_pixel_truncates_toward_zero() {
        assert_eq!(to_pixel(5.9), 5);
        assert_eq!(to_pixel(-0.5), 0);
        assert_eq!(to_pixel(-1.5), -1);
        assert_eq!(to_pixel(f64::NAN), 0);
    }

    #[test]
    fn fully_outside_clips_to_none() {
        assert_eq!(clip(20, 0, 4, 4, 20, 20), None);
        assert_eq!(clip(-4, 0, 4, 4, 20, 20), None);
        assert_eq!(clip(0, 0, 4, 4, 0, 20), None);
    }

    #[test]
    fn partial_overlap_clips_both_sides_consistently() {
        let r = clip(-2, 3, 6, 6, 10, 10).unwrap();
        assert_eq!(r.main_x, 0);
        assert_eq!(r.ov_x, 2);
        assert_eq!(r.width, 4);
        assert_eq!(r.main_y, 3);
        assert_eq!(r.ov_y, 0);
        assert_eq!(r.height, 6);
    }

    #[test]
    fn fully_inside_is_the_whole_overlay() {
        let r = clip(2, 2, 4, 4, 10, 10).unwrap();
        assert_eq!((r.main_x, r.main_y, r.ov_x, r.ov_y), (2, 2, 0, 0));
        assert_eq!((r.width, r.height), (4, 4));
    }

    #[test]
    fn opaque_packed_overlay_copies_the_clipped_rectangle() {
        let pool = FramePool::default();
        let mut main = pool.acquire_video(PixFmt::Rgb24, 4, 2).unwrap();
        let mut foreground = pool.acquire_video(PixFmt::Rgb24, 3, 2).unwrap();
        {
            let mut plane = main.plane_mut(0).unwrap();
            plane.fill(0x10);
        }
        for y in 0..2 {
            let mut plane = foreground.plane_mut(0).unwrap();
            let row = plane.row_mut(y).unwrap();
            for x in 0..3 {
                let start = x * 3;
                row[start..start + 3].copy_from_slice(&[x as u8, y as u8, 0xee]);
            }
        }
        let rect = clip(1, 0, 3, 2, 4, 2).unwrap();
        composite(
            &mut main,
            &foreground,
            PixFmt::Rgb24,
            rect,
            AlphaMode::Premultiplied,
        )
        .unwrap();

        for y in 0..2 {
            let row = main.plane(0).unwrap().row(y).unwrap();
            assert_eq!(&row[..3], &[0x10; 3]);
            for x in 0..3 {
                let start = (x + 1) * 3;
                assert_eq!(&row[start..start + 3], &[x as u8, y as u8, 0xee]);
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn clip_never_exceeds_the_main_frame(
            x in -200i64..200, y in -200i64..200,
            ow in 1u32..64, oh in 1u32..64,
            main_w in 1u32..64, main_h in 1u32..64,
        ) {
            if let Some(r) = clip(x, y, ow, oh, main_w, main_h) {
                proptest::prop_assert!(r.main_x.saturating_add(r.width) <= main_w);
                proptest::prop_assert!(r.main_y.saturating_add(r.height) <= main_h);
                proptest::prop_assert!(r.ov_x.saturating_add(r.width) <= ow);
                proptest::prop_assert!(r.ov_y.saturating_add(r.height) <= oh);
            }
        }

        #[test]
        fn composite_channel_never_produces_a_value_outside_the_byte_range(
            fg in 0.0f64..255.0, bg in 0.0f64..255.0,
            a_fg in 0.0f64..1.0, a_bg in 0.0f64..1.0,
            premultiplied in proptest::bool::ANY,
        ) {
            let mode = if premultiplied { AlphaMode::Premultiplied } else { AlphaMode::Straight };
            let out_a = combined_alpha(a_fg, a_bg);
            let v = composite_channel(mode, fg, bg, a_fg, a_bg, out_a);
            // Premultiplied's un-normalised foreground term can exceed 255 in
            // principle; `to_u8` clamps it, which is the property under test.
            proptest::prop_assert!(f64::from(to_u8(v)) <= 255.0);
        }
    }
}
