//! Metadata: `ftyp` brands, `udta ▸ meta ▸ ilst`, the `keys`-indexed
//! `QuickTime` form, the 3GPP `udta` boxes, Nero `chpl` chapters and `covr`
//! cover art.
//!
//! `vaco-format-isom` deliberately stops at `Movie::udta`, which hands the box
//! over unparsed (see its *Deferred* list), so the conversion tables live here.
//!
//! Order is output order: `ffprobe` prints format tags in the order they were
//! added, so `ftyp` first, then `ilst` in file order, then the 3GPP boxes.

use vaco_format_isom::boxes::BoxIter;
use vaco_format_isom::fourcc::boxes;
use vaco_format_isom::{FourCc, IsoBox};

/// Largest metadata value kept, before the caller's budget is consulted.
///
/// A `data` box can declare any length its parent admits; a title is not
/// megabytes long, and a value nothing will ever display is not worth the
/// residency.
pub(crate) const MAX_VALUE_BYTES: usize = 64 * 1024;

/// Largest number of `ilst` entries walked.
pub(crate) const MAX_ENTRIES: usize = 4096;

/// A cover image found in `ilst`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CoverArt {
    /// Absolute file offset of the image bytes.
    pub offset: u64,
    /// Their length.
    pub size: u32,
    /// The `data` box's well-known type: 13 JPEG, 14 PNG, 27 BMP.
    pub data_type: u32,
}

/// What one `udta` walk found.
#[derive(Debug, Default)]
pub(crate) struct Metadata {
    pub tags: Vec<(String, String)>,
    pub cover: Option<CoverArt>,
    /// Nero `chpl` chapters: (start in 100 ns units, title).
    pub chapters: Vec<(i64, String)>,
}

/// The iTunes four-character keys, mapped to the canonical names `ffprobe`
/// prints. Interface facts (D9), not expression.
const ILST_KEYS: &[(&[u8; 4], &str)] = &[
    (b"\xa9nam", "title"),
    (b"\xa9ART", "artist"),
    (b"aART", "album_artist"),
    (b"\xa9wrt", "composer"),
    (b"\xa9alb", "album"),
    (b"\xa9day", "date"),
    (b"\xa9cmt", "comment"),
    (b"\xa9gen", "genre"),
    (b"gnre", "genre"),
    (b"\xa9too", "encoder"),
    (b"\xa9enc", "encoded_by"),
    (b"desc", "description"),
    (b"ldes", "synopsis"),
    (b"tvsh", "show"),
    (b"tven", "episode_id"),
    (b"tvnn", "network"),
    (b"purd", "purchase_date"),
    (b"\xa9grp", "grouping"),
    (b"\xa9lyr", "lyrics"),
    (b"cprt", "copyright"),
    (b"\xa9cpy", "copyright"),
    (b"\xa9st3", "subtitle"),
    (b"\xa9dir", "director"),
    (b"\xa9prd", "producer"),
    (b"\xa9wrn", "warning"),
    (b"\xa9swr", "encoder"),
    (b"trkn", "track"),
    (b"disk", "disc"),
    (b"soal", "sort_album"),
    (b"soar", "sort_artist"),
    (b"sonm", "sort_name"),
    (b"\xa9xyz", "location"),
];

/// The 3GPP `udta` boxes (TS 26.244 §8), each a full box whose body is a
/// language code followed by a null-terminated UTF-8 string.
const THREEGPP_KEYS: &[(&[u8; 4], &str)] = &[
    (b"titl", "title"),
    (b"auth", "author"),
    (b"perf", "artist"),
    (b"gnre", "genre"),
    (b"dscp", "description"),
    (b"albm", "album"),
    (b"cprt", "copyright"),
    (b"yrrc", "date"),
    (b"kywd", "keywords"),
];

/// `QuickTime` `udta` string atoms, which are not full boxes and not `ilst`.
const QUICKTIME_UDTA_KEYS: &[(&[u8; 4], &str)] = &[
    (b"\xa9nam", "title"),
    (b"\xa9ART", "artist"),
    (b"\xa9alb", "album"),
    (b"\xa9day", "date"),
    (b"\xa9cmt", "comment"),
    (b"\xa9gen", "genre"),
    (b"\xa9wrt", "composer"),
    (b"\xa9cpy", "copyright"),
    (b"\xa9des", "description"),
    (b"\xa9inf", "comment"),
    (b"\xa9too", "encoder"),
    (b"\xa9swr", "encoder"),
    (b"\xa9mak", "make"),
    (b"\xa9mod", "model"),
    (b"\xa9xyz", "location"),
];

/// The `ID3v1` genre names `gnre`'s one-based index selects.
const ID3_GENRES: &[&str] = &[
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychadelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
];

/// The canonical name for a reverse-DNS `keys` entry, or the entry verbatim.
///
/// Unmapped keys pass through as written, which is what `-export_all` does.
fn keys_name(raw: &str) -> String {
    match raw {
        "com.apple.quicktime.creationdate" => "creation_time".to_owned(),
        "com.apple.quicktime.make" => "make".to_owned(),
        "com.apple.quicktime.model" => "model".to_owned(),
        "com.apple.quicktime.software" => "encoder".to_owned(),
        "com.apple.quicktime.title" => "title".to_owned(),
        "com.apple.quicktime.artist" => "artist".to_owned(),
        "com.apple.quicktime.album" => "album".to_owned(),
        "com.apple.quicktime.comment" => "comment".to_owned(),
        "com.apple.quicktime.description" => "description".to_owned(),
        "com.apple.quicktime.genre" => "genre".to_owned(),
        "com.apple.quicktime.location.ISO6709" => "location".to_owned(),
        other => other.to_owned(),
    }
}

/// Walk a `udta` box, collecting everything a demuxer reports.
pub(crate) fn parse_udta(udta: &IsoBox<'_>, chapters: bool) -> Metadata {
    let mut out = Metadata::default();
    for child in udta.children().flatten().take(MAX_ENTRIES) {
        match child.kind() {
            boxes::META => parse_meta(&child, &mut out),
            boxes::CHPL => {
                if chapters {
                    parse_chpl(&child, &mut out);
                }
            }
            // A `QuickTime` string atom sitting directly under `udta`:
            // `©swr`, `©nam`, `©xyz` and friends. Not a full box — the body is
            // a 16-bit length, a 16-bit language code, then the text. Measured
            // on a `.mov` written by `ffmpeg`, whose `udta` holds only
            // `©swr\x00\rU\xc4Lavf62.12.100`.
            k if k.0.first() == Some(&0xA9) => {
                if let Some(name) = QUICKTIME_UDTA_KEYS
                    .iter()
                    .find(|(code, _)| FourCc::new(code) == k)
                    .map(|&(_, n)| n)
                {
                    let mut r = vaco_bitstream::ByteReader::new(child.payload);
                    let len = usize::from(r.be16());
                    let _language = r.be16();
                    let body = r.bytes(len.min(MAX_VALUE_BYTES));
                    if let Some(v) = text(body)
                        && !v.is_empty()
                    {
                        push(&mut out.tags, name, v);
                    }
                }
            }
            k => {
                if let Some(name) = THREEGPP_KEYS
                    .iter()
                    .find(|(code, _)| FourCc::new(code) == k)
                    .map(|&(_, n)| n)
                    && let Ok(full) = child.full()
                {
                    // language(2) then a null-terminated UTF-8 string, which
                    // may carry a byte-order mark.
                    let body = full.body.get(2..).unwrap_or(&[]);
                    if let Some(v) = text(body)
                        && !v.is_empty()
                    {
                        push(&mut out.tags, name, v);
                    }
                }
            }
        }
    }
    out
}

/// `meta ▸ (hdlr, keys, ilst)`.
///
/// The `meta` box is a full box whose children follow its version and flags —
/// except in `QuickTime`, where it is a plain container. Both are accepted by
/// looking at whether the first four bytes could be a box size.
fn parse_meta(meta: &IsoBox<'_>, out: &mut Metadata) {
    let payload = meta.payload;
    let children = if looks_like_box(payload) {
        BoxIter::new(payload, meta.payload_offset())
    } else {
        BoxIter::new(
            payload.get(4..).unwrap_or(&[]),
            meta.payload_offset().saturating_add(4),
        )
    };
    let mut keys: Vec<String> = Vec::new();
    let mut ilst = None;
    for child in children.flatten().take(MAX_ENTRIES) {
        match child.kind() {
            boxes::KEYS => keys = parse_keys(&child),
            boxes::ILST => ilst = Some(child),
            _ => {}
        }
    }
    if let Some(ilst) = ilst {
        parse_ilst(&ilst, &keys, out);
    }
}

/// Whether `data`'s first eight bytes read as a box header — the test that
/// tells a `QuickTime` `meta` (a plain container) from an ISO one (a full box).
fn looks_like_box(data: &[u8]) -> bool {
    let Some(size) = data.first_chunk::<4>().map(|b| u32::from_be_bytes(*b)) else {
        return false;
    };
    let kind = data.get(4..8).unwrap_or(&[]);
    u64::from(size) >= vaco_format_isom::boxes::HEADER_LEN
        && kind.len() == 4
        && kind.iter().all(|&b| (0x20..=0x7E).contains(&b))
}

/// `keys` — the reverse-DNS key table an `ilst` indexes into.
fn parse_keys(keys: &IsoBox<'_>) -> Vec<String> {
    let Ok(full) = keys.full() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let body = full.body.get(4..).unwrap_or(&[]);
    for entry in BoxIter::new(body, 0).flatten().take(MAX_ENTRIES) {
        // Each entry is `size, namespace(4), key bytes`.
        out.push(
            core::str::from_utf8(entry.payload)
                .unwrap_or_default()
                .to_owned(),
        );
    }
    out
}

/// `ilst` — the value list.
fn parse_ilst(ilst: &IsoBox<'_>, keys: &[String], out: &mut Metadata) {
    for (i, entry) in ilst.children().flatten().take(MAX_ENTRIES).enumerate() {
        let _ = i;
        let code = entry.kind();
        // A `keys`-indexed entry's type is a one-based index into `keys`.
        let indexed = (!keys.is_empty())
            .then(|| u32::from_be_bytes(code.0))
            .filter(|n| *n >= 1 && (*n as usize) <= keys.len())
            .and_then(|n| keys.get((n as usize).saturating_sub(1)))
            .map(|k| keys_name(k));

        if code == FourCc::new(b"----") {
            if let Some((name, value)) = parse_freeform(&entry) {
                push(&mut out.tags, &name, value);
            }
            continue;
        }
        let Some(data) = entry.children().find(boxes::DATA) else {
            continue;
        };
        let mut r = vaco_bitstream::ByteReader::new(data.payload);
        let type_and_flags = r.be32();
        let _locale = r.be32();
        let body = data.payload.get(8..).unwrap_or(&[]);
        let data_type = type_and_flags & 0x00FF_FFFF;

        if code == FourCc::new(b"covr") {
            let len = body.len().min(u32::MAX as usize) as u32;
            if len > 0 && out.cover.is_none() {
                out.cover = Some(CoverArt {
                    offset: data.payload_offset().saturating_add(8),
                    size: len,
                    data_type,
                });
            }
            continue;
        }
        let name = indexed.or_else(|| {
            ILST_KEYS
                .iter()
                .find(|(k, _)| FourCc::new(k) == code)
                .map(|&(_, n)| n.to_owned())
        });
        let Some(name) = name else { continue };
        if let Some(value) = decode_value(code, data_type, body) {
            push(&mut out.tags, &name, value);
        }
    }
}

/// A `----` freeform atom: `mean` (namespace), `name` (key), `data` (value).
fn parse_freeform(entry: &IsoBox<'_>) -> Option<(String, String)> {
    let name = entry.children().find(boxes::NAME)?;
    let key = core::str::from_utf8(name.payload.get(4..)?)
        .ok()?
        .to_owned();
    let data = entry.children().find(boxes::DATA)?;
    let body = data.payload.get(8..)?;
    Some((key, text(body)?))
}

/// Decode one `data` payload according to its well-known type.
fn decode_value(code: FourCc, data_type: u32, body: &[u8]) -> Option<String> {
    if code == FourCc::new(b"trkn") || code == FourCc::new(b"disk") {
        // `00 00 <index:u16> <total:u16>`; the total is omitted when zero,
        // which is what `ffprobe` prints.
        let n = u16::from_be_bytes(*body.get(2..4)?.first_chunk::<2>()?);
        let total = body
            .get(4..6)
            .and_then(<[u8]>::first_chunk::<2>)
            .map_or(0, |b| u16::from_be_bytes(*b));
        return Some(if total == 0 {
            n.to_string()
        } else {
            format!("{n}/{total}")
        });
    }
    if code == FourCc::new(b"gnre") && data_type == 0 {
        let n = u16::from_be_bytes(*body.first_chunk::<2>()?);
        let name = ID3_GENRES.get(usize::from(n).checked_sub(1)?)?;
        return Some((*name).to_owned());
    }
    match data_type {
        // 1 UTF-8, 4 UTF-8 sort, 18 ISO-8859-1 treated as UTF-8 where valid.
        1 | 4 | 18 => text(body),
        21 => Some(signed_int(body)?.to_string()),
        22 => Some(unsigned_int(body)?.to_string()),
        _ => None,
    }
}

fn signed_int(body: &[u8]) -> Option<i64> {
    Some(match body.len() {
        1 => i64::from((*body.first()?).cast_signed()),
        2 => i64::from(i16::from_be_bytes(*body.first_chunk::<2>()?)),
        4 => i64::from(i32::from_be_bytes(*body.first_chunk::<4>()?)),
        8 => i64::from_be_bytes(*body.first_chunk::<8>()?),
        _ => return None,
    })
}

fn unsigned_int(body: &[u8]) -> Option<u64> {
    Some(match body.len() {
        1 => u64::from(*body.first()?),
        2 => u64::from(u16::from_be_bytes(*body.first_chunk::<2>()?)),
        4 => u64::from(u32::from_be_bytes(*body.first_chunk::<4>()?)),
        8 => u64::from_be_bytes(*body.first_chunk::<8>()?),
        _ => return None,
    })
}

/// Bytes to a `String`: UTF-8, trimmed at the first NUL, byte-order mark
/// removed, and length-capped.
fn text(body: &[u8]) -> Option<String> {
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    let slice = body.get(..end.min(MAX_VALUE_BYTES))?;
    let slice = slice.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(slice);
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// Nero `chpl`: a full box holding chapter start times in 100 ns units.
///
/// Version 1 inserts a four-byte field before the count. Both layouts occur.
fn parse_chpl(chpl: &IsoBox<'_>, out: &mut Metadata) {
    let Ok(full) = chpl.full() else { return };
    let mut r = vaco_bitstream::ByteReader::new(full.body);
    if full.version == 1 {
        let _reserved = r.be32();
    }
    let count = usize::from(r.u8());
    for _ in 0..count.min(MAX_ENTRIES) {
        if r.remaining() < 9 {
            break;
        }
        let start = r.be64().cast_signed();
        let len = usize::from(r.u8());
        let title = r.bytes(len.min(MAX_VALUE_BYTES));
        out.chapters
            .push((start, String::from_utf8_lossy(title).into_owned()));
    }
}

/// Append `key=value`, preserving duplicates the way a container does.
fn push(tags: &mut Vec<(String, String)>, key: &str, value: String) {
    if tags.iter().any(|(k, _)| k == key) {
        return;
    }
    tags.push((key.to_owned(), value));
}

/// The `ftyp` tags, which the reference prints before everything else.
pub(crate) fn file_type_tags(ft: &vaco_format_isom::FileType) -> Vec<(String, String)> {
    let mut compatible = String::new();
    for b in &ft.compatible_brands {
        compatible.push_str(&String::from_utf8_lossy(&b.0));
    }
    vec![
        (
            "major_brand".to_owned(),
            String::from_utf8_lossy(&ft.major_brand.0).into_owned(),
        ),
        ("minor_version".to_owned(), ft.minor_version.to_string()),
        ("compatible_brands".to_owned(), compatible),
    ]
}
