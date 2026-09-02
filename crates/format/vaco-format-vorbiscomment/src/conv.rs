//! The Vorbis-comment `MetadataConv` table.
//!
//! Measured with `ffprobe`/`ffmpeg 8.1` by writing each canonical
//! [`vaco_format_metadata::keys`] key with `-metadata` into a real
//! `-c:a flac` file and reading back its `VORBIS_COMMENT` block: most keys
//! round-trip **unchanged** (`title`, `artist`, `album`, `date`, `genre`,
//! `comment`, `composer`, `copyright`, `encoder`, `description`,
//! `performer`, `publisher` all appear verbatim, lower-case, in the file the
//! reference's own FLAC muxer writes). [`TABLE`] therefore only lists the
//! keys that are genuinely renamed — an unmapped key already passes through
//! unchanged, which is the correct behaviour for the rest, not merely the
//! default one.

use vaco_format_metadata::{ConvEntry, MetadataConv, keys};

/// The measured renames. `track`/`disc` become the community-standard
/// `TRACKNUMBER`/`DISCNUMBER` (not `TRACK`/`DISC`, and not the generic
/// spelling with a suffix), and `album_artist` loses its underscore.
pub const TABLE: MetadataConv = MetadataConv(&[
    ConvEntry {
        generic: keys::TRACK,
        native: "TRACKNUMBER",
    },
    ConvEntry {
        generic: keys::DISC,
        native: "DISCNUMBER",
    },
    ConvEntry {
        generic: keys::ALBUM_ARTIST,
        native: "ALBUMARTIST",
    },
]);

#[cfg(test)]
mod tests {
    use super::*;
    use vaco_format_metadata::Direction;

    #[test]
    fn the_measured_renames_map_both_ways() {
        assert_eq!(TABLE.to_native("track"), Some("TRACKNUMBER"));
        assert_eq!(TABLE.to_native("disc"), Some("DISCNUMBER"));
        assert_eq!(TABLE.to_native("album_artist"), Some("ALBUMARTIST"));
        assert_eq!(TABLE.to_generic("TRACKNUMBER"), Some("track"));
        assert_eq!(
            TABLE.to_generic("trackNUMBER"),
            Some("track"),
            "case-insensitive"
        );
    }

    #[test]
    fn every_other_measured_key_passes_through() {
        for key in [
            keys::TITLE,
            keys::ARTIST,
            keys::ALBUM,
            keys::DATE,
            keys::GENRE,
            keys::COMMENT,
            keys::COMPOSER,
            keys::COPYRIGHT,
            keys::ENCODER,
            keys::DESCRIPTION,
            keys::PERFORMER,
            keys::PUBLISHER,
        ] {
            assert_eq!(
                TABLE.to_native(key),
                None,
                "{key} is renamed but was not measured to be"
            );
            assert_eq!(
                TABLE.map_key(key, Direction::ToNative).as_ref(),
                key,
                "an unmapped key must pass through unchanged"
            );
        }
    }
}
