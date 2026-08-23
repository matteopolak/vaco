//! Content detection.
//!
//! **Measured** against `ffprobe 8.1`: `ffprobe -show_format opus.ogg` (and
//! the Vorbis and FLAC files built the same way) all report
//! `probe_score=100`. Ogg's capture pattern is a fixed four-byte magic
//! checked against a version byte and a self-consistency requirement (the
//! declared segment count must actually fit in the probe buffer), which is
//! exactly [`ProbeScore::MAGIC_CHECKED`]'s row in the convention table —
//! "unambiguous magic at a fixed offset, plus a self-consistency check" —
//! the same row `vaco-format-core::vacoraw` uses for the same reason.

use vaco_format_core::probe::{ProbeData, ProbeScore};

use crate::page;

/// The `DemuxerDesc::probe` function.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.tag(0) != Some(page::CAPTURE_PATTERN) {
        return ProbeScore::NONE;
    }
    // Real bytes only from here on: `ProbeData::get` reads zero past the end
    // of what was actually read, and `page_segments == 0` is a value a real
    // page can legitimately carry, so a padded read of it must not be
    // allowed to masquerade as a confirmed self-consistency check.
    match data.buf.get(4) {
        Some(&v) if v == page::SUPPORTED_VERSION => {}
        Some(_) => return ProbeScore::NONE,
        None => return ProbeScore::MAGIC,
    }
    let Some(&page_segments) = data.buf.get(26) else {
        return ProbeScore::MAGIC;
    };
    let table_end = page::FIXED_HEADER_LEN.saturating_add(usize::from(page_segments));
    if table_end <= data.buf.len() {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::MAGIC
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn minimal_page(page_segments: u8) -> Vec<u8> {
        let mut v = vec![0u8; page::FIXED_HEADER_LEN];
        v[0..4].copy_from_slice(&page::CAPTURE_PATTERN);
        v[26] = page_segments;
        v.extend_from_slice(&vec![1u8; usize::from(page_segments)]);
        v.extend_from_slice(&vec![0u8; usize::from(page_segments)]);
        v
    }

    #[test]
    fn a_measured_real_page_scores_max() {
        // The exact first 47 bytes of `ffmpeg -c:a libopus opus.ogg`.
        let mut bytes = vec![
            b'O', b'g', b'g', b'S', 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x9B, 0x1E, 0xF1, 0xE1, 0x00, 0x00, 0x00, 0x00, 0x78, 0x16, 0xD6, 0xC8, 0x01, 0x13,
        ];
        bytes.extend_from_slice(&[0u8; 0x13]);
        assert_eq!(probe(&ProbeData::new(&bytes)), ProbeScore::MAGIC_CHECKED);
    }

    #[test]
    fn wrong_magic_scores_nothing() {
        let mut bytes = minimal_page(1);
        bytes[0] = b'X';
        assert_eq!(probe(&ProbeData::new(&bytes)), ProbeScore::NONE);
    }

    #[test]
    fn wrong_version_scores_nothing() {
        let mut bytes = minimal_page(1);
        bytes[4] = 7;
        assert_eq!(probe(&ProbeData::new(&bytes)), ProbeScore::NONE);
    }

    #[test]
    fn an_empty_buffer_scores_nothing() {
        assert_eq!(probe(&ProbeData::new(&[])), ProbeScore::NONE);
    }

    #[test]
    fn a_truncated_but_matching_prefix_still_scores() {
        let bytes = minimal_page(1);
        assert_eq!(probe(&ProbeData::new(&bytes[..5])), ProbeScore::MAGIC);
    }

    #[test]
    fn the_probe_only_returns_values_from_its_own_table() {
        for len in [0usize, 4, 5, 6, 27, 30, 100] {
            let bytes = minimal_page(3);
            let truncated = &bytes[..len.min(bytes.len())];
            let s = probe(&ProbeData::new(truncated));
            assert!(
                s == ProbeScore::NONE || s == ProbeScore::MAGIC || s == ProbeScore::MAGIC_CHECKED,
                "unexpected score {s:?} at len {len}"
            );
        }
    }
}
