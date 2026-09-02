//! Two format families [`geometry`](crate::geometry)'s byte/word-addressable
//! model cannot reach at all: floating-point samples, and the one-bit-per-pixel
//! packing `monowhite`/`monoblack` need.
//!
//! Both are handled by converting through an ordinary integer proxy format
//! (`gray16le` or `rgb48le`, and `gray8`) rather than teaching the general
//! `geometry`/`rowio` pipeline a third sample shape it would then carry for
//! three format families out of 268. [`scaler`](crate::scaler) drives both:
//! it builds the real [`Plan`](crate::plan::Plan) against the proxy format,
//! runs the ordinary pipeline into (or out of) a temporary proxy [`Frame`],
//! and this module converts the temporary frame's samples to or from the
//! caller's real bytes at the edges.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::{Component, PixFmt};

// -------------------------------------------------------------- monowhite

/// The ordered-dither threshold table `monowhite`/`monoblack` output uses.
///
/// Measured against `ffmpeg` 8.1 by converting a synthetic ramp (`-f rawvideo
/// -pix_fmt gray8 ... -pix_fmt monow`) shaped so every `(x % 8, y % 8)`
/// position sees every possible 8-bit source value across the frame: each of
/// the 64 positions showed exactly one flip as the source value swept 0..255,
/// which is what a *positional* (Class A) ordered dither looks like and
/// error diffusion does not — a dependence only on `(value, x mod 8, y mod
/// 8)`, never on a neighbouring pixel's own value. Cross-checked against an
/// unrelated 32x32 gradient-plus-noise image fed through the same conversion:
/// 0 mismatches out of 1024 pixels.
///
/// A sample `v` (an 8-bit gray value) is "dark" — bit `1` in `monowhite`'s
/// 0-white/1-black convention — whenever `v < MONO_THRESHOLD[y % 8][x % 8]`.
/// The 64 thresholds are evenly spaced across roughly 17..234 rather than the
/// full 0..255 (average spacing ~3.44); recorded here as the measured data
/// rather than as a formula, because the closest analytic fit found (a linear
/// ramp fitted by least squares) still missed several thresholds by 1.
const MONO_THRESHOLD: [[u16; 8]; 8] = [
    [117, 172, 76, 131, 121, 176, 79, 134],
    [200, 35, 213, 48, 203, 38, 217, 52],
    [90, 145, 103, 158, 93, 148, 107, 162],
    [234, 69, 193, 28, 224, 59, 182, 17],
    [124, 179, 83, 138, 114, 169, 72, 127],
    [206, 41, 220, 55, 196, 31, 210, 45],
    [96, 151, 110, 165, 86, 141, 100, 155],
    [227, 62, 186, 21, 231, 66, 189, 24],
];

/// Which side of [`MONO_THRESHOLD`] a monochrome destination reads its bit
/// from.
///
/// Measured (`ffmpeg` 8.1): `monoblack`'s output is exactly `monowhite`'s
/// bitwise complement for the same source, checked over every pixel of the
/// same 32x32 probe used to recover the threshold table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MonoPolarity {
    /// `0` is white, `1` is black — [`MONO_THRESHOLD`]'s own convention.
    White,
    /// The complement of [`MonoPolarity::White`].
    Black,
}

/// [`MONO_THRESHOLD`]`[y % 8][x % 8]`, for tests that check
/// [`Scaler::scale_frame`](crate::Scaler::scale_frame)'s end-to-end output
/// against the same table [`pack_mono`] uses, without duplicating it.
#[cfg(test)]
#[must_use]
pub(crate) fn mono_threshold(x: usize, y: usize) -> u16 {
    MONO_THRESHOLD
        .get(y % 8)
        .and_then(|row| row.get(x % 8))
        .copied()
        .unwrap_or(128)
}

/// The polarity `fmt` needs, or `None` if it is not a monochrome bitmap
/// format at all.
#[must_use]
pub(crate) const fn mono_polarity(fmt: PixFmt) -> Option<MonoPolarity> {
    match fmt {
        PixFmt::MonoWhite => Some(MonoPolarity::White),
        PixFmt::MonoBlack => Some(MonoPolarity::Black),
        _ => None,
    }
}

/// Pack one `gray8` plane into a `monowhite`/`monoblack` destination's
/// bit-packed plane, MSB-first — the same convention
/// `vaco-codec-pnm::bits::set_bit` uses for PBM's raw raster, since a
/// `monowhite` frame's bytes are that raster verbatim.
///
/// # Errors
/// [`Error::InvalidData`] if either frame is not video or has no plane 0.
pub(crate) fn pack_mono(gray: &Frame, dst: &mut Frame, polarity: MonoPolarity) -> Result<()> {
    let FrameData::Video {
        width,
        height,
        planes: gray_planes,
        ..
    } = &gray.data
    else {
        return Err(Error::InvalidData("mono pack: source is not video"));
    };
    let (width, height) = (*width as usize, *height as usize);
    let gray_plane = gray_planes
        .first()
        .ok_or(Error::InvalidData("mono pack: source has no plane"))?;
    let gray_stride = gray_plane.stride;
    let gray_buf = gray_plane.data.as_slice();

    let FrameData::Video {
        planes: dst_planes, ..
    } = &mut dst.data
    else {
        return Err(Error::InvalidData("mono pack: destination is not video"));
    };
    let dst_plane = dst_planes
        .first_mut()
        .ok_or(Error::InvalidData("mono pack: destination has no plane"))?;
    let dst_stride = dst_plane.stride;
    let dst_buf = dst_plane.data.make_mut();

    for y in 0..height {
        let row_start = y.saturating_mul(gray_stride);
        let dst_row_start = y.saturating_mul(dst_stride);
        let threshold_row = MONO_THRESHOLD.get(y % 8);
        for x in 0..width {
            let v = gray_buf
                .get(row_start.saturating_add(x))
                .copied()
                .unwrap_or(0);
            let threshold = threshold_row
                .and_then(|row| row.get(x % 8))
                .copied()
                .unwrap_or(128);
            let dark = u16::from(v) < threshold;
            let bit = match polarity {
                MonoPolarity::White => dark,
                MonoPolarity::Black => !dark,
            };
            let Some(slot) = dst_buf.get_mut(dst_row_start.saturating_add(x >> 3)) else {
                continue;
            };
            let mask = 0x80u8 >> (x % 8);
            if bit {
                *slot |= mask;
            } else {
                *slot &= !mask;
            }
        }
    }
    Ok(())
}

/// Unpack a `monowhite`/`monoblack` frame's bit-packed plane into a fresh
/// `gray8` frame — the read-side inverse of [`pack_mono`], MSB-first within
/// each byte.
///
/// **Why this exists now.** `special` used to only ever *write* these two
/// formats, so a monochrome source was left to `geometry`, which reads a
/// plane as one byte per pixel. Feeding it a 1-bit raster made every eight
/// pixels come back as one packed byte reinterpreted as a grey level: a PBM
/// or XBM decoded through the CLI produced its own header bytes as pixels.
/// The still-image decoders (`vaco-codec-pnm`'s PBM, `vaco-codec-image-simple`'s
/// XBM, PAM's `BLACKANDWHITE`) all emit these formats, so the assumption that
/// nothing produces them was simply out of date.
///
/// The polarity is [`pack_mono`]'s, inverted: a set bit is the dark sample
/// under [`MonoPolarity::White`]. Measured against the reference — an
/// `ffmpeg`-written `P4` whose first raster byte is `0xC3` decodes to
/// `00 00 ff ff ff ff 00 00`.
///
/// # Errors
/// [`Error::InvalidData`] if `src` is not video or has no plane 0;
/// [`Error::LimitExceeded`] if the `gray8` proxy exceeds `budget`.
pub(crate) fn unpack_mono(
    src: &Frame,
    budget: &mut Budget,
    polarity: MonoPolarity,
) -> Result<Frame> {
    let FrameData::Video {
        width,
        height,
        planes,
        ..
    } = &src.data
    else {
        return Err(Error::InvalidData("mono unpack: source is not video"));
    };
    let (width, height) = (*width, *height);
    let src_plane = planes
        .first()
        .ok_or(Error::InvalidData("mono unpack: source has no plane"))?;
    let src_stride = src_plane.stride;
    let src_buf = src_plane.data.as_slice();

    let mut out = Frame::alloc_video(budget, PixFmt::Gray8, width, height)?;
    let FrameData::Video {
        planes: out_planes, ..
    } = &mut out.data
    else {
        return Err(Error::InvalidData("mono unpack: proxy is not video"));
    };
    let out_plane = out_planes
        .first_mut()
        .ok_or(Error::InvalidData("mono unpack: proxy has no plane"))?;
    let out_stride = out_plane.stride;
    let out_buf = out_plane.data.make_mut();

    for y in 0..height as usize {
        let src_row = y.saturating_mul(src_stride);
        let out_row = y.saturating_mul(out_stride);
        for x in 0..width as usize {
            let byte = src_buf
                .get(src_row.saturating_add(x >> 3))
                .copied()
                .unwrap_or(0);
            let set = byte & (0x80u8 >> (x % 8)) != 0;
            let dark = match polarity {
                MonoPolarity::White => set,
                MonoPolarity::Black => !set,
            };
            if let Some(slot) = out_buf.get_mut(out_row.saturating_add(x)) {
                *slot = if dark { 0 } else { 0xFF };
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ float

/// Which IEEE-754 width a float pixel format's samples use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FloatWidth {
    F32,
    F16,
}

/// Enough to read or write a float pixel format's samples generically.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FloatInfo {
    pub width: FloatWidth,
    /// 1 for the gray family, 3 for the rgb family — the only two shapes any
    /// registered encoder needs today (pfm/phm accept exactly these eight
    /// formats between them).
    pub channels: u8,
    pub big_endian: bool,
}

impl FloatInfo {
    /// The integer format this float format's samples are linearly mapped
    /// to/from.
    #[must_use]
    pub(crate) const fn proxy(self) -> PixFmt {
        if self.channels >= 3 {
            PixFmt::Rgb48le
        } else {
            PixFmt::Gray16le
        }
    }
}

/// `fmt`'s float layout, or `None` if it is not one of the eight float
/// formats this crate bridges.
#[must_use]
pub(crate) const fn float_info(fmt: PixFmt) -> Option<FloatInfo> {
    match fmt {
        PixFmt::Grayf32le => Some(FloatInfo {
            width: FloatWidth::F32,
            channels: 1,
            big_endian: false,
        }),
        PixFmt::Grayf32be => Some(FloatInfo {
            width: FloatWidth::F32,
            channels: 1,
            big_endian: true,
        }),
        PixFmt::Rgbf32le => Some(FloatInfo {
            width: FloatWidth::F32,
            channels: 3,
            big_endian: false,
        }),
        PixFmt::Rgbf32be => Some(FloatInfo {
            width: FloatWidth::F32,
            channels: 3,
            big_endian: true,
        }),
        PixFmt::Grayf16le => Some(FloatInfo {
            width: FloatWidth::F16,
            channels: 1,
            big_endian: false,
        }),
        PixFmt::Grayf16be => Some(FloatInfo {
            width: FloatWidth::F16,
            channels: 1,
            big_endian: true,
        }),
        PixFmt::Rgbf16le => Some(FloatInfo {
            width: FloatWidth::F16,
            channels: 3,
            big_endian: false,
        }),
        PixFmt::Rgbf16be => Some(FloatInfo {
            width: FloatWidth::F16,
            channels: 3,
            big_endian: true,
        }),
        _ => None,
    }
}

/// The scale a float sample and its integer proxy sample agree on.
///
/// Measured against `ffmpeg` 8.1 (`-pix_fmt gray16le` fed a handful of
/// specific 16-bit values, `-pix_fmt grayf32le` read back): `f = v / 65535.0`
/// exactly — `1 -> 1.5259022e-5`, `32768 -> 0.5000076`, `65535 -> 1.0`. `f16`
/// has no such reference to measure (this build's `ffmpeg` lists `grayf16le`/
/// `rgbf16le` as decode-only — `I` with no `O` in `-pix_fmts` — so it cannot
/// itself write one); the same scale is used for it since nothing else in the
/// format's own definition suggests a different one, and it round-trips
/// through this crate's own `f32`<->`f16` conversion exactly for every value
/// this scale produces.
const FLOAT_SCALE: f32 = 65535.0;

fn u16_to_f32(v: u16) -> f32 {
    f32::from(v) / FLOAT_SCALE
}

fn f32_to_u16(f: f32) -> u16 {
    if !f.is_finite() {
        return 0;
    }
    (f * FLOAT_SCALE).round().clamp(0.0, FLOAT_SCALE) as u16
}

/// IEEE-754 binary16 bits to `f32`, handling subnormals and inf/nan.
///
/// A real image's samples land in `0.0..=1.0` (`FLOAT_SCALE`'s own range),
/// which reaches half-float subnormals (anything below `2^-14`) but never
/// infinity or `NaN` from this crate's own writer — both branches exist so a
/// value read from a file this crate did not write cannot misbehave.
fn f16_bits_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exp = u32::from((bits >> 10) & 0x1F);
    let frac = u32::from(bits & 0x3FF);
    let bits32 = if exp == 0 {
        if frac == 0 {
            sign
        } else {
            // Subnormal half: `frac / 1024` with no implicit leading 1.
            // Normalize it the way a float's mantissa always is — find the
            // highest set bit at position `p` (0..=9) and shift it up to bit
            // 10, which is the same shape as an implicit-1 mantissa one bit
            // wider. The exponent that puts back is `p - 24` unbiased
            // (worked from `2^-14 * frac/1024 = 2^(p-24) * (frac << (10-p) &
            // 0x3FF) / 1024`, i.e. `p + 103` once rebiased to f32's 127).
            let p = frac.ilog2();
            let shift = 10 - p;
            let normalized = frac << shift;
            let exp32 = p + 103;
            sign | (exp32 << 23) | ((normalized & 0x3FF) << 13)
        }
    } else if exp == 0x1F {
        sign | (0xFFu32 << 23) | (frac << 13)
    } else {
        let exp32 = exp + (127 - 15);
        sign | (exp32 << 23) | (frac << 13)
    };
    f32::from_bits(bits32)
}

/// `f32` to IEEE-754 binary16 bits, rounding to nearest and flushing anything
/// smaller than the smallest half-float subnormal to zero.
fn f32_to_f16_bits(f: f32) -> u16 {
    let bits = f.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xFF).cast_signed();
    let frac = bits & 0x007F_FFFF;

    if exp == 0xFF {
        let nan_bit: u16 = if frac != 0 { 0x0200 } else { 0 };
        return sign | 0x7C00 | nan_bit;
    }

    let half_exp = exp - 127 + 15;
    if half_exp >= 0x1F {
        return sign | 0x7C00;
    }
    if half_exp <= 0 {
        if half_exp < -10 {
            return sign;
        }
        // Subnormal: shift the implicit-leading-1 mantissa right by however
        // far below the smallest normal exponent this value sits, rounding
        // the bit that falls off.
        let mantissa = frac | 0x0080_0000;
        let shift = (14 - half_exp) as u32;
        let rounded = (mantissa >> shift) + ((mantissa >> (shift - 1)) & 1);
        return sign | rounded as u16;
    }

    let rounded_frac = frac + 0x0000_1000;
    if rounded_frac & 0x0080_0000 != 0 {
        // Rounding the mantissa carried into the exponent.
        return sign | (((half_exp + 1) as u16) << 10);
    }
    sign | ((half_exp as u16) << 10) | ((rounded_frac >> 13) as u16)
}

fn component_offset(comp: Component, x: usize) -> usize {
    x.saturating_mul(comp.step as usize)
        .saturating_add(comp.offset as usize)
}

fn read_float_sample(row: &[u8], x: usize, comp: Component, info: FloatInfo) -> f32 {
    let off = component_offset(comp, x);
    let b0 = row.get(off).copied().unwrap_or(0);
    let b1 = row.get(off.saturating_add(1)).copied().unwrap_or(0);
    match info.width {
        FloatWidth::F16 => {
            let bits = if info.big_endian {
                u16::from_be_bytes([b0, b1])
            } else {
                u16::from_le_bytes([b0, b1])
            };
            f16_bits_to_f32(bits)
        }
        FloatWidth::F32 => {
            let b2 = row.get(off.saturating_add(2)).copied().unwrap_or(0);
            let b3 = row.get(off.saturating_add(3)).copied().unwrap_or(0);
            if info.big_endian {
                f32::from_be_bytes([b0, b1, b2, b3])
            } else {
                f32::from_le_bytes([b0, b1, b2, b3])
            }
        }
    }
}

fn write_float_sample(row: &mut [u8], x: usize, comp: Component, info: FloatInfo, value: f32) {
    let off = component_offset(comp, x);
    match info.width {
        FloatWidth::F16 => {
            let bits = f32_to_f16_bits(value);
            let bytes = if info.big_endian {
                bits.to_be_bytes()
            } else {
                bits.to_le_bytes()
            };
            for (i, b) in bytes.into_iter().enumerate() {
                if let Some(slot) = row.get_mut(off.saturating_add(i)) {
                    *slot = b;
                }
            }
        }
        FloatWidth::F32 => {
            let bytes = if info.big_endian {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            };
            for (i, b) in bytes.into_iter().enumerate() {
                if let Some(slot) = row.get_mut(off.saturating_add(i)) {
                    *slot = b;
                }
            }
        }
    }
}

fn read_u16_le(row: &[u8], x: usize, comp: Component) -> u16 {
    let off = component_offset(comp, x);
    let b0 = row.get(off).copied().unwrap_or(0);
    let b1 = row.get(off.saturating_add(1)).copied().unwrap_or(0);
    u16::from_le_bytes([b0, b1])
}

fn write_u16_le(row: &mut [u8], x: usize, comp: Component, value: u16) {
    let off = component_offset(comp, x);
    let bytes = value.to_le_bytes();
    if let Some(slot) = row.get_mut(off) {
        *slot = bytes[0];
    }
    if let Some(slot) = row.get_mut(off.saturating_add(1)) {
        *slot = bytes[1];
    }
}

/// Build a `gray16le`/`rgb48le` proxy frame carrying `src`'s samples,
/// linearly rescaled from `0.0..=1.0` to `0..=65535` per channel.
///
/// # Errors
/// [`Error::InvalidData`] if `src` is not video or has no plane 0.
/// [`Error::Unsupported`] if `src`'s format is not one [`float_info`]
/// recognises. Whatever [`Frame::alloc_video`] reports for the proxy.
pub(crate) fn float_frame_to_proxy(src: &Frame, budget: &mut Budget) -> Result<Frame> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &src.data
    else {
        return Err(Error::InvalidData("float proxy: source is not video"));
    };
    let info = float_info(*format).ok_or(Error::Unsupported("not a float pixel format"))?;
    let (width, height) = (*width, *height);
    let src_comps = format.descriptor().components;
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("float proxy: source has no plane"))?;
    let src_stride = plane.stride;
    let src_buf = plane.data.as_slice();

    let proxy_fmt = info.proxy();
    let mut proxy = Frame::alloc_video(budget, proxy_fmt, width, height)?;
    let proxy_comps = proxy_fmt.descriptor().components;
    let FrameData::Video {
        planes: proxy_planes,
        ..
    } = &mut proxy.data
    else {
        return Err(Error::InvalidData("float proxy: destination has no plane"));
    };
    let proxy_plane = proxy_planes
        .first_mut()
        .ok_or(Error::InvalidData("float proxy: destination has no plane"))?;
    let proxy_stride = proxy_plane.stride;
    let proxy_buf = proxy_plane.data.make_mut();

    for y in 0..height as usize {
        let src_row_start = y.saturating_mul(src_stride);
        let proxy_row_start = y.saturating_mul(proxy_stride);
        let Some(src_row) = src_buf.get(src_row_start..) else {
            continue;
        };
        let Some(proxy_row) = proxy_buf.get_mut(proxy_row_start..) else {
            continue;
        };
        for ch in 0..info.channels as usize {
            let (Some(&sc), Some(&pc)) = (src_comps.get(ch), proxy_comps.get(ch)) else {
                continue;
            };
            for x in 0..width as usize {
                let f = read_float_sample(src_row, x, sc, info);
                write_u16_le(proxy_row, x, pc, f32_to_u16(f));
            }
        }
    }
    Ok(proxy)
}

/// The inverse of [`float_frame_to_proxy`]: write `proxy`'s samples into
/// `dst`'s real float bytes.
///
/// # Errors
/// [`Error::InvalidData`] if either frame is not video or has no plane 0.
/// [`Error::Unsupported`] if `dst`'s format is not one [`float_info`]
/// recognises.
pub(crate) fn proxy_to_float_frame(proxy: &Frame, dst: &mut Frame) -> Result<()> {
    let FrameData::Video {
        format: proxy_fmt,
        planes: proxy_planes,
        ..
    } = &proxy.data
    else {
        return Err(Error::InvalidData("float proxy: source is not video"));
    };
    if !matches!(proxy_fmt, PixFmt::Gray16le | PixFmt::Rgb48le) {
        return Err(Error::InvalidData("float proxy: unexpected proxy format"));
    }
    let proxy_comps = proxy_fmt.descriptor().components;
    let proxy_plane = proxy_planes
        .first()
        .ok_or(Error::InvalidData("float proxy: source has no plane"))?;
    let proxy_stride = proxy_plane.stride;
    let proxy_buf = proxy_plane.data.as_slice();

    let FrameData::Video {
        format,
        width,
        height,
        planes: dst_planes,
    } = &mut dst.data
    else {
        return Err(Error::InvalidData("float proxy: destination is not video"));
    };
    let info = float_info(*format).ok_or(Error::Unsupported("not a float pixel format"))?;
    let (width, height) = (*width, *height);
    let dst_comps = format.descriptor().components;
    let dst_plane = dst_planes
        .first_mut()
        .ok_or(Error::InvalidData("float proxy: destination has no plane"))?;
    let dst_stride = dst_plane.stride;
    let dst_buf = dst_plane.data.make_mut();

    for y in 0..height as usize {
        let proxy_row_start = y.saturating_mul(proxy_stride);
        let dst_row_start = y.saturating_mul(dst_stride);
        let Some(proxy_row) = proxy_buf.get(proxy_row_start..) else {
            continue;
        };
        let Some(dst_row) = dst_buf.get_mut(dst_row_start..) else {
            continue;
        };
        for ch in 0..info.channels as usize {
            let (Some(&pc), Some(&dc)) = (proxy_comps.get(ch), dst_comps.get(ch)) else {
                continue;
            };
            for x in 0..width as usize {
                let v = read_u16_le(proxy_row, x, pc);
                write_float_sample(dst_row, x, dc, info, u16_to_f32(v));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "test code checking exact round trips"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn f16_round_trips_every_representable_value_from_the_float_scale() {
        for v in 0u32..=65535 {
            let f = u16_to_f32(v as u16);
            let bits = f32_to_f16_bits(f);
            let back = f16_bits_to_f32(bits);
            // Half precision has only 10 explicit mantissa bits, so this is
            // not bit-exact — bounded by half a quantisation step at the
            // value's own magnitude, not by a fixed epsilon.
            let ulp = (f.abs() * 2f32.powi(-10)).max(2f32.powi(-24));
            assert!(
                (back - f).abs() <= ulp * 2.0,
                "v={v} f={f} back={back} bits={bits:#06x}"
            );
        }
    }

    #[test]
    fn f16_zero_and_one_are_exact() {
        assert_eq!(f32_to_f16_bits(0.0), 0x0000);
        assert_eq!(f16_bits_to_f32(0x0000), 0.0);
        assert_eq!(f16_bits_to_f32(f32_to_f16_bits(1.0)), 1.0);
    }

    #[test]
    fn mono_threshold_is_a_permutation_of_the_measured_range() {
        let mut seen: Vec<u16> = MONO_THRESHOLD.iter().flatten().copied().collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 64, "64 distinct measured thresholds");
    }

    fn pack_gray_row(values: &[u8]) -> (Frame, u32) {
        let width = values.len() as u32;
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut gray = Frame::alloc_video(&mut budget, PixFmt::Gray8, width, 1).unwrap();
        let FrameData::Video { planes, .. } = &mut gray.data else {
            unreachable!()
        };
        let row = planes[0].data.make_mut();
        row[..values.len()].copy_from_slice(values);
        (gray, width)
    }

    fn bit_at(buf: &[u8], x: usize) -> bool {
        buf[x >> 3] & (0x80 >> (x % 8)) != 0
    }

    #[test]
    fn monoblack_packs_the_complement_of_monowhite_for_a_whole_byte() {
        let values: Vec<u8> = (0..8).map(|x| if x % 2 == 0 { 0 } else { 255 }).collect();
        let (gray, width) = pack_gray_row(&values);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut white = Frame::alloc_video(&mut budget, PixFmt::MonoWhite, width, 1).unwrap();
        let mut black = Frame::alloc_video(&mut budget, PixFmt::MonoBlack, width, 1).unwrap();
        pack_mono(&gray, &mut white, MonoPolarity::White).unwrap();
        pack_mono(&gray, &mut black, MonoPolarity::Black).unwrap();

        let FrameData::Video { planes: wp, .. } = &white.data else {
            unreachable!()
        };
        let FrameData::Video { planes: bp, .. } = &black.data else {
            unreachable!()
        };
        assert_eq!(wp[0].data.as_slice()[0], !bp[0].data.as_slice()[0]);
    }

    /// A real `ffmpeg`-written `P4`'s first raster byte, and the grey samples
    /// `ffmpeg -i src.pbm -f rawvideo -pix_fmt gray` produces from it.
    #[test]
    fn unpack_mono_matches_the_reference_grey_samples() {
        let mut budget = Budget::new(Limits::permissive());
        let mut src = Frame::alloc_video(&mut budget, PixFmt::MonoWhite, 8, 1).unwrap();
        {
            let FrameData::Video { planes, .. } = &mut src.data else {
                panic!("not video")
            };
            planes[0].data.make_mut()[0] = 0xC3;
        }
        let out = unpack_mono(&src, &mut budget, MonoPolarity::White).unwrap();
        let FrameData::Video { planes, .. } = &out.data else {
            panic!("not video")
        };
        assert_eq!(
            planes[0].data.as_slice()[..8],
            [0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00]
        );

        let black = unpack_mono(&src, &mut budget, MonoPolarity::Black).unwrap();
        let FrameData::Video { planes, .. } = &black.data else {
            panic!("not video")
        };
        assert_eq!(
            planes[0].data.as_slice()[..8],
            [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF]
        );
    }

    /// A width that is not a byte multiple leaves padding bits in the last
    /// byte; unpacking must stop at the declared width, not the byte edge.
    #[test]
    fn unpack_mono_stops_at_the_declared_width() {
        let mut budget = Budget::new(Limits::permissive());
        let mut src = Frame::alloc_video(&mut budget, PixFmt::MonoWhite, 13, 2).unwrap();
        let stride = {
            let FrameData::Video { planes, .. } = &mut src.data else {
                panic!("not video")
            };
            let stride = planes[0].stride;
            let buf = planes[0].data.make_mut();
            buf[0] = 0xFF;
            buf[1] = 0xFF;
            buf[stride] = 0x00;
            buf[stride + 1] = 0x00;
            stride
        };
        let _ = stride;
        let out = unpack_mono(&src, &mut budget, MonoPolarity::White).unwrap();
        let FrameData::Video { planes, .. } = &out.data else {
            panic!("not video")
        };
        let out_stride = planes[0].stride;
        let buf = planes[0].data.as_slice();
        assert!(buf[..13].iter().all(|&v| v == 0));
        assert!(buf[out_stride..out_stride + 13].iter().all(|&v| v == 0xFF));
    }

    #[test]
    fn pack_mono_handles_a_width_that_is_not_a_multiple_of_eight() {
        let values: Vec<u8> = (0..9).map(|x| if x % 2 == 0 { 0 } else { 255 }).collect();
        let (gray, width) = pack_gray_row(&values);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut white = Frame::alloc_video(&mut budget, PixFmt::MonoWhite, width, 1).unwrap();
        pack_mono(&gray, &mut white, MonoPolarity::White).unwrap();
        let FrameData::Video { planes, .. } = &white.data else {
            unreachable!()
        };
        let buf = planes[0].data.as_slice();
        for (x, &v) in values.iter().enumerate() {
            let threshold = MONO_THRESHOLD[0][x % 8];
            assert_eq!(bit_at(buf, x), u16::from(v) < threshold, "x={x}");
        }
    }

    #[test]
    fn u16_to_f32_matches_the_measured_reference_points() {
        assert_eq!(u16_to_f32(0), 0.0);
        assert!((u16_to_f32(1) - 1.525_902_2e-5).abs() < 1e-9);
        assert!((u16_to_f32(32768) - 0.500_007_6).abs() < 1e-6);
        assert_eq!(u16_to_f32(65535), 1.0);
    }

    #[test]
    fn f32_to_u16_round_trips_the_measured_reference_points() {
        assert_eq!(f32_to_u16(0.0), 0);
        assert_eq!(f32_to_u16(1.0), 65535);
        assert_eq!(f32_to_u16(u16_to_f32(32768)), 32768);
    }
}
