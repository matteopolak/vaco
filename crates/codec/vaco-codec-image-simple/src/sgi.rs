//! SGI (Silicon Graphics image): a 512-byte header, then either one plane of
//! raw samples per channel or, when RLE, an offset/length table indexed
//! `channel * height + row` followed by per-scanline RLE runs.
//!
//! `Vaco-Spec-Ref: sgi-image-spec` — the SGI RGB image file format, cross-
//! checked against the reference codec's observable byte behaviour (D17):
//! the RLE table's index order (channel-major, not row-major) and the fact
//! that scanlines are stored bottom row first were both confirmed by
//! decoding the reference encoder's own output and comparing against its
//! `ffmpeg`-reported raw pixels. Only 8-bit-per-channel images are supported;
//! 16-bit (`bpc == 2`) is [`Error::Unsupported`]. The encoder writes
//! uncompressed (`storage = 0`) rather than reproducing the reference's RLE
//! table layout, which this crate does not generate.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::reader::Reader;

const HEADER_LEN: usize = 512;

struct Header {
    rle: bool,
    width: u32,
    height: u32,
    channels: u32,
}

fn read_header(r: &mut Reader<'_>) -> Result<Header> {
    let magic = r.u16_be()?;
    if magic != 0x01DA {
        return Err(Error::InvalidData("sgi: bad magic"));
    }
    let storage = r.u8()?;
    let bpc = r.u8()?;
    if bpc != 1 {
        return Err(Error::Unsupported("sgi: only 8-bit channels are supported"));
    }
    let _dimension = r.u16_be()?;
    let width = u32::from(r.u16_be()?);
    let height = u32::from(r.u16_be()?);
    let channels = u32::from(r.u16_be()?);
    if width == 0 || height == 0 || !(1..=4).contains(&channels) {
        return Err(Error::InvalidData("sgi: invalid dimensions"));
    }
    r.seek(HEADER_LEN)?;
    Ok(Header {
        rle: storage == 1,
        width,
        height,
        channels,
    })
}

fn plane_for_channel(channels: u32, z: u32) -> usize {
    // The reference decodes 3-channel files to `gbrp` (plane order G, B, R)
    // and single-channel files to `gray8`.
    if channels == 1 {
        0
    } else {
        match z {
            0 => 2, // red -> plane 2
            1 => 0, // green -> plane 0
            _ => 1, // blue -> plane 1
        }
    }
}

fn rle_decode_scanline(data: &[u8], out: &mut [u8]) -> Result<()> {
    let mut i = 0usize;
    let mut pos = 0usize;
    while pos < out.len() {
        let byte = data.get(i).copied().ok_or(Error::UnexpectedEof)?;
        i += 1;
        let count = usize::from(byte & 0x7F);
        if count == 0 {
            break;
        }
        if byte & 0x80 != 0 {
            let src = data.get(i..i + count).ok_or(Error::UnexpectedEof)?;
            i += count;
            let end = pos + count;
            out.get_mut(pos..end)
                .ok_or(Error::InvalidData("sgi: rle overrun"))?
                .copy_from_slice(src);
            pos = end;
        } else {
            let value = data.get(i).copied().ok_or(Error::UnexpectedEof)?;
            i += 1;
            let end = pos + count;
            if let Some(dst) = out.get_mut(pos..end) {
                dst.fill(value);
            }
            pos = end;
        }
    }
    Ok(())
}

/// Decode an SGI image into `gray8` (one channel) or `gbrp` (three or more,
/// extra channels dropped).
///
/// # Errors
/// [`Error::Unsupported`] for 16-bit channels, [`Error::InvalidData`] for a
/// malformed header or scanline, [`Error::LimitExceeded`] if the declared
/// dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = Reader::new(data);
    let header = read_header(&mut r)?;
    let format = if header.channels == 1 {
        PixFmt::Gray8
    } else {
        PixFmt::Gbrp
    };
    let used_channels = if header.channels == 1 { 1 } else { 3 };

    let mut frame = Frame::alloc_video(budget, format, header.width, header.height)?;

    let table_entries = (header.height as usize).saturating_mul(header.channels as usize);
    let (offsets, lengths) = if header.rle {
        let mut offs = Vec::new();
        let mut lens = Vec::new();
        for _ in 0..table_entries {
            offs.push(r.u32_be()? as usize);
        }
        for _ in 0..table_entries {
            lens.push(r.u32_be()? as usize);
        }
        (offs, lens)
    } else {
        (Vec::new(), Vec::new())
    };

    let width = header.width as usize;
    let height = header.height as usize;
    let mut row_buf = vec![0u8; width];
    for z in 0..used_channels {
        let plane_idx = plane_for_channel(header.channels, z);
        let FrameData::Video { planes, .. } = &mut frame.data else {
            return Err(Error::InvalidData("sgi: expected a video frame"));
        };
        let plane = planes
            .get_mut(plane_idx)
            .ok_or(Error::InvalidData("sgi: missing plane"))?;
        let stride = plane.stride;
        let buf = plane.data.make_mut();

        for file_row in 0..height {
            if header.rle {
                let table_idx = (z as usize) * height + file_row;
                let off = offsets.get(table_idx).copied().unwrap_or(0);
                let len = lengths.get(table_idx).copied().unwrap_or(0);
                let scanline = data
                    .get(off..off.saturating_add(len))
                    .ok_or(Error::InvalidData("sgi: rle scanline out of bounds"))?;
                rle_decode_scanline(scanline, &mut row_buf)?;
            } else {
                let src = r.bytes(width)?;
                row_buf.copy_from_slice(src);
            }
            let image_row = height - 1 - file_row;
            let start = image_row.saturating_mul(stride);
            if let Some(dst) = buf.get_mut(start..start.saturating_add(width)) {
                dst.copy_from_slice(&row_buf);
            }
        }
    }
    Ok(frame)
}

/// Encode a `gray8` or `gbrp` frame as an uncompressed SGI image.
///
/// Not byte-identical to the reference encoder, which defaults to RLE
/// (`storage = 1`): this always writes `storage = 0` and skips the
/// offset/length table entirely, so a byte comparison against a
/// reference-encoded file will differ even though both decode to the same
/// pixels.
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
        return Err(Error::InvalidData("sgi: expected a video frame"));
    };
    let channels: u32 = match *format {
        PixFmt::Gray8 => 1,
        PixFmt::Gbrp => 3,
        _ => return Err(Error::Unsupported("sgi: encoder needs gray8 or gbrp input")),
    };
    let (width, height) = (*width, *height);

    let mut out = vec![0u8; HEADER_LEN];
    out.get_mut(0..2)
        .ok_or(Error::InvalidData("sgi: header"))?
        .copy_from_slice(&0x01DAu16.to_be_bytes());
    if let Some(slot) = out.get_mut(3) {
        *slot = 1; // bpc
    }
    out.get_mut(4..6)
        .ok_or(Error::InvalidData("sgi: header"))?
        .copy_from_slice(&(if channels == 1 { 2u16 } else { 3 }).to_be_bytes());
    out.get_mut(6..8)
        .ok_or(Error::InvalidData("sgi: header"))?
        .copy_from_slice(&(width as u16).to_be_bytes());
    out.get_mut(8..10)
        .ok_or(Error::InvalidData("sgi: header"))?
        .copy_from_slice(&(height as u16).to_be_bytes());
    out.get_mut(10..12)
        .ok_or(Error::InvalidData("sgi: header"))?
        .copy_from_slice(&(channels as u16).to_be_bytes());
    out.get_mut(13..14)
        .ok_or(Error::InvalidData("sgi: header"))?
        .copy_from_slice(&[0xff]); // pixmax low byte (pixmax = 255)

    for z in 0..channels {
        let plane_idx = plane_for_channel(channels, z);
        let plane = planes
            .get(plane_idx)
            .ok_or(Error::InvalidData("sgi: missing plane"))?;
        let src = plane.data.as_slice();
        for file_row in 0..height {
            let image_row = height - 1 - file_row;
            let start = (image_row as usize).saturating_mul(plane.stride);
            let row = src
                .get(start..start.saturating_add(width as usize))
                .ok_or(Error::InvalidData("sgi: row out of bounds"))?;
            out.extend_from_slice(row);
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
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn sample_gbrp(w: u32, h: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gbrp, w, h).expect("alloc");
        let FrameData::Video { planes, .. } = &mut frame.data else {
            panic!()
        };
        for (p, plane) in planes.iter_mut().enumerate() {
            let stride = plane.stride;
            let buf = plane.data.make_mut();
            for y in 0..h as usize {
                for x in 0..w as usize {
                    buf[y * stride + x] = ((x + y * 7 + p * 50) % 256) as u8;
                }
            }
        }
        frame
    }

    #[test]
    fn round_trips_uncompressed_gbrp() {
        let frame = sample_gbrp(5, 4);
        let encoded = encode(&frame).expect("encode");
        let mut budget = Budget::new(Limits::permissive());
        let decoded = decode(&encoded, &mut budget).expect("decode");
        assert_eq!(encode(&decoded).unwrap(), encoded);
    }

    #[test]
    fn rle_and_verbatim_agree() {
        let frame = sample_gbrp(6, 3);
        let _verbatim = encode(&frame).expect("encode");

        // Re-encode the same pixel data as RLE by hand: one run-of-N packet
        // per scanline (valid whenever a row has no more than 127 identical
        // pixels, which is true for every constant row built above only if
        // uniform; use a uniform image instead for a clean single-run case).
        let mut budget = Budget::new(Limits::permissive());
        let mut solid = Frame::alloc_video(&mut budget, PixFmt::Gray8, 4, 2).expect("alloc");
        let FrameData::Video { planes, .. } = &mut solid.data else {
            panic!()
        };
        let stride = planes[0].stride;
        planes[0].data.make_mut()[..2 * stride].fill(0); // ensure padding is zero
        for y in 0..2 {
            for x in 0..4 {
                planes[0].data.make_mut()[y * stride + x] = 42;
            }
        }
        let verbatim_solid = encode(&solid).expect("encode solid");

        let mut header = vec![0u8; HEADER_LEN];
        header[0..2].copy_from_slice(&0x01DAu16.to_be_bytes());
        header[2] = 1; // RLE
        header[3] = 1; // bpc
        header[4..6].copy_from_slice(&2u16.to_be_bytes());
        header[6..8].copy_from_slice(&4u16.to_be_bytes());
        header[8..10].copy_from_slice(&2u16.to_be_bytes());
        header[10..12].copy_from_slice(&1u16.to_be_bytes());
        // table: 2 rows * 1 channel = 2 entries; each scanline is a single
        // run of 4 pixels value 42: packet 0x04 (count=4, raw=0) then 42.
        let table_len = 2 * (4 + 4);
        let scan0_off = HEADER_LEN + table_len;
        let scan1_off = scan0_off + 2;
        let mut rle = header;
        rle.extend_from_slice(&(scan0_off as u32).to_be_bytes());
        rle.extend_from_slice(&(scan1_off as u32).to_be_bytes());
        rle.extend_from_slice(&2u32.to_be_bytes());
        rle.extend_from_slice(&2u32.to_be_bytes());
        rle.extend_from_slice(&[0x04, 42]);
        rle.extend_from_slice(&[0x04, 42]);

        let mut b1 = Budget::new(Limits::permissive());
        let mut b2 = Budget::new(Limits::permissive());
        let f_rle = decode(&rle, &mut b1).expect("rle decode");
        let f_verbatim = decode(&verbatim_solid, &mut b2).expect("verbatim decode");
        assert_eq!(encode(&f_rle).unwrap(), encode(&f_verbatim).unwrap());
    }

    #[test]
    fn rejects_16bit_channels() {
        let mut header = vec![0u8; HEADER_LEN];
        header[0..2].copy_from_slice(&0x01DAu16.to_be_bytes());
        header[3] = 2; // bpc = 2
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(decode(&header, &mut budget), Err(Error::Unsupported(_))));
    }
}
