//! Baseline JPEG encode (ITU-T T.81 Annex E).
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209`.
//!
//! Progressive encode is not implemented here — see the crate docs for the
//! scope this crate currently covers. The encoder always emits the Annex
//! K.3–K.6 default Huffman tables rather than building optimized ones per
//! image; that costs some compression ratio and nothing else, since Annex
//! K's tables are exactly as legal a choice as any other.

use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_pixfmt::{PixFmt, PixFmtFlags};

use crate::bits::EntropyWriter;
use crate::huffman::EncodeTable;
use crate::idct::Fdct8x8;
use crate::marker;
use crate::tables::{self, ZIGZAG};

/// What an encode call can be tuned with.
#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    /// `1..=100`, the IJG convention: higher is better quality and larger
    /// output. Values outside the range are clamped.
    pub quality: u8,
    /// MCUs (or, for a non-interleaved single-component encode, blocks)
    /// between restart markers. `0` disables restarts.
    pub restart_interval: u16,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: 90,
            restart_interval: 0,
        }
    }
}

/// One component's sampling factors and which of the two quant/Huffman
/// table pairs (0 = luma, 1 = chroma) it uses.
#[derive(Debug, Clone, Copy)]
struct CompLayout {
    h: u8,
    v: u8,
    table_set: usize,
}

struct Layout {
    precision: u8,
    components: arrayvec::ArrayVec<CompLayout, 4>,
}

fn layout_for(fmt: PixFmt) -> Result<Layout> {
    let depth = fmt.max_depth();
    let precision = if depth <= 8 {
        8
    } else if depth <= 12 {
        12
    } else {
        return Err(Error::Unsupported(
            "jpeg: only 8-bit and 12-bit samples are encodable",
        ));
    };
    let mut components = arrayvec::ArrayVec::new();
    match fmt.component_count() {
        1 => {
            let _ = components.try_push(CompLayout {
                h: 1,
                v: 1,
                table_set: 0,
            });
        }
        3 if !fmt.has(PixFmtFlags::RGB) => {
            let (log2w, log2h) = fmt.log2_chroma();
            let (h, v) = (1u8 << log2w, 1u8 << log2h);
            let _ = components.try_push(CompLayout { h, v, table_set: 0 });
            let _ = components.try_push(CompLayout {
                h: 1,
                v: 1,
                table_set: 1,
            });
            let _ = components.try_push(CompLayout {
                h: 1,
                v: 1,
                table_set: 1,
            });
        }
        _ => {
            return Err(Error::Unsupported(
                "jpeg: only grayscale and planar YCbCr (not RGB) frames are encodable; \
                 convert with a colour-space filter first",
            ));
        }
    }
    Ok(Layout {
        precision,
        components,
    })
}

/// The IJG quality-scaling formula: a widely used, purely mechanical
/// remapping of `quality` (`1..=100`) onto a percentage scale factor, then
/// applied to a base table. Not part of T.81 itself — the standard names no
/// particular scaling rule — so this is one reasonable choice among many
/// conformant ones, not a transcription of anything.
fn scale_quant_table(base: &[u16; 64], quality: u8, precision: u8) -> [u16; 64] {
    let quality = quality.clamp(1, 100);
    let scale = if quality < 50 {
        5000u32.checked_div(u32::from(quality)).unwrap_or(5000)
    } else {
        200 - 2 * u32::from(quality)
    };
    let depth_scale = if precision > 8 { 16u32 } else { 1 };
    let max_value = if precision > 8 { 32767u32 } else { 255 };
    let mut out = [0u16; 64];
    for (dst, &b) in out.iter_mut().zip(base.iter()) {
        let scaled = (u32::from(b) * depth_scale * scale + 50)
            .checked_div(100)
            .unwrap_or(0);
        *dst = scaled.clamp(1, max_value) as u16;
    }
    out
}

fn div_ceil_usize(a: usize, b: usize) -> usize {
    if b == 0 { 0 } else { a.div_ceil(b) }
}

fn read_sample(row: &[u8], x: usize, precision: u8) -> i32 {
    if precision <= 8 {
        i32::from(row.get(x).copied().unwrap_or(0))
    } else {
        let hi = row.get(x * 2).copied().unwrap_or(0);
        let lo = row.get(x * 2 + 1).copied().unwrap_or(0);
        i32::from(u16::from_le_bytes([hi, lo]))
    }
}

fn category_and_bits(value: i32) -> (u8, u32) {
    if value == 0 {
        return (0, 0);
    }
    let mag = value.unsigned_abs();
    let size = 32 - mag.leading_zeros();
    let mask = (1u32 << size) - 1;
    let bits = if value > 0 { mag } else { mask ^ mag };
    (size as u8, bits)
}

fn write_huffman(w: &mut EntropyWriter, table: &EncodeTable, symbol: u8) -> Result<()> {
    let (len, code) = table.code_for(symbol).ok_or(Error::InvalidData(
        "jpeg: encode table has no code for a required symbol",
    ))?;
    w.put_bits(u32::from(len), u32::from(code));
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "one call site; the parameters are exactly the per-block encode state"
)]
fn encode_block(
    w: &mut EntropyWriter,
    freq_natural: &[i32; 64],
    quant: &[u16; 64],
    dc_pred: &mut i32,
    dc_table: &EncodeTable,
    ac_table: &EncodeTable,
) -> Result<()> {
    let mut zz = [0i32; 64];
    for (k, &nat) in ZIGZAG.iter().enumerate() {
        let coeff = freq_natural.get(nat).copied().unwrap_or(0);
        let q = quant.get(nat).copied().unwrap_or(1).max(1);
        let quantized = f64::from(coeff) / f64::from(q);
        if let Some(slot) = zz.get_mut(k) {
            *slot = quantized.round() as i32;
        }
    }

    let dc_val = zz.first().copied().unwrap_or(0);
    let diff = dc_val - *dc_pred;
    *dc_pred = dc_val;
    let (dc_size, dc_bits) = category_and_bits(diff);
    write_huffman(w, dc_table, dc_size)?;
    w.put_bits(u32::from(dc_size), dc_bits);

    let mut run = 0u32;
    let mut last_nonzero = 0usize;
    for (k, &v) in zz.iter().enumerate().skip(1) {
        if v != 0 {
            last_nonzero = k;
        }
    }
    let mut k = 1usize;
    while k <= last_nonzero {
        let v = zz.get(k).copied().unwrap_or(0);
        if v == 0 {
            run += 1;
            if run == 16 {
                write_huffman(w, ac_table, 0xF0)?;
                run = 0;
            }
            k += 1;
            continue;
        }
        let (size, bits) = category_and_bits(v);
        let rs = ((run as u8) << 4) | size;
        write_huffman(w, ac_table, rs)?;
        w.put_bits(u32::from(size), bits);
        run = 0;
        k += 1;
    }
    if last_nonzero < 63 {
        write_huffman(w, ac_table, 0x00)?;
    }
    Ok(())
}

fn write_segment(out: &mut Vec<u8>, marker_byte: u8, payload: &[u8]) {
    out.push(0xFF);
    out.push(marker_byte);
    let len = (payload.len() + 2) as u16;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
}

/// Encode one [`Frame`] as a complete baseline JPEG stream.
///
/// Takes no [`vaco_limits::Budget`]: the output size is a function of the
/// input frame, which was itself already allocated under a budget, so there
/// is nothing attacker-controlled to bound here — matching this crate's
/// other pure `encode` functions. The caller wraps the result in a
/// [`vaco_packet::Packet`] under its own budget, same as
/// `vaco_codec_qoi::encode`.
///
/// # Errors
/// [`Error::Unsupported`] for anything other than grayscale or planar
/// YCbCr 8-/12-bit input (see the crate docs on why colour conversion is
/// not this crate's job).
pub fn encode(frame: &Frame, options: &EncodeOptions) -> Result<Vec<u8>> {
    let vaco_frame::FrameData::Video {
        format,
        width,
        height,
        ..
    } = &frame.data
    else {
        return Err(Error::Unsupported("jpeg: only video frames are encodable"));
    };
    let (width, height) = (*width, *height);
    let layout = layout_for(*format)?;
    let precision = layout.precision;

    let luma_quant = scale_quant_table(&tables::STD_LUMA_QUANT, options.quality, precision);
    let chroma_quant = scale_quant_table(&tables::STD_CHROMA_QUANT, options.quality, precision);
    let quant_tables = [luma_quant, chroma_quant];

    let dc_tables = [
        EncodeTable::from_spec(&tables::STD_DC_LUMA),
        EncodeTable::from_spec(&tables::STD_DC_CHROMA),
    ];
    let ac_tables = [
        EncodeTable::from_spec(&tables::STD_AC_LUMA),
        EncodeTable::from_spec(&tables::STD_AC_CHROMA),
    ];

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&[0xFF, marker::SOI]);

    // JFIF APP0: version 1.1, no density information asserted (units=0,
    // 1x1 "aspect ratio only" density), matching the common encoder default
    // this crate's own `vaco-parse-image` fixture was captured from.
    write_segment(
        &mut out,
        marker::APP0,
        &[b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0],
    );

    let precision_16 = quant_tables.iter().any(|t| t.iter().any(|&v| v > 255));
    for (idx, table) in quant_tables
        .iter()
        .enumerate()
        .take(if layout.components.len() > 1 { 2 } else { 1 })
    {
        let mut payload = Vec::new();
        payload.push((u8::from(precision_16) << 4) | (idx as u8));
        for &nat in &ZIGZAG {
            let v = table.get(nat).copied().unwrap_or(1);
            if precision_16 {
                payload.extend_from_slice(&v.to_be_bytes());
            } else {
                payload.push(v as u8);
            }
        }
        write_segment(&mut out, marker::DQT, &payload);
    }

    let mut sof = Vec::new();
    sof.push(precision);
    sof.extend_from_slice(&(height as u16).to_be_bytes());
    sof.extend_from_slice(&(width as u16).to_be_bytes());
    sof.push(layout.components.len() as u8);
    for (i, c) in layout.components.iter().enumerate() {
        sof.push((i + 1) as u8);
        sof.push((c.h << 4) | c.v);
        sof.push(c.table_set as u8);
    }
    write_segment(&mut out, marker::SOF0, &sof);

    let specs: &[(u8, &tables::HuffSpec)] = if layout.components.len() > 1 {
        &[
            (0, &tables::STD_DC_LUMA),
            (0, &tables::STD_AC_LUMA),
            (1, &tables::STD_DC_CHROMA),
            (1, &tables::STD_AC_CHROMA),
        ]
    } else {
        &[(0, &tables::STD_DC_LUMA), (0, &tables::STD_AC_LUMA)]
    };
    for (i, (idx, spec)) in specs.iter().enumerate() {
        let class = u8::from(i % 2 == 1);
        let mut payload = vec![(class << 4) | idx];
        payload.extend_from_slice(&spec.counts);
        payload.extend_from_slice(spec.values);
        write_segment(&mut out, marker::DHT, &payload);
    }

    if options.restart_interval > 0 {
        write_segment(
            &mut out,
            marker::DRI,
            &options.restart_interval.to_be_bytes(),
        );
    }

    let mut sos = vec![layout.components.len() as u8];
    for (i, c) in layout.components.iter().enumerate() {
        sos.push((i + 1) as u8);
        sos.push(((c.table_set as u8) << 4) | (c.table_set as u8));
    }
    sos.extend_from_slice(&[0, 63, 0]);
    write_segment(&mut out, marker::SOS, &sos);

    let h_max = u32::from(layout.components.iter().map(|c| c.h).max().unwrap_or(1));
    let v_max = u32::from(layout.components.iter().map(|c| c.v).max().unwrap_or(1));
    let mcus_per_line = div_ceil_usize(width as usize, (8 * h_max) as usize).max(1);
    let mcu_rows = div_ceil_usize(height as usize, (8 * v_max) as usize).max(1);

    let mut w = EntropyWriter::new();
    let mut dc_pred = [0i32; 4];
    let mut fdct = Fdct8x8::new()?;
    let mut units_done = 0usize;
    let total_units = mcus_per_line * mcu_rows;

    for mcu_row in 0..mcu_rows {
        for mcu_col in 0..mcus_per_line {
            if options.restart_interval > 0
                && units_done != 0
                && units_done.is_multiple_of(usize::from(options.restart_interval))
            {
                w.flush_to_byte();
                let rst = marker::RST0
                    + ((units_done
                        .checked_div(usize::from(options.restart_interval))
                        .unwrap_or(1)
                        - 1)
                        % 8) as u8;
                w.raw_marker(&[0xFF, rst]);
                dc_pred = [0i32; 4];
            }
            for (ci, c) in layout.components.iter().enumerate() {
                let Some(plane) = frame.plane(ci) else {
                    continue;
                };
                let comp_w =
                    div_ceil_usize(width as usize * usize::from(c.h), h_max as usize).max(1);
                let comp_h =
                    div_ceil_usize(height as usize * usize::from(c.v), v_max as usize).max(1);
                for by in 0..usize::from(c.v) {
                    for bx in 0..usize::from(c.h) {
                        let block_x = mcu_col * usize::from(c.h) + bx;
                        let block_y = mcu_row * usize::from(c.v) + by;
                        let mut samples = [0.0f64; 64];
                        let level = f64::from(1i32 << (precision - 1));
                        for row_in_block in 0..8usize {
                            let y = (block_y * 8 + row_in_block).min(comp_h.saturating_sub(1));
                            let row = plane.row(y).unwrap_or(&[]);
                            for col_in_block in 0..8usize {
                                let x = (block_x * 8 + col_in_block).min(comp_w.saturating_sub(1));
                                let sample = read_sample(row, x, precision);
                                if let Some(slot) = samples.get_mut(row_in_block * 8 + col_in_block)
                                {
                                    *slot = f64::from(sample) - level;
                                }
                            }
                        }
                        let mut freq = [0.0f64; 64];
                        fdct.apply(&samples, &mut freq);
                        let mut freq_i = [0i32; 64];
                        for (dst, &v) in freq_i.iter_mut().zip(freq.iter()) {
                            *dst = v.round() as i32;
                        }
                        let quant = quant_tables.get(c.table_set).copied().unwrap_or([1; 64]);
                        let pred = dc_pred
                            .get_mut(ci)
                            .ok_or(Error::InvalidData("jpeg: component index out of range"))?;
                        encode_block(
                            &mut w,
                            &freq_i,
                            &quant,
                            pred,
                            dc_tables
                                .get(c.table_set)
                                .ok_or(Error::InvalidData("jpeg: table set out of range"))?,
                            ac_tables
                                .get(c.table_set)
                                .ok_or(Error::InvalidData("jpeg: table set out of range"))?,
                        )?;
                    }
                }
            }
            units_done += 1;
        }
    }
    let _ = total_units;
    w.flush_to_byte();
    out.extend_from_slice(&w.finish());
    out.extend_from_slice(&[0xFF, marker::EOI]);
    Ok(out)
}
