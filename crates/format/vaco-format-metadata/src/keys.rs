//! The canonical generic metadata key names.
//!
//! These are the key names `-metadata <key>=<value>` accepts and
//! `-show_entries format_tags`/`stream_tags` prints back, measured with
//! `ffprobe 8.1` against real Matroska and MP3/ID3v2 files: writing
//! `-metadata title=…` and reading the container back round-trips the key
//! spelled exactly as below (lower-case, underscore-separated), whatever
//! case or spelling the container's own storage uses internally.
//!
//! This list is **not exhaustive** — it is the set actually measured for
//! this work package, not a transcription of every key the reference
//! recognises. Add to it as a real, measured need appears; do not guess a
//! name from what "seems consistent".

/// Track or file title.
pub const TITLE: &str = "title";
/// Track artist.
pub const ARTIST: &str = "artist";
/// Album title.
pub const ALBUM: &str = "album";
/// Album-level artist, distinct from [`ARTIST`] on a various-artists album.
pub const ALBUM_ARTIST: &str = "album_artist";
/// Release date. The reference accepts free-form text here (`"2024"`,
/// `"2024-01-02"`); it does not itself validate the format.
pub const DATE: &str = "date";
/// Track number, optionally `"N/total"`.
pub const TRACK: &str = "track";
/// Disc number, optionally `"N/total"`.
pub const DISC: &str = "disc";
/// Genre.
pub const GENRE: &str = "genre";
/// Free-text comment.
pub const COMMENT: &str = "comment";
/// Composer.
pub const COMPOSER: &str = "composer";
/// Copyright notice.
pub const COPYRIGHT: &str = "copyright";
/// The encoder that produced the file. Several muxers set this
/// automatically on write and a caller-supplied value competes with that —
/// measured on Matroska, where an explicit `-metadata encoder=…` was
/// overwritten by the muxer's own `Lavf…` string.
pub const ENCODER: &str = "encoder";
/// Free-text description.
pub const DESCRIPTION: &str = "description";
/// Performer, distinct from [`ARTIST`] where a container tracks both
/// (e.g. a classical recording's soloist versus its composer/artist).
pub const PERFORMER: &str = "performer";
/// Publisher or label.
pub const PUBLISHER: &str = "publisher";
/// A stream- or file-level language, an ISO 639-2 code (`"eng"`).
pub const LANGUAGE: &str = "language";

/// Every canonical key this module names, for a caller that wants to
/// enumerate them (`-h` help text, a completion list) rather than reference
/// one by name.
pub const ALL: &[&str] = &[
    TITLE,
    ARTIST,
    ALBUM,
    ALBUM_ARTIST,
    DATE,
    TRACK,
    DISC,
    GENRE,
    COMMENT,
    COMPOSER,
    COPYRIGHT,
    ENCODER,
    DESCRIPTION,
    PERFORMER,
    PUBLISHER,
    LANGUAGE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_lower_case_and_appears_in_all() {
        for &k in ALL {
            assert_eq!(k, k.to_ascii_lowercase());
            assert!(!k.is_empty());
        }
        assert_eq!(ALL.len(), 16);
    }

    #[test]
    fn all_has_no_duplicates() {
        for (i, a) in ALL.iter().enumerate() {
            let Some(rest) = ALL.get(i + 1..) else {
                continue;
            };
            for b in rest {
                assert_ne!(a, b);
            }
        }
    }
}
