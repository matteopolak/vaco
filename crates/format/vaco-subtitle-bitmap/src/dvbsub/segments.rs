//! EN 300 743 §7 subtitling-segment structure: page, region, CLUT and
//! object-data segments.
//!
//! # Why this module exists but the registered demuxer does not call it
//!
//! `ffmpeg -h demuxer=dvbsub` names it "raw dvbsub" and shows the exact same
//! `-raw_packet_size` option every other headerless elementary-stream
//! demuxer in the reference has — measured evidence that the reference's
//! `dvbsub` demuxer is a blind fixed-size chunk reader with no segment
//! awareness at all, and [`crate::dvbsub`]'s registered [`crate::dvbsub::DEMUXER`]
//! matches that. This module is the segment-aware structure EN 300 743 §7
//! actually describes, kept as a real, tested, fuzzed building block for
//! [`crate::dvbsub::probe`] (which *is* allowed to be stricter than the
//! reference's own probe, per `planning/AGENT-CONSTRAINTS.md`'s "Detection
//! and demuxing ask different questions") and for whatever decoder lands in
//! `crates/codec/` next.
//!
//! # What is parsed, and why it stops there
//!
//! [`parse_region_composition`] and [`parse_clut`] read only the fixed,
//! uncompressed fields at the front of a region-composition and a
//! CLUT-definition segment (§7.2.3, §7.2.4) — a region's declared
//! width/height, and a CLUT's `Y Cr Cb T` entries. Both are plain integer
//! fields with no run-length coding, so reading them is container/header
//! work, same as reading a PNG `IHDR`. What is *not* parsed: object-data
//! segments (§7.2.5) at all — their payload is the run-length pixel string,
//! which is decoder work — and region-composition's own object-list
//! (`region_object_id`, its position within the region), which is compressed
//! into a repeating variable-length structure and adds nothing to what this
//! crate needs to prove `Rect`/`Palette` bounds-checking against real
//! container fields.

use vaco_core::{Error, Result};
use vaco_format_subtitle_bitmap::color::ycbcrt_to_rgba;
use vaco_format_subtitle_bitmap::{Palette, Rect, Rgba};
use vaco_limits::Limits;

use crate::bytes::rb16;

/// `sync_byte`, EN 300 743 §7.2: every segment starts with this.
pub const SYNC_BYTE: u8 = 0x0F;

/// `sync_byte`(1) + `segment_type`(1) + `page_id`(2) + `segment_length`(2).
pub const HEADER_LEN: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType {
    PageComposition,
    RegionComposition,
    ClutDefinition,
    ObjectData,
    DisplayDefinition,
    EndOfDisplaySet,
    Other(u8),
}

impl SegmentType {
    #[must_use]
    pub const fn from_u8(b: u8) -> Self {
        match b {
            0x10 => Self::PageComposition,
            0x11 => Self::RegionComposition,
            0x12 => Self::ClutDefinition,
            0x13 => Self::ObjectData,
            0x14 => Self::DisplayDefinition,
            0x80 => Self::EndOfDisplaySet,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    pub kind: SegmentType,
    pub page_id: u16,
    /// Length of `segment_data_field`, in bytes.
    pub length: u16,
}

/// Parse the 6-byte header at the start of `buf`. `None` if short or not
/// [`SYNC_BYTE`]-prefixed.
#[must_use]
pub fn parse_header(buf: &[u8]) -> Option<SegmentHeader> {
    if buf.first() != Some(&SYNC_BYTE) {
        return None;
    }
    let kind = *buf.get(1)?;
    let page_id = rb16(buf, 2)?;
    let length = rb16(buf, 4)?;
    Some(SegmentHeader {
        kind: SegmentType::from_u8(kind),
        page_id,
        length,
    })
}

/// Walk `data` as subtitling segments. Lenient: stops cleanly (no error) at
/// the first position that is not `sync_byte`-prefixed or does not have
/// `length` bytes remaining — which is also how a legal stream ends, since
/// `0xFF` PES stuffing after the last segment is not a valid `sync_byte`.
#[must_use]
pub fn iter_segments(data: &[u8]) -> Segments<'_> {
    Segments { data, pos: 0 }
}

#[derive(Debug)]
pub struct Segments<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for Segments<'a> {
    type Item = (SegmentHeader, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let head = self.data.get(self.pos..)?;
        let header = parse_header(head)?;
        let total = HEADER_LEN.checked_add(usize::from(header.length))?;
        let record = head.get(..total)?;
        let payload = record.get(HEADER_LEN..)?;
        self.pos = self.pos.checked_add(total)?;
        Some((header, payload))
    }
}

/// A region-composition segment's declared size (EN 300 743 §7.2.3):
/// `region_id`(8), flags(8), `region_width`(16), `region_height`(16), …
/// (fields after this are not read).
///
/// # Errors
/// [`Error::InvalidData`] if `payload` is too short; whatever [`Rect::new`]
/// reports if the declared size exceeds `limits`.
pub fn parse_region_composition(payload: &[u8], limits: &Limits) -> Result<(u8, Rect)> {
    let region_id = *payload.first().ok_or(Error::InvalidData(
        "dvbsub: region-composition segment too short",
    ))?;
    let width = rb16(payload, 2).ok_or(Error::InvalidData(
        "dvbsub: region-composition segment too short",
    ))?;
    let height = rb16(payload, 4).ok_or(Error::InvalidData(
        "dvbsub: region-composition segment too short",
    ))?;
    let rect = Rect::new(0, 0, u32::from(width), u32::from(height), limits)?;
    Ok((region_id, rect))
}

/// A CLUT-definition segment's palette (EN 300 743 §7.2.4).
///
/// Layout per entry: `CLUT_entry_id`(8), `{8,4,2}_bit_entry_CLUT_flag`(3) +
/// reserved(4) + `full_range_flag`(1), then either `Y Cr Cb T`(8 each, if
/// `full_range_flag`) or `Y`(6) `Cr`(4) `Cb`(4) `T`(2) packed into the next
/// two bytes.
///
/// # Errors
/// [`Error::InvalidData`] if `payload` is too short for its header, or an
/// entry runs past the end.
pub fn parse_clut(payload: &[u8]) -> Result<(u8, Palette)> {
    let clut_id = *payload
        .first()
        .ok_or(Error::InvalidData("dvbsub: CLUT segment too short"))?;
    // Byte 1 is CLUT_version_number(4)+reserved(4); not needed here.
    let mut entries: Vec<(u8, Rgba)> = Vec::new();
    let mut i = 2usize;
    while let Some(&entry_id) = payload.get(i) {
        let flags = *payload
            .get(
                i.checked_add(1)
                    .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?,
            )
            .ok_or(Error::InvalidData("dvbsub: CLUT entry truncated"))?;
        let full_range = flags & 0x01 != 0;
        if full_range {
            let base = i
                .checked_add(2)
                .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?;
            let y = *payload
                .get(base)
                .ok_or(Error::InvalidData("dvbsub: CLUT entry truncated"))?;
            let cr = *payload
                .get(
                    base.checked_add(1)
                        .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?,
                )
                .ok_or(Error::InvalidData("dvbsub: CLUT entry truncated"))?;
            let cb = *payload
                .get(
                    base.checked_add(2)
                        .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?,
                )
                .ok_or(Error::InvalidData("dvbsub: CLUT entry truncated"))?;
            let t = *payload
                .get(
                    base.checked_add(3)
                        .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?,
                )
                .ok_or(Error::InvalidData("dvbsub: CLUT entry truncated"))?;
            entries.push((entry_id, ycbcrt_to_rgba(y, cb, cr, t)));
            i = base
                .checked_add(4)
                .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?;
        } else {
            let base = i
                .checked_add(2)
                .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?;
            let b0 = *payload
                .get(base)
                .ok_or(Error::InvalidData("dvbsub: CLUT entry truncated"))?;
            let b1 = *payload
                .get(
                    base.checked_add(1)
                        .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?,
                )
                .ok_or(Error::InvalidData("dvbsub: CLUT entry truncated"))?;
            // Y(6) Cr(4) Cb(4) T(2), packed MSB-first across the two bytes,
            // each field left-justified into a full byte (the standard
            // bit-expansion for a narrow field with no defined LSBs).
            let y = b0 & 0xFC;
            let cr = ((b0 & 0x03) << 6) | ((b1 & 0xF0) >> 2);
            let cb = (b1 & 0x0F) << 4;
            // The packed form's 2-bit `T` is too coarse to carry a useful
            // transparency range on its own; treated as fully opaque, same
            // as this crate's other "container states no alpha" case
            // (`vobsub`'s `.idx` palette).
            entries.push((entry_id, ycbcrt_to_rgba(y, cb, cr, 0xFF)));
            i = base
                .checked_add(2)
                .ok_or(Error::InvalidData("dvbsub: CLUT offset overflow"))?;
        }
        if entries.len() > Palette::MAX_ENTRIES {
            return Err(Error::InvalidData(
                "dvbsub: CLUT segment declares more than 256 entries",
            ));
        }
    }
    // Entries may arrive in any `CLUT_entry_id` order and are not required to
    // be dense; place each at its stated index, defaulting the rest to
    // transparent black, same convention `Palette` documents.
    let max_id = entries.iter().map(|(id, _)| *id).max().unwrap_or(0);
    let mut table = vec![Rgba::TRANSPARENT; usize::from(max_id).saturating_add(1)];
    for (id, rgba) in entries {
        if let Some(slot) = table.get_mut(usize::from(id)) {
            *slot = rgba;
        }
    }
    Ok((clut_id, Palette::new(table)?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn seg(kind: u8, page_id: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![SYNC_BYTE, kind];
        v.extend_from_slice(&page_id.to_be_bytes());
        v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn iter_segments_walks_a_display_set() {
        let mut data = seg(0x10, 1, &[9, 9]);
        data.extend(seg(0x11, 1, &[0, 0, 0, 100, 0, 50]));
        data.extend(seg(0x80, 1, &[]));
        let kinds: Vec<SegmentType> = iter_segments(&data).map(|(h, _)| h.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SegmentType::PageComposition,
                SegmentType::RegionComposition,
                SegmentType::EndOfDisplaySet,
            ]
        );
    }

    #[test]
    fn iter_segments_stops_at_non_sync_bytes() {
        let mut data = seg(0x10, 1, &[1]);
        data.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // PES stuffing, not a segment
        assert_eq!(iter_segments(&data).count(), 1);
    }

    #[test]
    fn parse_region_composition_reads_the_declared_size() {
        let payload = [0u8, 0, 0, 100, 1, 44]; // width=100, height=0x012C=300
        let (id, rect) = parse_region_composition(&payload, &Limits::permissive()).unwrap();
        assert_eq!(id, 0);
        assert_eq!((rect.width, rect.height), (100, 300));
    }

    #[test]
    fn parse_region_composition_rejects_an_oversized_region() {
        let payload = [0u8, 0, 0xFF, 0xFF, 0xFF, 0xFF]; // 65535 x 65535
        assert!(parse_region_composition(&payload, &Limits::strict()).is_err());
    }

    #[test]
    fn parse_clut_reads_full_range_entries() {
        // id=0, version/reserved byte, then one full-range entry: id=5, flags=1 (full_range), Y=255,Cr=128,Cb=128,T=255
        let payload = [7u8, 0, 5, 0x01, 255, 128, 128, 255];
        let (clut_id, palette) = parse_clut(&payload).unwrap();
        assert_eq!(clut_id, 7);
        let white = palette.get(5).unwrap();
        assert_eq!((white.r, white.g, white.b), (255, 255, 255));
    }

    #[test]
    fn parse_clut_rejects_a_truncated_entry() {
        let payload = [0u8, 0, 5, 0x01, 255]; // full-range flagged, but Cr/Cb/T missing
        assert!(parse_clut(&payload).is_err());
    }
}
