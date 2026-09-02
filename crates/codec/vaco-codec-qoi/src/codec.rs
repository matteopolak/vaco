//! The QOI codec itself: header, the 64-entry running array, and the seven
//! chunk tags.
//!
//! `Vaco-Spec-Ref: qoi-format-spec` — <https://qoiformat.org/qoi-specification.pdf>,
//! cross-checked against the reference `qoi.h` encoder/decoder's observable
//! byte behaviour (D17): this crate's source was never read, only its output.
//!
//! # The one subtlety that is easy to get backwards
//!
//! The running array is updated with the *newly decoded* pixel after every
//! chunk except `QOI_OP_RUN` — including `QOI_OP_INDEX` itself, which looks
//! redundant (`index[hash(index[i])]` is a no-op) but is exactly what the
//! reference does, so a decoder that special-cases it out is still correct by
//! accident, not by matching the algorithm.

use vaco_core::{Error, Result};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::reader::Reader;

/// `qoif`, always big-endian, always the first four bytes.
const MAGIC: [u8; 4] = *b"qoif";

/// The eight-byte marker that closes every well-formed stream.
const END_MARKER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];

/// Longest run a single [`Tag::Run`] chunk can encode. 62 and 63 are reserved
/// so `0b11111110`/`0b11111111` unambiguously mean RGB/RGBA, not a run.
const RUN_MAX: u32 = 62;

/// One entry of the running array: R, G, B, A in that order.
type Px = [u8; 4];

const START_PIXEL: Px = [0, 0, 0, 255];

/// `(r*3 + g*5 + b*7 + a*11) % 64`, widened so the sum cannot wrap before the
/// modulus is taken.
const fn hash(px: Px) -> usize {
    let [r, g, b, a] = px;
    ((r as usize) * 3 + (g as usize) * 5 + (b as usize) * 7 + (a as usize) * 11) % 64
}

/// The running array, wrapped so every access goes through `.get()`/`.get_mut()`
/// instead of `[]` — `indexing_slicing` is denied, and the index here, while
/// always in range by construction (a 6-bit field or a `% 64` hash), is not
/// something the compiler can see is in range.
#[derive(Debug)]
struct Table([Px; 64]);

impl Table {
    const fn new() -> Self {
        Self([[0, 0, 0, 0]; 64])
    }

    fn get(&self, i: usize) -> Px {
        self.0.get(i).copied().unwrap_or([0, 0, 0, 0])
    }

    fn set(&mut self, i: usize, px: Px) {
        if let Some(slot) = self.0.get_mut(i) {
            *slot = px;
        }
    }
}

fn write_pixel(buf: &mut [u8], offset: usize, bytes: &[u8]) -> Result<()> {
    let dst = buf
        .get_mut(offset..offset.saturating_add(bytes.len()))
        .ok_or(Error::InvalidData("qoi: pixel write out of bounds"))?;
    dst.copy_from_slice(bytes);
    Ok(())
}

fn read_pixel(buf: &[u8], offset: usize, n: usize) -> Result<&[u8]> {
    buf.get(offset..offset.saturating_add(n))
        .ok_or(Error::InvalidData("qoi: pixel read out of bounds"))
}

/// Everything a QOI header states: a 4-byte magic, two big-endian `u32`
/// dimensions, a channel count and a colorspace byte.
struct Header {
    width: u32,
    height: u32,
    format: PixFmt,
    channel_bytes: usize,
}

fn read_header(r: &mut Reader<'_>) -> Result<Header> {
    let magic = r.bytes(4)?;
    if magic != MAGIC {
        return Err(Error::InvalidData("qoi: bad magic"));
    }
    let width = r.u32_be()?;
    let height = r.u32_be()?;
    let channels = r.u8()?;
    let _colorspace = r.u8()?;

    if width == 0 || height == 0 {
        return Err(Error::InvalidData("qoi: zero-sized image"));
    }
    let (format, channel_bytes) = match channels {
        3 => (PixFmt::Rgb24, 3),
        4 => (PixFmt::Rgba, 4),
        _ => return Err(Error::InvalidData("qoi: channels must be 3 or 4")),
    };
    Ok(Header {
        width,
        height,
        format,
        channel_bytes,
    })
}

/// The stream description a QOI header states, without decoding a pixel.
///
/// Reads the same header [`decode`] does, so the pixel format reported here
/// and the one the frame carries cannot drift apart.
#[must_use]
pub fn parameters(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    let header = read_header(&mut Reader::new(data)).ok()?;
    let mut params =
        vaco_codec_core::CodecParameters::video().with_codec(vaco_codec_core::CodecId::Qoi);
    if let Some(v) = params.video.as_mut() {
        v.width = header.width;
        v.height = header.height;
        v.coded_width = header.width;
        v.coded_height = header.height;
        v.format = Some(header.format);
    }
    Some(params)
}

/// Decode a whole QOI image into an RGB24 or RGBA frame, chosen by the
/// header's declared channel count — never by anything observed in the pixel
/// data, per the "derive from the codec" rule.
///
/// # Errors
/// [`Error::InvalidData`] for a malformed header or truncated chunk stream,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<vaco_frame::Frame> {
    let mut r = Reader::new(data);
    let Header {
        width,
        height,
        format,
        channel_bytes: out_bpp,
    } = read_header(&mut r)?;

    let mut frame = vaco_frame::Frame::alloc_video(budget, format, width, height)?;
    let vaco_frame::FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("qoi: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("qoi: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();

    let mut table = Table::new();
    let mut prev: Px = START_PIXEL;
    let mut run: u32 = 0;

    for y in 0..height {
        for x in 0..width {
            if run > 0 {
                run -= 1;
            } else {
                let tag = r.u8()?;
                match tag {
                    0xFE => {
                        let rgb = r.bytes(3)?;
                        let &[nr, ng, nb] = rgb else {
                            return Err(Error::UnexpectedEof);
                        };
                        prev = [nr, ng, nb, prev[3]];
                        table.set(hash(prev), prev);
                    }
                    0xFF => {
                        let rgba = r.bytes(4)?;
                        let &[nr, ng, nb, na] = rgba else {
                            return Err(Error::UnexpectedEof);
                        };
                        prev = [nr, ng, nb, na];
                        table.set(hash(prev), prev);
                    }
                    t if t >> 6 == 0b00 => {
                        let i = (t & 0x3F) as usize;
                        prev = table.get(i);
                        table.set(hash(prev), prev);
                    }
                    t if t >> 6 == 0b01 => {
                        let dr = i16::from((t >> 4) & 0x03) - 2;
                        let dg = i16::from((t >> 2) & 0x03) - 2;
                        let db = i16::from(t & 0x03) - 2;
                        prev = [
                            prev[0].wrapping_add(dr as i8 as u8),
                            prev[1].wrapping_add(dg as i8 as u8),
                            prev[2].wrapping_add(db as i8 as u8),
                            prev[3],
                        ];
                        table.set(hash(prev), prev);
                    }
                    t if t >> 6 == 0b10 => {
                        let b2 = r.u8()?;
                        let vg = i16::from(t & 0x3F) - 32;
                        let vr = vg - 8 + i16::from((b2 >> 4) & 0x0F);
                        let vb = vg - 8 + i16::from(b2 & 0x0F);
                        prev = [
                            prev[0].wrapping_add(vr as i8 as u8),
                            prev[1].wrapping_add(vg as i8 as u8),
                            prev[2].wrapping_add(vb as i8 as u8),
                            prev[3],
                        ];
                        table.set(hash(prev), prev);
                    }
                    t => {
                        // t >> 6 == 0b11, and 0xFE/0xFF are already handled above.
                        run = u32::from(t & 0x3F);
                    }
                }
            }
            let out = if out_bpp == 4 {
                prev.as_slice()
            } else {
                prev.get(0..3).unwrap_or(&[])
            };
            let offset = (y as usize)
                .saturating_mul(stride)
                .saturating_add((x as usize).saturating_mul(out_bpp));
            write_pixel(buf, offset, out)?;
        }
    }

    let tail = r.bytes(END_MARKER.len())?;
    if tail != END_MARKER {
        return Err(Error::InvalidData("qoi: missing end marker"));
    }

    Ok(frame)
}

/// Encode an RGB24 or RGBA frame as QOI.
///
/// # Errors
/// [`Error::Unsupported`] for any pixel format other than RGB24/RGBA —
/// callers are expected to convert first, the way every other encoder in this
/// tree expects a scaler upstream rather than converting silently.
pub fn encode(frame: &vaco_frame::Frame) -> Result<Vec<u8>> {
    let vaco_frame::FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("qoi: expected a video frame"));
    };
    let (channels, in_bpp): (u8, usize) = match *format {
        PixFmt::Rgb24 => (3, 3),
        PixFmt::Rgba => (4, 4),
        _ => return Err(Error::Unsupported("qoi: encoder needs rgb24 or rgba input")),
    };
    let width = *width;
    let height = *height;
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("qoi: zero-sized image"));
    }
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("qoi: no plane 0"))?;
    let stride = plane.stride;
    let src = plane.data.as_slice();

    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.push(channels);
    out.push(0); // colourspace: sRGB with linear alpha — nothing upstream signals otherwise.

    let mut table = Table::new();
    let mut prev: Px = START_PIXEL;
    let mut run: u32 = 0;
    let total_pixels = u64::from(width) * u64::from(height);
    let mut seen: u64 = 0;

    for y in 0..height {
        for x in 0..width {
            seen += 1;
            let offset = (y as usize)
                .saturating_mul(stride)
                .saturating_add((x as usize).saturating_mul(in_bpp));
            let sample = read_pixel(src, offset, in_bpp)?;
            let px: Px = if channels == 4 {
                let &[r, g, b, a] = sample else {
                    return Err(Error::InvalidData("qoi: short pixel"));
                };
                [r, g, b, a]
            } else {
                let &[r, g, b] = sample else {
                    return Err(Error::InvalidData("qoi: short pixel"));
                };
                [r, g, b, 255]
            };

            if px == prev {
                run += 1;
                if run == RUN_MAX || seen == total_pixels {
                    out.push(0xC0 | ((run - 1) as u8));
                    run = 0;
                }
                continue;
            }
            if run > 0 {
                out.push(0xC0 | ((run - 1) as u8));
                run = 0;
            }

            let idx = hash(px);
            if table.get(idx) == px {
                out.push(idx as u8);
            } else {
                table.set(idx, px);
                if px[3] == prev[3] {
                    let vr = px[0].wrapping_sub(prev[0]).cast_signed();
                    let vg = px[1].wrapping_sub(prev[1]).cast_signed();
                    let vb = px[2].wrapping_sub(prev[2]).cast_signed();
                    let vg_r = vr.wrapping_sub(vg);
                    let vg_b = vb.wrapping_sub(vg);
                    if (-2..=1).contains(&vr) && (-2..=1).contains(&vg) && (-2..=1).contains(&vb) {
                        let byte = 0x40
                            | (((vr + 2) as u8) << 4)
                            | (((vg + 2) as u8) << 2)
                            | ((vb + 2) as u8);
                        out.push(byte);
                    } else if (-32..=31).contains(&vg)
                        && (-8..=7).contains(&vg_r)
                        && (-8..=7).contains(&vg_b)
                    {
                        out.push(0x80 | ((vg + 32) as u8));
                        out.push((((vg_r + 8) as u8) << 4) | ((vg_b + 8) as u8));
                    } else {
                        out.push(0xFE);
                        out.push(px[0]);
                        out.push(px[1]);
                        out.push(px[2]);
                    }
                } else {
                    out.push(0xFF);
                    out.extend_from_slice(&px);
                }
            }
            prev = px;
        }
    }

    out.extend_from_slice(&END_MARKER);
    Ok(out)
}
