//! DVD (`VobSub`) subpicture (SPU) decode: the control-sequence command
//! stream and the 2-bit-per-pixel, top/bottom-field-interlaced run-length
//! bitmap.
//!
//! # Provenance (D6/D7)
//!
//! No formal DVD subpicture specification is public. The SPU header, control
//! sequence and command byte layout below are from the "Sub-Pictures" page
//! of `mpucoder.com/DVD` (`vobsub-mpucoder-spu` in `provenance/sources.toml`,
//! fetched via the Wayback Machine — the live site is gone), which is the
//! same write-up `vaco_subtitle_bitmap::vobsub`'s own docs point a future
//! decoder at.
//!
//! # Framing this module expects
//!
//! [`decode_spu`] takes exactly one SPU unit's bytes, starting at its own
//! two-byte size field — the shape `vaco_demux_mpegps` already recovers per
//! `private_stream_1` packet after stripping the leading sub-id byte (see
//! `VobSubDemuxer::open_pair`'s own docs), and the shape a DVD's `.sub` file
//! stores at each `.idx` `filepos:`.
//!
//! # Palette: an explicit parameter, not packet side data
//!
//! `VobSubDemuxer` already models the `.idx`'s global 16-colour palette as a
//! plain accessor (`VobSubDemuxer::palette()`), not
//! `vaco_packet::PacketSideData::Palette` — its own docs say so explicitly,
//! reasoning that a `vaco_pool::Buffer`-shaped side channel is a dependency
//! this format's demuxer does not otherwise need. [`decode_spu`] matches
//! that existing shape rather than inventing a second convention: the
//! caller passes the `.idx`'s 16-entry [`Palette`] directly (or, for a
//! Matroska `S_VOBSUB` track, whatever it parses from `CodecPrivate` — see
//! `planning/TECH-DEBT.md` for the gap that nothing does that parsing yet).

use vaco_core::{Duration, Error, Result};
use vaco_format_subtitle_bitmap::{IndexedBitmap, Palette, Rect, Rgba};
use vaco_limits::{Budget, Limits};

use crate::SubtitleEvent;

/// Read `nibble_offset`'s 4-bit nibble (0 = the top nibble of byte 0).
fn get_nibble(data: &[u8], nibble_offset: usize) -> Option<u8> {
    let byte = *data.get(nibble_offset >> 1)?;
    Some(if nibble_offset.is_multiple_of(2) {
        byte >> 4
    } else {
        byte & 0x0F
    })
}

/// The classic 4/8/12/16-bit nibble-escalation run-length code: read nibbles
/// until the accumulated value is at least `0x04` (or four nibbles have been
/// read), then split it into a pixel count and 2-bit colour. A count of zero
/// after all four nibbles means "fill to the end of the line" (`remaining`).
fn decode_run(data: &[u8], nibble_offset: &mut usize, remaining: usize) -> Option<(usize, u8)> {
    let mut v = u32::from(get_nibble(data, *nibble_offset)?);
    *nibble_offset = nibble_offset.saturating_add(1);
    if v < 0x4 {
        v = (v << 4) | u32::from(get_nibble(data, *nibble_offset)?);
        *nibble_offset = nibble_offset.saturating_add(1);
        if v < 0x10 {
            v = (v << 4) | u32::from(get_nibble(data, *nibble_offset)?);
            *nibble_offset = nibble_offset.saturating_add(1);
            if v < 0x40 {
                v = (v << 4) | u32::from(get_nibble(data, *nibble_offset)?);
                *nibble_offset = nibble_offset.saturating_add(1);
                if v < 4 {
                    v |= u32::try_from(remaining).unwrap_or(0) << 2;
                }
            }
        }
    }
    let len = usize::try_from(v >> 2).unwrap_or(0);
    let colour = u8::try_from(v & 3).unwrap_or(0);
    Some((len, colour))
}

/// Decode one interlaced field's lines, each padded/truncated to exactly
/// `width` pixels, byte-aligning `nibble_offset` after every line per the
/// spec's own "four fill bits of 0 are added" rule.
fn decode_field(
    data: &[u8],
    start_byte: usize,
    width: u32,
    lines: u32,
    limits: &Limits,
) -> Result<Vec<Vec<u8>>> {
    if width > limits.max_dimension {
        return Err(Error::LimitExceeded {
            limit: "vobsub_spu_width",
            requested: u64::from(width),
            cap: u64::from(limits.max_dimension),
        });
    }
    let width = width as usize;
    let mut nibble_offset = start_byte.saturating_mul(2);
    let mut rows = Vec::new();
    for _ in 0..lines {
        let mut row = vec![0u8; width];
        let mut col = 0usize;
        while col < width {
            let Some((len, colour)) =
                decode_run(data, &mut nibble_offset, width.saturating_sub(col))
            else {
                break;
            };
            let end = col.saturating_add(len).min(width);
            if let Some(slice) = row.get_mut(col..end) {
                slice.fill(colour);
            }
            if len == 0 {
                // A malformed stream that never advances would otherwise
                // spin forever; a genuine zero-length run cannot occur from
                // `decode_run`'s own arithmetic once `remaining` is nonzero,
                // but nothing upstream guarantees that against hostile input.
                break;
            }
            col = end;
        }
        if !nibble_offset.is_multiple_of(2) {
            nibble_offset = nibble_offset.saturating_add(1);
        }
        rows.push(row);
    }
    Ok(rows)
}

/// One command's effect on the accumulated decoder state.
#[derive(Debug, Default)]
struct SpuState {
    colours: [u8; 4],
    alphas: [u8; 4],
    area: Option<(u32, u32, u32, u32)>,
    top_offset: Option<usize>,
    bottom_offset: Option<usize>,
    start_ticks: Option<u32>,
    stop_ticks: Option<u32>,
    forced: bool,
}

/// Walk the `SP_DCSQT` chain starting at `first_cs`, applying every
/// command's effect in date order. A self-referencing `SP_NXT_DCSQ_SA`
/// (pointing at its own control sequence) ends the chain, per the format's
/// own convention for "this is the last one".
fn parse_control_sequences(data: &[u8], first_cs: usize) -> Result<SpuState> {
    let mut state = SpuState::default();
    let mut cs = first_cs;
    let mut visited = 0u32;
    loop {
        visited = visited.saturating_add(1);
        if visited > 1024 {
            break;
        }
        let stm = u32::from(read_u16(data, cs)?);
        let next = usize::from(read_u16(
            data,
            cs.checked_add(2)
                .ok_or(Error::InvalidData("vobsub: SPU offset overflow"))?,
        )?);
        let mut i = cs
            .checked_add(4)
            .ok_or(Error::InvalidData("vobsub: SPU offset overflow"))?;
        loop {
            let cmd = *data.get(i).ok_or(Error::InvalidData(
                "vobsub: control sequence ran off the end",
            ))?;
            i = i.saturating_add(1);
            match cmd {
                0xFF => break,
                0x00 => {
                    state.forced = true;
                    state.start_ticks.get_or_insert(stm);
                }
                0x01 => {
                    state.start_ticks.get_or_insert(stm);
                }
                0x02 => {
                    state.stop_ticks = Some(stm);
                }
                0x03 => {
                    let b0 = byte_at(data, i)?;
                    let b1 = byte_at(data, i.saturating_add(1))?;
                    state.colours = [b1 & 0x0F, (b1 >> 4) & 0x0F, b0 & 0x0F, (b0 >> 4) & 0x0F];
                    i = i.saturating_add(2);
                }
                0x04 => {
                    let b0 = byte_at(data, i)?;
                    let b1 = byte_at(data, i.saturating_add(1))?;
                    // 0..=15 -> 0..=255, exact at both ends (0 and 15*17=255);
                    // `nibble * 17` is `nibble`'s value repeated in both
                    // hex digits, the standard 4-to-8-bit intensity expansion
                    // (the same one EN 300 743's own §9 "N-bit reduction"
                    // family runs in reverse).
                    let scale = |nibble: u8| nibble.saturating_mul(17);
                    state.alphas = [
                        scale(b1 & 0x0F),
                        scale((b1 >> 4) & 0x0F),
                        scale(b0 & 0x0F),
                        scale((b0 >> 4) & 0x0F),
                    ];
                    i = i.saturating_add(2);
                }
                0x05 => {
                    let b = [
                        byte_at(data, i)?,
                        byte_at(data, i.saturating_add(1))?,
                        byte_at(data, i.saturating_add(2))?,
                        byte_at(data, i.saturating_add(3))?,
                        byte_at(data, i.saturating_add(4))?,
                        byte_at(data, i.saturating_add(5))?,
                    ];
                    let sx = u32::from(b[0]) << 4 | u32::from(b[1] >> 4);
                    let ex = u32::from(b[1] & 0x0F) << 8 | u32::from(b[2]);
                    let sy = u32::from(b[3]) << 4 | u32::from(b[4] >> 4);
                    let ey = u32::from(b[4] & 0x0F) << 8 | u32::from(b[5]);
                    state.area = Some((sx, ex, sy, ey));
                    i = i.saturating_add(6);
                }
                0x06 => {
                    state.top_offset = Some(usize::from(read_u16(data, i)?));
                    state.bottom_offset = Some(usize::from(read_u16(data, i.saturating_add(2))?));
                    i = i.saturating_add(4);
                }
                0x07 => {
                    let size = usize::from(read_u16(data, i)?);
                    i = i.saturating_add(size.max(2));
                }
                _ => return Err(Error::InvalidData("vobsub: unknown SPU control command")),
            }
        }
        if next == cs || next >= data.len() {
            break;
        }
        cs = next;
    }
    Ok(state)
}

fn byte_at(data: &[u8], at: usize) -> Result<u8> {
    data.get(at).copied().ok_or(Error::InvalidData(
        "vobsub: control sequence argument truncated",
    ))
}

fn read_u16(data: &[u8], at: usize) -> Result<u16> {
    let hi = byte_at(data, at)?;
    let lo = byte_at(data, at.saturating_add(1))?;
    Ok(u16::from(hi) << 8 | u16::from(lo))
}

/// `SP_DCSQ_STM` units are a 90 kHz clock divided by 1024; this converts one
/// to microseconds.
#[allow(
    clippy::integer_division,
    reason = "PTS-clock scaling, not a length computed from an aligned base: inherent to the 90kHz/1024 unit conversion documented in vobsub-mpucoder-spu"
)]
fn ticks_to_micros(ticks: u32) -> i64 {
    (i64::from(ticks) * 1024 * 1_000_000) / 90_000
}

/// Decode one complete SPU unit.
///
/// `palette` is the `.idx`'s (or equivalent) up-to-16-entry colour table;
/// `SET_COLOR`'s four nibbles index into it directly.
///
/// # Errors
/// [`Error::InvalidData`] if the SPU header or control sequence is
/// structurally broken; [`Error::LimitExceeded`] if the declared display
/// area exceeds `limits`.
pub fn decode_spu(data: &[u8], palette: &Palette, limits: &Limits) -> Result<SubtitleEvent> {
    let dcsqta = usize::from(read_u16(data, 2)?);
    let state = parse_control_sequences(data, dcsqta)?;
    let (sx, ex, sy, ey) = state
        .area
        .ok_or(Error::InvalidData("vobsub: SPU never set a display area"))?;
    let width = ex.saturating_sub(sx).saturating_add(1);
    let height = ey.saturating_sub(sy).saturating_add(1);
    let rect = Rect::new(sx, sy, width, height, limits)?;

    let top_offset = state.top_offset.ok_or(Error::InvalidData(
        "vobsub: SPU never set a pixel data address",
    ))?;
    let bottom_offset = state.bottom_offset.unwrap_or(top_offset);
    let top_lines = height.div_ceil(2);
    let bottom_lines = height >> 1;
    let top_rows = decode_field(data, top_offset, width, top_lines, limits)?;
    let bottom_rows = decode_field(data, bottom_offset, width, bottom_lines, limits)?;

    let entries: Vec<Rgba> = state
        .colours
        .iter()
        .zip(state.alphas.iter())
        .map(|(&idx, &alpha)| {
            let base = palette.get(idx).unwrap_or(Rgba::TRANSPARENT);
            Rgba::new(base.r, base.g, base.b, alpha)
        })
        .collect();
    let out_palette = Palette::new(entries)?;

    let mut budget = Budget::new(limits.clone());
    let mut bitmap = IndexedBitmap::allocate(&mut budget, rect, out_palette)?;
    let width_usize = width as usize;
    for y in 0..height {
        let row = if y.is_multiple_of(2) {
            top_rows.get((y >> 1) as usize)
        } else {
            bottom_rows.get((y >> 1) as usize)
        };
        let Some(row) = row else { continue };
        let start = (y as usize).saturating_mul(width_usize);
        if let Some(dst) = bitmap
            .indices_mut()
            .get_mut(start..start.saturating_add(row.len()))
        {
            dst.copy_from_slice(row);
        }
    }

    Ok(SubtitleEvent {
        start: Duration::from_micros(state.start_ticks.map_or(0, ticks_to_micros)),
        end: state
            .stop_ticks
            .map(|t| Duration::from_micros(ticks_to_micros(t))),
        forced: state.forced,
        rects: vec![bitmap],
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// A 4x2 SPU: display area (0,0)-(3,1) i.e. 4 wide, 2 tall (one top line,
    /// one bottom line), solid pattern colour (index 1) everywhere, fully
    /// opaque.
    fn sample_spu() -> Vec<u8> {
        let mut body = Vec::new();
        // Placeholder SPDSZ/SP_DCSQTA, patched once the real offset is known.
        body.extend_from_slice(&[0, 0, 0, 0]);
        let top_offset = body.len();
        // Top field: one line of 4 pixels, colour 1. Shortest RLE form is one
        // nibble `nncc` (n=1,c=1 -> nibble 0x5); four pixels need four
        // nibbles, i.e. two bytes.
        body.push(0x55);
        body.push(0x55);
        let bottom_offset = body.len();
        body.push(0x55);
        body.push(0x55);
        let dcsqta = body.len();
        body.extend_from_slice(&0u16.to_be_bytes()); // SP_DCSQ_STM
        body.extend_from_slice(&(dcsqta as u16).to_be_bytes()); // self-referencing: last CS
        body.push(0x01); // STA_DSP
        body.push(0x03); // SET_COLOR
        body.push(0x21); // e2=2,e1=1
        body.push(0x30); // p=3,b=0
        body.push(0x04); // SET_CONTR
        body.push(0xFF); // e2=15,e1=15
        body.push(0xFF); // p=15,b=15
        body.push(0x05); // SET_DAREA
        // sx=0 ex=3 sy=0 ey=1, packed per the mpucoder layout: byte0 =
        // sx[11:4], byte1 = sx[3:0]<<4 | ex[11:8], byte2 = ex[7:0]; same
        // shape again for sy/ey.
        body.push(0x00); // sx[11:4]
        body.push(0x00); // sx[3:0]<<4 | ex[11:8]
        body.push(0x03); // ex[7:0]
        body.push(0x00); // sy[11:4]
        body.push(0x00); // sy[3:0]<<4 | ey[11:8]
        body.push(0x01); // ey[7:0]
        body.push(0x06); // SET_DSPXA
        body.extend_from_slice(&(top_offset as u16).to_be_bytes());
        body.extend_from_slice(&(bottom_offset as u16).to_be_bytes());
        body.push(0xFF); // CMD_END

        let size = body.len() as u16;
        body[0] = (size >> 8) as u8;
        body[1] = (size & 0xFF) as u8;
        body[2] = (dcsqta >> 8) as u8;
        body[3] = (dcsqta & 0xFF) as u8;
        body
    }

    fn sample_palette() -> Palette {
        Palette::new(vec![
            Rgba::new(0, 0, 0, 255),
            Rgba::new(10, 20, 30, 255),
            Rgba::new(255, 255, 255, 255),
            Rgba::new(1, 2, 3, 255),
        ])
        .unwrap()
    }

    #[test]
    fn decodes_area_and_pixels_from_a_hand_built_spu() {
        let data = sample_spu();
        let event = decode_spu(&data, &sample_palette(), &Limits::permissive()).unwrap();
        assert_eq!(event.rects.len(), 1);
        let rect = event.rects[0].rect();
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 4, 2));
        assert!(event.rects[0].indices().iter().all(|&v| v == 1));
        // colour index 1 (pattern) maps to palette entry `p=3` per this
        // fixture's SET_COLOR byte layout (byte0 = e2<<4|e1, byte1 = p<<4|b).
        let painted = event.rects[0].palette().get(1).unwrap();
        assert_eq!((painted.r, painted.g, painted.b), (1, 2, 3));
        assert!(!event.forced);
    }

    #[test]
    fn forced_start_sets_the_forced_flag() {
        let mut data = sample_spu();
        // Replace STA_DSP (0x01) with FSTA_DSP (0x00) at its known offset:
        // right after the two SP_DCSQT header fields (4 bytes) past dcsqta.
        let dcsqta = usize::from(read_u16(&data, 2).unwrap());
        data[dcsqta + 4] = 0x00;
        let event = decode_spu(&data, &sample_palette(), &Limits::permissive()).unwrap();
        assert!(event.forced);
    }

    #[test]
    fn missing_display_area_is_reported_not_panicked() {
        let mut data = vec![0u8, 0, 0, 0];
        let dcsqta = data.len();
        data[2] = (dcsqta >> 8) as u8;
        data[3] = (dcsqta & 0xFF) as u8;
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&(dcsqta as u16).to_be_bytes());
        data.push(0xFF);
        let size = data.len() as u16;
        data[0] = (size >> 8) as u8;
        data[1] = (size & 0xFF) as u8;
        assert!(decode_spu(&data, &sample_palette(), &Limits::permissive()).is_err());
    }
}
