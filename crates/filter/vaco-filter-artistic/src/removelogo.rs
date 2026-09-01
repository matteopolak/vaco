//! `removelogo` — like [`crate::delogo`], but the region to replace is read
//! from a bitmap mask file instead of a fixed rectangle.
//!
//! `ffmpeg -h filter=removelogo` (2026-08-28): `filename`/`f` (a mask
//! bitmap path, required). Not a documented option surface beyond that —
//! the mask *format* itself was probed directly against the reference
//! rather than assumed, per D17.
//!
//! # Measured: the mask is a plain PGM (P5), and it thresholds rather than blends
//!
//! A hand-built 8x8 `P5` PGM with a `4x4` block of some fixed grey value
//! `v` (elsewhere `0`) was accepted with no error, and drove exactly the
//! same border-replacement behaviour [`crate::delogo`] documents (a flat
//! `50` background with a `200`-valued box inside the masked region comes
//! back entirely `50`), confirming: (a) `removelogo` reads a standard PGM,
//! and (b) a masked pixel is either fully replaced or left alone — there is
//! no visible partial blend at intermediate mask values, at least not one
//! this crate's probes could distinguish from a hard threshold. Bisecting
//! the mask value byte by byte (an 8x8 `gray` source, `filter=removelogo`,
//! checking whether the masked pixel comes back replaced or untouched at
//! every value from `10` to `32`) pinned the cutoff **exactly**: `v=16`
//! leaves the pixel untouched and `v=17` replaces it, so the threshold
//! this module uses (`> 16`, `ACTIVE_THRESHOLD`) is not a conservative
//! guess inside a bracket — it is the measured cutoff itself. `ffmpeg -h
//! filter=removelogo` was checked directly rather than trusted from the
//! project's own docs mirror, per this project's established rule that a
//! shipped binary's option surface is the fact and its documentation is
//! not always current with it — see [`crate::delogo`]'s doc for the case
//! that rule was written for.
//!
//! # Reuses `delogo`'s border-interpolation core, over the mask's bounding box
//!
//! Rather than a second, independent interpolation formula, this module
//! computes the bounding rectangle of "active" (above-threshold) mask
//! pixels and calls [`crate::delogo::fill_box`] on it, then restores any
//! *inactive* pixel inside that bounding box to its original value — so a
//! non-rectangular mask still only touches the pixels it marks, while the
//! interpolation math is exactly `delogo`'s (including its own documented,
//! unresolved anomalous-column discrepancy — this module inherits that gap
//! rather than duplicating a second guess at fixing it).
//!
//! # Not framecrc-verified
//!
//! For the same reason `delogo` is not: this module's interpolation core
//! is `delogo`'s, and that core has a known discrepancy. The mask parsing
//! and thresholding are measured; the pixel fill is exactly as
//! structural/unverified as `delogo`'s own.
//!
//! # A user-supplied file: bounded allocation, and a fuzz target
//!
//! The mask's declared width/height come from the file itself, so the
//! pixel buffer is sized through [`vaco_limits::Budget::alloc`] rather than
//! a raw `Vec::with_capacity` — an attacker-controlled PGM header should
//! fail cleanly, not allocate on the header's say-so. `fuzz/fuzz_targets/
//! removelogo_pgm_parse.rs` exercises [`parse_pgm`] directly over arbitrary
//! bytes, since that is the actual untrusted-input surface here (unlike
//! every other filter in this crate, which only ever sees decoded frames
//! from a trusted pipeline stage).

use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::delogo::{Rect, fill_box};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "removelogo",
    description: "Remove a TV logo based on a mask image.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// A mask byte above this is "active" (part of the logo to remove). See
/// this module's doc for the byte-by-byte bisection against the reference
/// that pinned this as the exact cutoff (`16` inactive, `17` active), not
/// a guess inside a bracket.
const ACTIVE_THRESHOLD: u8 = 16;

/// A parsed PGM (P5) mask: row-major, one byte per pixel, already
/// thresholded to booleans so the caller never re-touches the raw bytes.
#[derive(Debug, Clone)]
pub struct Mask {
    pub width: i32,
    pub height: i32,
    active: Vec<bool>,
}

impl Mask {
    fn is_active(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return false;
        }
        let Ok(idx) = usize::try_from(y * self.width + x) else {
            return false;
        };
        self.active.get(idx).copied().unwrap_or(false)
    }

    /// The bounding rectangle of every active pixel, or `None` if the mask
    /// marks nothing.
    fn bounding_box(&self) -> Option<Rect> {
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.is_active(x, y) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        if x1 < x0 || y1 < y0 {
            return None;
        }
        Some(Rect {
            x0,
            y0,
            w: x1 - x0 + 1,
            h: y1 - y0 + 1,
        })
    }
}

/// Parse a whitespace-tolerant PGM (P5) header (`P5\n<w> <h>\n<maxval>\n`,
/// `#`-prefixed comments allowed between tokens, matching the format's own
/// public specification) followed by exactly `w*h` raw grey bytes.
/// Untrusted input: every declared size is bounds-checked and the pixel
/// buffer is sized through `budget` rather than allocated on the header's
/// say-so.
///
/// # Errors
/// A descriptive message for a bad magic number, a missing/oversized
/// header field, a truncated pixel section, or a budget that refuses the
/// declared size.
pub fn parse_pgm(bytes: &[u8], budget: &mut Budget) -> Result<Mask> {
    let mut pos = 0usize;
    let next_token = |data: &[u8], pos: &mut usize| -> Option<Vec<u8>> {
        loop {
            while data.get(*pos).is_some_and(u8::is_ascii_whitespace) {
                *pos += 1;
            }
            if data.get(*pos) == Some(&b'#') {
                while data.get(*pos).is_some_and(|&b| b != b'\n') {
                    *pos += 1;
                }
                continue;
            }
            break;
        }
        let start = *pos;
        while data.get(*pos).is_some_and(|b| !b.is_ascii_whitespace()) {
            *pos += 1;
        }
        if *pos == start {
            return None;
        }
        data.get(start..*pos).map(<[u8]>::to_vec)
    };
    let magic = next_token(bytes, &mut pos).ok_or(Error::InvalidData("removelogo: empty mask file"))?;
    if magic != b"P5" {
        return Err(Error::InvalidData("removelogo: mask is not a P5 PGM"));
    }
    let parse_dim = |data: &[u8], pos: &mut usize| -> Result<i32> {
        let tok = next_token(data, pos).ok_or(Error::InvalidData("removelogo: truncated PGM header"))?;
        std::str::from_utf8(&tok)
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|&v| v > 0 && v <= 1 << 16)
            .ok_or(Error::InvalidData("removelogo: bad PGM dimension"))
    };
    let width = parse_dim(bytes, &mut pos)?;
    let height = parse_dim(bytes, &mut pos)?;
    let _maxval = parse_dim(bytes, &mut pos)?;
    // Exactly one whitespace byte is the mandatory separator before the
    // raw pixel section; consuming more here would eat into binary data
    // that happens to look like whitespace.
    if bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    #[allow(
        clippy::cast_sign_loss,
        reason = "width/height are checked > 0 above"
    )]
    let pixel_count = (width as usize).saturating_mul(height as usize);
    let Some(pixel_bytes) = bytes.get(pos..) else {
        return Err(Error::InvalidData("removelogo: truncated PGM pixel data"));
    };
    if pixel_bytes.len() < pixel_count {
        return Err(Error::InvalidData("removelogo: PGM shorter than its declared size"));
    }
    let mut active: Vec<bool> = budget
        .alloc(pixel_count)
        .map_err(|_| Error::Unsupported("removelogo: mask too large for this build's limits"))?;
    for (dst, &src) in active.iter_mut().zip(pixel_bytes.iter()) {
        *dst = src > ACTIVE_THRESHOLD;
    }
    Ok(Mask {
        width,
        height,
        active,
    })
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "removelogo", help = "Remove a TV logo based on a mask image.")]
pub(crate) struct Opts {
    #[opt(name = "filename", alias = "f", help = "set bitmap filename", default = String::new(), flags(video, filtering))]
    pub filename: String,
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
    mask: Mask,
    bbox: Option<Rect>,
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(input));
        }
        let Some(b) = self.bbox else {
            return Ok(FrameOut::One(input));
        };
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let bw = common::to_i32(width);
        let bh = common::to_i32(height);
        let b = Rect {
            x0: b.x0.clamp(0, bw),
            y0: b.y0.clamp(0, bh),
            w: b.w.min(bw - b.x0.clamp(0, bw)),
            h: b.h.min(bh - b.y0.clamp(0, bh)),
        };
        if b.w <= 0 || b.h <= 0 {
            return Ok(FrameOut::One(input));
        }
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let ph = common::to_i32(format.plane_height(height, p8)).max(0);
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let mut rows: Vec<Vec<u8>> = (0..ph)
                .map(|y| {
                    usize::try_from(y)
                        .ok()
                        .and_then(|uy| src_plane.row(uy))
                        .map(<[u8]>::to_vec)
                        .unwrap_or_default()
                })
                .collect();
            if p == 0 {
                let original: Vec<Vec<u8>> = rows.clone();
                fill_box(&mut rows, b);
                // Only mask-active pixels stay replaced; an inactive pixel
                // inside the bounding box reverts to its original value, so
                // a non-rectangular mask still only touches what it marks.
                for y in b.y0..b.y0 + b.h {
                    let Ok(uy) = usize::try_from(y) else { continue };
                    for x in b.x0..b.x0 + b.w {
                        let Ok(ux) = usize::try_from(x) else { continue };
                        if !self.mask.is_active(x, y)
                            && let Some(row) = rows.get_mut(uy)
                            && let Some(px) = row.get_mut(ux)
                            && let Some(&orig) = original.get(uy).and_then(|r| r.get(ux))
                        {
                            *px = orig;
                        }
                    }
                }
            }
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in rows.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        common::copy_frame_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    if opts.filename.is_empty() {
        return Err("removelogo: `filename` is required".to_owned());
    }
    let bytes = std::fs::read(&opts.filename)
        .map_err(|e| format!("removelogo: could not read `{}`: {e}", opts.filename))?;
    let mut budget = Budget::new(Limits::strict());
    let mask = parse_pgm(&bytes, &mut budget).map_err(|e| format!("removelogo: {e}"))?;
    let bbox = mask.bounding_box();
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter { mask, bbox })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn pgm(w: usize, h: usize, pixels: &[u8]) -> Vec<u8> {
        let mut out = format!("P5\n{w} {h}\n255\n").into_bytes();
        out.extend_from_slice(pixels);
        out
    }

    #[test]
    fn parses_a_well_formed_mask_and_thresholds_it() {
        let bytes = pgm(2, 2, &[0, 10, 32, 255]);
        let mut budget = Budget::new(Limits::strict());
        let mask = parse_pgm(&bytes, &mut budget).unwrap();
        assert!(!mask.is_active(0, 0));
        assert!(!mask.is_active(1, 0));
        assert!(mask.is_active(0, 1));
        assert!(mask.is_active(1, 1));
    }

    /// Pinned against the reference's own cutoff (this module's doc): a
    /// byte-by-byte bisection against `ffmpeg -vf removelogo` found `16`
    /// leaves a pixel untouched and `17` replaces it — not a bracket this
    /// crate picked a conservative point inside of.
    #[test]
    fn the_active_threshold_matches_the_measured_cutoff_exactly() {
        let bytes = pgm(2, 1, &[16, 17]);
        let mut budget = Budget::new(Limits::strict());
        let mask = parse_pgm(&bytes, &mut budget).unwrap();
        assert!(!mask.is_active(0, 0), "16 must be inactive");
        assert!(mask.is_active(1, 0), "17 must be active");
    }

    #[test]
    fn bounding_box_covers_exactly_the_active_pixels() {
        let bytes = pgm(4, 4, &[
            0, 0, 0, 0,
            0, 200, 200, 0,
            0, 200, 200, 0,
            0, 0, 0, 0,
        ]);
        let mut budget = Budget::new(Limits::strict());
        let mask = parse_pgm(&bytes, &mut budget).unwrap();
        let b = mask.bounding_box().unwrap();
        assert_eq!((b.x0, b.y0, b.w, b.h), (1, 1, 2, 2));
    }

    #[test]
    fn an_all_inactive_mask_has_no_bounding_box() {
        let bytes = pgm(3, 3, &[0; 9]);
        let mut budget = Budget::new(Limits::strict());
        let mask = parse_pgm(&bytes, &mut budget).unwrap();
        assert!(mask.bounding_box().is_none());
    }

    #[test]
    fn rejects_a_bad_magic_number() {
        let mut budget = Budget::new(Limits::strict());
        assert!(parse_pgm(b"P6\n2 2\n255\n\x00\x00\x00\x00", &mut budget).is_err());
    }

    #[test]
    fn rejects_truncated_pixel_data() {
        let mut budget = Budget::new(Limits::strict());
        assert!(parse_pgm(b"P5\n4 4\n255\n\x00\x00", &mut budget).is_err());
    }

    #[test]
    fn rejects_a_declared_size_the_budget_refuses() {
        let header = b"P5\n65535 65535\n255\n".to_vec();
        let mut budget = Budget::new(Limits::strict());
        assert!(parse_pgm(&header, &mut budget).is_err());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for seed in 0u8..=255 {
            let bytes: Vec<u8> = (0..64).map(|i: u8| i.wrapping_mul(seed).wrapping_add(seed)).collect();
            let mut budget = Budget::new(Limits::strict());
            let _ = parse_pgm(&bytes, &mut budget);
        }
    }

    #[test]
    fn creatable_requires_a_filename() {
        let req = Instantiate {
            name: "removelogo",
            instance: "removelogo",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    proptest::proptest! {
        #[test]
        fn parse_pgm_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256)) {
            let mut budget = Budget::new(Limits::strict());
            let _ = parse_pgm(&bytes, &mut budget);
        }
    }
}
