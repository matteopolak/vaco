//! SMPTE 292M/424M "v210" 10-bit 4:2:2 packing, shared by `v210` and `v210x`.
//!
//! **Structurally present but not independently measured against the
//! reference** — see `vaco-demux-raw::rawvideo`'s crate docs, which record the
//! identical caveat for the demux side of this same packing and are the
//! reason this module exists at all (the two sides were built from the same
//! reading of the convention). This is a public, vendor-independent 10-bit
//! 4:2:2 packing (documented identically by multiple hardware vendors), not
//! an expression of the reference's own source, so implementing it is safe
//! under the clean-room policy (D7) — the caveat is about verification, not
//! provenance.
//!
//! `v210x` is described by the reference itself as "reverse-engineered" with
//! no public spec; this module gives it exactly `v210`'s formula, on the
//! unverified assumption (again, matching the demuxer's own note) that it
//! shares v210's row packing.
//!
//! # The packing
//!
//! Six pixels pack into four little-endian 32-bit words (16 bytes):
//!
//! ```text
//! word 0: bits 0-9 = Cb0, bits 10-19 = Y0, bits 20-29 = Cr0
//! word 1: bits 0-9 = Y1,  bits 10-19 = Cb2, bits 20-29 = Y2
//! word 2: bits 0-9 = Cr2, bits 10-19 = Y3,  bits 20-29 = Cb4
//! word 3: bits 0-9 = Y4,  bits 10-19 = Cr4, bits 20-29 = Y5
//! ```
//!
//! giving six luma samples and three chroma pairs — one (Cb, Cr) pair per two
//! luma samples, which is exactly 4:2:2 subsampling. Each row is further
//! padded to a 128-byte (48-pixel) boundary, matching
//! `vaco-demux-raw::rawvideo::frame_size`'s `Packing::V210` arm exactly (that
//! function is not reachable from this crate — layering keeps a codec from
//! depending on a demuxer — so the arithmetic is repeated here rather than
//! shared).
//!
//! # Decode target
//!
//! [`vaco_pixfmt::PixFmt::Yuv422p10le`] is the natural choice: three planes,
//! 4:2:2 horizontal-only chroma decimation, 10 significant bits per 16-bit
//! little-endian sample — the same depth and subsampling v210 itself carries,
//! so nothing is widened or narrowed. Each decoded sample is stored in the
//! low 10 bits of its 16-bit word with the top 6 bits zero, the same
//! convention every other `*p10le` format in this tree uses.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, Plane};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

/// Pixels per 16-byte group.
const GROUP_PIXELS: usize = 6;
/// Bytes per 16-byte group.
const GROUP_BYTES: usize = 16;
/// Row stride is rounded up to a whole number of 48-pixel (128-byte) blocks.
const ROW_PIXEL_ALIGN: usize = 48;
const ROW_BYTE_ALIGN: usize = 128;

const TEN_BIT_MASK: u32 = 0x3FF;

fn row_bytes(width: u32) -> usize {
    (width as usize)
        .div_ceil(ROW_PIXEL_ALIGN)
        .saturating_mul(ROW_BYTE_ALIGN)
}

fn read_u32_le(buf: &[u8], off: usize) -> Result<u32> {
    let src = buf
        .get(off..off.saturating_add(4))
        .ok_or(Error::UnexpectedEof)?;
    let &[a, b, c, d] = src else {
        return Err(Error::UnexpectedEof);
    };
    Ok(u32::from_le_bytes([a, b, c, d]))
}

fn write_u32_le(buf: &mut [u8], off: usize, value: u32) -> Result<()> {
    let dst = buf
        .get_mut(off..off.saturating_add(4))
        .ok_or(Error::InvalidData("v210: group write out of bounds"))?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn field(word: u32, shift: u32) -> u16 {
    ((word >> shift) & TEN_BIT_MASK) as u16
}

fn write_sample(buf: &mut [u8], byte_off: usize, value: u16) -> Result<()> {
    let dst = buf
        .get_mut(byte_off..byte_off.saturating_add(2))
        .ok_or(Error::InvalidData("v210: sample write out of bounds"))?;
    dst.copy_from_slice(&(value & 0x03FF).to_le_bytes());
    Ok(())
}

fn read_sample(buf: &[u8], byte_off: usize) -> Result<u16> {
    let src = buf
        .get(byte_off..byte_off.saturating_add(2))
        .ok_or(Error::InvalidData("v210: sample read out of bounds"))?;
    let &[a, b] = src else {
        return Err(Error::InvalidData("v210: sample read out of bounds"));
    };
    Ok(u16::from_le_bytes([a, b]) & 0x03FF)
}

/// Split a video frame's three planes into independent mutable borrows.
///
/// [`smallvec::SmallVec`]'s slice methods borrow through `Deref`, so
/// `split_at_mut` is reached the same way it would be on a `Vec`.
fn split_planes_mut(planes: &mut [Plane]) -> Result<(&mut Plane, &mut Plane, &mut Plane)> {
    if planes.len() != 3 {
        return Err(Error::InvalidData("v210: expected exactly three planes"));
    }
    let (first, rest) = planes.split_at_mut(1);
    let (second, third) = rest.split_at_mut(1);
    let y = first
        .get_mut(0)
        .ok_or(Error::InvalidData("v210: missing Y plane"))?;
    let cb = second
        .get_mut(0)
        .ok_or(Error::InvalidData("v210: missing Cb plane"))?;
    let cr = third
        .get_mut(0)
        .ok_or(Error::InvalidData("v210: missing Cr plane"))?;
    Ok((y, cb, cr))
}

/// Decode a `v210`/`v210x` payload into a [`PixFmt::Yuv422p10le`] frame.
///
/// # Errors
/// [`Error::InvalidData`] for a `0x0` picture size, [`Error::UnexpectedEof`]
/// if `payload` is shorter than the padded geometry implies.
pub(crate) fn decode(
    payload: &[u8],
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> Result<Frame> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("v210: picture size 0x0 is invalid"));
    }
    let stride = row_bytes(width);
    let total = stride.saturating_mul(height as usize);
    if payload.len() < total {
        return Err(Error::UnexpectedEof);
    }

    let mut frame = Frame::alloc_video(budget, PixFmt::Yuv422p10le, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("v210: expected a video frame"));
    };
    let (y_plane, cb_plane, cr_plane) = split_planes_mut(planes.as_mut_slice())?;
    let (y_stride, cb_stride, cr_stride) = (y_plane.stride, cb_plane.stride, cr_plane.stride);
    let y_buf = y_plane.data.make_mut();
    let cb_buf = cb_plane.data.make_mut();
    let cr_buf = cr_plane.data.make_mut();

    let groups = (width as usize).div_ceil(GROUP_PIXELS);
    let width = width as usize;

    for row in 0..height as usize {
        let src_row = row.saturating_mul(stride);
        let y_row = row.saturating_mul(y_stride);
        let cb_row = row.saturating_mul(cb_stride);
        let cr_row = row.saturating_mul(cr_stride);
        for g in 0..groups {
            let base = src_row.saturating_add(g.saturating_mul(GROUP_BYTES));
            let w0 = read_u32_le(payload, base)?;
            let w1 = read_u32_le(payload, base.saturating_add(4))?;
            let w2 = read_u32_le(payload, base.saturating_add(8))?;
            let w3 = read_u32_le(payload, base.saturating_add(12))?;

            let cb0 = field(w0, 0);
            let y0 = field(w0, 10);
            let cr0 = field(w0, 20);
            let y1 = field(w1, 0);
            let cb2 = field(w1, 10);
            let y2 = field(w1, 20);
            let cr2 = field(w2, 0);
            let y3 = field(w2, 10);
            let cb4 = field(w2, 20);
            let y4 = field(w3, 0);
            let cr4 = field(w3, 10);
            let y5 = field(w3, 20);

            let pixel_base = g.saturating_mul(GROUP_PIXELS);
            for (k, y_val) in [y0, y1, y2, y3, y4, y5].into_iter().enumerate() {
                let x = pixel_base.saturating_add(k);
                if x >= width {
                    break;
                }
                write_sample(y_buf, y_row.saturating_add(x.saturating_mul(2)), y_val)?;
            }
            for (p, (cb_val, cr_val)) in
                [(cb0, cr0), (cb2, cr2), (cb4, cr4)].into_iter().enumerate()
            {
                let x = pixel_base.saturating_add(p.saturating_mul(2));
                if x >= width {
                    break;
                }
                let cx = x >> 1;
                write_sample(cb_buf, cb_row.saturating_add(cx.saturating_mul(2)), cb_val)?;
                write_sample(cr_buf, cr_row.saturating_add(cx.saturating_mul(2)), cr_val)?;
            }
        }
    }
    Ok(frame)
}

/// Encode a [`PixFmt::Yuv422p10le`] frame as `v210`/`v210x`.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format, [`Error::InvalidData`]
/// for a `0x0` picture size.
pub(crate) fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("v210: expected a video frame"));
    };
    if *format != PixFmt::Yuv422p10le {
        return Err(Error::Unsupported("v210: encoder needs yuv422p10le input"));
    }
    let (width, height) = (*width, *height);
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("v210: picture size 0x0 is invalid"));
    }
    if planes.len() != 3 {
        return Err(Error::InvalidData("v210: expected exactly three planes"));
    }
    let y_plane = planes
        .first()
        .ok_or(Error::InvalidData("v210: missing Y plane"))?;
    let cb_plane = planes
        .get(1)
        .ok_or(Error::InvalidData("v210: missing Cb plane"))?;
    let cr_plane = planes
        .get(2)
        .ok_or(Error::InvalidData("v210: missing Cr plane"))?;
    let (y_stride, cb_stride, cr_stride) = (y_plane.stride, cb_plane.stride, cr_plane.stride);
    let (y_buf, cb_buf, cr_buf) = (
        y_plane.data.as_slice(),
        cb_plane.data.as_slice(),
        cr_plane.data.as_slice(),
    );

    let stride = row_bytes(width);
    let mut out = vec![0u8; stride.saturating_mul(height as usize)];
    let groups = (width as usize).div_ceil(GROUP_PIXELS);
    let width = width as usize;

    for row in 0..height as usize {
        let dst_row = row.saturating_mul(stride);
        let y_row = row.saturating_mul(y_stride);
        let cb_row = row.saturating_mul(cb_stride);
        let cr_row = row.saturating_mul(cr_stride);
        for g in 0..groups {
            let pixel_base = g.saturating_mul(GROUP_PIXELS);
            let mut y = [0u16; GROUP_PIXELS];
            for (k, slot) in y.iter_mut().enumerate() {
                let x = pixel_base.saturating_add(k);
                if x < width {
                    *slot = read_sample(y_buf, y_row.saturating_add(x.saturating_mul(2)))?;
                }
            }
            let mut chroma = [(0u16, 0u16); 3];
            for (p, slot) in chroma.iter_mut().enumerate() {
                let x = pixel_base.saturating_add(p.saturating_mul(2));
                if x < width {
                    let cx = x >> 1;
                    let cb = read_sample(cb_buf, cb_row.saturating_add(cx.saturating_mul(2)))?;
                    let cr = read_sample(cr_buf, cr_row.saturating_add(cx.saturating_mul(2)))?;
                    *slot = (cb, cr);
                }
            }

            let base = dst_row.saturating_add(g.saturating_mul(GROUP_BYTES));
            // Destructured rather than indexed: `indexing_slicing` is denied,
            // and a fixed-size array pattern is not indexing.
            let [y0, y1, y2, y3, y4, y5] = y;
            let [(cb0, cr0), (cb2, cr2), (cb4, cr4)] = chroma;
            let w0 = u32::from(cb0) | (u32::from(y0) << 10) | (u32::from(cr0) << 20);
            let w1 = u32::from(y1) | (u32::from(cb2) << 10) | (u32::from(y2) << 20);
            let w2 = u32::from(cr2) | (u32::from(y3) << 10) | (u32::from(cb4) << 20);
            let w3 = u32::from(y4) | (u32::from(cr4) << 10) | (u32::from(y5) << 20);
            write_u32_le(&mut out, base, w0)?;
            write_u32_le(&mut out, base.saturating_add(4), w1)?;
            write_u32_le(&mut out, base.saturating_add(8), w2)?;
            write_u32_le(&mut out, base.saturating_add(12), w3)?;
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the codec, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_core::Error;
    use vaco_limits::Limits;

    #[test]
    fn row_stride_rounds_to_a_128_byte_group() {
        // 6 pixels/group, 16 bytes/group, padded to 48 pixels (128 bytes) —
        // matches `vaco-demux-raw::rawvideo`'s own `v210_stride_rounds_to_a_
        // 128_byte_group` test exactly.
        assert_eq!(row_bytes(8), 128);
        assert_eq!(row_bytes(48), 128);
        assert_eq!(row_bytes(49), 256);
    }

    /// Pack three 10-bit fields into one v210 word, low-to-high — a function
    /// rather than an inline `a | b << 10 | c << 20` so the test's literal
    /// field values stay plain decimal arguments instead of tripping
    /// `clippy::decimal_bitwise_operands` (which looks at operands of `|`/
    /// `<<`, not at ordinary call arguments).
    fn pack_word(low: u32, mid: u32, high: u32) -> u32 {
        low | (mid << 10) | (high << 20)
    }

    #[test]
    fn round_trips_an_exact_group_width() {
        let mut budget = Budget::new(Limits::permissive());
        let width = 6u32;
        let height = 2u32;
        // One full group per row: word0..3 with distinct 10-bit fields.
        let mut payload = vec![0u8; row_bytes(width) * height as usize];
        for row in 0..height as usize {
            let base = row * row_bytes(width);
            let w0 = pack_word(5, 100, 900);
            let w1 = pack_word(200, 15, 300);
            let w2 = pack_word(901, 400, 6);
            let w3 = pack_word(500, 902, 600);
            payload[base..base + 4].copy_from_slice(&w0.to_le_bytes());
            payload[base + 4..base + 8].copy_from_slice(&w1.to_le_bytes());
            payload[base + 8..base + 12].copy_from_slice(&w2.to_le_bytes());
            payload[base + 12..base + 16].copy_from_slice(&w3.to_le_bytes());
        }
        let frame = decode(&payload, width, height, &mut budget).expect("decode");
        let re = encode(&frame).expect("encode");
        assert_eq!(re, payload);
    }

    #[test]
    fn round_trips_a_width_that_is_not_a_multiple_of_six() {
        let mut budget = Budget::new(Limits::permissive());
        let width = 8u32;
        let height = 1u32;
        let payload = vec![0u8; row_bytes(width)];
        let frame = decode(&payload, width, height, &mut budget).expect("decode");
        let FrameData::Video {
            format,
            width: w,
            height: h,
            ..
        } = &frame.data
        else {
            panic!("video frame")
        };
        assert_eq!(*format, PixFmt::Yuv422p10le);
        assert_eq!(*w, width);
        assert_eq!(*h, height);
        let re = encode(&frame).expect("encode");
        assert_eq!(re.len(), payload.len());
    }

    #[test]
    fn zero_size_is_rejected() {
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            decode(&[], 0, 0, &mut budget).unwrap_err(),
            Error::InvalidData(_)
        ));
    }

    #[test]
    fn encoder_rejects_the_wrong_pixel_format() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 4, 4).expect("alloc");
        assert!(matches!(encode(&frame).unwrap_err(), Error::Unsupported(_)));
    }
}
