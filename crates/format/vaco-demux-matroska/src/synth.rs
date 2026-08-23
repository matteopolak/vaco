//! A minimal EBML writer, for building fixtures.
//!
//! Public because the tests that matter most cannot be produced by any encoder
//! we have: `ffmpeg`'s Matroska muxer writes `FlagLacing=0` and never laces, so
//! a laced file has to be synthesised, and so does an unknown-size `Cluster`
//! with a hostile shape. It is also what seeds the fuzz corpus.
//!
//! This is **not** a muxer. It writes what it is told, including things a muxer
//! must not write, which is the point.
//!
//! The generic VINT and element-building primitives below are thin
//! delegations to [`vaco_format_ebml`], which now owns the one definition
//! (D19); everything from [`block_body`] down is Matroska-specific (RFC 9559
//! lacing and block shapes) and has no equivalent there.

/// Encode `value` as an EBML data size in `len` octets (RFC 8794 §5).
#[must_use]
pub fn vint(value: u64, len: u8) -> Vec<u8> {
    vaco_format_ebml::vint(value, len)
}

/// The shortest data-size encoding of `value`.
#[must_use]
pub fn vint_min(value: u64) -> Vec<u8> {
    vaco_format_ebml::vint_min(value)
}

/// The all-ones data size that marks an unknown-size element (RFC 8794 §6.2).
#[must_use]
pub fn vint_unknown(len: u8) -> Vec<u8> {
    vaco_format_ebml::vint_unknown(len)
}

/// An element ID, big-endian with its marker already in place.
#[must_use]
pub fn id_bytes(id: u32) -> Vec<u8> {
    vaco_format_ebml::id_bytes(id)
}

/// One complete element: ID, shortest size, body.
#[must_use]
pub fn element(id: u32, body: &[u8]) -> Vec<u8> {
    vaco_format_ebml::write_element(id, body)
}

/// An element whose size field is the unknown-size marker.
#[must_use]
pub fn element_unknown_size(id: u32, body: &[u8]) -> Vec<u8> {
    vaco_format_ebml::element_unknown_size(id, body)
}

/// An unsigned-integer element, in the fewest octets that hold `value`.
#[must_use]
pub fn uint(id: u32, value: u64) -> Vec<u8> {
    vaco_format_ebml::write_uint(id, value)
}

/// A signed-integer element.
#[must_use]
pub fn int(id: u32, value: i64) -> Vec<u8> {
    vaco_format_ebml::write_int(id, value)
}

/// An eight-octet float element.
#[must_use]
pub fn float(id: u32, value: f64) -> Vec<u8> {
    vaco_format_ebml::write_float(id, value)
}

/// A string element.
#[must_use]
pub fn string(id: u32, value: &str) -> Vec<u8> {
    vaco_format_ebml::write_string(id, value)
}

/// A `SimpleBlock` or `Block` body: header plus already-laced payload.
///
/// `track` is written as a one-octet VINT, which covers 1..=126.
#[must_use]
pub fn block_body(track: u8, rel_ts: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | track];
    out.extend_from_slice(&rel_ts.to_be_bytes());
    out.push(flags);
    out.extend_from_slice(payload);
    out
}

/// Xiph-lace `frames` into a block payload (RFC 9559 §10.3.2).
#[must_use]
pub fn xiph_lace(frames: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(frames.len().saturating_sub(1)) as u8];
    for f in frames.iter().take(frames.len().saturating_sub(1)) {
        let mut n = f.len();
        while n >= 255 {
            out.push(0xFF);
            n -= 255;
        }
        out.push(n as u8);
    }
    for f in frames {
        out.extend_from_slice(f);
    }
    out
}

/// EBML-lace `frames` into a block payload (RFC 9559 §10.3.3).
#[must_use]
pub fn ebml_lace(frames: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(frames.len().saturating_sub(1)) as u8];
    let mut prev: i64 = 0;
    for (i, f) in frames
        .iter()
        .take(frames.len().saturating_sub(1))
        .enumerate()
    {
        let len = i64::try_from(f.len()).unwrap_or(i64::MAX);
        if i == 0 {
            out.extend_from_slice(&vint_min(len as u64));
        } else {
            out.extend_from_slice(&signed_vint(len - prev));
        }
        prev = len;
    }
    for f in frames {
        out.extend_from_slice(f);
    }
    out
}

/// Fixed-size-lace `frames`, which must all be the same length (§10.3.4).
#[must_use]
pub fn fixed_lace(frames: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(frames.len().saturating_sub(1)) as u8];
    for f in frames {
        out.extend_from_slice(f);
    }
    out
}

/// The signed VINT of RFC 9559 §10.3.3: bias by `2^(7n-1) - 1`.
#[must_use]
pub fn signed_vint(value: i64) -> Vec<u8> {
    vaco_format_ebml::signed_vint(value)
}

/// A complete EBML header declaring `doc_type`.
#[must_use]
pub fn ebml_header(doc_type: &str) -> Vec<u8> {
    use crate::ebml::schema as el;
    let mut body = Vec::new();
    body.extend_from_slice(&uint(el::EBMLVERSION, 1));
    body.extend_from_slice(&uint(el::EBMLREADVERSION, 1));
    body.extend_from_slice(&uint(el::EBMLMAXIDLENGTH, 4));
    body.extend_from_slice(&uint(el::EBMLMAXSIZELENGTH, 8));
    body.extend_from_slice(&string(el::DOCTYPE, doc_type));
    body.extend_from_slice(&uint(el::DOCTYPEVERSION, 4));
    body.extend_from_slice(&uint(el::DOCTYPEREADVERSION, 2));
    element(el::EBML, &body)
}

/// How a synthesised file's `Segment` is sized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentSize {
    Known,
    Unknown,
}

/// Build a one-track file around `tracks_body` and `clusters`.
#[must_use]
pub fn file(
    doc_type: &str,
    info_body: &[u8],
    tracks_body: &[u8],
    clusters: &[Vec<u8>],
    size: SegmentSize,
) -> Vec<u8> {
    use crate::ebml::schema as el;
    let mut segment = Vec::new();
    segment.extend_from_slice(&element(el::INFO, info_body));
    segment.extend_from_slice(&element(el::TRACKS, tracks_body));
    for c in clusters {
        segment.extend_from_slice(c);
    }
    let mut out = ebml_header(doc_type);
    out.extend_from_slice(&match size {
        SegmentSize::Known => element(el::SEGMENT, &segment),
        SegmentSize::Unknown => element_unknown_size(el::SEGMENT, &segment),
    });
    out
}

/// A `TrackEntry` for a video track, with `codec_id` and the given pixel size.
#[must_use]
pub fn video_track(number: u64, codec_id: &str, width: u64, height: u64) -> Vec<u8> {
    use crate::ebml::schema as el;
    let mut video = Vec::new();
    video.extend_from_slice(&uint(el::PIXELWIDTH, width));
    video.extend_from_slice(&uint(el::PIXELHEIGHT, height));
    let mut body = Vec::new();
    body.extend_from_slice(&uint(el::TRACKNUMBER, number));
    body.extend_from_slice(&uint(el::TRACKUID, number));
    body.extend_from_slice(&uint(el::TRACKTYPE, 1));
    body.extend_from_slice(&string(el::CODECID, codec_id));
    body.extend_from_slice(&element(el::VIDEO, &video));
    element(el::TRACKENTRY, &body)
}

/// A `TrackEntry` for an audio track.
#[must_use]
pub fn audio_track(number: u64, codec_id: &str, rate: f64, channels: u64) -> Vec<u8> {
    use crate::ebml::schema as el;
    let mut audio = Vec::new();
    audio.extend_from_slice(&float(el::SAMPLINGFREQUENCY, rate));
    audio.extend_from_slice(&uint(el::CHANNELS, channels));
    let mut body = Vec::new();
    body.extend_from_slice(&uint(el::TRACKNUMBER, number));
    body.extend_from_slice(&uint(el::TRACKUID, number));
    body.extend_from_slice(&uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&string(el::CODECID, codec_id));
    body.extend_from_slice(&element(el::AUDIO, &audio));
    element(el::TRACKENTRY, &body)
}

/// A `Cluster` at `timestamp` holding the given already-built children.
#[must_use]
pub fn cluster(timestamp: u64, children: &[Vec<u8>], size: SegmentSize) -> Vec<u8> {
    use crate::ebml::schema as el;
    let mut body = uint(el::TIMESTAMP, timestamp);
    for c in children {
        body.extend_from_slice(c);
    }
    match size {
        SegmentSize::Known => element(el::CLUSTER, &body),
        SegmentSize::Unknown => element_unknown_size(el::CLUSTER, &body),
    }
}
