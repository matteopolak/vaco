//! `Block` and `SimpleBlock` headers, and all four lacing modes.
//!
//! # Specification
//!
//! RFC 9559 sections 10.1 (Block), 10.2 (`SimpleBlock`) and 10.3.1 to 10.3.4
//! (no / Xiph / EBML / fixed-size lacing). The worked examples in the RFC's own
//! tables 36, 38 and 39 are transcribed as tests.
//!
//! # Why the frame count needs no cap of its own
//!
//! The lace header is one octet holding *frames minus one*, so a lace can never
//! declare more than 256 frames however hostile the file is. That is the one
//! bound this format hands us for free. Everything else — the individual sizes,
//! their running sum, the fixed-size division — is attacker-controlled and is
//! checked against the bytes that are actually present, never against the
//! declared total.

use vaco_core::{Error, Result};

use crate::ebml;

/// Which lacing mode a block's flags select (RFC 9559 section 10.1, LACING).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lacing {
    None,
    Xiph,
    Ebml,
    Fixed,
}

impl Lacing {
    /// Decode the two LACING bits of a block flags octet.
    #[must_use]
    pub const fn from_flags(flags: u8) -> Self {
        match (flags >> 1) & 0b11 {
            0b01 => Self::Xiph,
            0b11 => Self::Ebml,
            0b10 => Self::Fixed,
            _ => Self::None,
        }
    }
}

/// A decoded block header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    pub track: u64,
    /// Timestamp relative to the enclosing `Cluster`, in track ticks.
    pub rel_timestamp: i16,
    pub keyframe: bool,
    pub invisible: bool,
    pub discardable: bool,
    pub lacing: Lacing,
    /// Octets the header occupied, so the lace data starts here.
    pub header_len: usize,
}

/// Parse the header of a `Block` or `SimpleBlock`.
///
/// `simple` selects the `SimpleBlock` flag layout, which adds KEY at bit 7 and DIS
/// at bit 0 where `Block` has reserved bits. A `Block` inside a `BlockGroup` is
/// a keyframe exactly when the group has no `ReferenceBlock`, which is the
/// caller's business because it is a property of the group and not of the block.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed track-number VINT, and
/// [`Error::UnexpectedEof`] when the block is shorter than its own header.
pub fn parse_header(data: &[u8], simple: bool) -> Result<BlockHeader> {
    // The track number is a VINT with its marker stripped, unlike an element ID,
    // so it decodes exactly like an element data size.
    let (size, used) = ebml::read_size(data, ebml::MAX_SIZE_LEN)
        .map_err(|_| Error::InvalidData("block track number is not a VINT"))?;
    let track = match size {
        ebml::Size::Known(t) => t,
        ebml::Size::Unknown => {
            return Err(Error::InvalidData(
                "block track number is the unknown marker",
            ));
        }
    };
    let ts_hi = *data.get(used).ok_or(Error::UnexpectedEof)?;
    let ts_lo = *data.get(used + 1).ok_or(Error::UnexpectedEof)?;
    let rel_timestamp = i16::from_be_bytes([ts_hi, ts_lo]);
    let flags = *data.get(used + 2).ok_or(Error::UnexpectedEof)?;
    Ok(BlockHeader {
        track,
        rel_timestamp,
        keyframe: simple && flags & 0x80 != 0,
        invisible: flags & 0x08 != 0,
        discardable: simple && flags & 0x01 != 0,
        lacing: Lacing::from_flags(flags),
        header_len: used + 3,
    })
}

/// A frame's extent within the block's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub offset: usize,
    pub len: usize,
}

/// Split a block's payload into frames.
///
/// `data` is the whole block element's data; `header` is what [`parse_header`]
/// returned for it. The returned frames are in storage order and are guaranteed
/// to lie inside `data` and not to overlap.
///
/// # Errors
///
/// [`Error::InvalidData`] when the declared sizes do not fit the block, when a
/// fixed-size lace does not divide evenly, or when a size delta goes negative.
pub fn frames(data: &[u8], header: &BlockHeader) -> Result<Vec<Frame>> {
    let body = data.get(header.header_len..).ok_or(Error::UnexpectedEof)?;
    let base = header.header_len;
    match header.lacing {
        Lacing::None => Ok(vec![Frame {
            offset: base,
            len: body.len(),
        }]),
        Lacing::Xiph => laced(body, base, xiph_sizes),
        Lacing::Ebml => laced(body, base, ebml_sizes),
        Lacing::Fixed => fixed(body, base),
    }
}

/// Shared tail for the two size-carrying lacings: read the count, delegate the
/// sizes, then place the frames and give the remainder to the last one.
fn laced(
    body: &[u8],
    base: usize,
    sizes: fn(&[u8], usize) -> Result<(Vec<usize>, usize)>,
) -> Result<Vec<Frame>> {
    // RFC 9559 section 10.3.2/10.3.3: "Lacing Head on 1 Octet: number of frames
    // in the lace minus 1". The +1 cannot overflow a usize from a u8.
    let count = usize::from(*body.first().ok_or(Error::UnexpectedEof)?) + 1;
    let (sizes, used) = sizes(body.get(1..).ok_or(Error::UnexpectedEof)?, count)?;
    let mut at = base
        .checked_add(1)
        .and_then(|v| v.checked_add(used))
        .ok_or(Error::InvalidData("lace header overflows the block"))?;
    let total_end = base
        .checked_add(body.len())
        .ok_or(Error::InvalidData("block length overflows"))?;
    let mut out = Vec::new();
    for len in sizes {
        let end = at
            .checked_add(len)
            .filter(|&e| e <= total_end)
            .ok_or(Error::InvalidData("laced frame runs past the block"))?;
        out.push(Frame { offset: at, len });
        at = end;
    }
    // RFC 9559 section 10.3.2: "the size of the last frame is deduced from the
    // size remaining in the Block".
    out.push(Frame {
        offset: at,
        len: total_end.saturating_sub(at),
    });
    Ok(out)
}

/// The `count - 1` explicit sizes of a Xiph lace, and the octets they occupied.
///
/// RFC 9559 section 10.3.2: each size is a run of `0xFF` octets followed by a
/// terminating octet below `0xFF`; the run and the terminator are summed.
fn xiph_sizes(buf: &[u8], count: usize) -> Result<(Vec<usize>, usize)> {
    let mut sizes = Vec::new();
    let mut pos = 0usize;
    for _ in 1..count {
        let mut size = 0usize;
        loop {
            let b = *buf.get(pos).ok_or(Error::UnexpectedEof)?;
            pos += 1;
            size = size
                .checked_add(usize::from(b))
                .ok_or(Error::InvalidData("xiph lace size overflows"))?;
            if b != 0xFF {
                break;
            }
            // Each iteration consumes an octet, so the loop is bounded by the
            // block length; there is no separate cap to get wrong.
        }
        sizes.push(size);
    }
    Ok((sizes, pos))
}

/// The `count - 1` explicit sizes of an EBML lace, and the octets they occupied.
///
/// RFC 9559 section 10.3.3: the first size is an unsigned VINT; every later one
/// is a signed VINT delta against its predecessor.
fn ebml_sizes(buf: &[u8], count: usize) -> Result<(Vec<usize>, usize)> {
    let mut sizes = Vec::new();
    let mut pos = 0usize;
    if count > 1 {
        let (size, used) = ebml::read_size(buf, ebml::MAX_SIZE_LEN)?;
        let first = match size {
            ebml::Size::Known(v) => v,
            ebml::Size::Unknown => {
                return Err(Error::InvalidData("ebml lace size is the unknown marker"));
            }
        };
        pos += used;
        let mut prev = i64::try_from(first)
            .map_err(|_| Error::InvalidData("ebml lace size does not fit an i64"))?;
        sizes.push(usize::try_from(prev).map_err(|_| Error::InvalidData("ebml lace size"))?);
        for _ in 2..count {
            let rest = buf.get(pos..).ok_or(Error::UnexpectedEof)?;
            let (delta, used) = ebml::read_signed_vint(rest)?;
            pos += used;
            prev = prev
                .checked_add(delta)
                .filter(|&v| v >= 0)
                .ok_or(Error::InvalidData("ebml lace size delta goes negative"))?;
            sizes.push(usize::try_from(prev).map_err(|_| Error::InvalidData("ebml lace size"))?);
        }
    }
    Ok((sizes, pos))
}

/// Fixed-size lacing: only the frame count is stored (RFC 9559 section 10.3.4).
fn fixed(body: &[u8], base: usize) -> Result<Vec<Frame>> {
    let count = usize::from(*body.first().ok_or(Error::UnexpectedEof)?) + 1;
    let payload = body.len().saturating_sub(1);
    if !payload.is_multiple_of(count) {
        return Err(Error::InvalidData(
            "fixed-size lace does not divide evenly into its frame count",
        ));
    }
    #[allow(
        clippy::integer_division,
        reason = "the remainder was just checked to be zero, and `count` is at least 1"
    )]
    let each = payload / count;
    let mut out = Vec::new();
    let mut at = base + 1;
    for _ in 0..count {
        out.push(Frame {
            offset: at,
            len: each,
        });
        at += each;
    }
    Ok(out)
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

    /// A one-octet track number, a relative timestamp and one flags octet.
    fn block(track: u8, ts: i16, flags: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0x80 | track];
        v.extend_from_slice(&ts.to_be_bytes());
        v.push(flags);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn simple_block_header() {
        let data = block(1, -300, 0x80, b"payload");
        let h = parse_header(&data, true).unwrap();
        assert_eq!(h.track, 1);
        assert_eq!(h.rel_timestamp, -300);
        assert!(h.keyframe);
        assert_eq!(h.lacing, Lacing::None);
        assert_eq!(h.header_len, 4);
        let f = frames(&data, &h).unwrap();
        assert_eq!(f, vec![Frame { offset: 4, len: 7 }]);
    }

    #[test]
    fn a_block_is_never_marked_key_by_its_own_flags() {
        // The KEY bit is SimpleBlock-only; in a `Block` that position is
        // reserved, and a file setting it must not make the frame a keyframe.
        let data = block(1, 0, 0x80, b"x");
        assert!(!parse_header(&data, false).unwrap().keyframe);
        assert!(parse_header(&data, true).unwrap().keyframe);
    }

    /// RFC 9559 table 36: 800, 500 and 1000-octet frames, Xiph-laced.
    #[test]
    fn xiph_lacing_rfc_example() {
        let mut body = vec![0x02, 0xFF, 0xFF, 0xFF, 0x23, 0xFF, 0xF5];
        body.extend(std::iter::repeat_n(0u8, 800 + 500 + 1000));
        let data = block(1, 0, 0x02, &body);
        let h = parse_header(&data, true).unwrap();
        assert_eq!(h.lacing, Lacing::Xiph);
        let f = frames(&data, &h).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].len, 800);
        assert_eq!(f[1].len, 500);
        assert_eq!(f[2].len, 1000);
        assert_eq!(f[0].offset, 4 + 7);
        assert_eq!(data.len(), 2311);
    }

    /// A size that is a multiple of 255 is terminated by a zero octet.
    #[test]
    fn xiph_multiple_of_255_terminates_with_zero() {
        let mut body = vec![0x01, 0xFF, 0xFF, 0xFF, 0x00];
        body.extend(std::iter::repeat_n(1u8, 765));
        body.extend(std::iter::repeat_n(2u8, 3));
        let data = block(1, 0, 0x02, &body);
        let h = parse_header(&data, true).unwrap();
        let f = frames(&data, &h).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].len, 765);
        assert_eq!(f[1].len, 3);
    }

    /// RFC 9559 table 38, whose own size octets are given in the spec.
    #[test]
    fn ebml_lacing_rfc_example() {
        let mut body = vec![0x02, 0x43, 0x20, 0x5E, 0xD3];
        body.extend(std::iter::repeat_n(0u8, 800 + 500 + 1000));
        let data = block(1, 0, 0x06, &body);
        let h = parse_header(&data, true).unwrap();
        assert_eq!(h.lacing, Lacing::Ebml);
        let f = frames(&data, &h).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].len, 800);
        assert_eq!(f[1].len, 500);
        assert_eq!(f[2].len, 1000);
    }

    /// RFC 9559 table 39.
    #[test]
    fn fixed_lacing_rfc_example() {
        let mut body = vec![0x02];
        body.extend(std::iter::repeat_n(0u8, 2400));
        let data = block(1, 0, 0x04, &body);
        let h = parse_header(&data, true).unwrap();
        assert_eq!(h.lacing, Lacing::Fixed);
        let f = frames(&data, &h).unwrap();
        assert_eq!(f.len(), 3);
        assert!(f.iter().all(|x| x.len == 800));
        assert_eq!(data.len(), 2405);
    }

    #[test]
    fn fixed_lacing_rejects_an_uneven_split() {
        let data = block(1, 0, 0x04, &[0x02, 0, 0, 0, 0]);
        let h = parse_header(&data, true).unwrap();
        assert!(frames(&data, &h).is_err());
    }

    #[test]
    fn a_lace_cannot_claim_more_bytes_than_the_block_holds() {
        // Two frames, the first claiming 100000 octets in a 10-octet block.
        let body = vec![0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x64, 0, 0, 0];
        let data = block(1, 0, 0x02, &body);
        let h = parse_header(&data, true).unwrap();
        assert!(frames(&data, &h).is_err());
    }

    #[test]
    fn a_truncated_lace_header_is_an_error_not_a_panic() {
        for n in 0..8usize {
            let body = vec![0xFF; n];
            let data = block(1, 0, 0x02, &body);
            let Ok(h) = parse_header(&data, true) else {
                continue;
            };
            let _ = frames(&data, &h);
        }
    }

    #[test]
    fn a_maximal_frame_count_is_still_bounded() {
        // 255 declares 256 frames; with no size octets left the parse must fail
        // rather than allocate 256 frames out of nothing.
        let data = block(1, 0, 0x02, &[0xFF]);
        let h = parse_header(&data, true).unwrap();
        assert!(frames(&data, &h).is_err());
    }
}
