//! HEIF/AVIF still images (ISO/IEC 23008-12): a file whose pictures are
//! `meta`-box *items* rather than `moov` tracks.
//!
//! Every coded image item becomes one video stream carrying exactly one
//! packet, and every `grid` derived item becomes a [`StreamGroup`] naming
//! the tile streams that compose it. The shape — hidden tiles exposed as
//! ordinary streams, the grid itself present only as a group, `time_base`
//! `1/1`, one frame at `pts 0` with `duration 1` — is **measured** against
//! `ffprobe 9.0.1` on an `ffmpeg -f avif` single image and on a
//! self-constructed 2×2 `grid` file that ffmpeg itself decodes to the
//! expected composite (see `docs/format/vaco-demux-mp4.md`).

use std::collections::VecDeque;

use vaco_core::{Disposition, MediaType, Rational, Timestamp};
use vaco_format_core::{Stream, StreamGroup, StreamGroupIndex, StreamGroupKind, TileGrid};
use vaco_format_isom::FourCc;
use vaco_format_isom::boxes::IsoBox;
use vaco_format_isom::fourcc::boxes as bt;
use vaco_format_isom::heif::{
    self, ConstructionMethod, ImageGrid, ItemInfo, ItemLocation, ItemPropertyAssociation,
    ItemReference,
};
use vaco_format_isom::stsd::{SampleEntry, VisualSampleEntry};
use vaco_limits::Budget;

use crate::read::{Reader, Source};
use crate::track;

/// A still image's own timeline: one tick, one frame.
pub(crate) const ITEM_TIME_BASE: Rational = Rational::new(1, 1);

/// Largest `grid` descriptor read from the file. `ImageGrid` is 8 or 12
/// bytes; this is a bound on a corrupt `iloc`, not a real maximum.
const MAX_GRID_DESCRIPTOR_BYTES: u64 = 64;

/// What one `meta` box yielded.
pub(crate) struct ItemStreams {
    pub streams: Vec<Stream>,
    pub readers: Vec<Reader>,
    pub groups: Vec<StreamGroup>,
}

/// Read a `meta` box's items. `read` fetches file bytes for an extent list
/// (a `grid` descriptor stored in `mdat` rather than `idat`), and may fail.
pub(crate) fn build(
    meta: &IsoBox<'_>,
    budget: &Budget,
    file_size: Option<u64>,
    read: &mut dyn FnMut(&[(u64, u64)]) -> Option<Vec<u8>>,
) -> ItemStreams {
    let mut out = ItemStreams {
        streams: Vec::new(),
        readers: Vec::new(),
        groups: Vec::new(),
    };
    let Some(parsed) = Meta::parse(meta) else {
        return out;
    };

    // Coded items become streams in `iinf` order — hidden ones included,
    // because a grid's tiles are hidden and they are exactly what has to be
    // read. Non-coded items (`grid`, `iovl`, `Exif`, `mime`) do not.
    // A grid's tiles carry `dependent` — **measured**: `ffprobe` prints
    // `dependent=1` on every `dimg`-referenced tile stream and on nothing else.
    let tiles: Vec<u32> = parsed
        .irefs
        .iter()
        .filter(|r| r.kind == bt::DIMG)
        .flat_map(|r| r.to_item_ids.iter().copied())
        .collect();
    let mut stream_of_item: Vec<(u32, u32)> = Vec::new();
    for info in &parsed.infos {
        if u64::from(out.streams.len() as u32) >= u64::from(budget.limits().max_streams) {
            break;
        }
        let Some(location) = parsed.location(info.item_id) else {
            continue;
        };
        let Some(extents) = parsed.resolve(location, file_size) else {
            continue;
        };
        let index = out.streams.len() as u32;
        let Some(mut stream) = parsed.coded_stream(info, index, budget) else {
            continue;
        };
        if tiles.contains(&info.item_id) {
            stream.disposition |= Disposition::DEPENDENT;
        }
        out.streams.push(stream);
        out.readers.push(item_reader(index, extents));
        stream_of_item.push((info.item_id, index));
    }

    for info in &parsed.infos {
        if info.item_type != bt::GRID {
            continue;
        }
        let Some(group) = parsed.grid_group(
            info,
            &out.streams,
            &stream_of_item,
            out.groups.len() as u32,
            file_size,
            read,
        ) else {
            continue;
        };
        out.groups.push(group);
    }
    out
}

fn item_reader(index: u32, extents: Vec<(u64, u64)>) -> Reader {
    Reader {
        stream_index: index,
        time_base: ITEM_TIME_BASE,
        media_type: MediaType::Video,
        dts_shift: 0,
        edit_shift: 0,
        trim_point: i64::MIN,
        trim_end: i64::MAX,
        frame_samples: 0,
        source: Source::Item {
            extents,
            emitted: false,
        },
        entries: Vec::new(),
        queue: VecDeque::new(),
        batch: 1,
        finished: false,
        blocked: false,
        encryption_error: None,
        raw_pcm: false,
        decrypt: None,
    }
}

/// The parsed `meta` children this module reads.
struct Meta<'a> {
    primary: Option<u32>,
    infos: Vec<ItemInfo>,
    locations: Vec<ItemLocation>,
    ipco: Vec<IsoBox<'a>>,
    ipma: Vec<ItemPropertyAssociation>,
    irefs: Vec<ItemReference>,
    /// `idat`'s payload and its absolute file offset, for
    /// `construction_method == 1`.
    idat: Option<(&'a [u8], u64)>,
}

impl<'a> Meta<'a> {
    /// `None` unless `hdlr` says `pict` — a `meta` box is also where a
    /// QuickTime file keeps `mdta` metadata, which has no items at all.
    fn parse(meta: &IsoBox<'a>) -> Option<Self> {
        let children = meta.children_after(4);
        let mut me = Self {
            primary: None,
            infos: Vec::new(),
            locations: Vec::new(),
            ipco: Vec::new(),
            ipma: Vec::new(),
            irefs: Vec::new(),
            idat: None,
        };
        let mut handler = None;
        for child in children.flatten() {
            match child.kind() {
                bt::HDLR => {
                    let full = child.full().ok()?;
                    let mut r = full.reader();
                    let _pre_defined = r.be32();
                    handler = Some(FourCc(<[u8; 4]>::try_from(r.bytes(4)).unwrap_or([0; 4])));
                }
                bt::PITM => me.primary = heif::parse_pitm(&child),
                bt::IINF => me.infos = heif::parse_iinf(&child),
                bt::ILOC => me.locations = heif::parse_iloc(&child),
                bt::IPRP => {
                    me.ipco = heif::parse_ipco(&child);
                    me.ipma = heif::parse_ipma(&child);
                }
                bt::IREF => me.irefs = heif::parse_iref(&child),
                bt::IDAT => me.idat = Some((child.payload, child.payload_offset())),
                _ => {}
            }
        }
        (handler == Some(bt::PICT)).then_some(me)
    }

    fn location(&self, item_id: u32) -> Option<&ItemLocation> {
        self.locations.iter().find(|l| l.item_id == item_id)
    }

    fn property(&self, item_id: u32, kind: FourCc) -> Option<&IsoBox<'a>> {
        let assoc = self.ipma.iter().find(|a| a.item_id == item_id)?;
        assoc.properties.iter().find_map(|&(_, index)| {
            let bx = usize::from(index)
                .checked_sub(1)
                .and_then(|i| self.ipco.get(i))?;
            (bx.kind() == kind).then_some(bx)
        })
    }

    /// An item's extents as absolute file ranges. `None` for an item this
    /// crate cannot place: `construction_method == 2` (offsets into another
    /// item's data), a `dref`-external item, or a range past the file.
    fn resolve(&self, location: &ItemLocation, file_size: Option<u64>) -> Option<Vec<(u64, u64)>> {
        if location.extents.is_empty() || location.data_reference_index != 0 {
            return None;
        }
        let base = match location.construction_method {
            ConstructionMethod::FileOffset => 0,
            ConstructionMethod::IdatOffset => self.idat?.1,
            ConstructionMethod::ItemOffset => return None,
        };
        let mut out = Vec::new();
        for &(offset, length) in &location.extents {
            let start = base.checked_add(offset)?;
            let end = start.checked_add(length)?;
            if length == 0 || file_size.is_some_and(|size| end > size) {
                return None;
            }
            if location.construction_method == ConstructionMethod::IdatOffset {
                let (idat, idat_offset) = self.idat?;
                if end > idat_offset.saturating_add(idat.len() as u64) {
                    return None;
                }
            }
            out.push((start, length));
        }
        Some(out)
    }

    /// The `ipco` boxes `ipma` associates with an item, re-serialised as a
    /// contiguous run of boxes so the sample-entry reader can treat them as
    /// the extension boxes they are the item-world spelling of.
    fn properties(&self, item_id: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let Some(assoc) = self.ipma.iter().find(|a| a.item_id == item_id) else {
            return out;
        };
        for &(_essential, index) in &assoc.properties {
            let Some(bx) = usize::from(index)
                .checked_sub(1)
                .and_then(|i| self.ipco.get(i))
            else {
                continue;
            };
            let size = u32::try_from(bx.payload.len().saturating_add(8)).unwrap_or(u32::MAX);
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(&bx.kind().0);
            out.extend_from_slice(bx.payload);
        }
        out
    }

    fn ispe(&self, item_id: u32) -> Option<(u32, u32)> {
        heif::parse_ispe(self.property(item_id, bt::ISPE)?)
    }

    fn clap(&self, item_id: u32) -> Option<heif::CleanAperture> {
        heif::parse_clap(self.property(item_id, bt::CLAP)?)
    }

    /// One coded item as a stream. `None` when the item type names no codec
    /// this crate's sample-entry table knows, so a derived or metadata item
    /// never becomes a stream nobody can decode.
    fn coded_stream(&self, info: &ItemInfo, index: u32, budget: &Budget) -> Option<Stream> {
        let (width, height) = self.ispe(info.item_id).unwrap_or((0, 0));
        let extensions = self.properties(info.item_id);
        let entry = SampleEntry {
            format: info.item_type,
            data_reference_index: 1,
            visual: Some(VisualSampleEntry {
                width: u16::try_from(width).unwrap_or(u16::MAX),
                height: u16::try_from(height).unwrap_or(u16::MAX),
                frame_count: 1,
                depth: 0x18,
                ..VisualSampleEntry::default()
            }),
            audio: None,
            tmcd: None,
            extensions: &extensions,
            extensions_offset: 0,
        };
        entry.codec()?;
        let mut params = track::codec_parameters_with_display(&entry, Some(MediaType::Video), None);
        if let Some(v) = params.video.as_mut() {
            // `ispe` is 32 bits wide; the 16-bit sample-entry fields above
            // were only ever a vehicle for the shared reader.
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.frame_rate = ITEM_TIME_BASE;
            // **Measured**: `ffprobe` prints `sample_aspect_ratio=1:1` for an
            // AVIF item with no `pasp` property, where the same codec in a
            // `moov` track prints nothing until a `pasp` or the decoder says.
            if v.sample_aspect_ratio.is_zero() {
                v.sample_aspect_ratio = Rational::ONE;
            }
        }
        params.validate(budget).ok()?;

        let mut stream = Stream::new(index, MediaType::Video, ITEM_TIME_BASE);
        stream.id = Some(i64::from(info.item_id));
        stream.params = params;
        stream.start_time = Timestamp::ZERO;
        stream.frame_count = Some(1);
        stream.r_frame_rate = ITEM_TIME_BASE;
        stream.avg_frame_rate = ITEM_TIME_BASE;
        if self.primary == Some(info.item_id) {
            stream.disposition = Disposition::DEFAULT;
        }
        if !info.name.is_empty() {
            stream
                .metadata
                .push(("title".to_owned(), info.name.clone()));
        }
        Some(stream)
    }

    /// A `grid` item as a tile-grid group over the streams its `dimg`
    /// references name. `None` — no group, rather than a wrong one — when a
    /// tile is not a stream, the tile count does not match `rows × columns`,
    /// or the descriptor cannot be read.
    fn grid_group(
        &self,
        info: &ItemInfo,
        streams: &[Stream],
        stream_of_item: &[(u32, u32)],
        group_index: u32,
        file_size: Option<u64>,
        read: &mut dyn FnMut(&[(u64, u64)]) -> Option<Vec<u8>>,
    ) -> Option<StreamGroup> {
        let location = self.location(info.item_id)?;
        let extents = self.resolve(location, file_size)?;
        let total: u64 = extents.iter().map(|e| e.1).sum();
        if total > MAX_GRID_DESCRIPTOR_BYTES {
            return None;
        }
        let bytes = match (location.construction_method, self.idat) {
            (ConstructionMethod::IdatOffset, Some((idat, idat_offset))) => {
                let mut v = Vec::new();
                for &(start, len) in &extents {
                    let at = usize::try_from(start.checked_sub(idat_offset)?).ok()?;
                    let n = usize::try_from(len).ok()?;
                    v.extend_from_slice(idat.get(at..at.checked_add(n)?)?);
                }
                v
            }
            _ => read(&extents)?,
        };
        let grid = ImageGrid::parse(&bytes)?;

        let tiles = self
            .irefs
            .iter()
            .find(|r| r.kind == bt::DIMG && r.from_item_id == info.item_id)?;
        let expected = usize::try_from(u64::from(grid.rows) * u64::from(grid.columns)).ok()?;
        if tiles.to_item_ids.len() != expected {
            return None;
        }
        let mut members = Vec::new();
        for id in &tiles.to_item_ids {
            let (_, index) = stream_of_item.iter().find(|(item, _)| item == id)?;
            members.push(*index);
        }
        let first_index = *members.first()?;
        let first = streams.iter().find(|s| s.index == first_index)?;
        let (tile_w, tile_h) = first.params.video.as_ref().map(|v| (v.width, v.height))?;
        if tile_w == 0 || tile_h == 0 {
            return None;
        }
        // Every tile has the same size (§6.6.2.3.1); the canvas is the
        // tiles laid edge to edge, and the output is cropped from it.
        let coded_width = tile_w.checked_mul(grid.columns)?;
        let coded_height = tile_h.checked_mul(grid.rows)?;
        if grid.output_width > coded_width || grid.output_height > coded_height {
            return None;
        }
        let (horizontal_offset, vertical_offset, output_width, output_height) =
            self.clap(info.item_id).map_or(
                Some((0, 0, grid.output_width, grid.output_height)),
                |aperture| aperture.integer_crop(grid.output_width, grid.output_height),
            )?;
        let tile_offsets = (0..grid.rows)
            .flat_map(|r| (0..grid.columns).map(move |c| (c * tile_w, r * tile_h)))
            .collect();

        let mut group = StreamGroup::new(
            StreamGroupIndex(group_index),
            StreamGroupKind::TileGrid(TileGrid {
                tile_rows: grid.rows,
                tile_columns: grid.columns,
                coded_width,
                coded_height,
                output_width,
                output_height,
                horizontal_offset,
                vertical_offset,
                tile_offsets,
            }),
        );
        group.id = i64::from(info.item_id);
        group.stream_indices = members;
        if self.primary == Some(info.item_id) {
            group.disposition = Disposition::DEFAULT;
        }
        if !info.name.is_empty() {
            group.metadata.push(("title".to_owned(), info.name.clone()));
        }
        Some(group)
    }
}
