//! `r10k` (AJA Kona 10-bit RGB) packed-pixel conversion.
//!
//! `r210` needs no code here at all: its wire format — a big-endian 32-bit
//! word per pixel, two padding bits then 10-bit R, G, B — is bit-for-bit
//! identical to [`vaco_pixfmt::PixFmt::X2rgb10be`]'s own in-memory layout
//! (`x2rgb10be`'s descriptor: R at bit 20, G at bit 10, B at bit 0, all
//! within a 4-byte big-endian container — see `vaco-pixfmt`'s table). So
//! `r210` decode/encode is exactly [`crate::raw::decode_raw`]/
//! [`crate::raw::encode_raw`] with `format = PixFmt::X2rgb10be`, registered
//! that way directly in `lib.rs`.
//!
//! `r10k` packs the same three 10-bit components into the same 4-byte
//! big-endian word, but with the two padding bits at the **bottom** instead
//! of the top: `R(10) | G(10) | B(10) | pad(2)`, most-significant bit first.
//! This is the AJA Kona convention as documented by multiple independent
//! implementations of "10-bit RGB, AJA style" — a public, vendor-described
//! packing, not the reference's own expression of it (D7). It has not been
//! independently measured against the reference here (no fixture with a
//! known-good `r10k` byte sequence was available in this pass); treat it the
//! same "best effort, modest depth" way the issue brief calls out explicitly
//! for this format.
//!
//! Because the padding sits in a different position than `x2rgb10be`'s own,
//! this needs a real per-pixel bit shift rather than a byte-identical copy:
//! decode reads each wire word's `R`/`G`/`B` fields out of their `r10k`
//! bit positions and re-assembles them into `x2rgb10be`'s bit positions (and
//! vice versa for encode). The decoded *values* are identical either way —
//! only the wire's own padding placement differs.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

const TEN_BIT_MASK: u32 = 0x3FF;

fn read_u32_be(buf: &[u8], off: usize) -> Result<u32> {
    let src = buf.get(off..off.saturating_add(4)).ok_or(Error::UnexpectedEof)?;
    let &[a, b, c, d] = src else {
        return Err(Error::UnexpectedEof);
    };
    Ok(u32::from_be_bytes([a, b, c, d]))
}

fn write_u32_be(buf: &mut [u8], off: usize, value: u32) -> Result<()> {
    let dst = buf
        .get_mut(off..off.saturating_add(4))
        .ok_or(Error::InvalidData("r10k: pixel write out of bounds"))?;
    dst.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

/// Decode an `r10k` payload into a [`PixFmt::X2rgb10be`] frame.
///
/// # Errors
/// [`Error::InvalidData`] for a `0x0` picture size, [`Error::UnexpectedEof`]
/// if `payload` is shorter than `width * height * 4` bytes.
pub fn decode_r10k(payload: &[u8], width: u32, height: u32, budget: &mut Budget) -> Result<Frame> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("r10k: picture size 0x0 is invalid"));
    }
    let row_bytes = (width as usize).saturating_mul(4);
    let total = row_bytes.saturating_mul(height as usize);
    if payload.len() < total {
        return Err(Error::UnexpectedEof);
    }

    let mut frame = Frame::alloc_video(budget, PixFmt::X2rgb10be, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("r10k: expected a video frame"));
    };
    let plane = planes.get_mut(0).ok_or(Error::InvalidData("r10k: no plane 0"))?;
    let dst_stride = plane.stride;
    let buf = plane.data.make_mut();

    for row in 0..height as usize {
        let src_row = row.saturating_mul(row_bytes);
        let dst_row = row.saturating_mul(dst_stride);
        for x in 0..width as usize {
            let word = read_u32_be(payload, src_row.saturating_add(x.saturating_mul(4)))?;
            let r = (word >> 22) & TEN_BIT_MASK;
            let g = (word >> 12) & TEN_BIT_MASK;
            let b = (word >> 2) & TEN_BIT_MASK;
            let out_word = (r << 20) | (g << 10) | b;
            write_u32_be(buf, dst_row.saturating_add(x.saturating_mul(4)), out_word)?;
        }
    }
    Ok(frame)
}

/// Encode a [`PixFmt::X2rgb10be`] frame as `r10k`.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format, [`Error::InvalidData`]
/// for a `0x0` picture size.
pub fn encode_r10k(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("r10k: expected a video frame"));
    };
    if *format != PixFmt::X2rgb10be {
        return Err(Error::Unsupported("r10k: encoder needs x2rgb10be input"));
    }
    let (width, height) = (*width, *height);
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("r10k: picture size 0x0 is invalid"));
    }
    let plane = planes.first().ok_or(Error::InvalidData("r10k: no plane 0"))?;
    let src_stride = plane.stride;
    let src = plane.data.as_slice();

    let row_bytes = (width as usize).saturating_mul(4);
    let mut out = vec![0u8; row_bytes.saturating_mul(height as usize)];
    for row in 0..height as usize {
        let src_row = row.saturating_mul(src_stride);
        let dst_row = row.saturating_mul(row_bytes);
        for x in 0..width as usize {
            let word = read_u32_be(src, src_row.saturating_add(x.saturating_mul(4)))?;
            let r = (word >> 20) & TEN_BIT_MASK;
            let g = (word >> 10) & TEN_BIT_MASK;
            let b = word & TEN_BIT_MASK;
            let out_word = (r << 22) | (g << 12) | (b << 2);
            write_u32_be(&mut out, dst_row.saturating_add(x.saturating_mul(4)), out_word)?;
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
    fn a_known_word_decodes_to_the_expected_components() {
        // r=0b1010101010 (682), g=0b0101010101 (341), b=0b1111100000 (992),
        // packed as R|G|B|pad(2) in a big-endian word.
        let r: u32 = 0b10_1010_1010;
        let g: u32 = 0b01_0101_0101;
        let b: u32 = 0b11_1110_0000;
        let word = (r << 22) | (g << 12) | (b << 2);
        let payload = word.to_be_bytes();
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_r10k(&payload, 1, 1, &mut budget).expect("decode");
        let FrameData::Video { planes, .. } = &frame.data else {
            panic!("video frame")
        };
        let plane = &planes[0];
        let buf = plane.data.as_slice();
        let got = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let expected = (r << 20) | (g << 10) | b;
        assert_eq!(got, expected);
    }

    #[test]
    fn round_trips() {
        let mut budget = Budget::new(Limits::permissive());
        let width = 4u32;
        let height = 2u32;
        let mut payload = vec![0u8; width as usize * height as usize * 4];
        for (i, chunk) in payload.chunks_mut(4).enumerate() {
            let r = (i * 7) as u32 % 1024;
            let g = (i * 13) as u32 % 1024;
            let b = (i * 31) as u32 % 1024;
            let word = (r << 22) | (g << 12) | (b << 2);
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        let frame = decode_r10k(&payload, width, height, &mut budget).expect("decode");
        let re = encode_r10k(&frame).expect("encode");
        assert_eq!(re, payload);
    }

    #[test]
    fn zero_size_is_rejected() {
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            decode_r10k(&[], 0, 0, &mut budget).unwrap_err(),
            Error::InvalidData(_)
        ));
    }

    #[test]
    fn encoder_rejects_the_wrong_pixel_format() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 4, 4).expect("alloc");
        assert!(matches!(encode_r10k(&frame).unwrap_err(), Error::Unsupported(_)));
    }
}
