//! `udta ▸ meta ▸ ilst` iTunes-style tags, cover art, Nero chapters and the
//! `tref ▸ chap` reference that ties a chapter track to its parent.
//!
//! Box bytes come from [`vaco_format_isom::writer`]; this module only decides
//! which boxes to build from [`MuxOptions`].
//!
//! # Reaching this from `Muxer::set_metadata` (M30, gap 1)
//!
//! [`MuxOptions::tags`]/`cover_art`/`chapters` are this crate's own shapes —
//! a fourcc-keyed tag list, not the generic string-keyed
//! [`vaco_format_core::metadata::MuxMetadata`] every muxer in the workspace
//! now receives. [`itunes_fourcc`] is the one measured against `ffmpeg 8.1`
//! (`ffmpeg -metadata title=... -metadata artist=... ... -f mp4 -`,
//! byte-inspected for which `ilst` child each key produced): [`crate::mux`]'s
//! `Muxer::set_metadata` override calls it once per `MuxMetadata::tags` entry
//! and drops a key with no mapping — MP4's `ilst` has no free-text fallback
//! atom this crate writes (`----` `mean`/`name`/`data` triples exist in the
//! format but are not implemented here; a key with no [`itunes_fourcc`] entry
//! is silently omitted rather than guessed at).

use vaco_format_isom::fourcc::FourCc;
use vaco_format_isom::lang::Language;
use vaco_format_isom::writer;

use crate::options::MuxOptions;

/// Map a generic `-metadata`-style key to the iTunes-style `ilst` child atom
/// it becomes, case-insensitively. Measured against `ffmpeg 8.1`: `copyright`
/// and `description` map to plain `cprt`/`desc` (no `0xA9` lead byte), unlike
/// every other text key here.
#[must_use]
pub fn itunes_fourcc(key: &str) -> Option<[u8; 4]> {
    let lower = key.to_ascii_lowercase();
    Some(match lower.as_str() {
        "title" => *b"\xa9nam",
        "artist" => *b"\xa9ART",
        "album_artist" => *b"aART",
        "album" => *b"\xa9alb",
        "comment" => *b"\xa9cmt",
        "genre" => *b"\xa9gen",
        "date" | "year" => *b"\xa9day",
        "composer" => *b"\xa9wrt",
        "copyright" => *b"cprt",
        "description" => *b"desc",
        "encoder" => *b"\xa9too",
        _ => return None,
    })
}

/// Parse a lowercase three-letter ISO-639-2/T code into [`Language`],
/// or `None` for anything else (an empty string, a BCP-47 tag with a
/// region subtag, "und") — `set_metadata` leaves the track's language
/// unchanged in every one of those cases rather than writing a bogus code.
#[must_use]
pub fn parse_iso639(s: &str) -> Option<Language> {
    let bytes = s.as_bytes();
    let [a, b, c] = <[u8; 3]>::try_from(bytes).ok()?;
    if [a, b, c].iter().all(u8::is_ascii_lowercase) {
        Some(Language::Iso639([a, b, c]))
    } else {
        None
    }
}

/// `udta`, built from `opts.tags`/`opts.cover_art`/`opts.chapters`. `None`
/// when there is nothing to write — an empty `udta` is legal but pointless.
#[must_use]
pub fn build_udta(opts: &MuxOptions) -> Option<Vec<u8>> {
    let mut udta_children = Vec::new();

    if !opts.tags.is_empty() || opts.cover_art.is_some() {
        let mut items = Vec::new();
        for (key, value) in &opts.tags {
            items.extend_from_slice(&writer::ilst_text(FourCc::new(key), value));
        }
        if let Some(art) = &opts.cover_art {
            items.extend_from_slice(&writer::covr(art.is_png, &art.data));
        }
        let ilst_bytes = writer::ilst(&items);
        // `mdir`/`mdta` handler type, matching what `ffmpeg 8.1` writes for
        // its iTunes-style `meta` (measured against a file muxed with
        // `-metadata title=...`).
        let hdlr_bytes = writer::hdlr(FourCc::new(b"mdir"), "");
        udta_children.extend_from_slice(&writer::meta(&hdlr_bytes, &ilst_bytes));
    }

    if !opts.chapters.is_empty() {
        let entries: Vec<Vec<u8>> = opts
            .chapters
            .iter()
            .map(|c| {
                let ticks = c.start.ticks().unwrap_or(0).max(0);
                // Nero's `chpl` counts 100 ns units since midnight, from a
                // presentation time in the chapter's own time base.
                let secs = ticks as f64 * c.time_base.to_f64();
                let hundred_ns = (secs * 10_000_000.0).max(0.0) as u64;
                writer::chpl_entry(hundred_ns, &c.title)
            })
            .collect();
        udta_children.extend_from_slice(&writer::chpl(&entries));
    }

    if udta_children.is_empty() {
        None
    } else {
        Some(writer::udta(&udta_children))
    }
}

/// `tref` naming `chapter_track_id` as this track's chapter track.
#[must_use]
pub fn build_chapter_tref(chapter_track_id: u32) -> Vec<u8> {
    let chap = writer::tref_entry(FourCc::new(b"chap"), &[chapter_track_id]);
    writer::tref(&chap)
}
