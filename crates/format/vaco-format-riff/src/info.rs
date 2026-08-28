//! The `LIST`/`INFO` chunk: RIFF's own container-metadata convention.
//!
//! A `LIST` chunk whose four-byte form type is `INFO` holds a flat sequence of
//! sub-chunks, each a standard tag (`IART` artist, `INAM` title, `ISFT`
//! software, ...). Only `ISFT` is mapped here, to `"encoder"` — the one field
//! measured against the reference: `ffmpeg`'s own `wav` muxer writes
//! `LIST/INFO/ISFT` holding its `Lavf...` signature, and `ffprobe
//! -show_format` reports it as `format.tags.encoder`. Every other `INFO`
//! sub-chunk is real RIFF and unmapped until something needs it.

use vaco_bitstream::ByteReader;

/// Reads an already-in-memory `LIST` chunk payload and returns the tags it
/// recognizes, as `(name, value)` pairs. Empty if the payload's form type is
/// not `INFO`, or it carries no recognized sub-chunk.
#[must_use]
pub fn list_info_tags(payload: &[u8]) -> Vec<(String, String)> {
    let mut r = ByteReader::new(payload);
    if r.bytes(4) != b"INFO".as_slice() {
        return Vec::new();
    }
    let mut tags = Vec::new();
    while r.remaining() >= 8 {
        let id = r.bytes(4);
        let is_isft = id == b"ISFT".as_slice();
        let size = r.le32() as usize;
        let value = r.bytes(size);
        if is_isft {
            let text = value.split(|&b| b == 0).next().unwrap_or(value);
            let s = String::from_utf8_lossy(text).trim().to_owned();
            if !s.is_empty() {
                tags.push(("encoder".to_owned(), s));
            }
        }
        if size % 2 == 1 {
            r.skip(1);
        }
    }
    tags
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::list_info_tags;

    fn info_chunk(subchunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut payload = b"INFO".to_vec();
        for (id, value) in subchunks {
            payload.extend_from_slice(*id);
            payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
            payload.extend_from_slice(value);
            if value.len() % 2 == 1 {
                payload.push(0);
            }
        }
        payload
    }

    #[test]
    fn isft_maps_to_encoder() {
        let payload = info_chunk(&[(b"ISFT", b"Lavf62.12.100\0")]);
        assert_eq!(
            list_info_tags(&payload),
            vec![("encoder".to_owned(), "Lavf62.12.100".to_owned())]
        );
    }

    #[test]
    fn an_unrecognized_subchunk_is_silently_skipped_not_misread() {
        let payload = info_chunk(&[(b"IART", b"an artist\0"), (b"ISFT", b"Lavf\0")]);
        assert_eq!(
            list_info_tags(&payload),
            vec![("encoder".to_owned(), "Lavf".to_owned())]
        );
    }

    #[test]
    fn a_non_info_form_yields_nothing() {
        let mut payload = b"adtl".to_vec();
        payload.extend_from_slice(b"ISFTfoo\0");
        assert!(list_info_tags(&payload).is_empty());
    }

    #[test]
    fn odd_length_value_is_word_aligned() {
        // An odd-length ISFT value needs its pad byte skipped, or the second
        // ISFT below is read starting one byte early and comes out garbled
        // (or is missed as an overrun) instead of as its own clean tag.
        let payload = info_chunk(&[(b"ISFT", b"Lav"), (b"ISFT", b"Second")]);
        assert_eq!(
            list_info_tags(&payload),
            vec![
                ("encoder".to_owned(), "Lav".to_owned()),
                ("encoder".to_owned(), "Second".to_owned()),
            ]
        );
    }

    #[test]
    fn truncated_payload_does_not_panic() {
        let payload = info_chunk(&[(b"ISFT", b"Lavf62.12.100\0")]);
        for n in 0..payload.len() {
            let _ = list_info_tags(&payload[..n]);
        }
    }

    #[test]
    fn empty_value_is_not_recorded() {
        let payload = info_chunk(&[(b"ISFT", b"\0")]);
        assert!(list_info_tags(&payload).is_empty());
    }
}
