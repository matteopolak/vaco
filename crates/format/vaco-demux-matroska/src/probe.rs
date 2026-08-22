//! Content probing.
//!
//! # Measured, not assumed
//!
//! `ffprobe 8.1` reports `probe_score=100` for both a `DocType=matroska` and a
//! `DocType=webm` file, and reports `format_name=matroska,webm` for both. The
//! score for an EBML document with some *other* `DocType` is 75 — the
//! [`ProbeScore::CONTENT`] row — because the magic is genuine EBML but the
//! document is not ours to read.

use vaco_format_core::probe::{ProbeData, ProbeScore};

use crate::ebml;

/// `EBML` element ID, which is also the file magic (RFC 8794 §11.2.1).
pub const MAGIC: &[u8; 4] = &[0x1A, 0x45, 0xDF, 0xA3];

/// Score a candidate buffer.
///
/// Full marks require the magic **and** a `DocType` we implement, which is what
/// keeps an EBML file that is not Matroska from being opened as one.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if !data.starts_with(MAGIC) {
        return ProbeScore::NONE;
    }
    match doc_type(data.buf) {
        Some(t) if t == "matroska" || t == "webm" => ProbeScore::MAX,
        // Genuine EBML, some other document type: a real format, just not ours.
        _ => ProbeScore::CONTENT,
    }
}

/// Read `DocType` out of the EBML header at the start of `buf`.
///
/// Returns `None` when the header is truncated or malformed, which the caller
/// treats the same as an unrecognised document type.
#[must_use]
pub fn doc_type(buf: &[u8]) -> Option<&str> {
    let (id, id_len) = ebml::read_id(buf, ebml::MAX_ID_LEN).ok()?;
    if id != ebml::schema::EBML {
        return None;
    }
    let (size, size_len) = ebml::read_size(buf.get(id_len..)?, ebml::MAX_SIZE_LEN).ok()?;
    let n = usize::try_from(size.known()?).ok()?;
    let start = id_len.checked_add(size_len)?;
    // A truncated probe buffer still yields whatever children arrived; the
    // header is a few dozen octets and the probe window is at least 2 KiB.
    let body = buf.get(start..)?;
    let body = body.get(..n.min(body.len()))?;
    ebml::Slice::new(body, ebml::Caps::default())
        .children()
        .find(|c| c.id == ebml::schema::DOCTYPE)
        .and_then(|c| ebml::as_str(c.data))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::synth::ebml_header;

    #[test]
    fn matroska_and_webm_both_score_full_marks() {
        for t in ["matroska", "webm"] {
            let buf = ebml_header(t);
            assert_eq!(probe(&ProbeData::new(&buf)), ProbeScore::MAX, "{t}");
        }
    }

    #[test]
    fn another_ebml_document_type_scores_content_not_max() {
        let buf = ebml_header("dtshd");
        assert_eq!(probe(&ProbeData::new(&buf)), ProbeScore::CONTENT);
    }

    #[test]
    fn a_non_ebml_buffer_scores_nothing() {
        assert_eq!(probe(&ProbeData::new(b"RIFF....WAVE")), ProbeScore::NONE);
        assert_eq!(probe(&ProbeData::new(&[])), ProbeScore::NONE);
    }

    #[test]
    fn every_truncation_of_a_header_is_answerable() {
        let buf = ebml_header("matroska");
        for n in 0..buf.len() {
            let _ = probe(&ProbeData::new(&buf[..n]));
        }
    }
}
