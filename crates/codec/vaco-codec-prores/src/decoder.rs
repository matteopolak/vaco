//! Assembles [`header`], [`coeff`], and [`scan`] into a [`vaco_codec_core::Decoder`]:
//! parse `frame()`, decode every slice of every picture, dequantize, inverse
//! transform (RDD 36 SS7.3/7.4), and write reconstructed samples into a
//! [`vaco_frame::Frame`] (SS7.5).

use std::collections::VecDeque;

use vaco_codec_core::Decoder;
use vaco_codec_dsp_idct::mpeg2::Idct8x8;
use vaco_core::{Error, MediaType, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::coeff::decode_scanned_coefficients;
use crate::header::{
    self, ChromaFormat, FrameHeader, InterlaceMode, PictureHeader, QuantMatrix, SliceHeader,
};
use crate::scan::{INTERLACED_SCAN, PROGRESSIVE_SCAN, gather_block_scanned, inverse_block_scan};

const FRAME_IDENTIFIER: &[u8; 4] = b"icpf";

/// Luma block offsets within a 16x16 macroblock, Figure 6 (`0 1 / 2 3`).
const LUMA_BLOCK_OFFSETS: [(usize, usize); 4] = [(0, 0), (8, 0), (0, 8), (8, 8)];
/// Chroma block offsets, 4:2:2, Figure 7 (`0` over `1`, one 8-wide column).
const CHROMA422_BLOCK_OFFSETS: [(usize, usize); 2] = [(0, 0), (0, 8)];
/// Chroma block offsets, 4:4:4, Figure 8 (`0 2 / 1 3` — column-major, unlike
/// luma's row-major Figure 6).
const CHROMA444_BLOCK_OFFSETS: [(usize, usize); 4] = [(0, 0), (0, 8), (8, 0), (8, 8)];

fn pix_fmt_for(fh: &FrameHeader) -> PixFmt {
    let has_alpha = fh.alpha_channel_type != 0;
    match (fh.chroma_format, has_alpha) {
        (ChromaFormat::Yuv422, false) => PixFmt::Yuv422p10le,
        (ChromaFormat::Yuv422, true) => PixFmt::Yuva422p10le,
        (ChromaFormat::Yuv444, false) => PixFmt::Yuv444p12le,
        (ChromaFormat::Yuv444, true) => PixFmt::Yuva444p12le,
    }
}

/// RDD 36 SS7.5.1: `s = clamp(round(2^b * (v + 256) / 512))`, full-range
/// clamp (`[0, 2^b - 1]`) — the option SS7.5.1 offers over the narrower
/// broadcast-legal range, chosen because it is the one that reproduces an
/// encoder's original samples without clipping legitimate
/// super-black/super-white headroom professional video carries.
fn sample_from_reconstructed(v: f64, bit_depth: u32) -> u16 {
    let max = f64::from((1u32 << bit_depth) - 1);
    let scaled = f64::from(1u32 << bit_depth) * (v + 256.0) / 512.0;
    scaled.round().clamp(0.0, max) as u16
}

/// RDD 36 SS7.5.2: promote/demote a decoded alpha value to `b`-bit pixel
/// samples, treating the smallest/largest alpha values as opacities 0.0/1.0.
fn alpha_sample(value: i32, sixteen_bit: bool, bit_depth: u32) -> u16 {
    let max_out = f64::from((1u32 << bit_depth) - 1);
    let max_in = if sixteen_bit { 65535.0 } else { 255.0 };
    let v = f64::from(value.clamp(0, if sixteen_bit { 65535 } else { 255 }));
    (max_out * v / max_in).round().clamp(0.0, max_out) as u16
}

fn write_sample_u16le(buf: &mut [u8], stride: usize, x: usize, y: usize, w: usize, h: usize, value: u16) {
    if x >= w || y >= h {
        return;
    }
    let off = y.saturating_mul(stride).saturating_add(x.saturating_mul(2));
    let bytes = value.to_le_bytes();
    if let Some(b0) = buf.get_mut(off) {
        *b0 = bytes[0];
    }
    if let Some(b1) = buf.get_mut(off.saturating_add(1)) {
        *b1 = bytes[1];
    }
}

/// Maps a picture-local row to the final frame row, honoring the
/// progressive/interlaced field interleave of RDD 36 SS7.5.3.
#[derive(Clone, Copy)]
struct FieldMap {
    interlaced: bool,
    is_top: bool,
}

impl FieldMap {
    fn row(self, picture_row: usize) -> usize {
        if !self.interlaced {
            picture_row
        } else if self.is_top {
            picture_row.saturating_mul(2)
        } else {
            picture_row.saturating_mul(2).saturating_add(1)
        }
    }
}

fn field_map(fh: &FrameHeader, is_first_picture: bool) -> FieldMap {
    let interlaced = fh.interlace_mode.is_interlaced();
    let is_top = match fh.interlace_mode {
        InterlaceMode::Progressive => true,
        InterlaceMode::TopFirst => is_first_picture,
        InterlaceMode::BottomFirst => !is_first_picture,
    };
    FieldMap { interlaced, is_top }
}

fn scan_table_for(fh: &FrameHeader) -> &'static [usize; 64] {
    if fh.interlace_mode.is_interlaced() {
        &INTERLACED_SCAN
    } else {
        &PROGRESSIVE_SCAN
    }
}

/// Reconstruct one 8x8 block and write it into `plane`, dequantizing with
/// weight matrix `w` and scale `q_scale` (SS7.3), inverse-transforming
/// (SS7.4), and converting to samples (SS7.5.1).
#[allow(clippy::too_many_arguments, reason = "one reconstruction call site; splitting would just move the same context into a struct nobody else uses")]
fn reconstruct_block(
    idct: &mut Idct8x8<f64>,
    qfs: &[i32; 64],
    scan_table: &[usize; 64],
    w: &QuantMatrix,
    q_scale: u32,
    bit_depth: u32,
    plane: &mut [u8],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    base_x: usize,
    base_y_picture: usize,
    field: FieldMap,
) {
    let qf = inverse_block_scan(qfs, scan_table);
    let mut f = [0f64; 64];
    for (idx, slot) in f.iter_mut().enumerate() {
        let qcoef = qf.get(idx).copied().unwrap_or(0);
        let weight = w.get(idx).copied().unwrap_or(4);
        *slot = f64::from(qcoef) * f64::from(weight) * f64::from(q_scale) / 8.0;
    }
    let mut out = [0f64; 64];
    idct.apply(&f, &mut out);
    for y in 0..8usize {
        for x in 0..8usize {
            let Some(&v) = out.get(y * 8 + x) else {
                continue;
            };
            let sample = sample_from_reconstructed(v, bit_depth);
            let final_row = field.row(base_y_picture + y);
            write_sample_u16le(plane, stride, base_x + x, final_row, plane_w, plane_h, sample);
        }
    }
}

#[allow(clippy::too_many_arguments, reason = "slice decode needs the whole picture's context; a struct would just rename these fields")]
fn decode_slice(
    slice_bytes: &[u8],
    fh: &FrameHeader,
    mb_row: u32,
    mb_col_offset: u32,
    slice_size_in_mb: u32,
    field: FieldMap,
    idct: &mut Idct8x8<f64>,
    frame_planes: &mut [vaco_frame::Plane],
    budget: &mut Budget,
) -> Result<()> {
    let has_alpha = fh.alpha_channel_type != 0;
    let mut r = vaco_bitstream::BitReader::new(slice_bytes);
    let sh: SliceHeader = header::parse_slice_header(&mut r, has_alpha)?;
    let header_bytes: usize = if has_alpha { 8 } else { 6 };
    let mut cursor = header_bytes;

    let n_c = fh.chroma_format.chroma_blocks_per_mb();
    let num_y_blocks = 4usize.saturating_mul(slice_size_in_mb as usize);
    let num_c_blocks = n_c.saturating_mul(slice_size_in_mb as usize);

    let y_len = sh.coded_size_of_y_data as usize;
    let cb_len = sh.coded_size_of_cb_data as usize;
    let y_data = slice_bytes
        .get(cursor..cursor.saturating_add(y_len))
        .ok_or(Error::InvalidData("prores: y data truncated"))?;
    cursor = cursor.saturating_add(y_len);
    let cb_data = slice_bytes
        .get(cursor..cursor.saturating_add(cb_len))
        .ok_or(Error::InvalidData("prores: cb data truncated"))?;
    cursor = cursor.saturating_add(cb_len);
    let cr_len = if let Some(explicit) = sh.coded_size_of_cr_data {
        explicit as usize
    } else {
        slice_bytes.len().saturating_sub(cursor)
    };
    let cr_data = slice_bytes
        .get(cursor..cursor.saturating_add(cr_len))
        .ok_or(Error::InvalidData("prores: cr data truncated"))?;
    cursor = cursor.saturating_add(cr_len);

    let y_coeffs = decode_scanned_coefficients(y_data, num_y_blocks, budget)?;
    let cb_coeffs = decode_scanned_coefficients(cb_data, num_c_blocks, budget)?;
    let cr_coeffs = decode_scanned_coefficients(cr_data, num_c_blocks, budget)?;

    let q_scale = sh.q_scale();
    let scan_table = scan_table_for(fh);
    let bit_depth = fh.bit_depth();

    let Some(luma_plane) = frame_planes.first_mut() else {
        return Err(Error::InvalidData("prores: frame has no luma plane"));
    };
    let (luma_stride, luma_w, luma_h) = (
        luma_plane.stride,
        fh.horizontal_size as usize,
        fh.vertical_size as usize,
    );
    for m in 0..slice_size_in_mb as usize {
        for (b, &(bx, by)) in LUMA_BLOCK_OFFSETS.iter().enumerate() {
            let qfs = gather_block_scanned(&y_coeffs, 4, slice_size_in_mb as usize, m, b);
            let base_x = (mb_col_offset as usize + m).saturating_mul(16) + bx;
            let base_y = (mb_row as usize).saturating_mul(16).saturating_add(by);
            reconstruct_block(
                idct, &qfs, scan_table, &fh.luma_quant, q_scale, bit_depth,
                luma_plane.data.make_mut(), luma_stride, luma_w, luma_h, base_x, base_y, field,
            );
        }
    }

    let chroma_offsets: &[(usize, usize)] = match fh.chroma_format {
        ChromaFormat::Yuv422 => &CHROMA422_BLOCK_OFFSETS,
        ChromaFormat::Yuv444 => &CHROMA444_BLOCK_OFFSETS,
    };
    let chroma_mb_width = match fh.chroma_format {
        ChromaFormat::Yuv422 => 8usize,
        ChromaFormat::Yuv444 => 16usize,
    };
    let chroma_w = match fh.chroma_format {
        ChromaFormat::Yuv422 => (fh.horizontal_size as usize).div_ceil(2),
        ChromaFormat::Yuv444 => fh.horizontal_size as usize,
    };
    let chroma_h = fh.vertical_size as usize;

    for (plane_idx, coeffs) in [(1usize, &cb_coeffs), (2usize, &cr_coeffs)] {
        let Some(plane) = frame_planes.get_mut(plane_idx) else {
            return Err(Error::InvalidData("prores: frame missing chroma plane"));
        };
        let stride = plane.stride;
        let buf = plane.data.make_mut();
        for m in 0..slice_size_in_mb as usize {
            for (b, &(bx, by)) in chroma_offsets.iter().enumerate() {
                let qfs = gather_block_scanned(coeffs, n_c, slice_size_in_mb as usize, m, b);
                let base_x = (mb_col_offset as usize + m).saturating_mul(chroma_mb_width) + bx;
                let base_y = (mb_row as usize).saturating_mul(16).saturating_add(by);
                reconstruct_block(
                    idct, &qfs, scan_table, &fh.chroma_quant, q_scale, bit_depth,
                    buf, stride, chroma_w, chroma_h, base_x, base_y, field,
                );
            }
        }
    }

    if has_alpha {
        let sixteen_bit = fh.alpha_channel_type == 2;
        let rows_above = mb_row.saturating_mul(16);
        let picture_vertical_size = u32::from(fh.vertical_size);
        let slice_vertical_size = 16u32
            .min(picture_vertical_size.saturating_sub(rows_above))
            .max(1);
        let row_width = 16u32.saturating_mul(slice_size_in_mb);
        let num_alpha_values = usize::try_from(
            row_width.saturating_mul(slice_vertical_size),
        )
        .unwrap_or(usize::MAX);
        let alpha_data = slice_bytes.get(cursor..).unwrap_or(&[]);
        let alpha_values =
            crate::coeff::decode_scanned_alpha(alpha_data, num_alpha_values, sixteen_bit, budget)?;
        if let Some(plane) = frame_planes.get_mut(3) {
            let stride = plane.stride;
            let buf = plane.data.make_mut();
            let row_width = row_width.max(1);
            for (idx, &value) in alpha_values.iter().enumerate() {
                let idx = u32::try_from(idx).unwrap_or(u32::MAX);
                // Raster row/col from a linear index — a genuine division,
                // not a shortcut around one (mirrors `vaco-codec-av1`'s own
                // `TileNum / TileCols` row derivation).
                #[allow(clippy::integer_division, reason = "raster row/col from linear alpha value index")]
                let (row, col) = (idx / row_width, idx % row_width);
                let base_x = (mb_col_offset.saturating_mul(16)).saturating_add(col) as usize;
                let base_y = mb_row.saturating_mul(16).saturating_add(row) as usize;
                let final_row = field.row(base_y);
                let sample = alpha_sample(value, sixteen_bit, bit_depth);
                write_sample_u16le(buf, stride, base_x, final_row, luma_w, luma_h, sample);
            }
        }
    }

    Ok(())
}

fn be16(data: &[u8], off: usize) -> Result<u16> {
    let b = data
        .get(off..off.saturating_add(2))
        .ok_or(Error::InvalidData("prores: truncated"))?;
    let arr: [u8; 2] = b.try_into().map_err(|_| Error::InvalidData("prores: truncated"))?;
    Ok(u16::from_be_bytes(arr))
}

fn be32(data: &[u8], off: usize) -> Result<u32> {
    let b = data
        .get(off..off.saturating_add(4))
        .ok_or(Error::InvalidData("prores: truncated"))?;
    let arr: [u8; 4] = b.try_into().map_err(|_| Error::InvalidData("prores: truncated"))?;
    Ok(u32::from_be_bytes(arr))
}

#[derive(Debug)]
pub struct ProresDecoder {
    limits: Limits,
    pending: VecDeque<Frame>,
    /// Set by `send_packet(None)`; makes `receive_frame` answer `Eof`
    /// once `pending` is empty instead of `NeedMoreInput` forever -- see
    /// `vaco-codec-ac3`'s decoder's own `draining` field doc for the full
    /// reasoning (measured against `vaco-sched`'s `ProgressGuard`
    /// watchdog, same contract violation as `vaco-codec-alac`'s and
    /// `vaco-codec-vorbis`'s decoders).
    draining: bool,
}

impl ProresDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self { limits, pending: VecDeque::new(), draining: false }
    }

    fn decode_frame_payload(&mut self, payload: &[u8]) -> Result<Frame> {
        if payload.get(4..8) != Some(FRAME_IDENTIFIER) {
            return Err(Error::InvalidData("prores: missing 'icpf' frame identifier"));
        }
        let header_start = 8usize;
        let frame_header_size = be16(payload, header_start)? as usize;
        if frame_header_size < 20 {
            return Err(Error::InvalidData("prores: frame_header_size too small"));
        }
        let header_bytes = payload
            .get(header_start..header_start.saturating_add(frame_header_size))
            .ok_or(Error::InvalidData("prores: frame header truncated"))?;
        let fh = {
            let mut r = vaco_bitstream::BitReader::new(header_bytes);
            header::parse_frame_header(&mut r)?
        };

        let mut budget = Budget::new(self.limits.clone());
        // ProRes is 4:2:2 (10-bit) or 4:4:4 (12-bit, optionally with alpha),
        // never packed RGBA — a flat 4 bytes per pixel over-charges the
        // common 4:2:2 case (real average is 2.5 bytes/pixel) and
        // under-charges 4:4:4 with alpha (up to 6). `pix_fmt_for` already
        // resolves the real format from the frame header, so charge its real
        // average bytes per pixel, the same quantity `Frame::alloc_video`
        // itself checks against right below.
        let pix_fmt = pix_fmt_for(&fh);
        let bpp = u32::from(pix_fmt.bits_per_pixel()).div_ceil(8).max(1);
        budget.check_frame(u32::from(fh.horizontal_size), u32::from(fh.vertical_size), bpp)?;
        let mut frame = Frame::alloc_video(
            &mut budget,
            pix_fmt,
            u32::from(fh.horizontal_size),
            u32::from(fh.vertical_size),
        )?;
        let FrameData::Video { planes, .. } = &mut frame.data else {
            return Err(Error::InvalidData("prores: allocated frame has no planes"));
        };

        let mut idct = Idct8x8::<f64>::new()?;
        let num_pictures = usize::from(fh.interlace_mode.is_interlaced()) + 1;
        let mut cursor = header_start.saturating_add(frame_header_size);
        for pic_idx in 0..num_pictures {
            let is_first = pic_idx == 0;
            let pic_bytes = payload
                .get(cursor..)
                .ok_or(Error::InvalidData("prores: picture truncated"))?;
            let (ph, ph_size): (PictureHeader, u32) = {
                let mut r = vaco_bitstream::BitReader::new(pic_bytes);
                header::parse_picture_header(&mut r)?
            };
            let picture_vh = header::picture_vertical_size(&fh, is_first);
            let height_in_mb = picture_vh.div_ceil(16);
            let width_in_mb = fh.width_in_mb();
            let slice_sizes = header::slice_sizes_in_mb(width_in_mb, ph.log2_desired_slice_size_in_mb);
            let num_slices_per_row = slice_sizes.len();
            let table_count = (height_in_mb as usize).saturating_mul(num_slices_per_row);

            let mut table_cursor = cursor.saturating_add(ph_size as usize);
            let table = {
                let table_bytes = payload
                    .get(table_cursor..)
                    .ok_or(Error::InvalidData("prores: slice table truncated"))?;
                let mut r = vaco_bitstream::BitReader::new(table_bytes);
                header::parse_slice_table(&mut r, table_count, &mut budget)?
            };
            table_cursor = table_cursor.saturating_add(table_count.saturating_mul(2));

            let field = field_map(&fh, is_first);
            let mut slice_cursor = table_cursor;
            let mut table_idx = 0usize;
            for i in 0..height_in_mb {
                let mut mb_col_offset = 0u32;
                for &slice_size in &slice_sizes {
                    let coded_size = *table.get(table_idx).ok_or(Error::InvalidData(
                        "prores: slice table short",
                    ))? as usize;
                    table_idx += 1;
                    let slice_bytes = payload
                        .get(slice_cursor..slice_cursor.saturating_add(coded_size))
                        .ok_or(Error::InvalidData("prores: slice truncated"))?;
                    decode_slice(
                        slice_bytes, &fh, i, mb_col_offset, slice_size, field, &mut idct,
                        &mut *planes, &mut budget,
                    )?;
                    mb_col_offset = mb_col_offset.saturating_add(slice_size);
                    slice_cursor = slice_cursor.saturating_add(coded_size);
                }
            }
            cursor = cursor.saturating_add(ph.picture_size as usize);
        }

        Ok(frame)
    }
}

impl Decoder for ProresDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            self.draining = true;
            return Ok(());
        };
        let payload = packet.payload();
        if payload.len() < 8 {
            return Err(Error::InvalidData("prores: packet too small"));
        }
        let frame_size = be32(payload, 0)? as usize;
        let body = if frame_size <= payload.len() && frame_size >= 8 {
            payload.get(..frame_size).unwrap_or(payload)
        } else {
            payload
        };
        let mut frame = self.decode_frame_payload(body)?;
        frame.pts = packet.pts;
        frame.duration = packet.duration;
        self.pending.push_back(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(if self.draining { Error::Eof } else { Error::NeedMoreInput })
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.draining = false;
    }
}

fn make(limits: Limits) -> Box<dyn Decoder> {
    Box::new(ProresDecoder::new(limits))
}

/// The registry descriptor for `ProRes` decode.
///
/// No `Caps::PATENT_ENCUMBERED`: SS5.1 of the legal register places
/// unmodified `ProRes` *decode* unconditionally in the default distributable
/// build (`†` only marks "decode only, never encode" — a scope note, not a
/// legal-risk flag).
pub const DECODER_PRORES: vaco_codec_core::DecoderDesc = vaco_codec_core::DecoderDesc {
    name: "prores",
    long_name: "Apple ProRes (iCodec Pro)",
    id: vaco_codec_core::CodecId::Prores,
    media_type: MediaType::Video,
    caps: vaco_codec_core::Caps::empty(),
    supported_rates: &[],
    make,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a test that cannot set up is a failed test")]
mod tests {
    use super::*;

    #[test]
    fn descriptor_answers_to_its_own_name() {
        assert_eq!(DECODER_PRORES.name, "prores");
        assert_eq!(DECODER_PRORES.id, vaco_codec_core::CodecId::Prores);
    }

    #[test]
    fn garbage_payload_is_a_clean_error_not_a_panic() {
        let mut dec = ProresDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[0xFFu8; 40]).unwrap();
        assert!(dec.send_packet(Some(&pkt)).is_err());
    }

    #[test]
    fn too_small_payload_is_a_clean_error() {
        let mut dec = ProresDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[0u8; 3]).unwrap();
        assert!(dec.send_packet(Some(&pkt)).is_err());
    }

    /// A legitimately large 4:2:2 frame must fit `Limits::strict`'s frame
    /// budget, not just `Limits::permissive`'s.
    ///
    /// Regression: `decode_frame_payload` used to charge a flat 4 bytes per
    /// pixel — the widest *packed* 8-bit layout, wildly wrong for `ProRes`,
    /// which is always planar 4:2:2/4:4:4 at 10/12 bits.
    /// `yuv422p10le`'s real average is 20 bits (2.5 bytes) per pixel, so the
    /// old flat 4 over-charged by 60%. At 2732x1536 that overshoot
    /// (16.79 MB) crosses `Limits::strict`'s 16 MiB `max_frame_bytes` cap
    /// even though the real 4:2:2 10-bit frame is only 12.6 MB — the exact
    /// false-rejection shape this session's fix addresses.
    #[test]
    fn a_legitimately_large_4_2_2_frame_is_accepted_by_the_frame_budget() {
        let fh = FrameHeader {
            bitstream_version: 0,
            horizontal_size: 2732,
            vertical_size: 1536,
            chroma_format: ChromaFormat::Yuv422,
            interlace_mode: InterlaceMode::Progressive,
            alpha_channel_type: 0,
            luma_quant: header::DEFAULT_QUANT_MATRIX,
            chroma_quant: header::DEFAULT_QUANT_MATRIX,
        };
        let pix_fmt = pix_fmt_for(&fh);
        assert_eq!(pix_fmt, PixFmt::Yuv422p10le);
        let bpp = u32::from(pix_fmt.bits_per_pixel()).div_ceil(8).max(1);
        assert_eq!(bpp, 3, "yuv422p10le averages 20 bits/pixel, not 32");

        let budget = Budget::new(Limits::strict());
        assert!(
            budget.check_frame(2732, 1536, bpp).is_ok(),
            "a real 4:2:2 10-bit frame this size must fit `strict`'s frame budget"
        );
    }


    /// `send_packet(None)` must make `receive_frame` answer `Eof` once
    /// `pending` is drained, not `NeedMoreInput` forever -- see
    /// `vaco-codec-ac3`'s decoder's own `draining` field doc for the full
    /// reasoning (measured against `vaco-sched`'s `ProgressGuard` livelock
    /// watchdog).
    #[test]
    fn draining_answers_eof_once_empty_not_need_more_input_forever() {
        let mut dec = ProresDecoder::new(Limits::permissive());
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)), "empty and not draining yet");
        dec.send_packet(None).unwrap();
        assert!(matches!(dec.receive_frame(), Err(Error::Eof)), "must answer Eof once drained and empty, not NeedMoreInput forever");
    }
}
