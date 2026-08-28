//! Building one structural-metadata set's value bytes: a sequence of
//! `Tag(u16 BE) Length(u16 BE) Value` items, the same shared local-set
//! encoding `vaco-demux-mxf::localset` reads (distinct from the KLV layer's
//! BER length).

use crate::ul::Ul;

/// Append one `Tag(u16) Length(u16) Value` item to `out`.
pub(crate) fn push_item(out: &mut Vec<u8>, tag: u16, value: &[u8]) {
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

pub(crate) fn push_u8(out: &mut Vec<u8>, tag: u16, v: u8) {
    push_item(out, tag, &[v]);
}

pub(crate) fn push_u16(out: &mut Vec<u8>, tag: u16, v: u16) {
    push_item(out, tag, &v.to_be_bytes());
}

pub(crate) fn push_u32(out: &mut Vec<u8>, tag: u16, v: u32) {
    push_item(out, tag, &v.to_be_bytes());
}

pub(crate) fn push_u64(out: &mut Vec<u8>, tag: u16, v: u64) {
    push_item(out, tag, &v.to_be_bytes());
}

pub(crate) fn push_i64(out: &mut Vec<u8>, tag: u16, v: i64) {
    push_item(out, tag, &v.to_be_bytes());
}

/// An 8-byte `{ numerator: i32, denominator: i32 }` rational — the encoding
/// every `EditRate`/`SampleRate`/`AspectRatio` property uses (matching
/// `vaco-demux-mxf::localset::rational_be`).
pub(crate) fn push_rational(out: &mut Vec<u8>, tag: u16, num: i32, den: i32) {
    let mut v = [0u8; 8];
    v[..4].copy_from_slice(&num.to_be_bytes());
    v[4..].copy_from_slice(&den.to_be_bytes());
    push_item(out, tag, &v);
}

pub(crate) fn push_uid16(out: &mut Vec<u8>, tag: u16, uid: [u8; 16]) {
    push_item(out, tag, &uid);
}

pub(crate) fn push_umid32(out: &mut Vec<u8>, tag: u16, umid: [u8; 32]) {
    push_item(out, tag, &umid);
}

/// A `StrongRefArray`/`WeakRefArray`/generic batch of fixed-size items:
/// `Count(u32 BE) ItemLength(u32 BE)` followed by `count` items of
/// `item_len` bytes each — the write side of `vaco-demux-mxf::localset::batch`.
pub(crate) fn push_batch16(out: &mut Vec<u8>, tag: u16, items: &[[u8; 16]]) {
    let mut v = Vec::new();
    v.extend_from_slice(&(items.len() as u32).to_be_bytes());
    v.extend_from_slice(&16u32.to_be_bytes());
    for item in items {
        v.extend_from_slice(item);
    }
    push_item(out, tag, &v);
}

/// Build one Primer Pack's value: `Count(u32) ItemLength(u32)` then one
/// `Tag(u16) UL(16 bytes)` entry per row, matching
/// `vaco-demux-mxf::primer`'s read shape.
#[must_use]
pub(crate) fn build_primer_pack(entries: &[(u16, Ul)]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    v.extend_from_slice(&18u32.to_be_bytes());
    for &(tag, ul) in entries {
        v.extend_from_slice(&tag.to_be_bytes());
        v.extend_from_slice(&ul.as_bytes());
    }
    v
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn push_item_matches_the_measured_tag_length_value_shape() {
        let mut out = Vec::new();
        push_u32(&mut out, 0x3006, 2);
        assert_eq!(out, vec![0x30, 0x06, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02]);
    }

    #[test]
    fn primer_pack_entry_is_eighteen_bytes_wide() {
        let ul = Ul::new([0xAA; 16]);
        let bytes = build_primer_pack(&[(0x3c0a, ul)]);
        // count(4) + item_len(4) + tag(2) + ul(16) = 26.
        assert_eq!(bytes.len(), 26);
        assert_eq!(&bytes[8..10], &[0x3c, 0x0a]);
        assert_eq!(&bytes[10..26], &[0xAA; 16]);
    }
}
