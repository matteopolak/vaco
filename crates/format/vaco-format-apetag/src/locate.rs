//! Finding an APE tag at the start or end of a file, honouring the `ID3v1`
//! coexistence rule (module docs on [`crate::tag`]).

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_io::IoContext;
use vaco_limits::Budget;

use crate::tag::{ApeTag, FOOTER_LEN, ID3V1_LEN, PREAMBLE};

/// The byte range of an APE tag found at the end of `data`, before any
/// caller-supplied trailing bytes are stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// Offset of the first byte of the tag (its header, if it has one,
    /// otherwise its first item).
    pub start: usize,
    /// Offset one past the tag's footer — i.e. where an `ID3v1` tag, if any,
    /// begins.
    pub end: usize,
    /// Whether an `ID3v1` tag (128 bytes, stripped from the search first)
    /// follows the APE tag in the file.
    pub id3v1_follows: bool,
}

/// Look for a trailing APE tag in `data` (the whole file, or at least its
/// tail).
///
/// Steps back over a trailing `ID3v1` tag first — the coexistence rule — so a
/// footer sitting immediately before one is still found. Returns `None` when
/// no `"APETAGEX"` footer is present at either candidate position.
#[must_use]
pub fn find_trailing(data: &[u8]) -> Option<Found> {
    let flen = usize::try_from(FOOTER_LEN).unwrap_or(32);
    let id3_len = usize::try_from(ID3V1_LEN).unwrap_or(128);

    let has_id3v1 = data.len() >= id3_len
        && data
            .get(data.len() - id3_len..data.len() - id3_len + 3)
            .is_some_and(|tag| tag == b"TAG");
    let search_end = if has_id3v1 {
        data.len().saturating_sub(id3_len)
    } else {
        data.len()
    };

    if search_end < flen {
        return None;
    }
    let footer = data.get(search_end - flen..search_end)?;
    if footer.get(..8) != Some(PREAMBLE.as_slice()) {
        return None;
    }
    let mut r = ByteReader::new(footer);
    r.skip(8); // preamble
    let _version = r.le32();
    let tag_size = r.le32();
    let _item_count = r.le32();
    let flags = r.le32();
    if r.check().is_err() {
        return None;
    }
    let has_header = flags & (1 << 31) != 0;
    // `tag_size` counts from the first item to the end of the footer,
    // inclusive, so the item list plus footer occupy exactly `items_len`
    // bytes ending at `search_end`. A header, if present, is a further
    // `FOOTER_LEN` bytes immediately before that — never trusted past what
    // the buffer actually holds.
    let items_len = usize::try_from(tag_size)
        .unwrap_or(usize::MAX)
        .min(search_end);
    let content_start = search_end.saturating_sub(items_len);
    let header_len = usize::from(has_header) * flen;
    let start = content_start.saturating_sub(header_len);
    Some(Found {
        start,
        end: search_end,
        id3v1_follows: has_id3v1,
    })
}

/// Parse the tag [`find_trailing`] locates in `data`, if any.
///
/// # Errors
/// [`Error::InvalidData`]/[`Error::LimitExceeded`] from [`ApeTag::parse`].
pub fn parse_trailing(data: &[u8], budget: &mut Budget) -> Result<Option<ApeTag>> {
    let Some(found) = find_trailing(data) else {
        return Ok(None);
    };
    let slice = data
        .get(found.start..found.end)
        .ok_or(Error::InvalidData("apetag: located range out of bounds"))?;
    Ok(Some(ApeTag::parse(slice, budget)?))
}

/// The [`find_trailing`]/[`parse_trailing`] logic driven from a seekable
/// [`IoContext`] instead of an in-memory slice, for a demuxer that does not
/// want to hold the whole file in memory.
///
/// Reads only the trailing [`FOOTER_LEN`] + [`ID3V1_LEN`] bytes first to
/// decide whether a tag is present at all, then (only if so) the tag's own
/// declared length. Leaves the source's position unspecified on return —
/// callers that care should record and restore it themselves, the same
/// convention `vaco_format_id3::skip::detect` documents for the analogous
/// leading-tag case.
///
/// # Errors
/// [`Error::NotSeekable`] if `io` cannot seek from the end.
/// Otherwise as [`ApeTag::parse`].
pub fn read_trailing(io: &mut IoContext, budget: &mut Budget) -> Result<Option<ApeTag>> {
    let flen = usize::try_from(FOOTER_LEN).unwrap_or(32);
    let id3_len = usize::try_from(ID3V1_LEN).unwrap_or(128);
    let Some(size) = io.size() else {
        return Ok(None);
    };
    let size = usize::try_from(size).unwrap_or(usize::MAX);

    // First probe: just enough to see a footer and decide whether an ID3v1
    // tag follows it, without yet knowing the tag's real length.
    let probe_len = (flen + id3_len).min(size);
    if probe_len < flen {
        return Ok(None);
    }
    io.seek_from_end(probe_len as u64)?;
    let mut probe = budget.alloc::<u8>(probe_len)?;
    io.read_exact(&mut probe)?;
    let Some(found) = find_trailing(&probe) else {
        return Ok(None);
    };

    // Now that the footer's own `tag_size` (and header flag) are known, fetch
    // exactly the span the tag occupies, re-reading from the true start if
    // the initial probe window did not reach it (a large embedded item can
    // make the tag longer than `probe_len`).
    let tag_len = found.end.saturating_sub(found.start);
    let trailer_len = if found.id3v1_follows { id3_len } else { 0 };
    let total = tag_len.saturating_add(trailer_len).min(size);
    io.seek_from_end(total as u64)?;
    let mut full = budget.alloc::<u8>(total)?;
    io.read_exact(&mut full)?;
    parse_trailing(&full, budget)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::tag::ApeItem;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::permissive())
    }

    #[test]
    fn a_footer_only_tag_at_eof_is_found() {
        let tag = ApeTag {
            version: 2000,
            items: vec![ApeItem::text("Title", "X")],
        };
        let mut file = b"fake audio data".to_vec();
        file.extend_from_slice(&tag.to_bytes().unwrap());
        let found = find_trailing(&file).unwrap();
        assert!(!found.id3v1_follows);
        assert_eq!(found.end, file.len());
        let parsed = parse_trailing(&file, &mut budget()).unwrap().unwrap();
        assert_eq!(parsed.get("title").unwrap().text_lossy(), "X");
    }

    #[test]
    fn a_tag_before_an_id3v1_tag_is_still_found() {
        let tag = ApeTag {
            version: 2000,
            items: vec![ApeItem::text("Artist", "Y")],
        };
        let mut file = b"fake audio data".to_vec();
        file.extend_from_slice(&tag.to_bytes().unwrap());
        let mut id3v1 = vec![0u8; 128];
        id3v1[0..3].copy_from_slice(b"TAG");
        file.extend_from_slice(&id3v1);

        let found = find_trailing(&file).unwrap();
        assert!(found.id3v1_follows);
        assert_eq!(found.end, file.len() - 128);
        let parsed = parse_trailing(&file, &mut budget()).unwrap().unwrap();
        assert_eq!(parsed.get("artist").unwrap().text_lossy(), "Y");
    }

    #[test]
    fn a_header_plus_footer_tag_is_found_from_its_true_start() {
        let tag = ApeTag {
            version: 2000,
            items: vec![ApeItem::text("Album", "Z")],
        };
        let mut file = b"leading audio".to_vec();
        let tag_start = file.len();
        file.extend_from_slice(&tag.to_bytes_with_header().unwrap());
        let found = find_trailing(&file).unwrap();
        assert_eq!(found.start, tag_start);
    }

    #[test]
    fn no_tag_present_is_none() {
        assert_eq!(find_trailing(b"just some plain audio bytes"), None);
        assert_eq!(find_trailing(b""), None);
    }
}
