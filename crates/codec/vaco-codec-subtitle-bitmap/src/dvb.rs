//! DVB subtitle decode (ETSI EN 300 743 `en-300743-1.5.1`): page/region/object
//! composition and the 2-/4-/8-bit run-length pixel-code grammars.
//!
//! # Framing this module expects
//!
//! `vaco_subtitle_bitmap::dvbsub`'s registered demuxer hands out fixed
//! [`vaco_subtitle_bitmap::dvbsub::RAW_PACKET_SIZE`]-byte chunks with no
//! segment awareness (see that module's docs) — real delivery is inside
//! MPEG-TS PES packets, where `data_alignment_indicator` means one PES
//! payload is normally one whole display-set epoch. [`DvbSubDecoder`] handles
//! both: it buffers pushed bytes and only decodes once it has walked a
//! complete chain ending in [`segments::SegmentType::EndOfDisplaySet`], so a
//! caller can feed it either whole PES payloads or arbitrary raw chunks.
//! [`decode_display_set`] is the non-buffering core, for a caller that
//! already has one complete epoch's bytes in hand.
//!
//! # What is decoded, and the simplifications from the full segment grammar
//!
//! `object_coding_method` `0x00` (run-length pixel data) is decoded in full,
//! against the exact grammars in EN 300 743 §7.2.5.2 (2-/4-/8-bit pixel code
//! strings) and the default-CLUT formulas in §10. `0x01` (a string of
//! character-table references, needing a text renderer this workspace has no
//! font for) is reported as [`vaco_core::Error::Unsupported`] rather than
//! guessed at. A CLUT segment's `2/4/8-bit_entry_CLUT_flag`s are not
//! distinguished — every entry lands in one flat table by `CLUT_entry_id`,
//! same as [`segments::parse_clut`] this module builds on — which matches a
//! CLUT that only ever tags entries for the region's own declared depth (the
//! common case) but would misplace a segment that packs more than one
//! bit-depth family into a single `CLUT_definition_segment`. The Disparity
//! Signalling Segment (§7.2.7, 3D-only) is skipped unread.

use std::collections::HashMap;

use vaco_core::{Duration, Error, MediaType, Result};
use vaco_format_subtitle_bitmap::{IndexedBitmap, Palette, Rect, Rgba};
use vaco_limits::{Budget, Limits};
use vaco_subtitle_bitmap::dvbsub::segments::{self, SegmentType};

use crate::SubtitleEvent;

/// A pushed buffer this large without ever completing a display set is
/// treated as a corrupt or hostile stream rather than grown further —
/// matching `vaco-subtitle-bitmap::vobsub`'s `MAX_IDX_BYTES` convention of a
/// plain constant bound for an accumulate-until-marker buffer.
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

/// One region's declared geometry and paint state, from a
/// `region_composition_segment` this module parses in full (beyond the
/// `region_id`/size pair [`segments::parse_region_composition`] stops at).
#[derive(Debug, Clone)]
struct RegionHeader {
    width: u32,
    height: u32,
    depth: PixelDepth,
    clut_id: u8,
    fill_flag: bool,
    fill_index: u8,
    objects: Vec<ObjectPlacement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PixelDepth {
    Two,
    Four,
    Eight,
}

#[derive(Debug, Clone, Copy)]
struct ObjectPlacement {
    object_id: u16,
    x: u32,
    y: u32,
}

/// A fully decoded object: interlaced top/bottom fields already merged into
/// one rectangular index grid, padded to its widest line.
#[derive(Debug, Clone, Default)]
struct DecodedObject {
    width: u32,
    height: u32,
    indices: Vec<u8>,
    non_modifying: bool,
}

impl DecodedObject {
    fn get(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = usize::try_from(u64::from(y) * u64::from(self.width) + u64::from(x)).ok()?;
        self.indices.get(at).copied()
    }
}

/// Streaming wrapper: accumulates pushed bytes until a complete display set
/// (terminated by [`SegmentType::EndOfDisplaySet`]) is available, then
/// decodes it via [`decode_display_set`]. See the module docs for why this
/// exists rather than assuming one push is one epoch.
#[derive(Debug)]
pub struct DvbSubDecoder {
    limits: Limits,
    pending: Vec<u8>,
    resync_discarded: u64,
}

impl DvbSubDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: Vec::new(),
            resync_discarded: 0,
        }
    }

    /// Bytes thrown away by resynchronisation since construction.
    ///
    /// Resync is a silent recovery — the stream keeps working and the caller
    /// gets frames — so without a counter "every packet is being discarded"
    /// and "the stream is clean" look identical from outside. A caller that
    /// wants to report degraded input reads this; nothing is required to.
    #[must_use]
    pub const fn resync_discarded(&self) -> u64 {
        self.resync_discarded
    }

    /// Feed more bytes (a whole PES payload, or an arbitrary raw chunk).
    /// Returns every display set completed by this call, in order.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if the pending buffer grows past
    /// [`MAX_PENDING_BYTES`] without ever completing a display set; whatever
    /// [`decode_display_set`] reports for a completed one.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SubtitleEvent>> {
        if self.pending.len().saturating_add(bytes.len()) > MAX_PENDING_BYTES {
            return Err(Error::LimitExceeded {
                limit: "dvbsub_pending_bytes",
                requested: (self.pending.len().saturating_add(bytes.len())) as u64,
                cap: MAX_PENDING_BYTES as u64,
            });
        }
        self.pending.extend_from_slice(bytes);

        let mut events = Vec::new();
        loop {
            // Resynchronise before parsing. A display set can only begin at a
            // `sync_byte`, so anything ahead of the first one is not framing
            // this decoder can recover — and leaving it in front would make
            // every *later* display set unparseable too, since the walk always
            // starts at offset zero. Without this a single corrupt byte (or a
            // stream joined mid-segment, which the registered `dvbsub`
            // demuxer's blind fixed-size chunking makes ordinary) silently
            // poisons the buffer until it hits its cap. Found by this crate's
            // own decoder test feeding a packet of prose.
            if !self.pending.is_empty()
                && self.pending.first() != Some(&segments::SYNC_BYTE)
            {
                if let Some(at) = self.pending.iter().position(|&b| b == segments::SYNC_BYTE) {
                    self.resync_discarded = self.resync_discarded.saturating_add(at as u64);
                    self.pending.drain(..at);
                } else {
                    self.resync_discarded = self
                        .resync_discarded
                        .saturating_add(self.pending.len() as u64);
                    self.pending.clear();
                    break;
                }
            }
            let Some(end) = find_display_set_end(&self.pending) else {
                break;
            };
            let Some(chunk) = self.pending.get(..end) else {
                break;
            };
            events.push(decode_display_set(chunk, &self.limits)?);
            self.pending.drain(..end);
        }
        Ok(events)
    }
}

/// The byte offset just past the first [`SegmentType::EndOfDisplaySet`]
/// segment in `data`, or `None` if no complete one is present yet.
fn find_display_set_end(data: &[u8]) -> Option<usize> {
    let mut pos = 0usize;
    for (header, _) in segments::iter_segments(data) {
        let total = segments::HEADER_LEN.checked_add(usize::from(header.length))?;
        pos = pos.checked_add(total)?;
        if header.kind == SegmentType::EndOfDisplaySet {
            return Some(pos);
        }
    }
    None
}

/// Decode one complete display-set epoch (a run of segments) into a
/// [`SubtitleEvent`]. `data` need not end exactly on a segment boundary —
/// trailing partial bytes are ignored, matching every other lenient demuxer
/// in this workspace.
///
/// # Errors
/// [`Error::LimitExceeded`] if a region or object claims a size over
/// `limits`; [`Error::Unsupported`] for a character-coded object;
/// [`Error::InvalidData`] for a structurally broken segment.
pub fn decode_display_set(data: &[u8], limits: &Limits) -> Result<SubtitleEvent> {
    let mut page_regions: Vec<(u8, u32, u32)> = Vec::new();
    let mut region_headers: HashMap<u8, RegionHeader> = HashMap::new();
    let mut cluts: HashMap<u8, Palette> = HashMap::new();
    let mut objects: HashMap<u16, DecodedObject> = HashMap::new();
    let mut page_time_out = 0u8;

    for (header, payload) in segments::iter_segments(data) {
        match header.kind {
            SegmentType::PageComposition => {
                let (timeout, regions) = parse_page_composition(payload)?;
                page_time_out = timeout;
                for (id, x, y) in regions {
                    if let Some(slot) = page_regions.iter_mut().find(|(r, _, _)| *r == id) {
                        *slot = (id, x, y);
                    } else {
                        page_regions.push((id, x, y));
                    }
                }
            }
            SegmentType::RegionComposition => {
                let (id, hdr) = parse_region_composition_full(payload, limits)?;
                region_headers.insert(id, hdr);
            }
            SegmentType::ClutDefinition => {
                let (id, palette) = segments::parse_clut(payload)?;
                cluts.insert(id, palette);
            }
            SegmentType::ObjectData => {
                let (id, obj) = decode_object_data(payload, limits)?;
                objects.insert(id, obj);
            }
            SegmentType::EndOfDisplaySet | SegmentType::DisplayDefinition | SegmentType::Other(_) => {}
        }
    }

    let mut rects = Vec::new();
    for (region_id, page_x, page_y) in &page_regions {
        let Some(hdr) = region_headers.get(region_id) else {
            continue;
        };
        let rect = Rect::new(*page_x, *page_y, hdr.width, hdr.height, limits)?;
        let palette = match cluts.get(&hdr.clut_id) {
            Some(p) => p.clone(),
            None => default_clut(hdr.depth)?,
        };
        let mut budget = Budget::new(limits.clone());
        let mut bitmap = IndexedBitmap::allocate(&mut budget, rect, palette)?;
        if hdr.fill_flag {
            for slot in bitmap.indices_mut() {
                *slot = hdr.fill_index;
            }
        }
        for placement in &hdr.objects {
            let Some(obj) = objects.get(&placement.object_id) else {
                continue;
            };
            blit(&mut bitmap, obj, placement.x, placement.y);
        }
        rects.push(bitmap);
    }

    Ok(SubtitleEvent {
        start: Duration::ZERO,
        end: Some(Duration::from_micros(i64::from(page_time_out).saturating_mul(1_000_000))),
        forced: false,
        rects,
    })
}

fn blit(dst: &mut IndexedBitmap, obj: &DecodedObject, x0: u32, y0: u32) {
    let dst_rect = dst.rect();
    for y in 0..obj.height {
        let Some(dy) = y0.checked_add(y) else { break };
        if dy >= dst_rect.height {
            break;
        }
        for x in 0..obj.width {
            let Some(dx) = x0.checked_add(x) else { break };
            if dx >= dst_rect.width {
                break;
            }
            let Some(value) = obj.get(x, y) else { continue };
            if obj.non_modifying && value == 1 {
                continue;
            }
            let row = u64::from(dy) * u64::from(dst_rect.width);
            let Some(at) = usize::try_from(row + u64::from(dx)).ok() else {
                continue;
            };
            if let Some(slot) = dst.indices_mut().get_mut(at) {
                *slot = value;
            }
        }
    }
}

// ------------------------------------------------------------ page composition

/// `page_time_out`(8) then repeated `region_id`(8) `reserved`(8)
/// `region_horizontal_address`(16) `region_vertical_address`(16). One header
/// byte (`page_version_number`/`page_state`/`reserved`) is skipped: nothing
/// downstream needs to distinguish a refresh from a full redraw.
fn parse_page_composition(payload: &[u8]) -> Result<(u8, Vec<(u8, u32, u32)>)> {
    let timeout = *payload
        .first()
        .ok_or(Error::InvalidData("dvbsub: page composition too short"))?;
    let mut regions = Vec::new();
    let mut i = 2usize;
    while let Some(&region_id) = payload.get(i) {
        let x = rb16_at(payload, i.checked_add(2).ok_or(Error::InvalidData("dvbsub: offset overflow"))?)?;
        let y = rb16_at(payload, i.checked_add(4).ok_or(Error::InvalidData("dvbsub: offset overflow"))?)?;
        regions.push((region_id, u32::from(x), u32::from(y)));
        i = i.checked_add(6).ok_or(Error::InvalidData("dvbsub: offset overflow"))?;
    }
    Ok((timeout, regions))
}

fn rb16_at(buf: &[u8], at: usize) -> Result<u16> {
    let hi = *buf
        .get(at)
        .ok_or(Error::InvalidData("dvbsub: segment truncated"))?;
    let lo = *buf
        .get(at.checked_add(1).ok_or(Error::InvalidData("dvbsub: offset overflow"))?)
        .ok_or(Error::InvalidData("dvbsub: segment truncated"))?;
    Ok(u16::from(hi) << 8 | u16::from(lo))
}

// ---------------------------------------------------------- region composition

/// Reads the full `region_composition_segment`, beyond the
/// `region_id`/width/height pair [`segments::parse_region_composition`]
/// already validates: fill flag/index, depth, `CLUT_id`, and the object
/// list.
fn parse_region_composition_full(payload: &[u8], limits: &Limits) -> Result<(u8, RegionHeader)> {
    let (region_id, rect) = segments::parse_region_composition(payload, limits)?;
    let flags = *payload
        .get(1)
        .ok_or(Error::InvalidData("dvbsub: region composition too short"))?;
    let fill_flag = flags & 0x80 != 0;
    let compat_depth = *payload
        .get(6)
        .ok_or(Error::InvalidData("dvbsub: region composition too short"))?;
    let depth = match (compat_depth >> 2) & 0x07 {
        1 => PixelDepth::Two,
        2 => PixelDepth::Four,
        _ => PixelDepth::Eight,
    };
    let clut_id = *payload
        .get(7)
        .ok_or(Error::InvalidData("dvbsub: region composition too short"))?;
    let pixel_8bit = *payload
        .get(8)
        .ok_or(Error::InvalidData("dvbsub: region composition too short"))?;
    let packed = *payload
        .get(9)
        .ok_or(Error::InvalidData("dvbsub: region composition too short"))?;
    let pixel_4bit = (packed >> 4) & 0x0F;
    let pixel_2bit = (packed >> 2) & 0x03;
    let fill_index = match depth {
        PixelDepth::Two => pixel_2bit,
        PixelDepth::Four => pixel_4bit,
        PixelDepth::Eight => pixel_8bit,
    };

    let mut objects = Vec::new();
    let mut i = 10usize;
    while let Some(slice) = payload.get(i..) {
        let Some(&id_hi) = slice.first() else { break };
        let Some(&id_lo) = slice.get(1) else { break };
        let object_id = u16::from(id_hi) << 8 | u16::from(id_lo);
        let b2 = *slice
            .get(2)
            .ok_or(Error::InvalidData("dvbsub: object entry truncated"))?;
        let b3 = *slice
            .get(3)
            .ok_or(Error::InvalidData("dvbsub: object entry truncated"))?;
        let b4 = *slice
            .get(4)
            .ok_or(Error::InvalidData("dvbsub: object entry truncated"))?;
        let b5 = *slice
            .get(5)
            .ok_or(Error::InvalidData("dvbsub: object entry truncated"))?;
        let object_type = (b2 >> 6) & 0x03;
        let x = (u32::from(b2) & 0x0F) << 8 | u32::from(b3);
        let y = (u32::from(b4) & 0x0F) << 8 | u32::from(b5);
        objects.push(ObjectPlacement { object_id, x, y });
        i = i
            .checked_add(if object_type == 1 || object_type == 2 { 8 } else { 6 })
            .ok_or(Error::InvalidData("dvbsub: offset overflow"))?;
    }

    Ok((
        region_id,
        RegionHeader {
            width: rect.width,
            height: rect.height,
            depth,
            clut_id,
            fill_flag,
            fill_index,
            objects,
        },
    ))
}

// -------------------------------------------------------------- object data

/// `object_id`(16) `object_version_number`(4) `object_coding_method`(2)
/// `non_modifying_colour_flag`(1) `reserved`(1)
/// `top_field_data_block_length`(16) `bottom_field_data_block_length`(16)
/// then the two fields' pixel-data sub-blocks.
fn decode_object_data(payload: &[u8], limits: &Limits) -> Result<(u16, DecodedObject)> {
    let id_hi = *payload
        .first()
        .ok_or(Error::InvalidData("dvbsub: object data too short"))?;
    let id_lo = *payload
        .get(1)
        .ok_or(Error::InvalidData("dvbsub: object data too short"))?;
    let object_id = u16::from(id_hi) << 8 | u16::from(id_lo);
    let flags = *payload
        .get(2)
        .ok_or(Error::InvalidData("dvbsub: object data too short"))?;
    let coding_method = (flags >> 2) & 0x03;
    let non_modifying = flags & 0x02 != 0;
    if coding_method != 0 {
        return Err(Error::Unsupported(
            "dvbsub: character-coded objects need a text renderer this crate has none of",
        ));
    }
    let top_len = usize::from(rb16_at(payload, 3)?);
    let bottom_len = usize::from(rb16_at(payload, 5)?);
    let top_start = 7usize;
    let top_end = top_start
        .checked_add(top_len)
        .ok_or(Error::InvalidData("dvbsub: object data length overflow"))?;
    let top_bytes = payload
        .get(top_start..top_end)
        .ok_or(Error::InvalidData("dvbsub: object data truncated"))?;
    let bottom_bytes = if bottom_len == 0 {
        top_bytes
    } else {
        let bottom_end = top_end
            .checked_add(bottom_len)
            .ok_or(Error::InvalidData("dvbsub: object data length overflow"))?;
        payload
            .get(top_end..bottom_end)
            .ok_or(Error::InvalidData("dvbsub: object data truncated"))?
    };

    let top_rows = decode_field(top_bytes, limits)?;
    let bottom_rows = decode_field(bottom_bytes, limits)?;
    let height = top_rows
        .len()
        .checked_add(bottom_rows.len())
        .ok_or(Error::InvalidData("dvbsub: object height overflow"))?;
    let height_u32 = u32::try_from(height).unwrap_or(u32::MAX);
    if height_u32 > limits.max_dimension {
        return Err(Error::LimitExceeded {
            limit: "dvbsub_object_height",
            requested: u64::from(height_u32),
            cap: u64::from(limits.max_dimension),
        });
    }
    let width = top_rows
        .iter()
        .chain(bottom_rows.iter())
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    let width_u32 = u32::try_from(width).unwrap_or(u32::MAX);
    if width_u32 > limits.max_dimension {
        return Err(Error::LimitExceeded {
            limit: "dvbsub_object_width",
            requested: u64::from(width_u32),
            cap: u64::from(limits.max_dimension),
        });
    }

    let mut budget = Budget::new(limits.clone());
    let total = width
        .checked_mul(height)
        .ok_or(Error::InvalidData("dvbsub: object area overflows"))?;
    let mut indices = budget.alloc::<u8>(total)?;
    for row in 0..height {
        let src = if row.is_multiple_of(2) {
            top_rows.get(row >> 1)
        } else {
            bottom_rows.get(row >> 1)
        };
        if let Some(src) = src {
            let start = row.saturating_mul(width);
            if let Some(dst) = indices.get_mut(start..start.saturating_add(src.len())) {
                dst.copy_from_slice(src);
            }
        }
    }

    Ok((
        object_id,
        DecodedObject {
            width: width_u32,
            height: height_u32,
            indices,
            non_modifying,
        },
    ))
}

/// Decode one field's pixel-data sub-blocks into a list of rows (one
/// `Vec<u8>` of pseudo-colour indices per line), stopping when `data` is
/// exhausted. A line ends at `0xF0` ("end of object line code") or when its
/// own pixel-code string signals its "end of string" marker.
fn decode_field(data: &[u8], limits: &Limits) -> Result<Vec<Vec<u8>>> {
    let mut rows = Vec::new();
    let mut i = 0usize;
    while let Some(&data_type) = data.get(i) {
        i = i.saturating_add(1);
        match data_type {
            0x10 => {
                let (row, consumed) = rle::decode_2bit(data.get(i..).unwrap_or(&[]), limits)?;
                rows.push(row);
                i = i.saturating_add(consumed);
            }
            0x11 => {
                let (row, consumed) = rle::decode_4bit(data.get(i..).unwrap_or(&[]), limits)?;
                rows.push(row);
                i = i.saturating_add(consumed);
            }
            0x12 => {
                let (row, consumed) = rle::decode_8bit(data.get(i..).unwrap_or(&[]), limits)?;
                rows.push(row);
                i = i.saturating_add(consumed);
            }
            0xF0 => {}
            // Map-table data (0x20/0x21/0x22) remaps pseudo-colours read at a
            // narrower depth onto a wider CLUT; not applied here (see the
            // module docs), but its declared length is still real payload
            // this loop must not misparse as a pixel string, so skip past it
            // rather than falling through to the catch-all below.
            0x20 => i = i.saturating_add(2),
            0x21 => i = i.saturating_add(4),
            0x22 => i = i.saturating_add(16),
            _ => break,
        }
        if rows.len() as u64 > u64::from(limits.max_dimension) {
            return Err(Error::LimitExceeded {
                limit: "dvbsub_object_rows",
                requested: rows.len() as u64,
                cap: u64::from(limits.max_dimension),
            });
        }
    }
    Ok(rows)
}

mod rle {
    use vaco_core::{Error, Result};
    use vaco_limits::Limits;

    /// A cursor over 2-bit nibble-pairs ("nibble" here meaning the format's
    /// own smallest code unit), MSB-first within each byte.
    struct Bits<'a> {
        data: &'a [u8],
        bit_pos: usize,
    }

    impl<'a> Bits<'a> {
        const fn new(data: &'a [u8]) -> Self {
            Self { data, bit_pos: 0 }
        }

        fn take(&mut self, n: u32) -> Option<u32> {
            let mut v = 0u32;
            for _ in 0..n {
                let byte = self.bit_pos >> 3;
                let bit = 7 - (self.bit_pos & 7);
                let b = *self.data.get(byte)?;
                v = (v << 1) | u32::from((b >> bit) & 1);
                self.bit_pos = self.bit_pos.saturating_add(1);
            }
            Some(v)
        }

        fn peek(&self, n: u32) -> Option<u32> {
            let mut clone = Self {
                data: self.data,
                bit_pos: self.bit_pos,
            };
            clone.take(n)
        }

        /// Bytes fully or partially consumed, rounded up to the next byte.
        fn bytes_consumed(&self) -> usize {
            self.bit_pos.div_ceil(8)
        }

        fn align_byte(&mut self) {
            self.bit_pos = self.bit_pos.div_ceil(8).saturating_mul(8);
        }
    }

    fn push_run(row: &mut Vec<u8>, colour: u8, len: u32, limits: &Limits) -> Result<()> {
        let new_len = row
            .len()
            .checked_add(len as usize)
            .ok_or(Error::InvalidData("dvbsub: pixel run overflows"))?;
        if new_len as u64 > u64::from(limits.max_dimension) {
            return Err(Error::LimitExceeded {
                limit: "dvbsub_row_width",
                requested: new_len as u64,
                cap: u64::from(limits.max_dimension),
            });
        }
        row.resize(new_len, colour);
        Ok(())
    }

    /// EN 300 743 §7.2.5.2, `2-bit/pixel_code_string()`. Returns the decoded
    /// row and how many bytes of `data` its bits (including trailing
    /// 2-bit-alignment stuffing) occupied.
    pub(super) fn decode_2bit(data: &[u8], limits: &Limits) -> Result<(Vec<u8>, usize)> {
        let mut bits = Bits::new(data);
        let mut row = Vec::new();
        loop {
            let first = bits
                .take(2)
                .ok_or(Error::InvalidData("dvbsub: truncated 2-bit pixel string"))?;
            if first != 0 {
                push_run(&mut row, u8::try_from(first).unwrap_or(0), 1, limits)?;
                continue;
            }
            let switch_1 = bits
                .take(1)
                .ok_or(Error::InvalidData("dvbsub: truncated 2-bit pixel string"))?;
            if switch_1 == 1 {
                let len = bits
                    .take(3)
                    .ok_or(Error::InvalidData("dvbsub: truncated 2-bit run"))?
                    .saturating_add(3);
                let colour = bits
                    .take(2)
                    .ok_or(Error::InvalidData("dvbsub: truncated 2-bit run"))?;
                push_run(&mut row, u8::try_from(colour).unwrap_or(0), len, limits)?;
                continue;
            }
            let switch_2 = bits
                .take(1)
                .ok_or(Error::InvalidData("dvbsub: truncated 2-bit pixel string"))?;
            if switch_2 == 1 {
                push_run(&mut row, 0, 1, limits)?;
                continue;
            }
            let switch_3 = bits
                .take(2)
                .ok_or(Error::InvalidData("dvbsub: truncated 2-bit pixel string"))?;
            match switch_3 {
                0b00 => break,
                0b01 => push_run(&mut row, 0, 2, limits)?,
                0b10 => {
                    let len = bits
                        .take(4)
                        .ok_or(Error::InvalidData("dvbsub: truncated 2-bit run"))?
                        .saturating_add(12);
                    let colour = bits
                        .take(2)
                        .ok_or(Error::InvalidData("dvbsub: truncated 2-bit run"))?;
                    push_run(&mut row, u8::try_from(colour).unwrap_or(0), len, limits)?;
                }
                _ => {
                    let len = bits
                        .take(8)
                        .ok_or(Error::InvalidData("dvbsub: truncated 2-bit run"))?
                        .saturating_add(29);
                    let colour = bits
                        .take(2)
                        .ok_or(Error::InvalidData("dvbsub: truncated 2-bit run"))?;
                    push_run(&mut row, u8::try_from(colour).unwrap_or(0), len, limits)?;
                }
            }
        }
        bits.align_byte();
        Ok((row, bits.bytes_consumed()))
    }

    /// `4-bit/pixel_code_string()`.
    pub(super) fn decode_4bit(data: &[u8], limits: &Limits) -> Result<(Vec<u8>, usize)> {
        let mut bits = Bits::new(data);
        let mut row = Vec::new();
        loop {
            let first = bits
                .take(4)
                .ok_or(Error::InvalidData("dvbsub: truncated 4-bit pixel string"))?;
            if first != 0 {
                push_run(&mut row, u8::try_from(first).unwrap_or(0), 1, limits)?;
                continue;
            }
            let switch_1 = bits
                .take(1)
                .ok_or(Error::InvalidData("dvbsub: truncated 4-bit pixel string"))?;
            if switch_1 == 0 {
                if bits.peek(3) == Some(0) {
                    let _ = bits.take(3);
                    break;
                }
                let len = bits
                    .take(3)
                    .ok_or(Error::InvalidData("dvbsub: truncated 4-bit run"))?
                    .saturating_add(2);
                push_run(&mut row, 0, len, limits)?;
                continue;
            }
            let switch_2 = bits
                .take(1)
                .ok_or(Error::InvalidData("dvbsub: truncated 4-bit pixel string"))?;
            if switch_2 == 0 {
                let len = bits
                    .take(2)
                    .ok_or(Error::InvalidData("dvbsub: truncated 4-bit run"))?
                    .saturating_add(4);
                let colour = bits
                    .take(4)
                    .ok_or(Error::InvalidData("dvbsub: truncated 4-bit run"))?;
                push_run(&mut row, u8::try_from(colour).unwrap_or(0), len, limits)?;
                continue;
            }
            let switch_3 = bits
                .take(2)
                .ok_or(Error::InvalidData("dvbsub: truncated 4-bit pixel string"))?;
            if switch_3 == 0b10 {
                let len = bits
                    .take(4)
                    .ok_or(Error::InvalidData("dvbsub: truncated 4-bit run"))?
                    .saturating_add(9);
                let colour = bits
                    .take(4)
                    .ok_or(Error::InvalidData("dvbsub: truncated 4-bit run"))?;
                push_run(&mut row, u8::try_from(colour).unwrap_or(0), len, limits)?;
            } else {
                let len = bits
                    .take(8)
                    .ok_or(Error::InvalidData("dvbsub: truncated 4-bit run"))?
                    .saturating_add(25);
                let colour = bits
                    .take(4)
                    .ok_or(Error::InvalidData("dvbsub: truncated 4-bit run"))?;
                push_run(&mut row, u8::try_from(colour).unwrap_or(0), len, limits)?;
            }
        }
        bits.align_byte();
        Ok((row, bits.bytes_consumed()))
    }

    /// `8-bit/pixel_code_string()`.
    pub(super) fn decode_8bit(data: &[u8], limits: &Limits) -> Result<(Vec<u8>, usize)> {
        let mut bits = Bits::new(data);
        let mut row = Vec::new();
        loop {
            let first = bits
                .take(8)
                .ok_or(Error::InvalidData("dvbsub: truncated 8-bit pixel string"))?;
            if first != 0 {
                push_run(&mut row, u8::try_from(first).unwrap_or(0), 1, limits)?;
                continue;
            }
            let switch_1 = bits
                .take(1)
                .ok_or(Error::InvalidData("dvbsub: truncated 8-bit pixel string"))?;
            if switch_1 == 0 {
                if bits.peek(7) == Some(0) {
                    let _ = bits.take(7);
                    break;
                }
                let len = bits
                    .take(7)
                    .ok_or(Error::InvalidData("dvbsub: truncated 8-bit run"))?;
                push_run(&mut row, 0, len, limits)?;
                continue;
            }
            let len = bits
                .take(7)
                .ok_or(Error::InvalidData("dvbsub: truncated 8-bit run"))?
                .saturating_add(3);
            let colour = bits
                .take(8)
                .ok_or(Error::InvalidData("dvbsub: truncated 8-bit run"))?;
            push_run(&mut row, u8::try_from(colour).unwrap_or(0), len, limits)?;
        }
        bits.align_byte();
        Ok((row, bits.bytes_consumed()))
    }
}

// -------------------------------------------------------------- default CLUT

/// EN 300 743 §10: the fixed default contents every CLUT family has before
/// any `CLUT_definition_segment` redefines (part of) it. Percentages are
/// rounded to the nearest `u8` (`round(pct * 255 / 100)`); an exact
/// byte-for-byte match against any particular decoder's rounding is not
/// asserted, only the documented percentages themselves.
fn default_clut(depth: PixelDepth) -> Result<Palette> {
    let entries = match depth {
        PixelDepth::Two => (0..4).map(default_clut_4).collect(),
        PixelDepth::Four => (0..16).map(default_clut_16).collect(),
        PixelDepth::Eight => (0..256).map(default_clut_256).collect(),
    };
    Palette::new(entries)
}

fn pct(p: f64) -> u8 {
    let scaled = (p * 255.0 / 100.0).round();
    let clamped = scaled.clamp(0.0, 255.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to 0.0..=255.0 immediately above"
    )]
    (clamped as u8)
}

/// §10.3, a 2-bit CLUT-entry index split into bits `b1 b2`.
fn default_clut_4(index: u32) -> Rgba {
    let b1 = (index >> 1) & 1;
    let b2 = index & 1;
    match (b1, b2) {
        (0, 0) => Rgba::TRANSPARENT,
        (0, 1) => Rgba::new(255, 255, 255, 255),
        (1, 0) => Rgba::new(0, 0, 0, 255),
        _ => Rgba::new(pct(50.0), pct(50.0), pct(50.0), 255),
    }
}

/// §10.2, a 4-bit CLUT-entry index split into bits `b1 b2 b3 b4`.
fn default_clut_16(index: u32) -> Rgba {
    let b1 = (index >> 3) & 1;
    let b2 = (index >> 2) & 1;
    let b3 = (index >> 1) & 1;
    let b4 = index & 1;
    if b1 == 0 {
        if b2 == 0 && b3 == 0 && b4 == 0 {
            return Rgba::TRANSPARENT;
        }
        return Rgba::new(pct(100.0 * f64::from(b4)), pct(100.0 * f64::from(b3)), pct(100.0 * f64::from(b2)), 255);
    }
    Rgba::new(
        pct(50.0 * f64::from(b4)),
        pct(50.0 * f64::from(b3)),
        pct(50.0 * f64::from(b2)),
        255,
    )
}

/// §10.1, an 8-bit CLUT-entry index split into bits `b1..b8`.
#[allow(clippy::many_single_char_names, reason = "b1..b8 are EN 300 743's own field names for this formula")]
fn default_clut_256(index: u32) -> Rgba {
    let b1 = (index >> 7) & 1;
    let b2 = (index >> 6) & 1;
    let b3 = (index >> 5) & 1;
    let b4 = (index >> 4) & 1;
    let b5 = (index >> 3) & 1;
    let b6 = (index >> 2) & 1;
    let b7 = (index >> 1) & 1;
    let b8 = index & 1;
    let f = f64::from;

    if b1 == 0 && b5 == 0 {
        if b2 == 0 && b3 == 0 && b4 == 0 {
            if b6 == 0 && b7 == 0 && b8 == 0 {
                return Rgba::new(0, 0, 0, 0);
            }
            return Rgba::new(pct(100.0 * f(b8)), pct(100.0 * f(b7)), pct(100.0 * f(b6)), pct(75.0));
        }
        return Rgba::new(
            pct(33.3 * f(b8) + 66.7 * f(b4)),
            pct(33.3 * f(b7) + 66.7 * f(b3)),
            pct(33.3 * f(b6) + 66.7 * f(b2)),
            255,
        );
    }
    if b1 == 0 && b5 == 1 {
        return Rgba::new(
            pct(33.3 * f(b8) + 66.7 * f(b4)),
            pct(33.3 * f(b7) + 66.7 * f(b3)),
            pct(33.3 * f(b6) + 66.7 * f(b2)),
            pct(50.0),
        );
    }
    if b1 == 1 && b5 == 0 {
        return Rgba::new(
            pct(16.7 * f(b8) + 33.3 * f(b4) + 50.0),
            pct(16.7 * f(b7) + 33.3 * f(b3) + 50.0),
            pct(16.7 * f(b6) + 33.3 * f(b2) + 50.0),
            255,
        );
    }
    Rgba::new(
        pct(16.7 * f(b8) + 33.3 * f(b4)),
        pct(16.7 * f(b7) + 33.3 * f(b3)),
        pct(16.7 * f(b6) + 33.3 * f(b2)),
        255,
    )
}

/// Enough of a hint for a future caller wiring this into a container's
/// `MediaType` check; this module otherwise takes raw bytes and never touches
/// `vaco_codec_core`.
pub const MEDIA_TYPE: MediaType = MediaType::Subtitle;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn seg(kind: u8, page_id: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![segments::SYNC_BYTE, kind];
        v.extend_from_slice(&page_id.to_be_bytes());
        v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// A 4x4 region, filled entirely by a single white object at (0,0),
    /// positioned at page (10,10) -- the same fixture this crate's
    /// `tests/fixtures/dvb_manual.py` generator builds, cross-checked there
    /// against ffmpeg's own `dvbsub` decoder via `PyAV`.
    fn sample_display_set() -> Vec<u8> {
        let mut page_payload = vec![5u8, 0x08];
        page_payload.extend_from_slice(&[0, 0]);
        page_payload.extend_from_slice(&10u16.to_be_bytes());
        page_payload.extend_from_slice(&10u16.to_be_bytes());
        let page = seg(0x10, 1, &page_payload);

        let mut region_payload = vec![0u8, 0x08];
        region_payload.extend_from_slice(&4u16.to_be_bytes());
        region_payload.extend_from_slice(&4u16.to_be_bytes());
        region_payload.extend_from_slice(&[0x24, 0x00, 0x00, 0x04]);
        region_payload.extend_from_slice(&1u16.to_be_bytes());
        region_payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        let region = seg(0x11, 1, &region_payload);

        let mut clut_payload = vec![0u8, 0x00];
        clut_payload.extend_from_slice(&[1, 0x81, 255, 128, 128, 255]);
        let clut = seg(0x12, 1, &clut_payload);

        let line = [0x10u8, 0x55, 0x00, 0xF0];
        let mut field = Vec::new();
        field.extend_from_slice(&line);
        field.extend_from_slice(&line);
        let mut obj_payload = vec![0u8, 1, 0x00];
        obj_payload.extend_from_slice(&(field.len() as u16).to_be_bytes());
        obj_payload.extend_from_slice(&(field.len() as u16).to_be_bytes());
        obj_payload.extend_from_slice(&field);
        obj_payload.extend_from_slice(&field);
        let obj = seg(0x13, 1, &obj_payload);

        let end = seg(0x80, 1, &[]);

        let mut all = Vec::new();
        all.extend(page);
        all.extend(region);
        all.extend(clut);
        all.extend(obj);
        all.extend(end);
        all
    }

    #[test]
    fn decodes_a_single_region_positioned_object() {
        let data = sample_display_set();
        let event = decode_display_set(&data, &Limits::permissive()).unwrap();
        assert_eq!(event.rects.len(), 1);
        let rect = event.rects[0].rect();
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (10, 10, 4, 4));
        assert!(event.rects[0].indices().iter().all(|&i| i == 1));
        let white = event.rects[0].palette().get(1).unwrap();
        assert_eq!((white.r, white.g, white.b), (255, 255, 255));
    }

    #[test]
    fn streaming_decoder_accepts_one_push_per_segment() {
        let data = sample_display_set();
        let mut dec = DvbSubDecoder::new(Limits::permissive());
        let mut events = Vec::new();
        for byte in &data {
            events.extend(dec.push(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].rects.len(), 1);
    }

    #[test]
    fn streaming_decoder_accepts_one_push_per_segment_whole() {
        let data = sample_display_set();
        let mut dec = DvbSubDecoder::new(Limits::permissive());
        let events = dec.push(&data).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn a_region_with_no_clut_segment_falls_back_to_the_default() {
        let mut data = sample_display_set();
        // Drop the CLUT segment (bytes 14..28, see `sample_display_set`'s
        // construction order) is fragile to hand-slice; instead decode twice
        // and confirm a display set missing any CLUT entirely still
        // produces a palette, never an error.
        let region_only = {
            let mut page_payload = vec![5u8, 0x08];
            page_payload.extend_from_slice(&[0, 0]);
            page_payload.extend_from_slice(&0u16.to_be_bytes());
            page_payload.extend_from_slice(&0u16.to_be_bytes());
            seg(0x10, 1, &page_payload)
        };
        let mut region_payload = vec![0u8, 0x00];
        region_payload.extend_from_slice(&2u16.to_be_bytes());
        region_payload.extend_from_slice(&2u16.to_be_bytes());
        region_payload.extend_from_slice(&[0x24, 0x00, 0x00, 0x00]);
        let region = seg(0x11, 1, &region_payload);
        let end = seg(0x80, 1, &[]);
        data.clear();
        data.extend(region_only);
        data.extend(region);
        data.extend(end);
        let event = decode_display_set(&data, &Limits::permissive()).unwrap();
        assert_eq!(event.rects.len(), 1);
        assert_eq!(event.rects[0].palette().len(), 4);
    }

    #[test]
    fn default_clut_4_matches_the_documented_corners() {
        let p = default_clut(PixelDepth::Two).unwrap();
        assert_eq!(p.get(0), Some(Rgba::TRANSPARENT));
        assert_eq!(p.get(1), Some(Rgba::new(255, 255, 255, 255)));
        assert_eq!(p.get(2), Some(Rgba::new(0, 0, 0, 255)));
    }

    #[test]
    fn an_oversized_region_is_rejected_before_any_pixel_buffer_is_sized() {
        let mut page_payload = vec![5u8, 0x08];
        page_payload.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let page = seg(0x10, 1, &page_payload);
        let mut region_payload = vec![0u8, 0x00];
        region_payload.extend_from_slice(&0xFFFFu16.to_be_bytes());
        region_payload.extend_from_slice(&0xFFFFu16.to_be_bytes());
        region_payload.extend_from_slice(&[0x24, 0x00, 0x00, 0x00]);
        let region = seg(0x11, 1, &region_payload);
        let end = seg(0x80, 1, &[]);
        let mut data = Vec::new();
        data.extend(page);
        data.extend(region);
        data.extend(end);
        assert!(decode_display_set(&data, &Limits::strict()).is_err());
    }
}
