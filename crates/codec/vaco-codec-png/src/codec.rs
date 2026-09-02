//! The byte format: PNG and APNG, wrapping the `png` crate.
//!
//! [`decode`] and [`encode`] are pure functions over bytes and [`Frame`]s; the
//! `SendReceive` wrappers in `lib.rs` never touch a `png::` type directly,
//! which is the whole point of the D11 boundary.

use std::io::Cursor;

use vaco_color::{ColorInfo, ColorPrimaries, MatrixCoefficients, TransferCharacteristic};
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

/// PNG's own colour type after we ask the decoder to expand palettes,
/// sub-8-bit grey and `tRNS` (`png::Transformations::EXPAND`): only these
/// four (colour type, bit depth) shapes remain.
fn output_pixfmt(color: png::ColorType, depth: png::BitDepth) -> Result<PixFmt> {
    use png::{BitDepth as D, ColorType as C};
    Ok(match (color, depth) {
        (C::Grayscale, D::Eight) => PixFmt::Gray8,
        (C::Grayscale, D::Sixteen) => native16(PixFmt::Gray16le, PixFmt::Gray16be),
        (C::GrayscaleAlpha, D::Eight) => PixFmt::Ya8,
        (C::GrayscaleAlpha, D::Sixteen) => native16(PixFmt::Ya16le, PixFmt::Ya16be),
        (C::Rgb, D::Eight) => PixFmt::Rgb24,
        (C::Rgb, D::Sixteen) => native16(PixFmt::Rgb48le, PixFmt::Rgb48be),
        (C::Rgba, D::Eight) => PixFmt::Rgba,
        (C::Rgba, D::Sixteen) => native16(PixFmt::Rgba64le, PixFmt::Rgba64be),
        _ => return Err(Error::Unsupported("png: colour type/bit depth after expansion")),
    })
}

/// The frame buffer's chosen 16-bit endianness. PNG always stores 16-bit
/// samples big-endian on the wire; we lay them out in whichever endianness is
/// native, matching every other decoder in this tree, and swap on the way in
/// and out.
const fn native16(le: PixFmt, be: PixFmt) -> PixFmt {
    if cfg!(target_endian = "big") { be } else { le }
}

fn is_be_native() -> bool {
    cfg!(target_endian = "big")
}

/// H.273 primaries code points PNG's `cICP` chunk can carry that this crate
/// currently maps; anything else stays [`ColorPrimaries::Unspecified`].
fn map_primaries(cp: u8) -> ColorPrimaries {
    match cp {
        1 => ColorPrimaries::Bt709,
        4 => ColorPrimaries::Bt470m,
        5 => ColorPrimaries::Bt470bg,
        6 => ColorPrimaries::Smpte170m,
        7 => ColorPrimaries::Smpte240m,
        8 => ColorPrimaries::Film,
        9 => ColorPrimaries::Bt2020,
        10 => ColorPrimaries::Smpte428,
        11 => ColorPrimaries::Smpte431,
        12 => ColorPrimaries::Smpte432,
        _ => ColorPrimaries::Unspecified,
    }
}

fn map_transfer(tf: u8) -> TransferCharacteristic {
    match tf {
        1 => TransferCharacteristic::Bt709,
        4 => TransferCharacteristic::Gamma22,
        5 => TransferCharacteristic::Gamma28,
        6 => TransferCharacteristic::Smpte170m,
        7 => TransferCharacteristic::Smpte240m,
        8 => TransferCharacteristic::Linear,
        13 => TransferCharacteristic::Iec61966_2_1,
        14 => TransferCharacteristic::Bt2020_10,
        15 => TransferCharacteristic::Bt2020_12,
        16 => TransferCharacteristic::Smpte2084,
        18 => TransferCharacteristic::AribStdB67,
        _ => TransferCharacteristic::Unspecified,
    }
}

fn map_matrix(mc: u8) -> MatrixCoefficients {
    match mc {
        0 => MatrixCoefficients::Identity,
        1 => MatrixCoefficients::Bt709,
        5 => MatrixCoefficients::Bt470bg,
        6 => MatrixCoefficients::Smpte170m,
        9 => MatrixCoefficients::Bt2020Ncl,
        10 => MatrixCoefficients::Bt2020Cl,
        _ => MatrixCoefficients::Unspecified,
    }
}

/// Reads `gAMA`/`cHRM`/`sRGB`/`cICP` into our own colour model.
///
/// `cICP` is preferred when present, since it is already H.273 code points —
/// the same table our own [`ColorInfo`] uses. Otherwise `sRGB` maps to the
/// sRGB primaries and transfer function exactly, and a bare `gAMA` of
/// 1/2.2 (PNG's overwhelmingly common non-sRGB value) maps to
/// [`TransferCharacteristic::Gamma22`]. Anything else is left
/// [`Default::default`] (`Unspecified`): an arbitrary `gAMA`/`cHRM` pair has
/// no H.273 code point to land on.
fn map_color_info(info: &png::Info<'_>) -> ColorInfo {
    if let Some(cicp) = &info.coding_independent_code_points {
        return ColorInfo {
            primaries: map_primaries(cicp.color_primaries),
            transfer: map_transfer(cicp.transfer_function),
            matrix: map_matrix(cicp.matrix_coefficients),
            range: if cicp.is_video_full_range_image {
                vaco_color::ColorRange::Full
            } else {
                vaco_color::ColorRange::Limited
            },
            ..Default::default()
        };
    }
    if info.srgb.is_some() {
        return ColorInfo {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferCharacteristic::Iec61966_2_1,
            ..Default::default()
        };
    }
    if let Some(gamma) = info.source_gamma {
        // `ScaledFloat` stores the value scaled by 100_000; 1/2.2 is 45455.
        let scaled = gamma.into_scaled();
        if scaled.abs_diff(45455) <= 50 {
            return ColorInfo {
                transfer: TransferCharacteristic::Gamma22,
                ..Default::default()
            };
        }
    }
    ColorInfo::default()
}

/// One decoded PNG frame plus the APNG placement/compositing metadata needed
/// to lay it onto the running canvas.
struct RawFrame {
    x_offset: u32,
    y_offset: u32,
    width: u32,
    height: u32,
    dispose_op: png::DisposeOp,
    blend_op: png::BlendOp,
    delay_num: u16,
    delay_den: u16,
    color: png::ColorType,
    depth: png::BitDepth,
    bytes: Vec<u8>,
}

/// Composite one 8-bit RGBA subframe onto an 8-bit RGBA canvas at
/// `(x, y)`, per `blend_op` (PNG spec §"Alpha channel information").
fn blend_rgba8(canvas: &mut [u8], canvas_w: u32, x: u32, y: u32, w: u32, h: u32, src: &[u8], over: bool) {
    for row in 0..h {
        let cy = y + row;
        let Some(src_row) = src.get(row as usize * w as usize * 4..(row as usize + 1) * w as usize * 4) else {
            continue;
        };
        let cx_start = cy as usize * canvas_w as usize + x as usize;
        let Some(dst_row) = canvas.get_mut(cx_start * 4..(cx_start + w as usize) * 4) else {
            continue;
        };
        for (dp, sp) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
            let &[sr, sg, sb, sa_u8] = sp else {
                continue;
            };
            if !over || sa_u8 == 255 {
                dp.copy_from_slice(sp);
                continue;
            }
            if sa_u8 == 0 {
                continue;
            }
            let [dr, dg, db, da_u8] = dp else {
                continue;
            };
            let sa = f32::from(sa_u8) / 255.0;
            let da = f32::from(*da_u8) / 255.0;
            let out_a = sa + da * (1.0 - sa);
            if out_a <= 0.0 {
                *dr = 0;
                *dg = 0;
                *db = 0;
                *da_u8 = 0;
                continue;
            }
            for (d, s) in [dr, dg, db].into_iter().zip([sr, sg, sb]) {
                let out = (f32::from(s) * sa + f32::from(*d) * da * (1.0 - sa)) / out_a;
                *d = out.round().clamp(0.0, 255.0) as u8;
            }
            *da_u8 = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Convert one subframe's own (colour type, bit depth) bytes to tightly
/// packed 8-bit RGBA, the common currency APNG compositing runs in.
fn to_rgba8(color: png::ColorType, depth: png::BitDepth, width: u32, height: u32, bytes: &[u8]) -> Vec<u8> {
    let px = (width as usize) * (height as usize);
    let mut out = vec![0u8; px * 4];
    let bpc = if depth == png::BitDepth::Sixteen { 2 } else { 1 };
    let stride = color.samples() * bpc;

    // One sample from a `bpc`-byte chunk: the high (first, big-endian) byte
    // for 16-bit, the only byte for 8-bit.
    let read_sample = |chunk: &[u8]| -> u8 {
        match chunk {
            &[hi, ..] => hi,
            [] => 0,
        }
    };

    for (src_px, out_px) in bytes.chunks_exact(stride).zip(out.chunks_exact_mut(4)) {
        let mut samples = src_px.chunks_exact(bpc).map(read_sample);
        let mut next = || samples.next().unwrap_or(0);
        let rgba = match color {
            png::ColorType::Grayscale => {
                let g = next();
                [g, g, g, 255]
            }
            png::ColorType::GrayscaleAlpha => {
                let g = next();
                let a = next();
                [g, g, g, a]
            }
            png::ColorType::Rgb => {
                let r = next();
                let g = next();
                let b = next();
                [r, g, b, 255]
            }
            png::ColorType::Rgba => [next(), next(), next(), next()],
            png::ColorType::Indexed => unreachable!("EXPAND removes indexed colour"),
        };
        out_px.copy_from_slice(&rgba);
    }
    out
}

/// Decode one PNG/APNG packet into every composited frame it carries.
///
/// A non-animated PNG (no `acTL`) always yields exactly one [`Frame`] in its
/// own native pixel format. An APNG composites each subframe onto a shared
/// canvas per its `fcTL` dispose/blend operations and yields
/// one 8-bit RGBA [`Frame`] per output frame — compositing collapses bit
/// depth to 8, a documented simplification (see the crate's module docs).
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed header, chunk sequence or missing
/// image data. [`Error::UnexpectedEof`] for a truncated stream.
/// [`Error::Unsupported`] for a colour type/bit-depth combination this crate
/// does not map to a [`vaco_pixfmt::PixFmt`], or dimensions the `png` crate's
/// own limits refuse. [`Error::LimitExceeded`] when a frame exceeds `budget`.
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Frame>> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|_| Error::InvalidData("png: header"))?;

    let color_info = map_color_info(reader.info());
    let is_animated = reader.info().animation_control.is_some();
    let canvas_w = reader.info().width;
    let canvas_h = reader.info().height;

    let buf_len = reader
        .output_buffer_size()
        .ok_or(Error::Unsupported("png: image too large"))?;
    let mut raw_frames: Vec<RawFrame> = Vec::new();

    loop {
        let mut buf: Vec<u8> = budget.alloc(buf_len)?;
        let info = match reader.next_frame(&mut buf) {
            Ok(info) => info,
            Err(_) if !raw_frames.is_empty() => break,
            Err(e) => return Err(map_decode_err(&e)),
        };
        let fc = reader.info().frame_control;
        let (x_offset, y_offset, dispose_op, blend_op, delay_num, delay_den) = match fc {
            Some(fc) => (
                fc.x_offset,
                fc.y_offset,
                fc.dispose_op,
                fc.blend_op,
                fc.delay_num,
                fc.delay_den,
            ),
            None => (0, 0, png::DisposeOp::None, png::BlendOp::Source, 1, 30),
        };
        let want = (info.width as usize) * (info.height as usize) * info.color_type.samples()
            * if info.bit_depth == png::BitDepth::Sixteen { 2 } else { 1 };
        raw_frames.push(RawFrame {
            x_offset,
            y_offset,
            width: info.width,
            height: info.height,
            dispose_op,
            blend_op,
            delay_num,
            delay_den,
            color: info.color_type,
            depth: info.bit_depth,
            bytes: buf.get(..want).unwrap_or(&buf).to_vec(),
        });
        if !is_animated {
            break;
        }
    }

    if raw_frames.is_empty() {
        return Err(Error::InvalidData("png: no image data"));
    }

    if !is_animated && raw_frames.len() == 1 {
        let Some(rf) = raw_frames.first() else {
            return Err(Error::InvalidData("png: no image data"));
        };
        let fmt = output_pixfmt(rf.color, rf.depth)?;
        let mut frame = Frame::alloc_video(budget, fmt, rf.width, rf.height)?;
        write_native(&mut frame, rf);
        frame.color = color_info;
        frame.flags = FrameFlags::KEY;
        return Ok(vec![frame]);
    }

    // APNG: composite every subframe onto a shared canvas in 8-bit RGBA.
    // Checked against the budget before a byte is touched, even though every
    // subframe is separately bounded by the `png` crate's own (fixed,
    // 64 MiB) `Limits` — this is what makes the temporary canvas respect a
    // caller's stricter `vaco_limits::Limits` too.
    budget.check_frame(canvas_w, canvas_h, 4)?;
    let mut canvas = vec![0u8; canvas_w as usize * canvas_h as usize * 4];
    let mut previous: Option<Vec<u8>> = None;
    let mut out = Vec::new();
    for rf in &raw_frames {
        if rf.dispose_op == png::DisposeOp::Previous {
            previous = Some(canvas.clone());
        }
        let rgba = to_rgba8(rf.color, rf.depth, rf.width, rf.height, &rf.bytes);
        blend_rgba8(
            &mut canvas,
            canvas_w,
            rf.x_offset,
            rf.y_offset,
            rf.width,
            rf.height,
            &rgba,
            rf.blend_op == png::BlendOp::Over,
        );

        let mut frame = Frame::alloc_video(budget, PixFmt::Rgba, canvas_w, canvas_h)?;
        for mut plane in frame.planes_mut() {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    let src_start = y * canvas_w as usize * 4;
                    if let Some(src) = canvas.get(src_start..src_start + row.len()) {
                        row.copy_from_slice(src);
                    }
                }
            }
        }
        frame.color = color_info;
        // APNG delay is `delay_num / delay_den` seconds (a denominator of 0
        // means "treat as 100", per the spec); express it directly as that
        // fraction's numerator of ticks in that fraction's own time base.
        let den = if rf.delay_den == 0 { 100 } else { rf.delay_den };
        frame.time_base = vaco_core::Rational::new(1, i32::from(den));
        frame.duration = vaco_core::Duration(i64::from(rf.delay_num));
        frame.flags = FrameFlags::KEY;
        out.push(frame);

        match rf.dispose_op {
            png::DisposeOp::Background => {
                for row in 0..rf.height {
                    let start = ((rf.y_offset + row) as usize * canvas_w as usize + rf.x_offset as usize) * 4;
                    if let Some(slice) = canvas.get_mut(start..start + rf.width as usize * 4) {
                        slice.fill(0);
                    }
                }
            }
            png::DisposeOp::Previous => {
                if let Some(p) = previous.take() {
                    canvas = p;
                }
            }
            png::DisposeOp::None => {}
        }
    }
    Ok(out)
}

fn map_decode_err(e: &png::DecodingError) -> Error {
    match e {
        png::DecodingError::IoError(_) => Error::UnexpectedEof,
        png::DecodingError::LimitsExceeded => {
            Error::LimitExceeded { limit: "png_frame", requested: 0, cap: 0 }
        }
        _ => Error::InvalidData("png: malformed stream"),
    }
}

/// Write a single non-animated frame's bytes into `frame`'s own native
/// planes, swapping 16-bit samples from PNG's big-endian wire format to
/// whichever endianness the pixel format declares.
fn write_native(frame: &mut Frame, rf: &RawFrame) {
    let bpc = if rf.depth == png::BitDepth::Sixteen { 2 } else { 1 };
    let channels = rf.color.samples();
    let row_bytes = rf.width as usize * channels * bpc;
    let swap = rf.depth == png::BitDepth::Sixteen && !is_be_native();
    for mut plane in frame.planes_mut() {
        for y in 0..plane.rows() {
            let src_start = y * row_bytes;
            let Some(src) = rf.bytes.get(src_start..src_start + row_bytes) else {
                break;
            };
            if let Some(dst) = plane.row_mut(y) {
                let n = dst.len().min(src.len());
                if let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) {
                    d.copy_from_slice(s);
                    if swap {
                        for pair in d.chunks_exact_mut(2) {
                            pair.swap(0, 1);
                        }
                    }
                }
            }
        }
    }
}

/// `-pred`'s six values (`ffmpeg -h encoder=png`), kept independent of the
/// D11 boundary's `png::Filter` -- that mapping lives in [`encode`] alone,
/// so nothing outside this module ever names a `png::` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Predictor {
    /// `0`/`none`: never filter a row.
    None,
    /// `1`/`sub`: filter against the pixel to the left.
    Sub,
    /// `2`/`up`: filter against the pixel above.
    Up,
    /// `3`/`avg`: filter against the average of left and above.
    Avg,
    /// `4`/`paeth`: the Paeth predictor. The `png` crate's own default when
    /// no encoder option overrides it.
    Paeth,
    /// `5`/`mixed`: choose the best filter independently for each row
    /// (`png::Filter::Adaptive`).
    Mixed,
}

/// Encoder knobs mirroring `ffmpeg png`'s own `AVOption`s, measured against
/// `ffmpeg -h encoder=png` and a real encode (`-compression_level 0` vs `9`
/// moves a 320x240 `testsrc` PNG between 231 KiB and 1.3 KiB). `None` in
/// either field leaves the `png` crate's own default untouched rather than
/// forcing a choice nobody asked for.
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeOptions {
    /// `-pred`.
    pub pred: Option<Predictor>,
    /// `-compression_level`, zlib level 0 (none) to 9 (max); this crate
    /// clamps to that range before it ever reaches here.
    pub compression_level: Option<u8>,
}

/// Encode one or more frames as a PNG (one frame) or APNG (more than one),
/// the first frame defining the canvas size, colour type and bit depth.
///
/// Fidelity is D11's "Equivalent": pixel values round-trip exactly, but the
/// compressed byte stream will not match the reference `ffmpeg` byte-for-byte
/// (a different deflate implementation).
///
/// # Errors
///
/// [`Error::InvalidData`] for an empty frame list or an encoder failure.
/// [`Error::Unsupported`] for a pixel format this crate does not map to a
/// PNG colour type/bit depth (see [`png_color_for`]).
pub fn encode(frames: &[Frame], _budget: &mut Budget, options: &EncodeOptions) -> Result<Vec<u8>> {
    let Some(first) = frames.first() else {
        return Err(Error::InvalidData("png: no frames to encode"));
    };
    let vaco_frame::FrameData::Video { format, width, height, .. } = &first.data else {
        return Err(Error::Unsupported("png: audio frame"));
    };
    let (width, height) = (*width, *height);
    let (color, depth) = png_color_for(*format)?;

    // Grown incrementally from bytes the encoder itself produces, not
    // pre-sized from an attacker-controlled field, so this is not the
    // `Vec::with_capacity`-from-an-option hazard `clippy.toml` bans: nothing
    // sizes it up front.
    let mut out: Vec<u8> = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if let Some(level) = options.compression_level {
            encoder.set_deflate_compression(png::DeflateCompression::Level(level.min(9)));
        }
        if let Some(pred) = options.pred {
            encoder.set_filter(match pred {
                Predictor::None => png::Filter::NoFilter,
                Predictor::Sub => png::Filter::Sub,
                Predictor::Up => png::Filter::Up,
                Predictor::Avg => png::Filter::Avg,
                Predictor::Paeth => png::Filter::Paeth,
                Predictor::Mixed => png::Filter::Adaptive,
            });
        }
        if frames.len() > 1 {
            encoder
                .set_animated(frames.len() as u32, 0)
                .map_err(|_| Error::InvalidData("png: invalid animation parameters"))?;
        }
        let mut writer = encoder
            .write_header()
            .map_err(|_| Error::InvalidData("png: header encode"))?;
        for frame in frames {
            if frames.len() > 1 {
                let (num, den) = frame_delay(frame);
                writer
                    .set_frame_delay(num, den)
                    .map_err(|_| Error::InvalidData("png: frame delay"))?;
            }
            let bytes = native_bytes(frame, color, depth)?;
            writer
                .write_image_data(&bytes)
                .map_err(|_| Error::InvalidData("png: frame encode"))?;
        }
        writer
            .finish()
            .map_err(|_| Error::InvalidData("png: trailer encode"))?;
    }
    Ok(out)
}

/// `Frame::duration`/`Frame::time_base` as a `(numerator, denominator)` pair
/// of `u16`s clamped to APNG's `fcTL` field widths.
fn frame_delay(frame: &Frame) -> (u16, u16) {
    let num = frame.duration.0.clamp(0, i64::from(u16::MAX)) as u16;
    let den = frame.time_base.den.clamp(1, i32::from(u16::MAX)) as u16;
    (num, den)
}

fn png_color_for(fmt: PixFmt) -> Result<(png::ColorType, png::BitDepth)> {
    use png::{BitDepth as D, ColorType as C};
    Ok(match fmt {
        PixFmt::Gray8 => (C::Grayscale, D::Eight),
        PixFmt::Gray16le | PixFmt::Gray16be => (C::Grayscale, D::Sixteen),
        PixFmt::Ya8 => (C::GrayscaleAlpha, D::Eight),
        PixFmt::Ya16le | PixFmt::Ya16be => (C::GrayscaleAlpha, D::Sixteen),
        PixFmt::Rgb24 => (C::Rgb, D::Eight),
        PixFmt::Rgb48le | PixFmt::Rgb48be => (C::Rgb, D::Sixteen),
        PixFmt::Rgba => (C::Rgba, D::Eight),
        PixFmt::Rgba64le | PixFmt::Rgba64be => (C::Rgba, D::Sixteen),
        _ => return Err(Error::Unsupported("png: encode pixel format")),
    })
}

/// Bytes for one frame's plane, big-endian for 16-bit samples (what PNG's
/// wire format requires), packed with no row padding.
fn native_bytes(frame: &Frame, color: png::ColorType, depth: png::BitDepth) -> Result<Vec<u8>> {
    let vaco_frame::FrameData::Video { width, height, planes, .. } = &frame.data else {
        return Err(Error::Unsupported("png: audio frame"));
    };
    let plane = planes.first().ok_or(Error::InvalidData("png: no plane"))?;
    let bpc = if depth == png::BitDepth::Sixteen { 2 } else { 1 };
    let row_bytes = *width as usize * color.samples() * bpc;
    let swap = depth == png::BitDepth::Sixteen && !is_be_native();
    let mut out = vec![0u8; row_bytes * *height as usize];
    for y in 0..*height as usize {
        let start = y * plane.stride;
        let Some(src) = plane.data.as_slice().get(start..start + row_bytes) else {
            continue;
        };
        let Some(dst) = out.get_mut(y * row_bytes..y * row_bytes + row_bytes) else {
            continue;
        };
        dst.copy_from_slice(src);
        if swap {
            for pair in dst.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
    }
    Ok(out)
}
