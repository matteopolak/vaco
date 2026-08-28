//! Baseline and progressive decode (ITU-T T.81 Annex E and Annex G).
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209`.
//!
//! # One engine for both
//!
//! A baseline scan (`Ss=0, Se=63, Ah=Al=0`, one Huffman-coded pass) is a
//! degenerate case of progressive spectral selection with successive
//! approximation: its "no more nonzero coefficients" symbol is exactly
//! progressive's `EOBn` with a run of zero, and its DC/AC handling is
//! exactly progressive's "first" scans with `Al=0`. So this module has one
//! coefficient-accumulation engine ([`decode_scan`] and what it calls), fed
//! by every scan a stream contains, and one finishing pass
//! ([`finish_frame`]) that dequantizes and inverse-transforms every block —
//! baseline reaches it after its one scan, progressive after its last one.

use arrayvec::ArrayVec;
use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::bits::{EntropyReader, extend};
use crate::header::{self, ComponentSpec, FrameHeader, MAX_COMPONENTS, QuantTable, ScanHeader};
use crate::huffman::DecodeTable;
use crate::idct::SpecExactIdct;
use crate::marker;
use crate::tables::ZIGZAG;

/// Per-component decode state: the coefficient store, sized to the
/// MCU-padded block grid every interleaved scan iterates.
struct CompState {
    blocks_w: usize,
    blocks_h: usize,
    coeffs: Vec<i32>,
    quant_index: u8,
}

fn div_ceil_usize(a: usize, b: usize) -> usize {
    if b == 0 { 0 } else { a.div_ceil(b) }
}

/// The unpadded block extent a non-interleaved scan over `comp` iterates:
/// `ceil(ceil(width * h / h_max) / 8)`, which can be smaller than the
/// MCU-padded grid at the right/bottom edge.
fn real_block_extent(frame: &FrameHeader, comp: ComponentSpec) -> (usize, usize) {
    let h_max = frame.h_max().max(1) as usize;
    let v_max = frame.v_max().max(1) as usize;
    let comp_w = div_ceil_usize(frame.width as usize * comp.h as usize, h_max);
    let comp_h = div_ceil_usize(frame.height as usize * comp.v as usize, v_max);
    (
        div_ceil_usize(comp_w.max(1), 8),
        div_ceil_usize(comp_h.max(1), 8),
    )
}

/// Everything that persists across markers while walking a stream once.
struct DecodeState {
    frame: Option<FrameHeader>,
    quant: [QuantTable; 4],
    dc_huff: [Option<DecodeTable>; 4],
    ac_huff: [Option<DecodeTable>; 4],
    /// The Annex K.3/K.4 (DC) and K.5/K.6 (AC) default tables, `[luma,
    /// chroma]`, used when a scan selects a table index no `DHT` ever
    /// defined. Some Motion JPEG streams (informally "MJPEG-A/B", carried
    /// over from the QuickTime/AVI world) omit `DQT`/`DHT` per frame
    /// entirely and rely on these — the same tables a still-image encoder
    /// would otherwise have to spell out itself.
    default_dc: [DecodeTable; 2],
    default_ac: [DecodeTable; 2],
    restart_interval: u16,
    jfif: Option<header::JfifInfo>,
    adobe_transform: Option<u8>,
    comps: ArrayVec<CompState, MAX_COMPONENTS>,
    mcus_per_line: usize,
    mcu_rows: usize,
    /// Set when a scan's entropy data ran out without ever reaching a
    /// marker — a truncated file. Reported on the decoded [`Frame`] via
    /// [`vaco_frame::FrameFlags::CORRUPT`] rather than failing the whole
    /// decode outright: a partial image is still useful.
    truncated: bool,
}

impl DecodeState {
    fn new() -> Self {
        Self {
            frame: None,
            quant: [QuantTable::default(); 4],
            dc_huff: [None, None, None, None],
            ac_huff: [None, None, None, None],
            default_dc: [
                DecodeTable::from_spec(&crate::tables::STD_DC_LUMA),
                DecodeTable::from_spec(&crate::tables::STD_DC_CHROMA),
            ],
            default_ac: [
                DecodeTable::from_spec(&crate::tables::STD_AC_LUMA),
                DecodeTable::from_spec(&crate::tables::STD_AC_CHROMA),
            ],
            restart_interval: 0,
            jfif: None,
            adobe_transform: None,
            comps: ArrayVec::new(),
            mcus_per_line: 0,
            mcu_rows: 0,
            truncated: false,
        }
    }

    fn start_frame(&mut self, fh: FrameHeader, budget: &mut Budget) -> Result<()> {
        let h_max = fh.h_max() as usize;
        let v_max = fh.v_max() as usize;
        self.mcus_per_line = div_ceil_usize(fh.width as usize, 8 * h_max.max(1));
        self.mcu_rows = div_ceil_usize(fh.height as usize, 8 * v_max.max(1));
        self.comps.clear();
        for c in &fh.components {
            let blocks_w = self.mcus_per_line.saturating_mul(usize::from(c.h));
            let blocks_h = self.mcu_rows.saturating_mul(usize::from(c.v));
            let n = blocks_w
                .checked_mul(blocks_h)
                .and_then(|x| x.checked_mul(64))
                .ok_or(Error::InvalidData("jpeg: component block grid overflows"))?;
            let coeffs: Vec<i32> = budget.alloc(n)?;
            if self
                .comps
                .try_push(CompState {
                    blocks_w,
                    blocks_h,
                    coeffs,
                    quant_index: c.tq,
                })
                .is_err()
            {
                return Err(Error::InvalidData("jpeg: too many SOF components"));
            }
        }
        self.frame = Some(fh);
        Ok(())
    }
}

/// The Huffman tables a scan can select, `DHT`-defined or Annex K default.
struct Tables<'a> {
    dc_huff: &'a [Option<DecodeTable>; 4],
    ac_huff: &'a [Option<DecodeTable>; 4],
    default_dc: &'a [DecodeTable; 2],
    default_ac: &'a [DecodeTable; 2],
}

impl<'a> Tables<'a> {
    fn dc(&self, td: u8) -> Option<&'a DecodeTable> {
        self.dc_huff
            .get(usize::from(td))
            .and_then(|t| t.as_ref())
            .or_else(|| self.default_dc.get(usize::from(td) % 2))
    }

    fn ac(&self, ta: u8) -> Option<&'a DecodeTable> {
        self.ac_huff
            .get(usize::from(ta))
            .and_then(|t| t.as_ref())
            .or_else(|| self.default_ac.get(usize::from(ta) % 2))
    }
}

/// Apply one correction bit to `block[nat]`, but only if it is already
/// nonzero: a coefficient that is still zero costs no bit at all here,
/// which is the entire reason run-length coding of the zero ones pays off.
/// Reading unconditionally (checking zero-ness only after consuming a bit)
/// desynchronises the whole rest of the entropy-coded segment the moment a
/// scan visits more than a couple of already-zero positions in its `EOBn`
/// correction sweep.
fn apply_correction(er: &mut EntropyReader<'_>, block: &mut [i32], nat: usize, p1: i32, m1: i32) {
    let Some(slot) = block.get_mut(nat) else {
        return;
    };
    if *slot == 0 {
        return;
    }
    if er.get_bit() == 1 {
        *slot = if *slot > 0 {
            slot.saturating_add(p1)
        } else {
            slot.saturating_add(m1)
        };
    }
}

/// Progressive/baseline "first" AC pass (`Ah == 0`): run-length decode over
/// `ss..=se`, with `EOBn` able to span future blocks via `eobrun`. A
/// baseline scan (`ss=1, se=63` after the DC coefficient) is this function
/// with `al=0` and an `eobrun` that can never exceed zero, since a plain
/// `EOB` (`R=0`) sets `eobrun` to exactly zero.
fn ac_first(
    er: &mut EntropyReader<'_>,
    table: &DecodeTable,
    block: &mut [i32],
    ss: u8,
    se: u8,
    al: u8,
    eobrun: &mut u32,
) -> Result<()> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let se = u32::from(se);
    let mut k = u32::from(ss);
    while k <= se {
        let rs = table.decode(er)?;
        let run = u32::from(rs >> 4);
        let size = rs & 0x0F;
        if size == 0 {
            if run < 15 {
                *eobrun = (1u32 << run) - 1;
                if run > 0 {
                    *eobrun += er.get_bits(run);
                }
                break;
            }
            k += 16;
            continue;
        }
        k += run;
        if k > se {
            break;
        }
        let bits = er.get_bits(u32::from(size));
        let value = extend(bits.cast_signed(), u32::from(size)) << al;
        if let Some(&nat) = ZIGZAG.get(k as usize)
            && let Some(slot) = block.get_mut(nat)
        {
            *slot = value;
        }
        k += 1;
    }
    Ok(())
}

/// Progressive AC successive-approximation refinement (`Ah != 0`, Annex
/// G.1.2.3): every already-nonzero coefficient in the band gets a one-bit
/// correction each time it is visited; a new nonzero coefficient can only
/// be introduced with magnitude `1 << al`, sign given by one raw bit.
fn ac_refine(
    er: &mut EntropyReader<'_>,
    table: &DecodeTable,
    block: &mut [i32],
    ss: u8,
    se: u8,
    al: u8,
    eobrun: &mut u32,
) -> Result<()> {
    let p1 = 1i32 << al;
    let m1 = -p1;
    let se = u32::from(se);

    if *eobrun > 0 {
        for k in u32::from(ss)..=se {
            if let Some(&nat) = ZIGZAG.get(k as usize) {
                apply_correction(er, block, nat, p1, m1);
            }
        }
        *eobrun -= 1;
        return Ok(());
    }

    let mut k = u32::from(ss);
    while k <= se {
        let rs = table.decode(er)?;
        let run_field = u32::from(rs >> 4);
        let size = rs & 0x0F;

        if size == 0 && run_field < 15 {
            *eobrun = (1u32 << run_field) - 1;
            if run_field > 0 {
                *eobrun += er.get_bits(run_field);
            }
            while k <= se {
                if let Some(&nat) = ZIGZAG.get(k as usize) {
                    apply_correction(er, block, nat, p1, m1);
                }
                k += 1;
            }
            break;
        }

        let mut run = if size == 0 { 16 } else { run_field };
        let new_value = if size == 0 {
            None
        } else {
            Some(if er.get_bit() == 1 { p1 } else { m1 })
        };

        while k <= se {
            let nat = ZIGZAG.get(k as usize).copied().unwrap_or(0);
            let existing = block.get(nat).copied().unwrap_or(0);
            if existing == 0 {
                if run == 0 {
                    break;
                }
                run -= 1;
                k += 1;
            } else {
                apply_correction(er, block, nat, p1, m1);
                k += 1;
            }
        }
        if let Some(v) = new_value
            && k <= se
        {
            let nat = ZIGZAG.get(k as usize).copied().unwrap_or(0);
            if let Some(slot) = block.get_mut(nat) {
                *slot = v;
            }
            k += 1;
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "one call site per block; splitting would add indirection without reducing the parameters that must flow through it"
)]
fn decode_one_block(
    er: &mut EntropyReader<'_>,
    scan: &ScanHeader,
    sc_index: usize,
    comps: &mut ArrayVec<CompState, MAX_COMPONENTS>,
    tables: &Tables<'_>,
    dc_pred: &mut [i32; MAX_COMPONENTS],
    eobrun: &mut u32,
    block_x: usize,
    block_y: usize,
) -> Result<()> {
    let Some(sc) = scan.components.get(sc_index) else {
        return Ok(());
    };
    let (td, ta, comp_idx) = (sc.td, sc.ta, sc.component_index);
    let Some(comp) = comps.get_mut(comp_idx) else {
        return Ok(());
    };
    if block_x >= comp.blocks_w || block_y >= comp.blocks_h {
        return Ok(());
    }
    let base = (block_y * comp.blocks_w + block_x) * 64;
    let Some(block) = comp.coeffs.get_mut(base..base + 64) else {
        return Ok(());
    };

    if scan.ss == 0 {
        if scan.ah == 0 {
            let table = tables.dc(td).ok_or(Error::InvalidData(
                "jpeg: DC scan with no Huffman table selected",
            ))?;
            let size = table.decode(er)?;
            let bits = er.get_bits(u32::from(size));
            let diff = extend(bits.cast_signed(), u32::from(size));
            let pred = dc_pred
                .get_mut(comp_idx)
                .ok_or(Error::InvalidData("jpeg: component index out of range"))?;
            *pred = pred.wrapping_add(diff);
            if let Some(dc) = block.first_mut() {
                *dc = pred.wrapping_shl(u32::from(scan.al));
            }
        } else if er.get_bit() == 1
            && let Some(dc) = block.first_mut()
        {
            *dc |= 1i32.wrapping_shl(u32::from(scan.al));
        }
    }
    if scan.se == 0 {
        return Ok(());
    }
    let ac_start = scan.ss.max(1);
    let table = tables.ac(ta).ok_or(Error::InvalidData(
        "jpeg: AC scan with no Huffman table selected",
    ))?;
    if scan.ah == 0 {
        ac_first(er, table, block, ac_start, scan.se, scan.al, eobrun)?;
    } else {
        ac_refine(er, table, block, ac_start, scan.se, scan.al, eobrun)?;
    }
    Ok(())
}

/// Decode one scan's entropy-coded data, handling restart intervals.
/// Returns the byte offset to resume marker scanning from.
fn decode_scan(
    data: &[u8],
    start: usize,
    scan: &ScanHeader,
    state: &mut DecodeState,
    budget: &mut Budget,
) -> Result<(usize, bool)> {
    let frame = state
        .frame
        .as_ref()
        .ok_or(Error::InvalidData("jpeg: SOS before SOF"))?;
    let interleaved = scan.components.len() > 1;

    let (units_per_row, total_units) = if interleaved {
        (
            state.mcus_per_line,
            state.mcus_per_line.saturating_mul(state.mcu_rows),
        )
    } else {
        let sc = scan
            .components
            .first()
            .ok_or(Error::InvalidData("jpeg: SOS with no components"))?;
        let comp_spec = frame
            .components
            .get(sc.component_index)
            .ok_or(Error::InvalidData(
                "jpeg: scan component index out of range",
            ))?;
        let (bw, bh) = real_block_extent(frame, *comp_spec);
        (bw.max(1), bw.saturating_mul(bh))
    };

    let mut er = EntropyReader::new(data, start);
    let mut dc_pred = [0i32; MAX_COMPONENTS];
    let mut eobrun = 0u32;
    // Borrowed as individual fields, disjoint from `state.comps`, so the
    // block loop below can hold both a `Tables` borrow and `&mut
    // state.comps` at once.
    let tables = Tables {
        dc_huff: &state.dc_huff,
        ac_huff: &state.ac_huff,
        default_dc: &state.default_dc,
        default_ac: &state.default_ac,
    };

    let mut unit_index = 0usize;
    while unit_index < total_units {
        if state.restart_interval != 0
            && unit_index != 0
            && unit_index.is_multiple_of(usize::from(state.restart_interval))
        {
            let pos = er.pos();
            if data.get(pos) == Some(&0xFF)
                && matches!(data.get(pos + 1), Some(&b) if (marker::RST0..=marker::RST7).contains(&b))
            {
                er = EntropyReader::new(data, pos + 2);
            }
            dc_pred = [0i32; MAX_COMPONENTS];
            eobrun = 0;
        }
        budget.consume_fuel(1)?;

        if interleaved {
            let mcu_row = unit_index.checked_div(units_per_row.max(1)).unwrap_or(0);
            let mcu_col = unit_index % units_per_row.max(1);
            for sc_index in 0..scan.components.len() {
                let Some(sc) = scan.components.get(sc_index) else {
                    continue;
                };
                let Some(comp_spec) = frame.components.get(sc.component_index) else {
                    continue;
                };
                let (h, v) = (usize::from(comp_spec.h), usize::from(comp_spec.v));
                for by in 0..v {
                    for bx in 0..h {
                        let block_x = mcu_col * h + bx;
                        let block_y = mcu_row * v + by;
                        decode_one_block(
                            &mut er,
                            scan,
                            sc_index,
                            &mut state.comps,
                            &tables,
                            &mut dc_pred,
                            &mut eobrun,
                            block_x,
                            block_y,
                        )?;
                    }
                }
            }
        } else {
            let block_x = unit_index % units_per_row.max(1);
            let block_y = unit_index.checked_div(units_per_row.max(1)).unwrap_or(0);
            decode_one_block(
                &mut er,
                scan,
                0,
                &mut state.comps,
                &tables,
                &mut dc_pred,
                &mut eobrun,
                block_x,
                block_y,
            )?;
        }
        unit_index += 1;
    }

    Ok((er.pos(), er.marker().is_some()))
}

/// The subsampling patterns this crate's decoder maps directly onto a
/// [`PixFmt`]'s own chroma decimation: the four the JFIF/EXIF world
/// actually produces, plus grayscale. Anything else (arbitrary sampling
/// factors) is rejected with [`Error::Unsupported`] rather than resampled,
/// since `PixFmt` has no model for a non-power-of-two chroma ratio.
fn pixel_format(frame: &FrameHeader, precision: u8) -> Result<PixFmt> {
    let suffix = if precision > 8 {
        format!("{precision}")
    } else {
        String::new()
    };
    let name = match frame.components.as_slice() {
        [_] => format!("gray{suffix}"),
        [y, cb, _cr] if cb.h > 0 && cb.v > 0 => {
            let h_ratio = y.h.checked_div(cb.h).unwrap_or(0);
            let v_ratio = y.v.checked_div(cb.v).unwrap_or(0);
            let base = match (h_ratio, v_ratio) {
                (2, 2) => "420p",
                (2, 1) => "422p",
                (1, 1) => "444p",
                (1, 2) => "440p",
                _ => {
                    return Err(Error::Unsupported(
                        "jpeg: chroma sampling factors outside the 4:4:4/4:2:2/4:2:0/4:4:0 \
                         family have no matching PixFmt",
                    ));
                }
            };
            let family = if precision <= 8 { "yuvj" } else { "yuv" };
            format!("{family}{base}{suffix}")
        }
        _ => {
            return Err(Error::Unsupported(
                "jpeg: this component layout is not decodable",
            ));
        }
    };
    PixFmt::from_name(&name).map_err(|_| {
        Error::Unsupported("jpeg: no PixFmt matches this precision/subsampling combination")
    })
}

fn write_sample(row: &mut [u8], x: usize, precision: u8, value: i32) {
    if precision <= 8 {
        if let Some(dst) = row.get_mut(x) {
            *dst = value.clamp(0, 255) as u8;
        }
    } else {
        let v = (value.clamp(0, 0xFFFF)) as u16;
        let bytes = v.to_le_bytes();
        if let Some(dst) = row.get_mut(x * 2..x * 2 + 2) {
            dst.copy_from_slice(&bytes);
        }
    }
}

/// Dequantize and inverse-transform every block of every component, and
/// write the result into a freshly allocated [`Frame`].
fn finish_frame(state: &DecodeState, budget: &mut Budget) -> Result<Frame> {
    let frame_header = state
        .frame
        .as_ref()
        .ok_or(Error::InvalidData("jpeg: no SOF was ever seen"))?;
    if frame_header.components.len() != state.comps.len() {
        return Err(Error::InvalidData("jpeg: component count mismatch"));
    }
    let precision = if frame_header.precision == 12 { 12 } else { 8 };
    let pix_fmt = pixel_format(frame_header, precision)?;

    let mut out = Frame::alloc_video(
        budget,
        pix_fmt,
        u32::from(frame_header.width),
        u32::from(frame_header.height),
    )?;
    out.flags |= FrameFlags::KEY;
    if state.truncated {
        out.flags |= FrameFlags::CORRUPT;
    }
    // JFIF defines `density_unit == 0` as meaning the X/Y density fields
    // carry the pixel aspect ratio directly rather than a physical dot
    // density; for the other two units (inch, centimetre) the ratio between
    // them is still exactly the aspect ratio, since both densities share
    // the same unit. Version is checked because a handful of encoders reuse
    // the `JFIF\0` tag for a non-JFIF-1.x payload; 1.0-1.2 are what this
    // reads.
    if let Some(jfif) = state.jfif
        && jfif.version.0 == 1
        && jfif.density_unit <= 2
        && jfif.x_density > 0
        && jfif.y_density > 0
    {
        out.sample_aspect_ratio = vaco_core::Rational {
            num: i32::from(jfif.x_density),
            den: i32::from(jfif.y_density),
        };
    }

    let mut idct = SpecExactIdct::new()?;
    let h_max = frame_header.h_max().max(1);
    let v_max = frame_header.v_max().max(1);

    for (plane_index, (comp_spec, comp_state)) in frame_header
        .components
        .iter()
        .zip(state.comps.iter())
        .enumerate()
    {
        let quant = state
            .quant
            .get(usize::from(comp_state.quant_index))
            .copied()
            .unwrap_or_default();
        let comp_w = div_ceil_usize(
            frame_header.width as usize * usize::from(comp_spec.h),
            h_max as usize,
        )
        .max(1);
        let comp_h = div_ceil_usize(
            frame_header.height as usize * usize::from(comp_spec.v),
            v_max as usize,
        )
        .max(1);

        let Some(mut plane) = out.plane_mut(plane_index) else {
            continue;
        };
        let mut pixel_block = [0i32; 64];
        for by in 0..comp_state.blocks_h {
            for bx in 0..comp_state.blocks_w {
                let base = (by * comp_state.blocks_w + bx) * 64;
                let Some(coeffs) = comp_state.coeffs.get(base..base + 64) else {
                    continue;
                };
                let mut coeffs_arr = [0i32; 64];
                coeffs_arr.copy_from_slice(coeffs);
                idct.apply(&coeffs_arr, &quant.values, precision, &mut pixel_block);

                for row_in_block in 0..8usize {
                    let y = by * 8 + row_in_block;
                    if y >= comp_h {
                        continue;
                    }
                    let Some(dst_row) = plane.row_mut(y) else {
                        continue;
                    };
                    for col_in_block in 0..8usize {
                        let x = bx * 8 + col_in_block;
                        if x >= comp_w {
                            continue;
                        }
                        let v = pixel_block
                            .get(row_in_block * 8 + col_in_block)
                            .copied()
                            .unwrap_or(0);
                        write_sample(dst_row, x, precision, v);
                    }
                }
            }
        }
    }

    Ok(out)
}

/// Decode a complete JPEG stream (`SOI` through `EOI`) into one [`Frame`].
///
/// # Errors
/// [`Error::InvalidData`] for a malformed stream, [`Error::Unsupported`] for
/// arithmetic coding, lossless JPEG, or a component layout with no matching
/// [`PixFmt`] (four-component CMYK/YCCK JPEGs, and any subsampling outside
/// 4:4:4/4:2:2/4:2:0/4:4:0), and whatever [`vaco_limits::Budget`] returns
/// when a declared size exceeds its caps.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = ByteReader::new(data);
    if r.be16() != 0xFFD8 {
        return Err(Error::InvalidData("jpeg: missing SOI"));
    }
    let mut state = DecodeState::new();

    loop {
        let mut b = r.u8();
        while b == 0xFF {
            b = r.u8();
        }
        let m = b;
        if r.overrun() {
            break;
        }
        budget.consume_fuel(1)?;
        if m == marker::EOI {
            break;
        }
        if marker::has_no_payload(m) {
            continue;
        }

        if marker::is_sof(m) {
            if marker::is_arithmetic_sof(m) {
                return Err(Error::Unsupported(
                    "jpeg: arithmetic entropy coding (Annex D) is not implemented",
                ));
            }
            if marker::is_lossless_sof(m) {
                return Err(Error::Unsupported(
                    "jpeg: lossless JPEG (Annex H) is not implemented",
                ));
            }
            let len = r.be16();
            if len < 2 {
                return Err(Error::InvalidData("jpeg: SOF segment too short"));
            }
            let payload = r.bytes(usize::from(len) - 2);
            let fh = header::parse_sof(m, payload)?;
            state.start_frame(fh, budget)?;
            continue;
        }

        match m {
            marker::DQT => {
                let len = r.be16();
                if len < 2 {
                    return Err(Error::InvalidData("jpeg: DQT segment too short"));
                }
                let payload = r.bytes(usize::from(len) - 2);
                header::parse_dqt(payload, |idx, table| {
                    if let Some(slot) = state.quant.get_mut(idx) {
                        *slot = table;
                    }
                })?;
            }
            marker::DHT => {
                let len = r.be16();
                if len < 2 {
                    return Err(Error::InvalidData("jpeg: DHT segment too short"));
                }
                let payload = r.bytes(usize::from(len) - 2);
                header::parse_dht(payload, |class, idx, counts, values| {
                    let table = DecodeTable::build(&counts, values.as_slice());
                    let dst = if class == 0 {
                        &mut state.dc_huff
                    } else {
                        &mut state.ac_huff
                    };
                    if let Some(slot) = dst.get_mut(idx) {
                        *slot = Some(table);
                    }
                })?;
            }
            marker::DRI => {
                let len = r.be16();
                if len < 2 {
                    return Err(Error::InvalidData("jpeg: DRI segment too short"));
                }
                let payload = r.bytes(usize::from(len) - 2);
                state.restart_interval = header::parse_dri(payload)?;
            }
            marker::APP0 => {
                let len = r.be16();
                if len < 2 {
                    return Err(Error::InvalidData("jpeg: APP0 segment too short"));
                }
                let payload = r.bytes(usize::from(len) - 2);
                if state.jfif.is_none() {
                    state.jfif = header::parse_app0_jfif(payload);
                }
            }
            marker::APP14 => {
                let len = r.be16();
                if len < 2 {
                    return Err(Error::InvalidData("jpeg: APP14 segment too short"));
                }
                let payload = r.bytes(usize::from(len) - 2);
                if let Some(t) = header::parse_app14_adobe(payload) {
                    state.adobe_transform = Some(t);
                }
            }
            marker::SOS => {
                let len = r.be16();
                if len < 2 {
                    return Err(Error::InvalidData("jpeg: SOS segment too short"));
                }
                let payload = r.bytes(usize::from(len) - 2);
                let frame = state
                    .frame
                    .as_ref()
                    .ok_or(Error::InvalidData("jpeg: SOS before SOF"))?;
                let scan = header::parse_sos(payload, frame)?;
                if !frame.is_progressive()
                    && (scan.ss != 0 || scan.se != 63 || scan.ah != 0 || scan.al != 0)
                {
                    return Err(Error::InvalidData(
                        "jpeg: a sequential SOF's scan must cover the whole spectrum with no successive approximation",
                    ));
                }
                let (resume_at, ended_cleanly) =
                    decode_scan(data, r.pos(), &scan, &mut state, budget)?;
                if !ended_cleanly {
                    state.truncated = true;
                }
                r.seek(resume_at);
            }
            _ => {
                let len = r.be16();
                if len < 2 {
                    return Err(Error::InvalidData("jpeg: segment too short"));
                }
                r.skip(usize::from(len) - 2);
            }
        }
    }

    finish_frame(&state, budget)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_dc_only_block_decodes_to_a_uniform_pixel() {
        let mut block = [0i32; 64];
        block[0] = 8; // DC=8, quant=8 -> dequantized DC=64 -> uniform pixel level
        let quant = [8u16; 64];
        let mut idct = SpecExactIdct::new().unwrap();
        let mut out = [0i32; 64];
        idct.apply(&block, &quant, 8, &mut out);
        let first = out[0];
        assert!(out.iter().all(|&v| v == first));
    }

    #[test]
    fn eobrun_from_ac_first_is_consumed_by_the_next_block() {
        // A progressive scan's own DHT can assign a code to R=1,S=0 (EOBn
        // with run field 1) even though the Annex K default tables never
        // do — baseline has no use for a multi-block EOB run, so K.5/K.6
        // simply omit it. Build a minimal one-symbol table for it instead.
        let counts: [u8; 16] = {
            let mut c = [0u8; 16];
            c[0] = 1;
            c
        };
        let values: [u8; 1] = [0x10]; // R=1, S=0
        let table = DecodeTable::build(&counts, &values);
        let enc = crate::huffman::EncodeTable::build(&counts, &values);
        let (len, code) = enc.code_for(0x10).unwrap(); // R=1,S=0
        let mut w = crate::bits::EntropyWriter::new();
        w.put_bits(u32::from(len), u32::from(code));
        w.put_bits(1, 1); // extra run bit -> eobrun = 1 + 1 = 2
        w.flush_to_byte();
        let bytes = w.finish();
        let mut er = EntropyReader::new(&bytes, 0);
        let mut eobrun = 0u32;
        let mut block_a = [0i32; 64];
        ac_first(&mut er, &table, &mut block_a, 1, 63, 0, &mut eobrun).unwrap();
        assert_eq!(eobrun, 2);
        let mut block_b = [0i32; 64];
        ac_first(&mut er, &table, &mut block_b, 1, 63, 0, &mut eobrun).unwrap();
        assert_eq!(eobrun, 1);
        assert!(block_b.iter().all(|&v| v == 0));
    }

    #[test]
    fn pixel_format_rejects_four_components() {
        let payload = [
            0x08, 0x00, 0x10, 0x00, 0x10, 0x04, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11,
            0x01, 0x04, 0x11, 0x01,
        ];
        let fh = header::parse_sof(marker::SOF0, &payload).unwrap();
        assert!(pixel_format(&fh, 8).is_err());
    }

    #[test]
    fn truncated_streams_never_panic() {
        let data = [0xFFu8, 0xD8, 0xFF, 0xDB, 0x00, 0x05, 0x00, 1, 2, 3];
        for n in 0..data.len() {
            let mut budget = Budget::new(vaco_limits::Limits::permissive());
            let _ = decode(data.get(..n).unwrap_or(&[]), &mut budget);
        }
    }

    fn segment(out: &mut Vec<u8>, marker_byte: u8, payload: &[u8]) {
        out.push(0xFF);
        out.push(marker_byte);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
    }

    /// A hand-built two-scan progressive stream (`SOF2`): one 8x8 grayscale
    /// block, a DC scan, then an AC scan whose only content is `EOB` (the
    /// block's AC coefficients are all zero, since the source is flat) —
    /// exercising [`decode`]'s real multi-scan marker-driven path end to
    /// end, not just [`ac_first`]/[`ac_refine`] in isolation.
    #[test]
    fn a_hand_built_progressive_stream_decodes_through_two_scans() {
        use crate::huffman::EncodeTable;

        let mut data = Vec::new();
        data.extend_from_slice(&[0xFF, marker::SOI]);

        // 8-bit precision, quant table 0, all-ones (lossless quantization).
        let mut dqt_payload = vec![0x00u8];
        dqt_payload.extend_from_slice(&[1u8; 64]);
        segment(&mut data, marker::DQT, &dqt_payload);

        // SOF2: precision=8, 8x8, one component (id=1, 1x1, table 0).
        segment(&mut data, marker::SOF2, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]);

        let mut dht_dc = vec![0x00u8];
        dht_dc.extend_from_slice(&crate::tables::STD_DC_LUMA.counts);
        dht_dc.extend_from_slice(crate::tables::STD_DC_LUMA.values);
        segment(&mut data, marker::DHT, &dht_dc);

        let mut dht_ac = vec![0x10u8];
        dht_ac.extend_from_slice(&crate::tables::STD_AC_LUMA.counts);
        dht_ac.extend_from_slice(crate::tables::STD_AC_LUMA.values);
        segment(&mut data, marker::DHT, &dht_ac);

        // A flat 100-valued block's only nonzero coefficient is DC = -224
        // (see the IDCT normalisation in idct.rs: pixel = DC/8, and this
        // block's centred sample is 100 - 128 = -28).
        let dc_table = EncodeTable::from_spec(&crate::tables::STD_DC_LUMA);
        let (dc_len, dc_code) = dc_table.code_for(8).unwrap(); // category 8
        let mut w = crate::bits::EntropyWriter::new();
        w.put_bits(u32::from(dc_len), u32::from(dc_code));
        w.put_bits(8, 0b0001_1111); // extend(0b00011111, 8) == -224
        w.flush_to_byte();
        let dc_entropy = w.finish();

        segment(&mut data, marker::SOS, &[1, 1, 0x00, 0, 0, 0]);
        data.extend_from_slice(&dc_entropy);

        let ac_table = EncodeTable::from_spec(&crate::tables::STD_AC_LUMA);
        let (ac_len, ac_code) = ac_table.code_for(0x00).unwrap(); // EOB
        let mut w = crate::bits::EntropyWriter::new();
        w.put_bits(u32::from(ac_len), u32::from(ac_code));
        w.flush_to_byte();
        let ac_entropy = w.finish();

        segment(&mut data, marker::SOS, &[1, 1, 0x00, 1, 63, 0]);
        data.extend_from_slice(&ac_entropy);

        data.extend_from_slice(&[0xFF, marker::EOI]);

        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let frame = decode(&data, &mut budget).unwrap();
        let vaco_frame::FrameData::Video { width, height, .. } = frame.data else {
            unreachable!()
        };
        assert_eq!((width, height), (8, 8));
        let plane = frame.plane(0).unwrap();
        for y in 0..8 {
            for &b in plane.row(y).unwrap() {
                assert_eq!(b, 100, "row {y}");
            }
        }
    }
}
