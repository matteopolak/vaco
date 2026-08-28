//! Baseline and progressive JPEG encode (ITU-T T.81 Annex E and Annex G).
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209`.
//!
//! Progressive output (`EncodeOptions::progressive`) is spectral-selection
//! only — one interleaved DC scan (`Ss=0,Se=0`) followed by one
//! non-interleaved AC scan per component (`Ss=1,Se=63`) — never successive
//! approximation. That is a deliberate, narrower scope than a real encoder
//! like `cjpeg -progressive` (which also splits the AC band and refines it
//! over several bit planes): it is still a genuine `SOF2` progressive
//! stream any conformant decoder must accept, and reuses the exact same
//! per-coefficient Huffman coding baseline already does, just split across
//! scans instead of combined into one — with none of successive
//! approximation's own decode-side subtlety (see `decode.rs`'s `ac_refine`)
//! on the write side. The encoder always emits the Annex K.3–K.6 default
//! Huffman tables rather than building optimized ones per image; that costs
//! some compression ratio and nothing else, since Annex K's tables are
//! exactly as legal a choice as any other.

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
    /// Emit `SOF2` (spectral-selection-only progressive) instead of `SOF0`
    /// baseline. See the module doc for exactly which scan structure this
    /// produces.
    pub progressive: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: 90,
            restart_interval: 0,
            progressive: false,
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

/// A component's own unpadded block extent — the same `ceil(ceil(width *
/// h/h_max)/8)` formula `decode.rs`'s `real_block_extent` computes, so a
/// non-interleaved progressive AC scan here transmits exactly the blocks
/// the decoder expects one for (never the extra MCU-padding blocks at the
/// right/bottom edge that only the interleaved DC scan touches).
fn real_block_extent(
    width: u32,
    height: u32,
    h: u8,
    v: u8,
    h_max: u32,
    v_max: u32,
) -> (usize, usize) {
    let comp_w = div_ceil_usize(width as usize * usize::from(h), h_max as usize).max(1);
    let comp_h = div_ceil_usize(height as usize * usize::from(v), v_max as usize).max(1);
    (div_ceil_usize(comp_w, 8), div_ceil_usize(comp_h, 8))
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

/// Dequantize one block's raw forward-DCT output (natural order, already
/// rounded to `i32` once by the caller) into quantized coefficients, still
/// in natural order — matching `decode.rs`'s own storage convention, so
/// both scan writers below index into it with the same `ZIGZAG` lookup the
/// decoder uses to go the other way.
fn quantize_natural_block(freq_natural: &[i32; 64], quant: &[u16; 64]) -> [i32; 64] {
    let mut out = [0i32; 64];
    for (nat, slot) in out.iter_mut().enumerate() {
        let coeff = freq_natural.get(nat).copied().unwrap_or(0);
        let q = quant.get(nat).copied().unwrap_or(1).max(1);
        let quantized = f64::from(coeff) / f64::from(q);
        *slot = quantized.round() as i32;
    }
    out
}

/// Write one block's DC coefficient: the Huffman-coded size category of its
/// prediction difference, followed by that many raw magnitude bits.
fn write_dc(
    w: &mut EntropyWriter,
    block_natural: &[i32; 64],
    dc_pred: &mut i32,
    dc_table: &EncodeTable,
) -> Result<()> {
    let dc_val = block_natural.first().copied().unwrap_or(0);
    let diff = dc_val - *dc_pred;
    *dc_pred = dc_val;
    let (dc_size, dc_bits) = category_and_bits(diff);
    write_huffman(w, dc_table, dc_size)?;
    w.put_bits(u32::from(dc_size), dc_bits);
    Ok(())
}

/// Write one block's AC coefficients over zigzag positions `ss..=se`
/// (`1..=63` for baseline and this crate's progressive AC-first scans;
/// `ac_refine`'s successive-approximation counterpart has no writer here —
/// see the module doc) as `(run, size)` symbols plus magnitude bits, ending
/// in `EOB` unless the band's last position is itself nonzero.
fn write_ac_band(
    w: &mut EntropyWriter,
    block_natural: &[i32; 64],
    ac_table: &EncodeTable,
    ss: u8,
    se: u8,
) -> Result<()> {
    let ss = usize::from(ss);
    let se = usize::from(se);
    let mut last_nonzero = ss.saturating_sub(1);
    for k in ss..=se {
        let nat = ZIGZAG.get(k).copied().unwrap_or(0);
        if block_natural.get(nat).copied().unwrap_or(0) != 0 {
            last_nonzero = k;
        }
    }
    let mut run = 0u32;
    let mut k = ss;
    while k <= last_nonzero {
        let nat = ZIGZAG.get(k).copied().unwrap_or(0);
        let v = block_natural.get(nat).copied().unwrap_or(0);
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
    if last_nonzero < se {
        write_huffman(w, ac_table, 0x00)?;
    }
    Ok(())
}

/// One component's quantized coefficients over the full MCU-padded block
/// grid (`decode.rs`'s `CompState` is the read-side mirror of this: same
/// `blocks_w`/`blocks_h`, same natural-order-per-block layout), computed
/// once up front so both the single combined baseline scan and progressive's
/// separate DC/AC scans read from identical numbers.
struct CompCoeffs {
    blocks_w: usize,
    blocks_h: usize,
    coeffs: Vec<i32>,
}

impl CompCoeffs {
    fn block(&self, block_x: usize, block_y: usize) -> Option<&[i32; 64]> {
        if block_x >= self.blocks_w || block_y >= self.blocks_h {
            return None;
        }
        let base = (block_y * self.blocks_w + block_x) * 64;
        self.coeffs
            .get(base..base + 64)
            .and_then(|s| s.try_into().ok())
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one call site; the parameters are exactly the frame's own encode geometry"
)]
fn compute_coeffs(
    frame: &Frame,
    layout: &Layout,
    quant_tables: &[[u16; 64]; 2],
    precision: u8,
    mcus_per_line: usize,
    mcu_rows: usize,
    h_max: u32,
    v_max: u32,
    width: u32,
    height: u32,
) -> Result<arrayvec::ArrayVec<CompCoeffs, 4>> {
    let mut fdct = Fdct8x8::new()?;
    let level = f64::from(1i32 << (precision - 1));
    let mut out = arrayvec::ArrayVec::new();
    for (ci, c) in layout.components.iter().enumerate() {
        let Some(plane) = frame.plane(ci) else {
            continue;
        };
        let blocks_w = mcus_per_line * usize::from(c.h);
        let blocks_h = mcu_rows * usize::from(c.v);
        let comp_w = div_ceil_usize(width as usize * usize::from(c.h), h_max as usize).max(1);
        let comp_h = div_ceil_usize(height as usize * usize::from(c.v), v_max as usize).max(1);
        let quant = quant_tables.get(c.table_set).copied().unwrap_or([1; 64]);
        let mut coeffs = vec![0i32; blocks_w.saturating_mul(blocks_h).saturating_mul(64)];
        for block_y in 0..blocks_h {
            for block_x in 0..blocks_w {
                let mut samples = [0.0f64; 64];
                for row_in_block in 0..8usize {
                    let y = (block_y * 8 + row_in_block).min(comp_h.saturating_sub(1));
                    let row = plane.row(y).unwrap_or(&[]);
                    for col_in_block in 0..8usize {
                        let x = (block_x * 8 + col_in_block).min(comp_w.saturating_sub(1));
                        let sample = read_sample(row, x, precision);
                        if let Some(slot) = samples.get_mut(row_in_block * 8 + col_in_block) {
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
                let block = quantize_natural_block(&freq_i, &quant);
                let base = (block_y * blocks_w + block_x) * 64;
                if let Some(slot) = coeffs.get_mut(base..base + 64) {
                    slot.copy_from_slice(&block);
                }
            }
        }
        if out
            .try_push(CompCoeffs {
                blocks_w,
                blocks_h,
                coeffs,
            })
            .is_err()
        {
            return Err(Error::InvalidData("jpeg: too many components to encode"));
        }
    }
    Ok(out)
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
    write_segment(
        &mut out,
        if options.progressive {
            marker::SOF2
        } else {
            marker::SOF0
        },
        &sof,
    );

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

    let h_max = u32::from(layout.components.iter().map(|c| c.h).max().unwrap_or(1));
    let v_max = u32::from(layout.components.iter().map(|c| c.v).max().unwrap_or(1));
    let mcus_per_line = div_ceil_usize(width as usize, (8 * h_max) as usize).max(1);
    let mcu_rows = div_ceil_usize(height as usize, (8 * v_max) as usize).max(1);

    let coeffs = compute_coeffs(
        frame,
        &layout,
        &quant_tables,
        precision,
        mcus_per_line,
        mcu_rows,
        h_max,
        v_max,
        width,
        height,
    )?;

    if options.progressive {
        write_progressive_scans(
            &mut out,
            &layout,
            &coeffs,
            &dc_tables,
            &ac_tables,
            options.restart_interval,
            mcus_per_line,
            mcu_rows,
            h_max,
            v_max,
            width,
            height,
        )?;
    } else {
        write_baseline_scan(
            &mut out,
            &layout,
            &coeffs,
            &dc_tables,
            &ac_tables,
            options.restart_interval,
            mcus_per_line,
            mcu_rows,
        )?;
    }

    out.extend_from_slice(&[0xFF, marker::EOI]);
    Ok(out)
}

/// Emit a restart marker (flushing the current byte first) when `units_done`
/// lands on a restart boundary. Returns whether it did, so a caller that
/// tracks its own per-scan state (baseline/DC-scan `dc_pred`) knows to reset
/// it too — an AC-only scan has no such state.
fn maybe_write_restart(w: &mut EntropyWriter, units_done: usize, restart_interval: u16) -> bool {
    if restart_interval > 0
        && units_done != 0
        && units_done.is_multiple_of(usize::from(restart_interval))
    {
        w.flush_to_byte();
        let rst = marker::RST0
            + ((units_done
                .checked_div(usize::from(restart_interval))
                .unwrap_or(1)
                - 1)
                % 8) as u8;
        w.raw_marker(&[0xFF, rst]);
        true
    } else {
        false
    }
}

/// One `SOF0` scan covering every component's full coefficient band —
/// `write_dc` then `write_ac_band(1, 63)` per block, interleaved in MCU
/// order, matching how every baseline decoder (including this crate's own)
/// expects a single-scan stream.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site; the parameters are exactly this scan's own geometry"
)]
fn write_baseline_scan(
    out: &mut Vec<u8>,
    layout: &Layout,
    coeffs: &arrayvec::ArrayVec<CompCoeffs, 4>,
    dc_tables: &[EncodeTable; 2],
    ac_tables: &[EncodeTable; 2],
    restart_interval: u16,
    mcus_per_line: usize,
    mcu_rows: usize,
) -> Result<()> {
    let mut sos = vec![layout.components.len() as u8];
    for (i, c) in layout.components.iter().enumerate() {
        sos.push((i + 1) as u8);
        sos.push(((c.table_set as u8) << 4) | (c.table_set as u8));
    }
    sos.extend_from_slice(&[0, 63, 0]);
    write_segment(out, marker::SOS, &sos);

    let mut w = EntropyWriter::new();
    let mut dc_pred = [0i32; 4];
    let mut units_done = 0usize;
    for mcu_row in 0..mcu_rows {
        for mcu_col in 0..mcus_per_line {
            if maybe_write_restart(&mut w, units_done, restart_interval) {
                dc_pred = [0i32; 4];
            }
            for (ci, c) in layout.components.iter().enumerate() {
                let comp = coeffs
                    .get(ci)
                    .ok_or(Error::InvalidData("jpeg: component index out of range"))?;
                let dc_table = dc_tables
                    .get(c.table_set)
                    .ok_or(Error::InvalidData("jpeg: table set out of range"))?;
                let ac_table = ac_tables
                    .get(c.table_set)
                    .ok_or(Error::InvalidData("jpeg: table set out of range"))?;
                for by in 0..usize::from(c.v) {
                    for bx in 0..usize::from(c.h) {
                        let block_x = mcu_col * usize::from(c.h) + bx;
                        let block_y = mcu_row * usize::from(c.v) + by;
                        let block = comp
                            .block(block_x, block_y)
                            .ok_or(Error::InvalidData("jpeg: block index out of range"))?;
                        let pred = dc_pred
                            .get_mut(ci)
                            .ok_or(Error::InvalidData("jpeg: component index out of range"))?;
                        write_dc(&mut w, block, pred, dc_table)?;
                        write_ac_band(&mut w, block, ac_table, 1, 63)?;
                    }
                }
            }
            units_done += 1;
        }
    }
    w.flush_to_byte();
    out.extend_from_slice(&w.finish());
    Ok(())
}

/// This crate's progressive output: one interleaved `SOF2` DC scan
/// (`Ss=0,Se=0`) for every component, followed by one non-interleaved AC
/// scan (`Ss=1,Se=63`) per component — spectral selection only, no
/// successive approximation. See the module doc for why.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site; the parameters are exactly this scan sequence's own geometry"
)]
fn write_progressive_scans(
    out: &mut Vec<u8>,
    layout: &Layout,
    coeffs: &arrayvec::ArrayVec<CompCoeffs, 4>,
    dc_tables: &[EncodeTable; 2],
    ac_tables: &[EncodeTable; 2],
    restart_interval: u16,
    mcus_per_line: usize,
    mcu_rows: usize,
    h_max: u32,
    v_max: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    let mut sos = vec![layout.components.len() as u8];
    for (i, c) in layout.components.iter().enumerate() {
        sos.push((i + 1) as u8);
        sos.push(((c.table_set as u8) << 4) | (c.table_set as u8));
    }
    sos.extend_from_slice(&[0, 0, 0]);
    write_segment(out, marker::SOS, &sos);

    let mut w = EntropyWriter::new();
    let mut dc_pred = [0i32; 4];
    let mut units_done = 0usize;
    for mcu_row in 0..mcu_rows {
        for mcu_col in 0..mcus_per_line {
            if maybe_write_restart(&mut w, units_done, restart_interval) {
                dc_pred = [0i32; 4];
            }
            for (ci, c) in layout.components.iter().enumerate() {
                let comp = coeffs
                    .get(ci)
                    .ok_or(Error::InvalidData("jpeg: component index out of range"))?;
                let dc_table = dc_tables
                    .get(c.table_set)
                    .ok_or(Error::InvalidData("jpeg: table set out of range"))?;
                for by in 0..usize::from(c.v) {
                    for bx in 0..usize::from(c.h) {
                        let block_x = mcu_col * usize::from(c.h) + bx;
                        let block_y = mcu_row * usize::from(c.v) + by;
                        let block = comp
                            .block(block_x, block_y)
                            .ok_or(Error::InvalidData("jpeg: block index out of range"))?;
                        let pred = dc_pred
                            .get_mut(ci)
                            .ok_or(Error::InvalidData("jpeg: component index out of range"))?;
                        write_dc(&mut w, block, pred, dc_table)?;
                    }
                }
            }
            units_done += 1;
        }
    }
    w.flush_to_byte();
    out.extend_from_slice(&w.finish());

    for (ci, c) in layout.components.iter().enumerate() {
        let comp = coeffs
            .get(ci)
            .ok_or(Error::InvalidData("jpeg: component index out of range"))?;
        let ac_table = ac_tables
            .get(c.table_set)
            .ok_or(Error::InvalidData("jpeg: table set out of range"))?;
        let (bw, bh) = real_block_extent(width, height, c.h, c.v, h_max, v_max);

        // `Td` (the DC-table nibble) is irrelevant for an AC-only scan — the
        // decoder never reads it when `Ss != 0` — so `Ta` alone occupies the
        // low nibble here, matching `header::parse_sos`'s `td = byte >> 4,
        // ta = byte & 0xF` split.
        let sos = [1u8, (ci + 1) as u8, c.table_set as u8, 1, 63, 0];
        write_segment(out, marker::SOS, &sos);

        let mut w = EntropyWriter::new();
        let mut units_done = 0usize;
        for block_y in 0..bh {
            for block_x in 0..bw {
                let _ = maybe_write_restart(&mut w, units_done, restart_interval);
                let block = comp
                    .block(block_x, block_y)
                    .ok_or(Error::InvalidData("jpeg: block index out of range"))?;
                write_ac_band(&mut w, block, ac_table, 1, 63)?;
                units_done += 1;
            }
        }
        w.flush_to_byte();
        out.extend_from_slice(&w.finish());
    }
    Ok(())
}
