//! Generic Container essence element keys: [`GC_ESSENCE_PREFIX`] plus a
//! 4-byte per-track "track number", matched verbatim against the Track's
//! own `EssenceTrackNumber` property — the write side of
//! `vaco-demux-mxf::essence`'s read-side rule.

use vaco_core::MediaType;

use crate::ul::GC_ESSENCE_PREFIX;

/// Item-type byte (key index 12): `0x15` frame-wrapped Picture, `0x16`
/// frame-wrapped Sound (ST 379-1 Table 1; Picture is measured against a
/// real file in `vaco-demux-mxf::essence`'s module docs, Sound follows the
/// same table one item-type higher and is not separately measured there
/// since this crate's own reader matches by track number, not by
/// interpreting the item-type byte's meaning).
const ITEM_TYPE_PICTURE_FRAME_WRAPPED: u8 = 0x15;
const ITEM_TYPE_SOUND_FRAME_WRAPPED: u8 = 0x16;

/// Assign a Generic Container track number for the `n`th (0-based) essence
/// track of `media_type` in this file. Only needs to be unique within the
/// file and to equal the number this crate itself writes onto the matching
/// Track's `EssenceTrackNumber` property — see the module docs.
#[must_use]
pub(crate) fn track_number(media_type: MediaType, n: u32) -> [u8; 4] {
    let item_type = match media_type {
        MediaType::Audio => ITEM_TYPE_SOUND_FRAME_WRAPPED,
        _ => ITEM_TYPE_PICTURE_FRAME_WRAPPED,
    };
    [item_type, 0x01, (n + 1) as u8, 0x00]
}

/// Build one essence element's full 16-byte key from its track number.
#[must_use]
pub(crate) fn essence_key(track_number: [u8; 4]) -> [u8; 16] {
    let p = GC_ESSENCE_PREFIX;
    [
        p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], track_number[0],
        track_number[1], track_number[2], track_number[3],
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn track_numbers_differ_by_media_type_and_index() {
        assert_eq!(track_number(MediaType::Video, 0), [0x15, 0x01, 0x01, 0x00]);
        assert_eq!(track_number(MediaType::Audio, 0), [0x16, 0x01, 0x01, 0x00]);
    }
}
