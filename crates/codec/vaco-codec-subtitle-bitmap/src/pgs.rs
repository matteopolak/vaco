//! PGS/HDMV presentation graphics decode: composition (PCS), window (WDS)
//! and palette (PDS) segments, and object (ODS) run-length pixel decode.
//!
//! # Provenance (D6/D7)
//!
//! There is no free official PGS specification (same situation
//! `vaco_subtitle_bitmap::sup`'s own docs record for its segment framing).
//! The segment field layouts and the object run-length grammar below are
//! from a public community write-up (`pgs-scorpius-writeup` in
//! `provenance/sources.toml`); the composition-object flag byte's split
//! between `object_cropped_flag` (`0x80`) and `forced_on_flag` (`0x40`) —
//! genuinely ambiguous across public write-ups, several of which describe
//! only one flag — was cross-checked against `BDSup2Sub`, an independent
//! open-source implementation unrelated to this project's own reference
//! binary (`pgs-bdsup2sub-forced-flag`).
//!
//! # Framing this module expects
//!
//! `vaco_subtitle_bitmap::sup::PgsDemuxer` hands out one packet **per
//! segment**, payload verbatim including the 13-byte `"PG"` header (see that
//! module's docs) — [`PgsDecoder::push_segment`] takes exactly that shape,
//! so a caller wires this crate directly onto that demuxer's packets with no
//! reframing in between.

use std::collections::HashMap;

use vaco_core::{Duration, Error, Result, Timestamp};
use vaco_format_subtitle_bitmap::{IndexedBitmap, Palette, Rect, Rgba};
use vaco_limits::{Budget, Limits};
use vaco_subtitle_bitmap::sup::{self, SegmentType};

use crate::SubtitleEvent;

/// An object's run-length data can legally arrive split across several ODS
/// segments (`last_in_sequence_flag`'s `0x80`/`0x40`/`0xC0`); this bounds how
/// many raw bytes one in-progress object may accumulate before its `0x40`
/// (last) fragment arrives, so a stream that never sends one cannot grow
/// this decoder's memory without bound.
const MAX_OBJECT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct CompositionObject {
    object_id: u16,
    x: u32,
    y: u32,
    forced: bool,
    crop: Option<(u32, u32, u32, u32)>,
}

#[derive(Debug, Default)]
struct InProgressObject {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// A fully decoded PGS object: an indexed bitmap with no position of its own
/// yet (that comes from the composition object referencing it).
#[derive(Debug, Clone)]
struct DecodedObject {
    width: u32,
    height: u32,
    indices: Vec<u8>,
}

/// Accumulates one epoch's PCS/WDS/PDS/ODS segments and emits a
/// [`SubtitleEvent`] on `END`.
#[derive(Debug, Default)]
pub struct PgsDecoder {
    composition: Vec<CompositionObject>,
    palette_id: u8,
    start_pts: Timestamp,
    palettes: HashMap<u8, Palette>,
    in_progress: HashMap<u16, InProgressObject>,
    objects: HashMap<u16, DecodedObject>,
}

impl PgsDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one segment record (header bytes included, as
    /// `sup::PgsDemuxer::read_packet` hands out). Returns a completed
    /// [`SubtitleEvent`] when `record` is an `END` segment.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `record` is not a well-formed segment record
    /// or a segment's own payload is structurally short;
    /// [`Error::LimitExceeded`] if a claimed object size is unreasonable or
    /// an in-progress object's accumulated bytes pass [`MAX_OBJECT_BYTES`].
    pub fn push_segment(
        &mut self,
        record: &[u8],
        limits: &Limits,
    ) -> Result<Option<SubtitleEvent>> {
        let header =
            sup::parse_header(record).ok_or(Error::InvalidData("pgs: not a segment record"))?;
        let payload = record.get(sup::HEADER_LEN..).ok_or(Error::InvalidData(
            "pgs: segment shorter than its own header",
        ))?;
        match header.kind {
            SegmentType::Pcs => {
                self.start_pts = Timestamp::new(i64::from(header.pts));
                self.composition = parse_pcs(payload)?;
                // `width`(2) `height`(2) `frame_rate`(1) `composition_number`(2)
                // `composition_state`(1) `palette_update_flag`(1) `palette_id`(1).
                self.palette_id = payload.get(9).copied().unwrap_or(0);
            }
            SegmentType::Pds => {
                let (id, palette) = parse_pds(payload)?;
                self.palettes.insert(id, palette);
            }
            SegmentType::Ods => {
                self.push_ods(payload, limits)?;
            }
            SegmentType::End => {
                return Ok(Some(self.compose(limits)?));
            }
            // A window definition segment carries no state this decoder
            // needs: composition objects are positioned in absolute screen
            // coordinates already (see the module docs), and any other
            // segment type is simply not one of the five PGS names.
            SegmentType::Wds | SegmentType::Other(_) => {}
        }
        Ok(None)
    }

    fn push_ods(&mut self, payload: &[u8], limits: &Limits) -> Result<()> {
        let object_id = rb16(payload, 0)?;
        let flags = *payload
            .get(3)
            .ok_or(Error::InvalidData("pgs: ODS too short"))?;
        let first = flags & 0x80 != 0;
        let last = flags & 0x40 != 0;

        let entry = self.in_progress.entry(object_id).or_default();
        let body = if first {
            let declared = rb24(payload, 4)?;
            let width = u32::from(rb16(payload, 7)?);
            let height = u32::from(rb16(payload, 9)?);
            if width > limits.max_dimension || height > limits.max_dimension {
                return Err(Error::LimitExceeded {
                    limit: "pgs_object_dimension",
                    requested: u64::from(width.max(height)),
                    cap: u64::from(limits.max_dimension),
                });
            }
            let declared_len = declared.saturating_sub(4);
            if declared_len > MAX_OBJECT_BYTES {
                return Err(Error::LimitExceeded {
                    limit: "pgs_object_declared_bytes",
                    requested: declared_len as u64,
                    cap: MAX_OBJECT_BYTES as u64,
                });
            }
            *entry = InProgressObject {
                width,
                height,
                data: Vec::new(),
            };
            payload.get(11..).unwrap_or(&[])
        } else {
            payload.get(4..).unwrap_or(&[])
        };

        if entry.data.len().saturating_add(body.len()) > MAX_OBJECT_BYTES {
            return Err(Error::LimitExceeded {
                limit: "pgs_object_bytes",
                requested: entry.data.len().saturating_add(body.len()) as u64,
                cap: MAX_OBJECT_BYTES as u64,
            });
        }
        entry.data.extend_from_slice(body);

        if last && let Some(done) = self.in_progress.remove(&object_id) {
            let indices = rle::decode(&done.data, done.width, done.height, limits)?;
            self.objects.insert(
                object_id,
                DecodedObject {
                    width: done.width,
                    height: done.height,
                    indices,
                },
            );
        }
        Ok(())
    }

    fn compose(&mut self, limits: &Limits) -> Result<SubtitleEvent> {
        let palette = self
            .palettes
            .get(&self.palette_id)
            .cloned()
            .unwrap_or_default();
        let mut rects = Vec::new();
        let mut forced = false;
        for comp in &self.composition {
            forced |= comp.forced;
            let Some(obj) = self.objects.get(&comp.object_id) else {
                continue;
            };
            let (crop_x, crop_y, width, height) =
                comp.crop.unwrap_or((0, 0, obj.width, obj.height));
            let rect = Rect::new(comp.x, comp.y, width, height, limits)?;
            let mut budget = Budget::new(limits.clone());
            let mut bitmap = IndexedBitmap::allocate(&mut budget, rect, palette.clone())?;
            for y in 0..height {
                let Some(sy) = crop_y.checked_add(y) else {
                    break;
                };
                for x in 0..width {
                    let Some(sx) = crop_x.checked_add(x) else {
                        break;
                    };
                    if sx >= obj.width || sy >= obj.height {
                        continue;
                    }
                    let Some(src_at) =
                        usize::try_from(u64::from(sy) * u64::from(obj.width) + u64::from(sx)).ok()
                    else {
                        continue;
                    };
                    let Some(&value) = obj.indices.get(src_at) else {
                        continue;
                    };
                    let Some(dst_at) =
                        usize::try_from(u64::from(y) * u64::from(width) + u64::from(x)).ok()
                    else {
                        continue;
                    };
                    if let Some(slot) = bitmap.indices_mut().get_mut(dst_at) {
                        *slot = value;
                    }
                }
            }
            rects.push(bitmap);
        }
        Ok(SubtitleEvent {
            start: Duration::from_micros(self.start_pts.ticks().unwrap_or(0)),
            end: None,
            forced,
            rects,
        })
    }
}

fn rb16(buf: &[u8], at: usize) -> Result<u16> {
    let hi = *buf
        .get(at)
        .ok_or(Error::InvalidData("pgs: segment truncated"))?;
    let lo = *buf
        .get(
            at.checked_add(1)
                .ok_or(Error::InvalidData("pgs: offset overflow"))?,
        )
        .ok_or(Error::InvalidData("pgs: segment truncated"))?;
    Ok(u16::from(hi) << 8 | u16::from(lo))
}

fn rb24(buf: &[u8], at: usize) -> Result<usize> {
    let b0 = *buf
        .get(at)
        .ok_or(Error::InvalidData("pgs: segment truncated"))?;
    let b1 = *buf
        .get(
            at.checked_add(1)
                .ok_or(Error::InvalidData("pgs: offset overflow"))?,
        )
        .ok_or(Error::InvalidData("pgs: segment truncated"))?;
    let b2 = *buf
        .get(
            at.checked_add(2)
                .ok_or(Error::InvalidData("pgs: offset overflow"))?,
        )
        .ok_or(Error::InvalidData("pgs: segment truncated"))?;
    Ok((usize::from(b0) << 16) | (usize::from(b1) << 8) | usize::from(b2))
}

/// `width`(2) `height`(2) `frame_rate`(1) `composition_number`(2)
/// `composition_state`(1) `palette_update_flag`(1) `palette_id`(1)
/// `num_composition_objects`(1), then that many composition-object entries.
fn parse_pcs(payload: &[u8]) -> Result<Vec<CompositionObject>> {
    let count = *payload
        .get(10)
        .ok_or(Error::InvalidData("pgs: PCS too short"))?;
    let mut out = Vec::new();
    let mut i = 11usize;
    for _ in 0..count {
        let object_id = rb16(payload, i)?;
        let flags = *payload
            .get(
                i.checked_add(3)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )
            .ok_or(Error::InvalidData("pgs: composition object truncated"))?;
        let cropped = flags & 0x80 != 0;
        let forced = flags & 0x40 != 0;
        let x = u32::from(rb16(
            payload,
            i.checked_add(4)
                .ok_or(Error::InvalidData("pgs: offset overflow"))?,
        )?);
        let y = u32::from(rb16(
            payload,
            i.checked_add(6)
                .ok_or(Error::InvalidData("pgs: offset overflow"))?,
        )?);
        let mut next = i
            .checked_add(8)
            .ok_or(Error::InvalidData("pgs: offset overflow"))?;
        let crop = if cropped {
            let cx = u32::from(rb16(payload, next)?);
            let cy = u32::from(rb16(
                payload,
                next.checked_add(2)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )?);
            let cw = u32::from(rb16(
                payload,
                next.checked_add(4)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )?);
            let ch = u32::from(rb16(
                payload,
                next.checked_add(6)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )?);
            next = next
                .checked_add(8)
                .ok_or(Error::InvalidData("pgs: offset overflow"))?;
            Some((cx, cy, cw, ch))
        } else {
            None
        };
        out.push(CompositionObject {
            object_id,
            x,
            y,
            forced,
            crop,
        });
        i = next;
    }
    Ok(out)
}

/// `palette_id`(1) `palette_version`(1), then repeated `entry_id`(1) `Y`(1)
/// `Cr`(1) `Cb`(1) `alpha`(1) until the payload ends.
fn parse_pds(payload: &[u8]) -> Result<(u8, Palette)> {
    let id = *payload
        .first()
        .ok_or(Error::InvalidData("pgs: PDS too short"))?;
    let mut table = vec![Rgba::TRANSPARENT; 256];
    let mut i = 2usize;
    while let Some(&entry_id) = payload.get(i) {
        let y = *payload
            .get(
                i.checked_add(1)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )
            .ok_or(Error::InvalidData("pgs: PDS entry truncated"))?;
        let cr = *payload
            .get(
                i.checked_add(2)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )
            .ok_or(Error::InvalidData("pgs: PDS entry truncated"))?;
        let cb = *payload
            .get(
                i.checked_add(3)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )
            .ok_or(Error::InvalidData("pgs: PDS entry truncated"))?;
        let alpha = *payload
            .get(
                i.checked_add(4)
                    .ok_or(Error::InvalidData("pgs: offset overflow"))?,
            )
            .ok_or(Error::InvalidData("pgs: PDS entry truncated"))?;
        if let Some(slot) = table.get_mut(usize::from(entry_id)) {
            *slot = vaco_format_subtitle_bitmap::ycbcrt_to_rgba(y, cb, cr, alpha);
        }
        i = i
            .checked_add(5)
            .ok_or(Error::InvalidData("pgs: offset overflow"))?;
    }
    Ok((id, Palette::new(table)?))
}

mod rle {
    use vaco_core::{Error, Result};
    use vaco_limits::{Budget, Limits};

    /// The Scorpius write-up's byte-pair run-length grammar: a non-zero byte
    /// is one pixel of that colour; `0x00` starts a two-bit-flagged run
    /// (colour 0 or an explicit colour, short or long form), or `0x00 0x00`
    /// for an explicit end-of-line.
    pub(super) fn decode(data: &[u8], width: u32, height: u32, limits: &Limits) -> Result<Vec<u8>> {
        let area = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(Error::InvalidData("pgs: object area overflows"))?;
        let len = usize::try_from(area)
            .map_err(|_| Error::InvalidData("pgs: object area too large for this platform"))?;
        let mut budget = Budget::new(limits.clone());
        let mut out = budget.alloc::<u8>(len)?;
        let width = width as usize;

        let mut i = 0usize;
        let mut row = 0usize;
        let mut col = 0usize;
        while i < data.len() {
            let Some(&b0) = data.get(i) else { break };
            i = i.saturating_add(1);
            if b0 != 0 {
                write_run(&mut out, width, row, &mut col, b0, 1);
                continue;
            }
            let Some(&b1) = data.get(i) else { break };
            i = i.saturating_add(1);
            if b1 == 0 {
                row = row.saturating_add(1);
                col = 0;
                continue;
            }
            let flag = (b1 >> 6) & 0x03;
            let short_len = usize::from(b1 & 0x3F);
            match flag {
                0 => {
                    // `00LLLLLL`: L pixels of colour 0 (1..63).
                    write_run(&mut out, width, row, &mut col, 0, short_len);
                }
                1 => {
                    // `01LLLLLL LLLLLLLL`: L pixels of colour 0 (1..16383).
                    let Some(&b2) = data.get(i) else { break };
                    i = i.saturating_add(1);
                    let len = (short_len << 8) | usize::from(b2);
                    write_run(&mut out, width, row, &mut col, 0, len);
                }
                2 => {
                    // `10LLLLLL CCCCCCCC`: L pixels of colour C (1..63).
                    let Some(&colour) = data.get(i) else { break };
                    i = i.saturating_add(1);
                    write_run(&mut out, width, row, &mut col, colour, short_len);
                }
                _ => {
                    // `11LLLLLL LLLLLLLL CCCCCCCC`: L pixels of colour C.
                    let Some(&b2) = data.get(i) else { break };
                    i = i.saturating_add(1);
                    let Some(&colour) = data.get(i) else { break };
                    i = i.saturating_add(1);
                    let len = (short_len << 8) | usize::from(b2);
                    write_run(&mut out, width, row, &mut col, colour, len);
                }
            }
        }
        Ok(out)
    }

    fn write_run(
        out: &mut [u8],
        width: usize,
        row: usize,
        col: &mut usize,
        colour: u8,
        len: usize,
    ) {
        for _ in 0..len {
            if *col >= width {
                break;
            }
            let Some(at) = row.checked_mul(width).and_then(|r| r.checked_add(*col)) else {
                break;
            };
            if let Some(slot) = out.get_mut(at) {
                *slot = colour;
            }
            *col = col.saturating_add(1);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn segment(kind: u8, pts: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&sup::MAGIC);
        v.extend_from_slice(&pts.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.push(kind);
        v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// One 2x2 object, solid colour 1, palette entry 1 = opaque white,
    /// positioned at (5,5), not forced, not cropped.
    fn sample_epoch() -> Vec<Vec<u8>> {
        let mut pcs_payload = Vec::new();
        pcs_payload.extend_from_slice(&1920u16.to_be_bytes());
        pcs_payload.extend_from_slice(&1080u16.to_be_bytes());
        pcs_payload.push(0x10);
        pcs_payload.extend_from_slice(&0u16.to_be_bytes());
        pcs_payload.push(0x80); // epoch start
        pcs_payload.push(0x00); // palette_update_flag
        pcs_payload.push(0); // palette_update_flag was already pushed above; this is palette_id
        pcs_payload.push(1); // one composition object
        pcs_payload.extend_from_slice(&1u16.to_be_bytes()); // object_id
        pcs_payload.push(0); // window_id
        pcs_payload.push(0x00); // flags: not cropped, not forced
        pcs_payload.extend_from_slice(&5u16.to_be_bytes());
        pcs_payload.extend_from_slice(&5u16.to_be_bytes());
        let pcs = segment(0x16, 90_000, &pcs_payload);

        let mut pds_payload = vec![0u8, 0]; // palette_id, version
        pds_payload.extend_from_slice(&[1, 255, 128, 128, 255]); // entry 1: opaque white
        let pds = segment(0x14, 90_000, &pds_payload);

        let mut ods_payload = Vec::new();
        ods_payload.extend_from_slice(&1u16.to_be_bytes()); // object_id
        ods_payload.push(0); // version
        ods_payload.push(0xC0); // first and last
        let rle = [1u8, 1, 0, 0, 1, 1]; // row0: colour1 colour1, end-of-line; row1: colour1 colour1
        let data_len = 4 + rle.len();
        ods_payload.extend_from_slice(&[0, (data_len >> 8) as u8, data_len as u8]);
        ods_payload.extend_from_slice(&2u16.to_be_bytes()); // width
        ods_payload.extend_from_slice(&2u16.to_be_bytes()); // height
        ods_payload.extend_from_slice(&rle);
        let ods = segment(0x15, 90_000, &ods_payload);

        let end = segment(0x80, 90_000, &[]);
        vec![pcs, pds, ods, end]
    }

    #[test]
    fn decodes_one_object_positioned_from_its_composition_entry() {
        let mut dec = PgsDecoder::new();
        let limits = Limits::permissive();
        let mut event = None;
        for seg in sample_epoch() {
            if let Some(e) = dec.push_segment(&seg, &limits).unwrap() {
                event = Some(e);
            }
        }
        let event = event.unwrap();
        assert_eq!(event.rects.len(), 1);
        let rect = event.rects[0].rect();
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (5, 5, 2, 2));
        assert!(event.rects[0].indices().iter().all(|&v| v == 1));
        assert!(!event.forced);
        let white = event.rects[0].palette().get(1).unwrap();
        assert_eq!((white.r, white.g, white.b), (255, 255, 255));
    }

    #[test]
    fn forced_flag_on_a_composition_object_is_reported() {
        let mut segs = sample_epoch();
        // Flip the composition object's flags byte (offset 11+3=14 within
        // the PCS payload, i.e. absolute offset 13(header)+11+3 in the
        // record) to 0x40 (forced, not cropped).
        let pcs = &mut segs[0];
        let flag_at = sup::HEADER_LEN + 11 + 3;
        pcs[flag_at] = 0x40;
        let mut dec = PgsDecoder::new();
        let limits = Limits::permissive();
        let mut event = None;
        for seg in segs {
            if let Some(e) = dec.push_segment(&seg, &limits).unwrap() {
                event = Some(e);
            }
        }
        assert!(event.unwrap().forced);
    }

    #[test]
    fn an_absurd_object_dimension_is_rejected() {
        let mut ods_payload = Vec::new();
        ods_payload.extend_from_slice(&1u16.to_be_bytes());
        ods_payload.push(0);
        ods_payload.push(0xC0);
        ods_payload.extend_from_slice(&[0, 0, 4]);
        ods_payload.extend_from_slice(&0xFFFFu16.to_be_bytes());
        ods_payload.extend_from_slice(&0xFFFFu16.to_be_bytes());
        let ods = segment(0x15, 0, &ods_payload);
        let mut dec = PgsDecoder::new();
        assert!(dec.push_segment(&ods, &Limits::strict()).is_err());
    }
}
