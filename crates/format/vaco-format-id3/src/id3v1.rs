//! `ID3v1` and its widely-implemented `ID3v1.1` track-number extension.
//!
//! The last 128 bytes of a file, if they start with `"TAG"`:
//!
//! ```text
//! "TAG"  title:30  artist:30  album:30  year:4  comment:30  genre:1
//! ```
//!
//! `ID3v1.1` (never formally standardised, but universal) repurposes the last
//! two bytes of `comment` as a zero byte followed by a track number when
//! `comment[28] == 0` — leaving 28 usable comment bytes instead of 30.
//! [`Id3v1Tag::parse`] detects this the way every reader does: `comment[28]
//! == 0` is taken as "this is `ID3v1.1`", full stop, since a genuine 30-byte
//! comment ending in two null bytes and a real v1.1 tag are indistinguishable
//! from the bytes alone and every implementation resolves the ambiguity the
//! same way.
//!
//! Text is ISO-8859-1 (there is no encoding byte — no version of `ID3v1`
//! has one), right-padded with `$00` or occasionally spaces; both are
//! trimmed.

use crate::encoding::{self, Encoding};

/// Bytes in an `ID3v1` tag.
pub const LEN: usize = 128;

/// A parsed `ID3v1`/`ID3v1.1` tag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Id3v1Tag {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Kept as the four raw digits (or whatever garbage occupies the field)
    /// rather than parsed to a number: `ffprobe`'s `date` tag is the literal
    /// string, and a non-numeric year should still round-trip rather than
    /// disappear.
    pub year: String,
    pub comment: String,
    /// `Some` only when the `ID3v1.1` convention (`comment[28] == 0`) is
    /// detected.
    pub track: Option<u8>,
    /// The raw genre byte. See [`genre_name`] for the reference's name for
    /// it, which is `None` for `192..=255` — probed, not a guess, per
    /// [`genre_name`]'s own docs.
    pub genre: u8,
}

impl Id3v1Tag {
    /// Parse a 128-byte tag. `data` must be exactly [`LEN`] bytes — the
    /// caller (typically [`crate::skip`] or a demuxer reading the file's own
    /// last 128 bytes) is what locates them; this function does not search
    /// for the `"TAG"` marker itself.
    ///
    /// `None` if `data` is not [`LEN`] bytes or does not start with `"TAG"`.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() != LEN || data.get(..3) != Some(b"TAG") {
            return None;
        }
        let title = latin1_trimmed(data.get(3..33)?);
        let artist = latin1_trimmed(data.get(33..63)?);
        let album = latin1_trimmed(data.get(63..93)?);
        let year = latin1_trimmed(data.get(93..97)?);
        let comment_field = data.get(97..127)?;
        let genre = *data.get(127)?;

        // ID3v1.1: byte 28 of the 30-byte comment field is zero and byte 29
        // holds the track number.
        let is_v11 = comment_field.get(28) == Some(&0);
        let (comment, track) = if is_v11 {
            (
                latin1_trimmed(comment_field.get(..28)?),
                comment_field.get(29).copied().filter(|&t| t != 0),
            )
        } else {
            (latin1_trimmed(comment_field), None)
        };

        Some(Self {
            title,
            artist,
            album,
            year,
            comment,
            track,
            genre,
        })
    }

    /// This tag's fields as `(key, value)` pairs in the order `ffprobe`
    /// prints them, using the same key names as the `ID3v2` frame table
    /// (`crate::frames::metadata_key`) so a caller merging both tags does
    /// not need two vocabularies. Empty fields are omitted, matching the
    /// reference (probed: a blank `ID3v1` field produces no tag at all rather
    /// than an empty-string one).
    #[must_use]
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut push = |k: &str, v: &str| {
            if !v.is_empty() {
                out.push((k.to_string(), v.to_string()));
            }
        };
        push("title", &self.title);
        push("artist", &self.artist);
        push("album", &self.album);
        push("date", &self.year);
        push("comment", &self.comment);
        if let Some(t) = self.track {
            out.push(("track".to_string(), t.to_string()));
        }
        if let Some(name) = genre_name(self.genre) {
            out.push(("genre".to_string(), name.to_string()));
        }
        out
    }
}

fn latin1_trimmed(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = encoding::decode(Encoding::Latin1, bytes.get(..end).unwrap_or(&[]));
    s.trim_end_matches(' ').to_string()
}

use crate::id3v1_genres::ID3V1_GENRES;

/// The reference's name for an `ID3v1` genre byte, or `None`.
///
/// `None` covers `192..=255` deliberately: probed directly (a synthetic
/// `ID3v1` tag for every byte value `0..=255`, `ffprobe -show_entries
/// format_tags=genre`), the reference emits **no** `genre` tag at all once
/// the byte reaches 192 — confirmed at 200 and at 255, the conventional
/// "unspecified" sentinel — rather than an empty string or a numeric
/// fallback. `ID3V1_GENRES` therefore only needs to cover `0..=191`.
#[must_use]
pub fn genre_name(genre: u8) -> Option<&'static str> {
    ID3V1_GENRES.get(usize::from(genre)).copied()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn tag_bytes(title: &str, comment28: &[u8; 28], track: u8, genre: u8) -> Vec<u8> {
        let mut out = b"TAG".to_vec();
        let mut field = |s: &str, len: usize| {
            let mut v = s.as_bytes().to_vec();
            v.resize(len, 0);
            out.extend_from_slice(&v);
        };
        field(title, 30);
        field("Artist", 30);
        field("Album", 30);
        field("2024", 4);
        out.extend_from_slice(comment28);
        out.push(0);
        out.push(track);
        out.push(genre);
        out
    }

    #[test]
    fn parses_a_v11_tag_with_track_number() {
        let mut comment = [0u8; 28];
        comment[..5].copy_from_slice(b"Hello");
        let data = tag_bytes("A Title", &comment, 5, 17);
        let t = Id3v1Tag::parse(&data).unwrap();
        assert_eq!(t.title, "A Title");
        assert_eq!(t.artist, "Artist");
        assert_eq!(t.album, "Album");
        assert_eq!(t.year, "2024");
        assert_eq!(t.comment, "Hello");
        assert_eq!(t.track, Some(5));
        assert_eq!(t.genre, 17);
        assert_eq!(genre_name(t.genre), Some("Rock"));
    }

    #[test]
    fn a_thirty_byte_comment_with_no_trailing_zero_is_plain_v1() {
        // comment[28] is non-zero, so this is not read as ID3v1.1.
        let mut data = tag_bytes("T", &[b'x'; 28], 0, 0);
        // Overwrite the last two comment bytes (indices 125, 126) directly.
        data[125] = b'y';
        data[126] = b'z';
        let t = Id3v1Tag::parse(&data).unwrap();
        assert_eq!(t.track, None);
        assert!(t.comment.ends_with("yz"));
    }

    #[test]
    fn wrong_length_is_none() {
        assert!(Id3v1Tag::parse(&[0; 100]).is_none());
        assert!(Id3v1Tag::parse(&[0; 128]).is_none()); // no "TAG" magic
    }

    #[test]
    fn entries_omit_empty_fields() {
        let data = tag_bytes("", &[0; 28], 0, 255);
        let t = Id3v1Tag::parse(&data).unwrap();
        let entries = t.entries();
        assert!(entries.iter().all(|(k, _)| k != "title"));
        assert!(entries.iter().all(|(k, _)| k != "genre")); // 255 is unmapped
    }

    #[test]
    fn genre_192_and_above_have_no_name() {
        assert_eq!(genre_name(0), Some("Blues"));
        assert_eq!(genre_name(191), Some("Psybient"));
        assert_eq!(genre_name(192), None);
        assert_eq!(genre_name(255), None);
    }
}
