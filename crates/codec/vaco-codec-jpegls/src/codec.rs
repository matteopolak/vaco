//! The per-image decode/encode loop: marker parsing, sample-order (line- or
//! non-interleaved), and the regular/run-mode dispatch per sample.
//!
//! Only `NEAR = 0` (lossless), 8-bit, 1- or 3-component, non-interleaved or
//! line-interleaved scans are covered — see [`crate`]'s module doc for why.

use crate::bits::{BitReader, BitWriter};
use crate::context::{
    Contexts as CtxTable, RunModeState, context_index, med,
};
use crate::golomb::{self, map_alternate, map_regular, unmap_alternate, unmap_regular};
use crate::marker;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

/// 8-bit alphabet size; the only depth this crate decodes.
const ALPHA: i32 = 256;

/// Reduce a prediction-error-shaped value into `[-ALPHA/2, ALPHA/2 - 1]`
/// (LOCO-I §3.2.1, "the actual value of the prediction residual can be
/// reduced, modulo alpha, to a value between `-floor(alpha/2)` and
/// `ceil(alpha/2)-1`").
fn reduce_mod_alpha(v: i32) -> i32 {
    let r = v.rem_euclid(ALPHA);
    if r >= (ALPHA >> 1) { r - ALPHA } else { r }
}

/// The inverse of [`reduce_mod_alpha`] composed with reconstruction: fold an
/// arbitrary integer back into a valid 8-bit sample.
fn wrap_to_sample(v: i32) -> u8 {
    v.rem_euclid(ALPHA).clamp(0, 255) as u8
}

/// Geometry shared by every sample access into one packed plane: `Gray8` is
/// `bpp = 1`; `Rgb24`'s three components share one plane at `bpp = 3` with
/// `comp` selecting the byte offset within each pixel.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    stride: usize,
    bpp: usize,
    width: usize,
    height: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct Neighbors {
    a: i32,
    b: i32,
    c: i32,
    d: i32,
}

/// One component's state that is *not* shared with the others: its run-mode
/// adaptation index (component-dependent per the Appendix) and the causal
/// template's column-0 carry (§3.6's boundary rule).
#[derive(Debug, Clone, Copy)]
struct CompState {
    run: RunModeState,
    prev_first_a: i32,
}

impl CompState {
    const fn new() -> Self {
        Self {
            run: RunModeState::new(),
            prev_first_a: 0,
        }
    }
}

fn sample_get(buf: &[u8], geo: Geometry, comp: usize, x: usize, y: usize) -> i32 {
    let idx = y
        .saturating_mul(geo.stride)
        .saturating_add(x.saturating_mul(geo.bpp))
        .saturating_add(comp);
    i32::from(buf.get(idx).copied().unwrap_or(0))
}

fn sample_set(buf: &mut [u8], geo: Geometry, comp: usize, x: usize, y: usize, v: u8) {
    let idx = y
        .saturating_mul(geo.stride)
        .saturating_add(x.saturating_mul(geo.bpp))
        .saturating_add(comp);
    if let Some(slot) = buf.get_mut(idx) {
        *slot = v;
    }
}

/// §3.6's boundary rules: zero north neighbours on the first line; `a`/`d`
/// copy `b` at the left/right edges; `c` at the left edge carries the `a`
/// used for column 0 of the *previous* line.
#[allow(clippy::many_single_char_names, reason = "matches the LOCO-I paper's own notation: a/b/c/d neighbours, g/m run-mode parameter, k Golomb parameter, x/y coordinates")]
fn neighbors(buf: &[u8], geo: Geometry, comp: usize, x: usize, y: usize, prev_first_a: i32) -> Neighbors {
    if y == 0 {
        let a = if x == 0 { 0 } else { sample_get(buf, geo, comp, x - 1, y) };
        Neighbors { a, b: 0, c: 0, d: 0 }
    } else {
        let b = sample_get(buf, geo, comp, x, y - 1);
        let d = if x + 1 < geo.width {
            sample_get(buf, geo, comp, x + 1, y - 1)
        } else {
            b
        };
        let (a, c) = if x == 0 {
            (b, prev_first_a)
        } else {
            (
                sample_get(buf, geo, comp, x - 1, y),
                sample_get(buf, geo, comp, x - 1, y - 1),
            )
        };
        Neighbors { a, b, c, d }
    }
}

fn context_missing() -> Error {
    Error::InvalidData("jpegls: context index out of range")
}

fn decode_regular_sample(
    ctxs: &mut CtxTable,
    reader: &mut BitReader<'_>,
    n: Neighbors,
    (g1, g2, g3): (i32, i32, i32),
) -> Result<u8> {
    let (idx, flip) = context_index(g1, g2, g3);
    let ctx = ctxs.regular.get_mut(idx).ok_or_else(context_missing)?;
    let predicted = med(n.a, n.b, n.c);
    let corrected = (predicted + ctx.bias(flip)).clamp(0, 255);
    let k = ctx.k();
    let use_alt = ctx.use_alternate_mapping(k);
    let y = golomb::decode(reader, k)?;
    let mapped_eps = if use_alt { unmap_alternate(y) } else { unmap_regular(y) };
    ctx.update(mapped_eps);
    let raw_eps = if flip { -mapped_eps } else { mapped_eps };
    Ok(wrap_to_sample(corrected + raw_eps))
}

fn encode_regular_sample(
    ctxs: &mut CtxTable,
    writer: &mut BitWriter,
    n: Neighbors,
    (g1, g2, g3): (i32, i32, i32),
    actual: u8,
) -> Result<()> {
    let (idx, flip) = context_index(g1, g2, g3);
    let ctx = ctxs.regular.get_mut(idx).ok_or_else(context_missing)?;
    let predicted = med(n.a, n.b, n.c);
    let corrected = (predicted + ctx.bias(flip)).clamp(0, 255);
    let raw_eps = i32::from(actual) - corrected;
    let signed_eps = if flip { -raw_eps } else { raw_eps };
    let eps = reduce_mod_alpha(signed_eps);
    let k = ctx.k();
    let use_alt = ctx.use_alternate_mapping(k);
    let mapped = if use_alt { map_alternate(eps) } else { map_regular(eps) };
    golomb::encode(writer, mapped, k);
    ctx.update(eps);
    Ok(())
}

/// §3.5's run-interruption sample coding. `a_val == b_val` selects the
/// context in which `eps` is known never to be zero, per the paper's own
/// text; see [`context`](crate::context)'s module doc for how the exact
/// modified mapping was checked.
fn ri_context_index(a_val: i32, b_val: i32) -> usize {
    usize::from(a_val != b_val)
}

/// §3.5's run-interruption sample decode.
///
/// Two things here are not stated numerically in the LOCO-I paper (only in
/// the ISO text this crate could not reach) and were instead pinned down by
/// encoding deliberately two-toned and flat synthetic images with this
/// crate and diffing against `ffmpeg -c:v jpegls`'s own decode (D17):
///
/// - The `a == b` context always uses the alternate mapping `M'`, and the
///   `a != b` context always uses `M` — neither is the `k == 0`-conditional
///   choice [`RegularCtx`] makes; a value read via the wrong mapping still
///   decodes *a* number, just the wrong one, which is what "a test that
///   asserts well-formedness does not assert correctness" looks like here.
/// - In the `a != b` context, once `k == 0` the sign of the decoded value
///   is flipped before being added to `b`. Measured by encoding a
///   two-segment image (`ffmpeg`'s own encoder) where a run's flat value is
///   below its upper neighbour and comparing pixel-for-pixel against
///   `ffmpeg`'s decode: unflipped, every `k == 0` sample in that context
///   reconstructed to the wrong value while every `k > 0` sample was right.
fn decode_ri_sample(
    ctxs: &mut CtxTable,
    reader: &mut BitReader<'_>,
    a_val: i32,
    b_val: i32,
    run_g: u32,
) -> Result<u8> {
    let same = a_val == b_val;
    let ctx = ctxs
        .ri
        .get_mut(ri_context_index(a_val, b_val))
        .ok_or_else(context_missing)?;
    let k = ctx.k();
    let y = golomb::decode_limited(reader, k, golomb::ri_qmax(run_g))?;
    let shifted = if same { unmap_alternate(y) } else { unmap_regular(y) };
    ctx.update(shifted);
    let eps = if same {
        if shifted >= 0 { shifted + 1 } else { shifted }
    } else if k == 0 {
        -shifted
    } else {
        shifted
    };
    Ok(wrap_to_sample(b_val + eps))
}

fn encode_ri_sample(
    ctxs: &mut CtxTable,
    writer: &mut BitWriter,
    a_val: i32,
    b_val: i32,
    actual: u8,
    run_g: u32,
) -> Result<()> {
    let same = a_val == b_val;
    let raw_eps = i32::from(actual) - b_val;
    let eps = reduce_mod_alpha(raw_eps);
    let ctx = ctxs
        .ri
        .get_mut(ri_context_index(a_val, b_val))
        .ok_or_else(context_missing)?;
    let k = ctx.k();
    let shifted = if same {
        if eps > 0 { eps - 1 } else { eps }
    } else if k == 0 {
        -eps
    } else {
        eps
    };
    let mapped = if same { map_alternate(shifted) } else { map_regular(shifted) };
    golomb::encode_limited(writer, mapped, k, golomb::ri_qmax(run_g));
    ctx.update(shifted);
    Ok(())
}

/// §3.5's run-mode subroutine, decode direction. Returns the column after
/// the run (either `width`, or one past the decoded run-interruption
/// sample).
#[allow(clippy::too_many_arguments, clippy::many_single_char_names, reason = "one call site per run; every argument is load-bearing plane geometry or shared state, and names match the paper (g/m run parameter, x/y coordinates)")]
fn decode_run(
    ctxs: &mut CtxTable,
    comp_state: &mut CompState,
    reader: &mut BitReader<'_>,
    buf: &mut [u8],
    geo: Geometry,
    comp: usize,
    mut x: usize,
    y: usize,
    a_val: i32,
) -> Result<usize> {
    let a_sample = wrap_to_sample(a_val);
    loop {
        let g = comp_state.run.g();
        let m = 1usize << g;
        let bit = reader.get_bit()?;
        if bit == 1 {
            if geo.width.saturating_sub(x) >= m {
                for i in 0..m {
                    sample_set(buf, geo, comp, x + i, y, a_sample);
                }
                x += m;
                comp_state.run.bump_up();
                if x == geo.width {
                    return Ok(x);
                }
            } else {
                for i in x..geo.width {
                    sample_set(buf, geo, comp, i, y, a_sample);
                }
                return Ok(geo.width);
            }
        } else {
            let r = reader.get_bits(g)? as usize;
            for i in 0..r {
                sample_set(buf, geo, comp, x + i, y, a_sample);
            }
            x += r;
            comp_state.run.bump_down();
            let b_val = if y == 0 { 0 } else { sample_get(buf, geo, comp, x, y - 1) };
            let sample_val = decode_ri_sample(ctxs, reader, a_val, b_val, g)?;
            sample_set(buf, geo, comp, x, y, sample_val);
            return Ok(x + 1);
        }
    }
}

/// §3.5's run-mode subroutine, encode direction.
#[allow(clippy::too_many_arguments, clippy::many_single_char_names, reason = "one call site per run; every argument is load-bearing plane geometry or shared state, and names match the paper (g/m run parameter, x/y coordinates)")]
fn encode_run(
    ctxs: &mut CtxTable,
    comp_state: &mut CompState,
    writer: &mut BitWriter,
    buf: &[u8],
    geo: Geometry,
    comp: usize,
    mut x: usize,
    y: usize,
    a_val: i32,
) -> Result<usize> {
    loop {
        let g = comp_state.run.g();
        let m = 1usize << g;
        let avail = geo.width.saturating_sub(x);
        let cap = m.min(avail);
        let mut r = 0usize;
        while r < cap && sample_get(buf, geo, comp, x + r, y) == a_val {
            r += 1;
        }
        if r == m {
            writer.put_bits(1, 1);
            x += m;
            comp_state.run.bump_up();
            if x == geo.width {
                return Ok(x);
            }
        } else if r == avail {
            writer.put_bits(1, 1);
            return Ok(geo.width);
        } else {
            writer.put_bits(0, 1);
            writer.put_bits(r as u32, g);
            x += r;
            comp_state.run.bump_down();
            let b_val = if y == 0 { 0 } else { sample_get(buf, geo, comp, x, y - 1) };
            let actual = sample_get(buf, geo, comp, x, y) as u8;
            encode_ri_sample(ctxs, writer, a_val, b_val, actual, g)?;
            return Ok(x + 1);
        }
    }
}

fn decode_row(
    ctxs: &mut CtxTable,
    comp_state: &mut CompState,
    reader: &mut BitReader<'_>,
    buf: &mut [u8],
    geo: Geometry,
    comp: usize,
    y: usize,
) -> Result<()> {
    let mut x = 0usize;
    while x < geo.width {
        let n = neighbors(buf, geo, comp, x, y, comp_state.prev_first_a);
        if x == 0 {
            comp_state.prev_first_a = n.a;
        }
        let (g1, g2, g3) = (n.d - n.b, n.b - n.c, n.c - n.a);
        if g1 == 0 && g2 == 0 && g3 == 0 {
            x = decode_run(ctxs, comp_state, reader, buf, geo, comp, x, y, n.a)?;
        } else {
            let val = decode_regular_sample(ctxs, reader, n, (g1, g2, g3))?;
            sample_set(buf, geo, comp, x, y, val);
            x += 1;
        }
    }
    Ok(())
}

fn encode_row(
    ctxs: &mut CtxTable,
    comp_state: &mut CompState,
    writer: &mut BitWriter,
    buf: &[u8],
    geo: Geometry,
    comp: usize,
    y: usize,
) -> Result<()> {
    let mut x = 0usize;
    while x < geo.width {
        let n = neighbors(buf, geo, comp, x, y, comp_state.prev_first_a);
        if x == 0 {
            comp_state.prev_first_a = n.a;
        }
        let (g1, g2, g3) = (n.d - n.b, n.b - n.c, n.c - n.a);
        if g1 == 0 && g2 == 0 && g3 == 0 {
            x = encode_run(ctxs, comp_state, writer, buf, geo, comp, x, y, n.a)?;
        } else {
            let actual = sample_get(buf, geo, comp, x, y) as u8;
            encode_regular_sample(ctxs, writer, n, (g1, g2, g3), actual)?;
            x += 1;
        }
    }
    Ok(())
}

fn comp_state_mut(comps: &mut [CompState], comp: usize) -> Result<&mut CompState> {
    comps
        .get_mut(comp)
        .ok_or(Error::InvalidData("jpegls: component index out of range"))
}

/// The pixel format a JPEG-LS frame header's component count denotes: one
/// component is `gray8`, three are `rgb24`. Shared with [`decode`] so the
/// reported format is the one the frame actually carries.
const fn pixel_format(num_components: u8) -> PixFmt {
    if num_components == 1 {
        PixFmt::Gray8
    } else {
        PixFmt::Rgb24
    }
}

/// The stream description a JPEG-LS `SOF55` states, without decoding a pixel.
///
/// Walks the marker segments the same way [`decode`] does, stopping at the
/// frame header rather than the scan.
#[must_use]
pub fn parameters(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    let (soi_code, soi_pos) = marker::find_marker(data, 0).ok()?;
    if soi_code != marker::SOI {
        return None;
    }
    let mut pos = soi_pos + 2;
    let fh = loop {
        let (code, marker_pos) = marker::find_marker(data, pos).ok()?;
        let seg_pos = marker_pos + 2;
        if code == marker::SOF55 {
            break marker::parse_sof55(data, seg_pos).ok()?.0;
        } else if code == marker::SOS || code == marker::EOI || code == marker::SOI {
            return None;
        }
        pos = marker::skip_segment(data, seg_pos).ok()?;
    };
    if fh.precision != 8 || fh.width == 0 || fh.height == 0 {
        return None;
    }
    let mut params =
        vaco_codec_core::CodecParameters::video().with_codec(vaco_codec_core::CodecId::JpegLs);
    if let Some(v) = params.video.as_mut() {
        v.width = u32::from(fh.width);
        v.height = u32::from(fh.height);
        v.coded_width = v.width;
        v.coded_height = v.height;
        v.format = Some(pixel_format(fh.num_components));
    }
    Some(params)
}

/// Decode one whole JPEG-LS image.
///
/// # Errors
/// [`Error::InvalidData`] for a malformed stream, [`Error::Unsupported`] for
/// a scan shape this crate does not implement (near-lossless, more than 3
/// components, subsampling, or sample-interleaved multi-component scans),
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let (soi_code, soi_pos) = marker::find_marker(data, 0)?;
    if soi_code != marker::SOI {
        return Err(Error::InvalidData("jpegls: missing SOI"));
    }
    let mut pos = soi_pos + 2;
    let mut frame_header: Option<marker::FrameHeader> = None;
    let (scan_header, entropy_start) = loop {
        let (code, marker_pos) = marker::find_marker(data, pos)?;
        let seg_pos = marker_pos + 2;
        if code == marker::SOF55 {
            let (fh, next) = marker::parse_sof55(data, seg_pos)?;
            frame_header = Some(fh);
            pos = next;
        } else if code == marker::LSE {
            pos = marker::check_lse_is_default(data, seg_pos)?;
        } else if code == marker::SOS {
            let (sh, next) = marker::parse_sos(data, seg_pos)?;
            break (sh, next);
        } else if code == marker::EOI || code == marker::SOI {
            return Err(Error::InvalidData("jpegls: SOS not found before EOI"));
        } else {
            pos = marker::skip_segment(data, seg_pos)?;
        }
    };

    let fh = frame_header.ok_or(Error::InvalidData("jpegls: SOS before SOF55"))?;
    if fh.precision != 8 {
        return Err(Error::Unsupported("jpegls: only 8-bit samples are decoded"));
    }
    if scan_header.near != 0 {
        return Err(Error::Unsupported(
            "jpegls: only NEAR=0 (lossless) scans are decoded",
        ));
    }
    if scan_header.num_components != fh.num_components {
        return Err(Error::Unsupported(
            "jpegls: a scan covering fewer components than the frame is not decoded",
        ));
    }
    for i in 0..usize::from(scan_header.num_components) {
        let selector = scan_header.selectors.get(i).copied().unwrap_or(0);
        let declared = fh.components.get(i).map_or(0, |c| c.id);
        if selector != declared {
            return Err(Error::InvalidData(
                "jpegls: scan component selectors do not match the frame header",
            ));
        }
    }
    if scan_header.ilv == 2 {
        return Err(Error::Unsupported(
            "jpegls: sample-interleaved multi-component scans are not decoded",
        ));
    }

    let nf = usize::from(fh.num_components);
    let format = pixel_format(fh.num_components);
    let bpp = if nf == 1 { 1usize } else { 3usize };
    let mut frame = Frame::alloc_video(budget, format, u32::from(fh.width), u32::from(fh.height))?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("jpegls: expected a video frame"));
    };
    let plane = planes.get_mut(0).ok_or(Error::InvalidData("jpegls: no plane 0"))?;
    let geo = Geometry {
        stride: plane.stride,
        bpp,
        width: usize::from(fh.width),
        height: usize::from(fh.height),
    };
    let buf = plane.data.make_mut();

    let mut ctxs = CtxTable::new();
    let mut comps = vec![CompState::new(); nf];
    let mut reader = BitReader::new(data.get(entropy_start..).unwrap_or(&[]));

    if nf == 1 || scan_header.ilv == 0 {
        for comp in 0..nf {
            let cs = comp_state_mut(&mut comps, comp)?;
            for y in 0..geo.height {
                decode_row(&mut ctxs, cs, &mut reader, buf, geo, comp, y)?;
            }
        }
    } else {
        for y in 0..geo.height {
            for comp in 0..nf {
                let cs = comp_state_mut(&mut comps, comp)?;
                decode_row(&mut ctxs, cs, &mut reader, buf, geo, comp, y)?;
            }
        }
    }

    Ok(frame)
}

/// Encode one whole frame as a lossless JPEG-LS image.
///
/// # Errors
/// [`Error::Unsupported`] if the frame's pixel format is neither `Gray8` nor
/// `Rgb24`, [`Error::InvalidData`] for a zero-sized or oversized (either
/// dimension past `u16::MAX`) frame.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("jpegls: expected a video frame"));
    };
    let (nf, bpp): (u8, usize) = match *format {
        PixFmt::Gray8 => (1, 1),
        PixFmt::Rgb24 => (3, 3),
        _ => return Err(Error::Unsupported("jpegls: encoder needs gray8 or rgb24 input")),
    };
    if *width == 0 || *height == 0 || *width > u32::from(u16::MAX) || *height > u32::from(u16::MAX) {
        return Err(Error::InvalidData("jpegls: zero-sized or oversized frame"));
    }
    let plane = planes.first().ok_or(Error::InvalidData("jpegls: no plane 0"))?;
    let geo = Geometry {
        stride: plane.stride,
        bpp,
        width: *width as usize,
        height: *height as usize,
    };
    let buf = plane.data.as_slice();

    let mut out = Vec::new();
    marker::write_soi(&mut out);
    #[allow(clippy::cast_possible_truncation, reason = "already bounded to u16::MAX above")]
    let (w16, h16) = (*width as u16, *height as u16);
    marker::write_sof55(&mut out, w16, h16, nf);
    let ilv = u8::from(nf != 1);
    marker::write_sos(&mut out, nf, ilv);

    let mut ctxs = CtxTable::new();
    let mut comps = vec![CompState::new(); usize::from(nf)];
    let mut writer = BitWriter::new();

    if nf == 1 {
        let cs = comp_state_mut(&mut comps, 0)?;
        for y in 0..geo.height {
            encode_row(&mut ctxs, cs, &mut writer, buf, geo, 0, y)?;
        }
    } else {
        for y in 0..geo.height {
            for comp in 0..usize::from(nf) {
                let cs = comp_state_mut(&mut comps, comp)?;
                encode_row(&mut ctxs, cs, &mut writer, buf, geo, comp, y)?;
            }
        }
    }

    out.extend_from_slice(&writer.finish());
    marker::write_eoi(&mut out);
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn gray_frame(width: u32, height: u32, pixels: &[u8]) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gray8, width, height).unwrap();
        let FrameData::Video { planes, .. } = &mut frame.data else {
            unreachable!()
        };
        let plane = &mut planes[0];
        let stride = plane.stride;
        let buf = plane.data.make_mut();
        for y in 0..height as usize {
            for x in 0..width as usize {
                buf[y * stride + x] = pixels[y * width as usize + x];
            }
        }
        frame
    }

    fn decode_gray(data: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(data, &mut budget).unwrap();
        let FrameData::Video { width, height, planes, .. } = &frame.data else {
            unreachable!()
        };
        let plane = &planes[0];
        let stride = plane.stride;
        let raw = plane.data.as_slice();
        let mut out = Vec::new();
        for y in 0..*height as usize {
            for x in 0..*width as usize {
                out.push(raw[y * stride + x]);
            }
        }
        (*width, *height, out)
    }

    #[test]
    fn a_flat_image_round_trips() {
        let pixels = vec![42u8; 16 * 12];
        let frame = gray_frame(16, 12, &pixels);
        let encoded = encode(&frame).unwrap();
        let (w, h, decoded) = decode_gray(&encoded);
        assert_eq!((w, h), (16, 12));
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn a_gradient_image_round_trips() {
        let (width, height) = (33u32, 17u32);
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.push(((x * 7 + y * 13) % 256) as u8);
            }
        }
        let frame = gray_frame(width, height, &pixels);
        let encoded = encode(&frame).unwrap();
        let (w, h, decoded) = decode_gray(&encoded);
        assert_eq!((w, h), (width, height));
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn noise_round_trips() {
        let (width, height) = (23u32, 19u32);
        let mut state: u32 = 0x1234_5678;
        let mut pixels = Vec::new();
        for _ in 0..(width * height) {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            pixels.push((state & 0xFF) as u8);
        }
        let frame = gray_frame(width, height, &pixels);
        let encoded = encode(&frame).unwrap();
        let (w, h, decoded) = decode_gray(&encoded);
        assert_eq!((w, h), (width, height));
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn runs_of_varying_length_round_trip() {
        let width = 200u32;
        let height = 5u32;
        let mut pixels = Vec::new();
        let mut val = 0u8;
        let mut run_len = 1usize;
        let mut count = 0usize;
        for _ in 0..(width * height) {
            pixels.push(val);
            count += 1;
            if count == run_len {
                count = 0;
                run_len += 1;
                val = val.wrapping_add(37);
            }
        }
        let frame = gray_frame(width, height, &pixels);
        let encoded = encode(&frame).unwrap();
        let (w, h, decoded) = decode_gray(&encoded);
        assert_eq!((w, h), (width, height));
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn a_single_pixel_image_round_trips() {
        let frame = gray_frame(1, 1, &[200]);
        let encoded = encode(&frame).unwrap();
        let (w, h, decoded) = decode_gray(&encoded);
        assert_eq!((w, h), (1, 1));
        assert_eq!(decoded, vec![200]);
    }

    #[test]
    fn rgb_round_trips_line_interleaved() {
        let (width, height) = (12usize, 9usize);
        let mut expected = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let base = (y * width + x) * 3;
                expected[base] = ((x + y) % 256) as u8;
                expected[base + 1] = ((x * 3) % 256) as u8;
                expected[base + 2] = 128;
            }
        }

        let mut budget = Budget::new(Limits::permissive());
        let mut frame =
            Frame::alloc_video(&mut budget, PixFmt::Rgb24, width as u32, height as u32).unwrap();
        {
            let FrameData::Video { planes, .. } = &mut frame.data else {
                unreachable!()
            };
            let plane = &mut planes[0];
            let stride = plane.stride;
            let buf = plane.data.make_mut();
            for y in 0..height {
                for x in 0..width {
                    let dst = y * stride + x * 3;
                    let src = (y * width + x) * 3;
                    buf[dst] = expected[src];
                    buf[dst + 1] = expected[src + 1];
                    buf[dst + 2] = expected[src + 2];
                }
            }
        }

        let encoded = encode(&frame).unwrap();
        let mut budget2 = Budget::new(Limits::permissive());
        let decoded = decode(&encoded, &mut budget2).unwrap();
        let FrameData::Video {
            planes: dp,
            width: dw,
            height: dh,
            ..
        } = &decoded.data
        else {
            unreachable!()
        };
        assert_eq!((*dw as usize, *dh as usize), (width, height));
        let dplane = &dp[0];
        let dstride = dplane.stride;
        let draw = dplane.data.as_slice();
        for y in 0..height {
            for x in 0..width {
                let dbase = y * dstride + x * 3;
                let ebase = (y * width + x) * 3;
                assert_eq!(draw[dbase], expected[ebase]);
                assert_eq!(draw[dbase + 1], expected[ebase + 1]);
                assert_eq!(draw[dbase + 2], expected[ebase + 2]);
            }
        }
    }
}

