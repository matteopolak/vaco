//! `SimpleBlock` and `BlockGroup` encoding.
//!
//! # `SimpleBlock` versus `BlockGroup`
//!
//! RFC 9559 section 10.3 gives `SimpleBlock` no `BlockDuration` and no
//! `ReferenceBlock` child — those exist only on the `Block` inside a
//! `BlockGroup`. So a packet whose duration is not the track's
//! `DefaultDuration`, or whose decode order differs from its presentation
//! order (`pts != dts`, i.e. a reordered frame with a real inter-frame
//! reference), needs the long form; everything else — the common case, one
//! frame with no reordering — is a `SimpleBlock`.
//!
//! # Lacing is implemented but never chosen by this crate's own muxer
//!
//! `vaco-demux-matroska`'s own module docs record a measurement worth
//! repeating here because it resolves what would otherwise be a real design
//! question: *"`ffmpeg`'s Matroska muxer writes `FlagLacing=0` and never
//! laces."* [`crate::mux::MatroskaMuxer`] matches that and always writes one
//! frame per block. [`lace`] exists because the deliverable asks for lacing
//! encode support and because it is exercised by this crate's round-trip
//! tests against the demuxer's decoder — a caller assembling frames some
//! other way can still reach it.

use vaco_core::{Error, Result};
use vaco_demux_matroska::ebml::schema as el;
use vaco_format_ebml::{vint_min, write_element, write_int, write_uint};

/// `SimpleBlock`'s keyframe flag (RFC 9559 section 10.3.1).
const FLAG_KEYFRAME: u8 = 0x80;

/// The lacing selector RFC 9559 section 10.3.1's flags octet: bits 1-2 give
/// none / Xiph / fixed / EBML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaceKind {
    #[default]
    None,
    Xiph,
    FixedSize,
    Ebml,
}

impl LaceKind {
    /// The two LACING bits of RFC 9559 section 10.1, already shifted into
    /// place — the mirror of `vaco_demux_matroska::block::Lacing::from_flags`,
    /// which this crate's own test checks the two agree on.
    #[must_use]
    pub const fn flag_bits(self) -> u8 {
        match self {
            Self::None => 0x00,
            Self::Xiph => 0x02,
            Self::Ebml => 0x06,
            Self::FixedSize => 0x04,
        }
    }
}

/// Encode the shared `Block`/`SimpleBlock` header: the track number VINT, the
/// signed 16-bit relative timestamp, and the flags octet — everything before
/// the (possibly laced) frame payload.
///
/// # Errors
///
/// [`Error::Unsupported`] when `rel_ts` does not fit the signed 16-bit field
/// RFC 9559 section 10.3.1 gives it; the caller (a new `Cluster`) is what
/// keeps this from happening in practice.
fn block_header(track: u64, rel_ts: i64, flags: u8) -> Result<Vec<u8>> {
    let rel_ts = i16::try_from(rel_ts)
        .map_err(|_| Error::Unsupported("matroska: block timestamp does not fit a cluster"))?;
    let mut out = vint_min(track);
    out.extend_from_slice(&rel_ts.to_be_bytes());
    out.push(flags);
    Ok(out)
}

/// Lace `frames` into one block payload, per RFC 9559 section 10.3.2 (Xiph),
/// 10.3.3 (EBML) or 10.3.4 (fixed-size).
///
/// # Errors
///
/// [`Error::Unsupported`] for more than 256 frames (the lace frame count is
/// one octet, biased by one) or, for [`LaceKind::FixedSize`], frames that are
/// not all the same length.
pub fn lace(kind: LaceKind, frames: &[&[u8]]) -> Result<Vec<u8>> {
    if frames.len() > 256 {
        return Err(Error::Unsupported(
            "matroska: lace holds at most 256 frames",
        ));
    }
    match kind {
        LaceKind::None => Ok(frames.first().map(|f| f.to_vec()).unwrap_or_default()),
        LaceKind::Xiph => Ok(vaco_demux_matroska::synth::xiph_lace(frames)),
        LaceKind::Ebml => Ok(vaco_demux_matroska::synth::ebml_lace(frames)),
        LaceKind::FixedSize => {
            let len = frames.first().map_or(0, |f| f.len());
            if frames.iter().any(|f| f.len() != len) {
                return Err(Error::Unsupported(
                    "matroska: fixed-size lacing needs frames of equal length",
                ));
            }
            Ok(vaco_demux_matroska::synth::fixed_lace(frames))
        }
    }
}

/// A complete `SimpleBlock` element.
///
/// # Errors
///
/// As [`block_header`].
pub fn simple_block(track: u64, rel_ts: i64, keyframe: bool, payload: &[u8]) -> Result<Vec<u8>> {
    let flags = if keyframe { FLAG_KEYFRAME } else { 0 };
    let mut body = block_header(track, rel_ts, flags)?;
    body.extend_from_slice(payload);
    Ok(write_element(el::SIMPLEBLOCK, &body))
}

/// A complete `BlockGroup` element: `Block`, plus `BlockDuration` and
/// `ReferenceBlock` when the caller supplies them.
///
/// `duration_ticks` is the packet's duration in the container's tick unit
/// (`TimestampScale`-scaled); `reference_ticks` is the signed delta, in the
/// same unit, from this block's timestamp to the frame it depends on — RFC
/// 9559 section 10.3.1's own convention, negative for a past reference.
///
/// # Errors
///
/// As [`block_header`].
pub fn block_group(
    track: u64,
    rel_ts: i64,
    payload: &[u8],
    duration_ticks: Option<u64>,
    reference_ticks: Option<i64>,
) -> Result<Vec<u8>> {
    let mut block_body = block_header(track, rel_ts, 0)?;
    block_body.extend_from_slice(payload);

    let mut group = write_element(el::BLOCK, &block_body);
    if let Some(refd) = reference_ticks {
        group.extend_from_slice(&write_int(el::REFERENCEBLOCK, refd));
    }
    if let Some(dur) = duration_ticks {
        group.extend_from_slice(&write_uint(el::BLOCKDURATION, dur));
    }
    Ok(write_element(el::BLOCKGROUP, &group))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_demux_matroska::block::{self as decode_block, Lacing as DecodeLacing};

    /// Strip a `SimpleBlock`/`Block` element's own ID and size, returning its
    /// data — the slice the demuxer's `parse_header`/`frames` operate on.
    fn element_data(bytes: &[u8], want_id: u32) -> &[u8] {
        let (id, id_len) = vaco_format_ebml::read_id(bytes, 4).unwrap();
        assert_eq!(id, want_id);
        let rest = &bytes[id_len..];
        let (size, size_len) = vaco_format_ebml::read_size(rest, 8).unwrap();
        &rest[size_len..][..size.known().unwrap() as usize]
    }

    #[test]
    fn a_simple_block_decodes_back_through_the_demuxers_reader() {
        let bytes = simple_block(3, -120, true, b"hello").unwrap();
        let body = element_data(&bytes, el::SIMPLEBLOCK);
        let parsed = decode_block::parse_header(body, true).unwrap();
        assert_eq!(parsed.track, 3);
        assert_eq!(parsed.rel_timestamp, -120);
        assert!(parsed.keyframe);
        assert_eq!(parsed.lacing, DecodeLacing::None);
        let f = decode_block::frames(body, &parsed).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(&body[f[0].offset..f[0].offset + f[0].len], b"hello");
    }

    #[test]
    fn an_out_of_range_timestamp_is_refused_not_truncated() {
        assert!(simple_block(1, i64::from(i16::MAX) + 1, false, b"x").is_err());
        assert!(simple_block(1, i64::from(i16::MIN) - 1, false, b"x").is_err());
    }

    #[test]
    fn block_group_carries_duration_and_reference() {
        let bytes = block_group(1, 10, b"payload", Some(40), Some(-40)).unwrap();
        let body = element_data(&bytes, el::BLOCKGROUP);
        let mut saw_block = false;
        let mut saw_duration = false;
        let mut saw_reference = false;
        for child in
            vaco_format_ebml::Slice::new(body, vaco_format_ebml::Caps::default()).children()
        {
            match child.id {
                x if x == el::BLOCK => {
                    saw_block = true;
                    let h = decode_block::parse_header(child.data, false).unwrap();
                    assert_eq!(h.track, 1);
                    assert_eq!(h.rel_timestamp, 10);
                }
                x if x == el::BLOCKDURATION => {
                    saw_duration = true;
                    assert_eq!(vaco_format_ebml::as_uint(child.data), Some(40));
                }
                x if x == el::REFERENCEBLOCK => {
                    saw_reference = true;
                    assert_eq!(vaco_format_ebml::as_int(child.data), Some(-40));
                }
                _ => {}
            }
        }
        assert!(saw_block && saw_duration && saw_reference);
    }

    #[test]
    fn lacing_round_trips_through_the_demuxers_lace_decoder() {
        let want: [&[u8]; 3] = [b"a", b"bb", b"ccc"];
        for kind in [LaceKind::Xiph, LaceKind::Ebml] {
            let flags = kind.flag_bits();
            let laced = lace(kind, &want).unwrap();
            // Wrap the laced payload in a real SimpleBlock so the demuxer's
            // own header parser selects the matching lacing from the flags
            // octet, exactly as it would reading a file this crate wrote.
            let mut block_bytes = vint_min(1);
            block_bytes.extend_from_slice(&0i16.to_be_bytes());
            block_bytes.push(flags);
            block_bytes.extend_from_slice(&laced);
            let h = decode_block::parse_header(&block_bytes, true).unwrap();
            let f = decode_block::frames(&block_bytes, &h).unwrap();
            let got: Vec<Vec<u8>> = f
                .iter()
                .map(|fr| block_bytes[fr.offset..fr.offset + fr.len].to_vec())
                .collect();
            assert_eq!(got, want.iter().map(|f| f.to_vec()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn lacing_flag_bits_agree_with_the_demuxers_decoder() {
        for (kind, decode_kind) in [
            (LaceKind::None, DecodeLacing::None),
            (LaceKind::Xiph, DecodeLacing::Xiph),
            (LaceKind::Ebml, DecodeLacing::Ebml),
            (LaceKind::FixedSize, DecodeLacing::Fixed),
        ] {
            assert_eq!(
                DecodeLacing::from_flags(kind.flag_bits()),
                decode_kind,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn fixed_lacing_needs_equal_length_frames() {
        let frames: [&[u8]; 2] = [b"aa", b"bb"];
        assert!(lace(LaceKind::FixedSize, &frames).is_ok());
        let uneven: [&[u8]; 2] = [b"a", b"bb"];
        assert!(lace(LaceKind::FixedSize, &uneven).is_err());
    }
}
