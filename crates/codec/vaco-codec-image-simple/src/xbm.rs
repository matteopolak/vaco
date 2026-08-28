//! XBM (X BitMap): a C source fragment — `#define <name>_width`/`_height`,
//! then a `<name>_bits[]` array of hex byte literals, one bit per pixel,
//! packed **LSB-first** within each byte (the opposite bit order from PBM).
//!
//! `Vaco-Spec-Ref: x11-xbm-format` — the X bitmap file format used by
//! `XReadBitmapFile`, cross-checked against the reference codec's observable
//! byte behaviour (D17): its `1`/`0` polarity matches `monowhite` exactly,
//! the reference converts between the two bit orders by reversing each raw
//! byte whole (confirmed by decoding a PBM and an XBM built from the same
//! source image and finding `xbm_byte.reverse_bits() == pbm_byte` for every
//! byte, padding included — so this decoder does the same rather than
//! masking bits past the declared width to zero), and its encoder always
//! names the identifier `image` regardless of the output filename (measured
//! by encoding to two different filenames and finding byte-identical output).
//! The decoder accepts any identifier; the encoder reproduces the `image`
//! spelling and the reference's exact comma/space/line-wrap layout (one row
//! per line, `", "`-joined, no trailing comma on the very last byte).

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

fn find_number_after(text: &str, marker: &str) -> Option<u32> {
    let start = text.find(marker)? + marker.len();
    let rest = text.get(start..)?;
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn row_bytes_for_bits(width: u32) -> usize {
    (width as usize).div_ceil(8)
}

/// Decode an XBM image into [`PixFmt::MonoWhite`].
///
/// # Errors
/// [`Error::InvalidData`] if the text is not valid UTF-8 or the required
/// `_width`/`_height`/hex-byte fields cannot be found,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let text = std::str::from_utf8(data).map_err(|_| Error::InvalidData("xbm: not valid text"))?;
    let width = find_number_after(text, "_width").ok_or(Error::InvalidData("xbm: no _width"))?;
    let height = find_number_after(text, "_height").ok_or(Error::InvalidData("xbm: no _height"))?;
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("xbm: zero-sized image"));
    }
    let row_bytes = row_bytes_for_bits(width);
    let needed = row_bytes.saturating_mul(height as usize);

    let bits_at = text.find("_bits").ok_or(Error::InvalidData("xbm: no _bits array"))?;
    let body = text.get(bits_at..).ok_or(Error::InvalidData("xbm: truncated"))?;
    let mut bytes = Vec::new();
    let mut rest = body;
    while bytes.len() < needed {
        let Some(off) = rest.find("0x").or_else(|| rest.find("0X")) else {
            return Err(Error::InvalidData("xbm: not enough byte literals"));
        };
        let hex_start = off + 2;
        let hex_rest = rest.get(hex_start..).ok_or(Error::InvalidData("xbm: truncated"))?;
        let hex: String = hex_rest.chars().take_while(char::is_ascii_hexdigit).take(2).collect();
        let value = u8::from_str_radix(&hex, 16).map_err(|_| Error::InvalidData("xbm: bad hex byte"))?;
        bytes.push(value);
        rest = hex_rest.get(hex.len()..).ok_or(Error::InvalidData("xbm: truncated"))?;
    }

    let mut frame = Frame::alloc_video(budget, PixFmt::MonoWhite, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("xbm: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("xbm: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();

    for y in 0..height as usize {
        for xb in 0..row_bytes {
            let byte = bytes.get(y * row_bytes + xb).copied().unwrap_or(0);
            let out_off = y.saturating_mul(stride).saturating_add(xb);
            if let Some(slot) = buf.get_mut(out_off) {
                *slot = byte.reverse_bits();
            }
        }
    }
    Ok(frame)
}

/// Encode a [`PixFmt::MonoWhite`] frame as XBM, identifier `image`.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("xbm: expected a video frame"));
    };
    if *format != PixFmt::MonoWhite {
        return Err(Error::Unsupported("xbm: encoder needs monowhite input"));
    }
    let (width, height) = (*width, *height);
    let plane = planes.first().ok_or(Error::InvalidData("xbm: no plane 0"))?;
    let stride = plane.stride;
    let src = plane.data.as_slice();
    let row_bytes = row_bytes_for_bits(width);

    let mut bytes = Vec::new();
    for y in 0..height as usize {
        for xb in 0..row_bytes {
            let byte_off = y.saturating_mul(stride).saturating_add(xb);
            let b = src.get(byte_off).copied().unwrap_or(0);
            bytes.push(b.reverse_bits());
        }
    }

    let mut out = format!(
        "#define image_width {width}\n#define image_height {height}\nstatic unsigned char image_bits[] = {{\n"
    )
    .into_bytes();
    let total = bytes.len();
    for (row, chunk) in bytes.chunks(row_bytes).enumerate() {
        out.push(b' ');
        for (i, b) in chunk.iter().enumerate() {
            out.extend_from_slice(format!("0x{b:02X}").as_bytes());
            let is_last_overall = row * row_bytes + i + 1 == total;
            if !is_last_overall {
                out.extend_from_slice(b",");
                if i + 1 != chunk.len() {
                    out.push(b' ');
                }
            }
        }
        out.push(b'\n');
    }
    out.extend_from_slice(b" };\n");
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    const SAMPLE: &[u8] = b"#define image_width 6\n#define image_height 4\nstatic unsigned char image_bits[] = {\n 0xB3,\n 0x4D,\n 0xAB,\n 0x55\n };\n";

    #[test]
    fn decodes_reference_sample() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(SAMPLE, &mut budget).expect("decode");
        let FrameData::Video { width, height, .. } = &frame.data else {
            panic!()
        };
        assert_eq!((*width, *height), (6, 4));
    }

    #[test]
    fn round_trips_exact_bytes() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(SAMPLE, &mut budget).expect("decode");
        let encoded = encode(&frame).expect("encode");
        assert_eq!(encoded, SAMPLE);
    }

    #[test]
    fn wide_multi_byte_row_round_trips() {
        let sample: &[u8] = b"#define image_width 20\n#define image_height 3\nstatic unsigned char image_bits[] = {\n 0xBF, 0x23, 0xA2,\n 0x57, 0x45, 0x51,\n 0x83, 0xE8, 0xAE\n };\n";
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(sample, &mut budget).expect("decode");
        assert_eq!(encode(&frame).expect("encode"), sample);
    }

    #[test]
    fn matches_pbm_bit_convention() {
        // XBM's first byte 0xB3 (LSB-first) must decode to the same in-memory
        // byte as PBM's 0xCD (MSB-first): both represent pixels 1,1,0,0,1,1.
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(SAMPLE, &mut budget).expect("decode");
        let FrameData::Video { planes, .. } = &frame.data else {
            panic!()
        };
        assert_eq!(planes[0].data.as_slice()[0], 0xCD);
    }
}
