//! `udta ▸ meta ▸ ilst` iTunes-style tags, cover art, Nero chapters and the
//! `tref ▸ chap` reference that ties a chapter track to its parent.
//!
//! Box bytes come from [`vaco_format_isom::writer`]; this module only decides
//! which boxes to build from [`MuxOptions`].

use vaco_format_isom::fourcc::FourCc;
use vaco_format_isom::writer;

use crate::options::MuxOptions;

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
