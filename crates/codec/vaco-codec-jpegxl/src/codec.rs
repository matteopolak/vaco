//! The byte format: JPEG XL, wrapping the `jxl-oxide` crate.
//!
//! [`decode`] is a pure function over bytes and [`Frame`]s; the
//! `SendReceive` wrapper in `lib.rs` never touches a `jxl_oxide::` type.
//! There is no `encode`: `jxl-oxide` is a decoder only.

use std::io::Cursor;

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

const fn native32(le: PixFmt, be: PixFmt) -> PixFmt {
    if cfg!(target_endian = "big") { be } else { le }
}

/// Map `jxl_oxide::PixelFormat` to one of our `f32` [`PixFmt`]s.
///
/// CMYK/CMYK+alpha are a documented coverage gap (plan 15 §4A.2's JPEG XL
/// risk note singles out colour management as the main risk area): there is
/// no CMYK `PixFmt` family in this tree, so those report
/// [`Error::Unsupported`] rather than being forced into RGB.
fn output_pixfmt(format: jxl_oxide::PixelFormat) -> Result<PixFmt> {
    use jxl_oxide::PixelFormat as F;
    Ok(match format {
        F::Gray => native32(PixFmt::Grayf32le, PixFmt::Grayf32be),
        F::Graya => native32(PixFmt::Yaf32le, PixFmt::Yaf32be),
        F::Rgb => native32(PixFmt::Rgbf32le, PixFmt::Rgbf32be),
        F::Rgba => native32(PixFmt::Rgbaf32le, PixFmt::Rgbaf32be),
        F::Cmyk | F::Cmyka => return Err(Error::Unsupported("jpegxl: CMYK colour space")),
    })
}

/// Decode every keyframe of a JPEG XL image (one, unless the file is the
/// animated `jpegxl_anim` form) into `f32` frames.
///
/// Output stays in `f32` regardless of the source's own bit depth: `VarDCT`
/// (lossy) content is inherently a floating-point transform, and Modular
/// (lossless) content upconverts losslessly, so this never discards
/// precision the file did not already lack (D11 "Equivalent" for `VarDCT`,
/// "Exact" for Modular, per plan 15 §4A.2).
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed container or bitstream.
/// [`Error::Unsupported`] for a CMYK colour space, or a render failure.
/// [`Error::LimitExceeded`] when a frame exceeds `budget`.
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Frame>> {
    let image = jxl_oxide::JxlImage::read_with_defaults(Cursor::new(bytes))
        .map_err(|_| Error::InvalidData("jpegxl: header"))?;
    let format = output_pixfmt(image.pixel_format())?;
    let channels = image.pixel_format().channels();
    let width = image.width();
    let height = image.height();
    budget.check_frame(width, height, u32::try_from(channels).unwrap_or(4) * 4)?;

    let tps = image
        .image_header()
        .metadata
        .animation
        .as_ref()
        .map(|a| (a.tps_numerator, a.tps_denominator));

    let num_frames = image.num_loaded_keyframes();
    if num_frames == 0 {
        return Err(Error::InvalidData("jpegxl: no keyframes"));
    }

    let mut out = Vec::new();
    for i in 0..num_frames {
        let render = image
            .render_frame(i)
            .map_err(|_| Error::Unsupported("jpegxl: frame render"))?;
        let mut stream = render.stream();
        let mut samples: Vec<f32> = budget.alloc(width as usize * height as usize * channels)?;
        stream.write_to_buffer(&mut samples);

        let mut frame = Frame::alloc_video(budget, format, width, height)?;
        for mut plane in frame.planes_mut() {
            for row in 0..plane.rows() {
                let row_samples = width as usize * channels;
                let src_start = row * row_samples;
                let Some(src) = samples.get(src_start..src_start + row_samples) else {
                    break;
                };
                if let Some(dst) = plane.row_mut(row) {
                    for (d, &s) in dst.chunks_exact_mut(4).zip(src.iter()) {
                        d.copy_from_slice(&s.to_ne_bytes());
                    }
                }
            }
        }
        if let Some((num, den)) = tps
            && num > 0
        {
            frame.time_base = vaco_core::Rational::new(
                i32::try_from(den).unwrap_or(1),
                i32::try_from(num).unwrap_or(1),
            );
            frame.duration = vaco_core::Duration(i64::from(render.duration()));
        }
        frame.flags = FrameFlags::KEY;
        out.push(frame);
    }
    Ok(out)
}
