//! HEIF/AVIF item model (ISO/IEC 23008-12 §9): `hdlr(pict)`, `pitm`, `iloc`,
//! `iinf`/`infe`, `iprp`/`ipco`/`ipma`, `iref`.
//!
//! A HEIF/AVIF file's `meta` box describes *items* — one coded image, or a
//! derived image (a tile grid, an overlay) composed from others — rather
//! than the *tracks* every other box family this crate reads describes.
//! That is a genuinely different shape, which is why it is its own module.
//!
//! # What was measured, and what was transcribed
//!
//! A real `ffmpeg 8.1 -c:v libsvtav1 -f avif` single-image file was read
//! back byte for byte: `iloc` version 0 (`offset_size=4`, `length_size=4`,
//! `base_offset_size=0`, one item, one extent — the extent's offset and
//! length landed exactly on that file's `mdat` payload), `infe` version 2
//! (16-bit `item_ID`, `item_type='av01'`, a null-terminated `item_name`),
//! `ipma` version 0 with `flags=0` (one-byte associations: bit 7 essential,
//! bits 6-0 a 1-based `ipco` index — `ipco`'s own children in that file were
//! `[ispe, pixi, av1C, colr]`, matched by `ipma`'s four associations
//! `1, 2, 0x83, 4`). `iref`, a multi-item/grid `iloc` (`construction_method`,
//! multiple extents, non-zero `base_offset`) and `infe` versions other than
//! 2 were not exercised by that file and are transcribed directly from the
//! specification instead.

use crate::boxes::IsoBox;
use crate::fourcc::{FourCc, boxes};

/// Largest number of items this crate reads from one `iinf`/`iloc`/`ipma`.
pub const MAX_ITEMS: usize = 4096;
/// Largest number of extents read from one `iloc` entry.
pub const MAX_EXTENTS_PER_ITEM: usize = 256;
/// Largest number of `to_item_ID`s read from one `iref` entry.
pub const MAX_REFS_PER_ENTRY: usize = 4096;

/// One `infe` entry: what an item *is*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInfo {
    pub item_id: u32,
    /// `item_name`, the null-terminated UTF-8 string after `item_type` in
    /// `infe` version 2+ (`"Color"`, `"Alpha"`, a grid's `"Grid"`). Empty
    /// when absent, which is what a writer that has nothing to say emits.
    pub name: String,
    /// The coding format (`av01`, `hvc1`, ...) or a derived-image type
    /// (`grid`, `iovl`). All-zero for `infe` versions before 2, which name
    /// the type in a field this crate does not read (unmeasured — every
    /// writer this crate has seen uses version 2).
    pub item_type: FourCc,
    /// `flags & 1` (§9.2): a hidden item is a building block — a grid's own
    /// tiles, an auxiliary alpha/depth plane — not meant to be presented on
    /// its own.
    pub hidden: bool,
}

impl ItemInfo {
    fn parse(infe: &IsoBox<'_>) -> Option<Self> {
        let full = infe.full().ok()?;
        if full.version > 3 {
            return None;
        }
        let mut r = full.reader();
        let item_id = if full.version >= 3 {
            r.be32()
        } else {
            u32::from(r.be16())
        };
        let _protection_index = r.be16();
        let item_type = if full.version >= 2 {
            let raw = r.bytes(4);
            let mut buf = [0u8; 4];
            let n = raw.len().min(4);
            if let (Some(dst), Some(src)) = (buf.get_mut(..n), raw.get(..n)) {
                dst.copy_from_slice(src);
            }
            FourCc(buf)
        } else {
            FourCc::new(b"\0\0\0\0")
        };
        let name = if full.version >= 2 {
            let rest = full.body.get(r.pos()..).unwrap_or(&[]);
            let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            String::from_utf8_lossy(rest.get(..end).unwrap_or(&[])).into_owned()
        } else {
            String::new()
        };
        Some(Self {
            item_id,
            name,
            item_type,
            hidden: full.flags & 1 != 0,
        })
    }
}

/// `iinf` — every item's `infe`.
#[must_use]
pub fn parse_iinf(iinf: &IsoBox<'_>) -> Vec<ItemInfo> {
    let Ok(full) = iinf.full() else {
        return Vec::new();
    };
    // `entry_count` is 16 bits in version 0, 32 bits otherwise (§8.11.6.2).
    let count_width = if full.version == 0 { 2 } else { 4 };
    let mut r = full.reader();
    let entry_count = if full.version == 0 {
        u32::from(r.be16())
    } else {
        r.be32()
    };
    if r.overrun() {
        return Vec::new();
    }
    iinf.children_after(4usize.saturating_add(count_width))
        .flatten()
        .take(
            usize::try_from(entry_count)
                .unwrap_or(MAX_ITEMS)
                .min(MAX_ITEMS),
        )
        .filter(|b| b.kind() == boxes::INFE)
        .filter_map(|b| ItemInfo::parse(&b))
        .collect()
}

/// `pitm` — the primary item id.
#[must_use]
pub fn parse_pitm(pitm: &IsoBox<'_>) -> Option<u32> {
    let full = pitm.full().ok()?;
    let mut r = full.reader();
    Some(if full.version == 0 {
        u32::from(r.be16())
    } else {
        r.be32()
    })
}

/// `construction_method` (§8.11.3.3): where an extent's offset is measured
/// from. Only `FileOffset` is resolved to bytes anywhere in this crate — the
/// other two need context (`idat`'s own location, or another item's already-
/// resolved extents) that a caller, not this parse step, has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConstructionMethod {
    #[default]
    FileOffset,
    IdatOffset,
    ItemOffset,
}

/// One item's byte ranges, from `iloc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemLocation {
    pub item_id: u32,
    pub construction_method: ConstructionMethod,
    /// `data_reference_index`: 0 for this file, otherwise a 1-based `dref`
    /// entry naming another file — which no caller here follows.
    pub data_reference_index: u16,
    /// `(offset, length)` pairs, `base_offset` already folded in.
    pub extents: Vec<(u64, u64)>,
}

/// Whether an `iloc` field width is one of the grammar's defined values.
const fn valid_iloc_size(size: u8) -> bool {
    matches!(size, 0 | 4 | 8)
}

/// Read a big-endian value `size` bytes wide after [`valid_iloc_size`] has
/// accepted the containing header.
fn read_sized(r: &mut vaco_bitstream::ByteReader<'_>, size: u8) -> u64 {
    match size {
        4 => u64::from(r.be32()),
        8 => r.be64(),
        _ => 0,
    }
}

/// Parse `iloc` (§8.11.3) fully — bounded by [`MAX_ITEMS`] and
/// [`MAX_EXTENTS_PER_ITEM`], and by the reader's own truncation flag, which
/// stops the loop the instant a declared count runs past the payload rather
/// than reading zeros for the rest.
#[must_use]
pub fn parse_iloc(iloc: &IsoBox<'_>) -> Vec<ItemLocation> {
    let Ok(full) = iloc.full() else {
        return Vec::new();
    };
    if full.version > 2 {
        return Vec::new();
    }
    let mut r = full.reader();
    let sizes_a = r.u8();
    let (offset_size, length_size) = (sizes_a >> 4, sizes_a & 0x0F);
    let sizes_b = r.u8();
    let base_offset_size = sizes_b >> 4;
    let index_size = if full.version == 1 || full.version == 2 {
        sizes_b & 0x0F
    } else {
        0
    };
    if !valid_iloc_size(offset_size)
        || !valid_iloc_size(length_size)
        || !valid_iloc_size(base_offset_size)
        || !valid_iloc_size(index_size)
    {
        return Vec::new();
    }
    let item_count = if full.version < 2 {
        u32::from(r.be16())
    } else {
        r.be32()
    };
    let mut out = Vec::new();
    for _ in 0..item_count.min(u32::try_from(MAX_ITEMS).unwrap_or(u32::MAX)) {
        let item_id = if full.version < 2 {
            u32::from(r.be16())
        } else {
            r.be32()
        };
        let construction_method = if full.version >= 1 {
            match r.be16() {
                0 => ConstructionMethod::FileOffset,
                1 => ConstructionMethod::IdatOffset,
                2 => ConstructionMethod::ItemOffset,
                _ => return Vec::new(),
            }
        } else {
            ConstructionMethod::FileOffset
        };
        let data_reference_index = r.be16();
        let base_offset = read_sized(&mut r, base_offset_size);
        let extent_count = r.be16();
        let mut extents = Vec::new();
        for _ in
            0..u32::from(extent_count).min(u32::try_from(MAX_EXTENTS_PER_ITEM).unwrap_or(u32::MAX))
        {
            if (full.version == 1 || full.version == 2) && index_size > 0 {
                let _extent_index = read_sized(&mut r, index_size);
            }
            let extent_offset = read_sized(&mut r, offset_size);
            let extent_length = read_sized(&mut r, length_size);
            extents.push((base_offset.saturating_add(extent_offset), extent_length));
        }
        out.push(ItemLocation {
            item_id,
            construction_method,
            data_reference_index,
            extents,
        });
        if r.overrun() {
            break;
        }
    }
    out
}

/// `iprp ▸ ipco` — the property boxes, in file order. `ipma`'s
/// `property_index` is 1-based into this list.
#[must_use]
pub fn parse_ipco<'a>(iprp: &IsoBox<'a>) -> Vec<IsoBox<'a>> {
    let Some(ipco) = iprp.children().find(boxes::IPCO) else {
        return Vec::new();
    };
    ipco.children().flatten().take(MAX_ITEMS).collect()
}

/// One item's property associations: `(essential, 1-based ipco index)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemPropertyAssociation {
    pub item_id: u32,
    pub properties: Vec<(bool, u16)>,
}

/// `iprp ▸ ipma`.
#[must_use]
pub fn parse_ipma(iprp: &IsoBox<'_>) -> Vec<ItemPropertyAssociation> {
    let Some(ipma) = iprp.children().find(boxes::IPMA) else {
        return Vec::new();
    };
    let Ok(full) = ipma.full() else {
        return Vec::new();
    };
    if full.version > 1 {
        return Vec::new();
    }
    let mut r = full.reader();
    let entry_count = r.be32();
    let large_index = full.flags & 1 != 0;
    let mut out = Vec::new();
    for _ in 0..entry_count.min(u32::try_from(MAX_ITEMS).unwrap_or(u32::MAX)) {
        let item_id = if full.version < 1 {
            u32::from(r.be16())
        } else {
            r.be32()
        };
        let association_count = r.u8();
        let mut properties = Vec::new();
        for _ in 0..association_count {
            if large_index {
                let v = r.be16();
                properties.push((v & 0x8000 != 0, v & 0x7FFF));
            } else {
                let v = r.u8();
                properties.push((v & 0x80 != 0, u16::from(v & 0x7F)));
            }
        }
        out.push(ItemPropertyAssociation {
            item_id,
            properties,
        });
        if r.overrun() {
            break;
        }
    }
    out
}

/// One `iref` entry: a reference type and the item ids it names, from one
/// item to potentially several — a grid's own tiles are `dimg` references
/// from the grid item to each tile, in raster order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemReference {
    pub kind: FourCc,
    pub from_item_id: u32,
    pub to_item_ids: Vec<u32>,
}

/// `iref` (§8.11.12): a full box whose children are themselves typed
/// reference records, not boxes wrapping a further full-box header.
#[must_use]
pub fn parse_iref(iref: &IsoBox<'_>) -> Vec<ItemReference> {
    let Ok(full) = iref.full() else {
        return Vec::new();
    };
    let wide = full.version >= 1;
    let mut out = Vec::new();
    for child in iref.children_after(4).flatten().take(MAX_ITEMS) {
        let mut r = vaco_bitstream::ByteReader::new(child.payload);
        let from_item_id = if wide { r.be32() } else { u32::from(r.be16()) };
        let ref_count = r.be16();
        let mut to_item_ids = Vec::new();
        for _ in 0..u32::from(ref_count).min(u32::try_from(MAX_REFS_PER_ENTRY).unwrap_or(u32::MAX))
        {
            to_item_ids.push(if wide { r.be32() } else { u32::from(r.be16()) });
            if r.overrun() {
                break;
            }
        }
        out.push(ItemReference {
            kind: child.kind(),
            from_item_id,
            to_item_ids,
        });
    }
    out
}

/// `ispe` (`ImageSpatialExtentsProperty`, §6.5.3.1): an item's pixel
/// dimensions.
#[must_use]
pub fn parse_ispe(ispe: &IsoBox<'_>) -> Option<(u32, u32)> {
    let full = ispe.full().ok()?;
    let mut r = full.reader();
    let width = r.be32();
    let height = r.be32();
    r.check().ok()?;
    Some((width, height))
}

/// `clap` (`CleanApertureBox`, HEIF §6.5.9 and ISOBMFF §12.1.4): an
/// image crop expressed as exact rational dimensions and centre offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanAperture {
    width_n: u32,
    width_d: u32,
    height_n: u32,
    height_d: u32,
    horizontal_offset_n: i32,
    horizontal_offset_d: u32,
    vertical_offset_n: i32,
    vertical_offset_d: u32,
}

impl CleanAperture {
    /// Convert the rational aperture to an integer-pixel crop of an input
    /// image. Fractional *fields* are valid when their combined edge still
    /// lands on a whole pixel; a fractional edge cannot be represented by
    /// `TileGrid` and is refused by returning `None`.
    #[must_use]
    pub fn integer_crop(self, input_width: u32, input_height: u32) -> Option<(u32, u32, u32, u32)> {
        let width = positive_integer(self.width_n, self.width_d)?;
        let height = positive_integer(self.height_n, self.height_d)?;
        let x = integer_aperture_origin(
            input_width,
            width,
            self.horizontal_offset_n,
            self.horizontal_offset_d,
        )?;
        let y = integer_aperture_origin(
            input_height,
            height,
            self.vertical_offset_n,
            self.vertical_offset_d,
        )?;
        (x.checked_add(width)? <= input_width && y.checked_add(height)? <= input_height)
            .then_some((x, y, width, height))
    }
}

fn positive_integer(numerator: u32, denominator: u32) -> Option<u32> {
    if numerator > 0 && denominator > 0 && numerator.is_multiple_of(denominator) {
        numerator.checked_div(denominator)
    } else {
        None
    }
}

/// `origin = (input - aperture) / 2 + centre_offset`, kept rational until
/// the final divisibility check so a half-pixel centre offset can cancel the
/// half produced by dimensions with different parity.
fn integer_aperture_origin(input: u32, aperture: u32, offset_n: i32, offset_d: u32) -> Option<u32> {
    if offset_d == 0 {
        return None;
    }
    let denominator = i128::from(offset_d).checked_mul(2)?;
    let numerator = (i128::from(input) - i128::from(aperture))
        .checked_mul(i128::from(offset_d))?
        .checked_add(i128::from(offset_n).checked_mul(2)?)?;
    if numerator < 0 || numerator.checked_rem(denominator)? != 0 {
        return None;
    }
    u32::try_from(numerator.checked_div(denominator)?).ok()
}

/// Parse one HEIF/AVIF clean-aperture item property.
#[must_use]
pub fn parse_clap(clap: &IsoBox<'_>) -> Option<CleanAperture> {
    let mut r = vaco_bitstream::ByteReader::new(clap.payload);
    let aperture = CleanAperture {
        width_n: r.be32(),
        width_d: r.be32(),
        height_n: r.be32(),
        height_d: r.be32(),
        horizontal_offset_n: i32::from_be_bytes(r.be32().to_be_bytes()),
        horizontal_offset_d: r.be32(),
        vertical_offset_n: i32::from_be_bytes(r.be32().to_be_bytes()),
        vertical_offset_d: r.be32(),
    };
    r.check().ok()?;
    Some(aperture)
}

/// `ImageGrid` (§6.6.2.3.2): a derived image's own bytes (located via
/// `iloc` exactly like a coded item's, but holding this small descriptor
/// instead of compressed data) — the grid's output size and how many tiles
/// (referenced by a sibling `iref ▸ dimg`) tile it, in raster order. Actually
/// compositing the tiles into one image is decode-level work this box layer
/// does not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageGrid {
    pub rows: u32,
    pub columns: u32,
    pub output_width: u32,
    pub output_height: u32,
}

impl ImageGrid {
    /// Parse a `grid` item's own payload bytes (not a box — `ImageGrid` has
    /// no four-character-code header of its own).
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut r = vaco_bitstream::ByteReader::new(data);
        let _version = r.u8();
        let flags = r.u8();
        let rows = u32::from(r.u8()).saturating_add(1);
        let columns = u32::from(r.u8()).saturating_add(1);
        let large = flags & 1 != 0;
        let (output_width, output_height) = if large {
            (r.be32(), r.be32())
        } else {
            (u32::from(r.be16()), u32::from(r.be16()))
        };
        r.check().ok()?;
        Some(Self {
            rows,
            columns,
            output_width,
            output_height,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::testutil::{bx, first_box, fullbx};

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Bytes read back from a real `ffmpeg 8.1 -c:v libsvtav1 -f avif`
    /// single-image file (see the module doc): `infe` version 2, item id 1,
    /// type `av01`, name `"Color"`.
    #[test]
    fn infe_matches_a_real_ffmpeg_avif_file() {
        let body = hex_bytes("0001000061763031436f6c6f7200");
        let raw = fullbx(b"infe", 2, 0, &body);
        let info = ItemInfo::parse(&first_box(&raw)).unwrap();
        assert_eq!(info.item_id, 1);
        assert_eq!(info.item_type, FourCc::new(b"av01"));
        assert_eq!(info.name, "Color");
        assert!(!info.hidden);

        let iinf = fullbx(b"iinf", 0, 0, &[&[0, 1], raw.as_slice()].concat());
        assert_eq!(parse_iinf(&first_box(&iinf)), vec![info]);
    }

    #[test]
    fn infe_rejects_an_unknown_version() {
        let body = [
            0, 0, 0, 1, // item_id (the version 3 layout)
            0, 0, // item_protection_index
            b'a', b'v', b'0', b'1', // item_type
            0,    // item_name
        ];
        let raw = fullbx(b"infe", 4, 0, &body);
        assert!(ItemInfo::parse(&first_box(&raw)).is_none());
    }

    #[test]
    fn iinf_refuses_entries_past_its_declared_count() {
        // This is the real AVIF item's `infe`, after an `iinf` claiming no
        // entries. The child must not manufacture an item outside the table.
        let infe = fullbx(b"infe", 2, 0, &hex_bytes("0001000061763031436f6c6f7200"));
        let iinf = fullbx(b"iinf", 0, 0, &[&[0, 0], infe.as_slice()].concat());
        assert!(parse_iinf(&first_box(&iinf)).is_empty());
    }

    /// `iloc` bytes from the same file: version 0, one item, one extent
    /// whose offset/length landed exactly on that file's `mdat` payload.
    #[test]
    fn iloc_matches_a_real_ffmpeg_avif_file() {
        let body = hex_bytes("4400000100010000000100000121000002f9");
        let raw = fullbx(b"iloc", 0, 0, &body);
        let items = parse_iloc(&first_box(&raw));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, 1);
        assert_eq!(items[0].construction_method, ConstructionMethod::FileOffset);
        assert_eq!(items[0].extents, vec![(0x121, 0x2f9)]);
    }

    #[test]
    fn iloc_rejects_an_unsupported_field_width() {
        let body = [
            0x43, 0, // offset_size=4, unsupported length_size=3, base_offset_size=0
            0, 1, // item_count
            0, 1, // item_id
            0, 0, // data_reference_index
            0, 1, // extent_count
            0, 0, 0, 1, // extent_offset
        ];
        let raw = fullbx(b"iloc", 0, 0, &body);
        assert!(parse_iloc(&first_box(&raw)).is_empty());
    }

    #[test]
    fn iloc_rejects_an_unknown_or_reserved_construction_method() {
        for construction_method in [3u16, 0x10] {
            let mut body = vec![
                0x44, 0, // offset_size=4, length_size=4, base_offset_size=0
                0, 1, // item_count
                0, 1, // item_id
            ];
            body.extend_from_slice(&construction_method.to_be_bytes());
            body.extend_from_slice(&[
                0, 0, // data_reference_index
                0, 1, // extent_count
                0, 0, 0, 1, // extent_offset
                0, 0, 0, 1, // extent_length
            ]);
            let raw = fullbx(b"iloc", 1, 0, &body);
            assert!(parse_iloc(&first_box(&raw)).is_empty());
        }
    }

    #[test]
    fn iloc_rejects_an_unknown_version() {
        let body = [
            0x44, 0, // offset_size=4, length_size=4, base_offset_size=0
            0, 0, 0, 1, // item_count (the version 2 layout)
            0, 0, 0, 1, // item_id
            0, 0, // construction_method
            0, 0, // data_reference_index
            0, 1, // extent_count
            0, 0, 0, 1, // extent_offset
            0, 0, 0, 1, // extent_length
        ];
        let raw = fullbx(b"iloc", 3, 0, &body);
        assert!(parse_iloc(&first_box(&raw)).is_empty());
    }

    #[test]
    fn ispe_reports_width_and_height() {
        let body = [0, 0, 0, 0x40, 0, 0, 0, 0x30];
        let raw = fullbx(b"ispe", 0, 0, &body);
        assert_eq!(parse_ispe(&first_box(&raw)), Some((64, 48)));
    }

    #[test]
    fn clap_resolves_exact_integer_edges() {
        let values = [26u32, 1, 6, 1, 1, 1, 0, 1];
        let body = values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect::<Vec<_>>();
        let raw = bx(b"clap", &body);
        let aperture = parse_clap(&first_box(&raw)).unwrap();
        assert_eq!(aperture.integer_crop(30, 8), Some((3, 1, 26, 6)));

        let values = [26u32, 1, 6, 1, u32::MAX, 1, 0, 1];
        let body = values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect::<Vec<_>>();
        let negative_offset = parse_clap(&first_box(&bx(b"clap", &body))).unwrap();
        assert_eq!(negative_offset.integer_crop(30, 8), Some((1, 1, 26, 6)));

        // A half-pixel centre offset is representable when it cancels the
        // half-pixel introduced by an odd input/output size difference.
        let half_offset = CleanAperture {
            width_n: 4,
            width_d: 1,
            height_n: 4,
            height_d: 1,
            horizontal_offset_n: 1,
            horizontal_offset_d: 2,
            vertical_offset_n: 1,
            vertical_offset_d: 2,
        };
        assert_eq!(half_offset.integer_crop(5, 5), Some((1, 1, 4, 4)));
    }

    #[test]
    fn clap_refuses_fractional_or_out_of_bounds_edges() {
        let aperture = CleanAperture {
            width_n: 4,
            width_d: 1,
            height_n: 4,
            height_d: 1,
            horizontal_offset_n: 0,
            horizontal_offset_d: 1,
            vertical_offset_n: 0,
            vertical_offset_d: 1,
        };
        assert_eq!(aperture.integer_crop(5, 5), None, "half-pixel edge");
        assert_eq!(aperture.integer_crop(3, 3), None, "aperture exceeds input");

        let fractional_width = CleanAperture {
            width_n: 7,
            width_d: 2,
            ..aperture
        };
        assert_eq!(fractional_width.integer_crop(5, 5), None);
    }

    /// `ipma` bytes from the same file: one entry, item id 1, four
    /// associations `1, 2, 0x83, 4` — matching that file's `ipco` order
    /// `[ispe, pixi, av1C, colr]`, with `av1C` marked essential.
    #[test]
    fn ipma_matches_a_real_ffmpeg_avif_file() {
        let ipma_body = hex_bytes("0000000100010401028304");
        let ipma = fullbx(b"ipma", 0, 0, &ipma_body);
        let ipco = bx(
            b"ipco",
            &[
                bx(b"ispe", &[]),
                bx(b"pixi", &[]),
                bx(b"av1C", &[]),
                bx(b"colr", &[]),
            ]
            .concat(),
        );
        let mut iprp_body = ipco;
        iprp_body.extend_from_slice(&ipma);
        let iprp = bx(b"iprp", &iprp_body);
        let entry = first_box(&iprp);
        assert_eq!(parse_ipco(&entry).len(), 4);
        let assocs = parse_ipma(&entry);
        assert_eq!(assocs.len(), 1);
        assert_eq!(assocs[0].item_id, 1);
        assert_eq!(
            assocs[0].properties,
            vec![(false, 1), (false, 2), (true, 3), (false, 4)]
        );
    }

    #[test]
    fn ipma_rejects_an_unknown_version() {
        let body = [
            0, 0, 0, 1, // entry_count
            0, 0, 0, 1, // item_id (the version 1 layout)
            1, // association_count
            1, // non-essential property_index
        ];
        let ipma = fullbx(b"ipma", 2, 0, &body);
        let raw = bx(b"iprp", &ipma);
        assert!(parse_ipma(&first_box(&raw)).is_empty());
    }

    #[test]
    fn iref_reads_dimg_references_for_a_grid() {
        // One `dimg` record: from item 1, to items 2 and 3.
        let mut record = 1u16.to_be_bytes().to_vec(); // from_item_id
        record.extend_from_slice(&2u16.to_be_bytes()); // reference_count
        record.extend_from_slice(&2u16.to_be_bytes());
        record.extend_from_slice(&3u16.to_be_bytes());
        let mut body = 0u32.to_be_bytes().to_vec(); // version/flags
        body.extend_from_slice(&bx(b"dimg", &record));
        let raw = bx(b"iref", &body);
        let refs = parse_iref(&first_box(&raw));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, FourCc::new(b"dimg"));
        assert_eq!(refs[0].from_item_id, 1);
        assert_eq!(refs[0].to_item_ids, vec![2, 3]);
    }

    #[test]
    fn image_grid_parses_rows_columns_and_output_size() {
        // version 0, flags 0 (16-bit output size): 1 row, 2 columns
        // (rows_minus_one=0, columns_minus_one=1), output 128x64.
        let data = [0, 0, 0, 1, 0, 128, 0, 64];
        let grid = ImageGrid::parse(&data).unwrap();
        assert_eq!(grid.rows, 1);
        assert_eq!(grid.columns, 2);
        assert_eq!((grid.output_width, grid.output_height), (128, 64));
    }

    #[test]
    fn a_truncated_iloc_never_panics() {
        for n in 0..40 {
            let raw = fullbx(b"iloc", 0, 0, &vec![0u8; n]);
            let _ = parse_iloc(&first_box(&raw));
        }
    }

    #[test]
    fn a_truncated_ipma_never_panics() {
        for n in 0..40 {
            let ipma = fullbx(b"ipma", 0, 0, &vec![0u8; n]);
            let raw = bx(b"iprp", &ipma);
            let _ = parse_ipma(&first_box(&raw));
        }
    }
}
