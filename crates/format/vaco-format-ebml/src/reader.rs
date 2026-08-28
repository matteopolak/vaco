//! Reading elements: a flat in-memory child walker, value accessors, and a
//! single-header reader over a seekable stream.
//!
//! # The two readers, and why there are two
//!
//! | Reader | Input | Used for |
//! |---|---|---|
//! | [`Slice`] | `&[u8]` already in memory | any bounded master read whole |
//! | [`read_header`] | [`IoContext`] | one element header at the stream's current position |
//!
//! Bounded masters are read whole and walked in memory because that is both
//! simpler and faster; the streaming path exists for elements that may be of
//! unknown size and arbitrarily large — a Matroska `Cluster` is the motivating
//! case, in both the demuxer that reads one and the muxer that writes one.

use vaco_core::{Error, Result};
use vaco_io::IoContext;

use crate::element::{Caps, Header};
use crate::vint::{Size, all_ones, read_id, read_size, vint_len};

/// A cursor over one master element's data, already in memory.
///
/// Yields direct children only. Nesting is the caller's business, which is
/// what keeps recursion explicit and countable against a caller-chosen depth
/// cap (see [`crate::element::MAX_DEPTH`]).
#[derive(Debug, Clone, Copy)]
pub struct Slice<'a> {
    data: &'a [u8],
    caps: Caps,
}

/// One child element, with its data already sliced out.
#[derive(Debug, Clone, Copy)]
pub struct Child<'a> {
    pub id: u32,
    pub data: &'a [u8],
    /// Offset of the element's ID octet within the parent's data.
    pub offset: usize,
    /// Offset of the element's *data* within the parent's data.
    pub data_offset: usize,
}

impl<'a> Slice<'a> {
    #[must_use]
    pub const fn new(data: &'a [u8], caps: Caps) -> Self {
        Self { data, caps }
    }

    /// Iterate the direct children.
    ///
    /// A malformed child ends the iteration rather than failing the parse: a
    /// truncated master should still yield the children that were complete,
    /// which is what every other implementation does and what a partially
    /// written file needs.
    ///
    /// **A child whose declared size overruns this master is clamped to
    /// what `data` actually holds, not rejected** (see [`Children::next`]).
    /// This is the same house answer `vaco-format-isom::boxes::BoxIter`,
    /// `vaco-format-riff::chunk::ChunkIter` and
    /// `vaco-format-asf::object::ObjectIter` independently reached for the
    /// identical shape — a flat walk over an already-in-memory,
    /// already-bounded buffer — and for the identical reason: clamping
    /// never reads a byte that was not already part of this same buffer,
    /// so it cannot escape into a sibling container's data the way
    /// resynchronising by scanning content for a new header would.
    /// [`crate::reader::read_header`]'s own streaming path (used for a
    /// large or unknown-size master like a Matroska `Cluster`, read
    /// directly off an [`IoContext`] rather than into memory first) makes
    /// no such promise at all — it parses whatever size a header declares
    /// verbatim and leaves bounding it to the caller, which is where
    /// `vaco-demux-matroska::demux::read_body` does reject a size that
    /// overruns the file, deliberately (plan 13 section 2.2.2 rule 3):
    /// a corrupted length seeked over rather than held in memory could
    /// otherwise swallow real, unrelated data into a mis-sized sibling,
    /// the same asymmetry `vaco-format-isom` states between `BoxIter` and
    /// `TopLevelScanner`.
    #[must_use]
    pub const fn children(&self) -> Children<'a> {
        Children {
            data: self.data,
            pos: 0,
            caps: self.caps,
        }
    }

    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }
}

/// Iterator over the direct children of a master element.
///
/// Deliberately neither `Copy` nor `Clone`: a copied iterator that keeps its
/// own cursor is a trap in a parser, where "iterate the children" and
/// "iterate them again from where I stopped" look identical at the call site.
#[derive(Debug)]
pub struct Children<'a> {
    data: &'a [u8],
    pos: usize,
    caps: Caps,
}

impl<'a> Iterator for Children<'a> {
    type Item = Child<'a>;

    fn next(&mut self) -> Option<Child<'a>> {
        let rest = self.data.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        let (id, id_len) = read_id(rest, self.caps.max_id_len).ok()?;
        let after_id = rest.get(id_len..)?;
        let (size, size_len) = read_size(after_id, self.caps.max_size_len).ok()?;
        let header_len = id_len.checked_add(size_len)?;
        let body = rest.get(header_len..)?;
        // An unknown size inside an in-memory master runs to the end of that
        // master: there is nothing after it to terminate against. A *known*
        // size that overruns `body` is clamped the same way, deliberately —
        // see `Slice::children`'s doc comment for why this is safe here and
        // is not the same question as `read_header`'s streaming path.
        let n = match size {
            Size::Known(n) => usize::try_from(n).ok()?.min(body.len()),
            Size::Unknown => body.len(),
        };
        let data = body.get(..n)?;
        let offset = self.pos;
        let data_offset = offset.checked_add(header_len)?;
        self.pos = data_offset.checked_add(n)?;
        Some(Child {
            id,
            data,
            offset,
            data_offset,
        })
    }
}

// --------------------------------------------------------------- accessors

/// An unsigned integer element's value, per RFC 8794 section 7.1.
///
/// Lengths above eight octets are rejected rather than truncated.
#[must_use]
pub fn as_uint(data: &[u8]) -> Option<u64> {
    if data.len() > 8 {
        return None;
    }
    Some(data.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
}

/// A signed integer element's value, per RFC 8794 section 7.2: big-endian
/// two's complement, sign-extended from whatever length was stored.
#[must_use]
pub fn as_int(data: &[u8]) -> Option<i64> {
    if data.len() > 8 {
        return None;
    }
    let mut v: i64 = match data.first() {
        Some(&b) if b & 0x80 != 0 => -1,
        Some(_) => 0,
        None => return Some(0),
    };
    for &b in data {
        v = (v << 8) | i64::from(b);
    }
    Some(v)
}

/// A float element's value, per RFC 8794 section 7.3: IEEE 754 in 0, 4 or 8
/// octets. A zero-length float is 0.0 (RFC 8794 section 6.1's empty element).
#[must_use]
pub fn as_float(data: &[u8]) -> Option<f64> {
    match data.len() {
        0 => Some(0.0),
        4 => data
            .try_into()
            .ok()
            .map(|b| f64::from(f32::from_be_bytes(b))),
        8 => data.try_into().ok().map(f64::from_be_bytes),
        _ => None,
    }
}

/// A string element's value with any trailing `NUL` padding removed.
///
/// RFC 8794 section 7.4 permits zero octets after the string, and real files
/// use them to pad a field that is rewritten in place.
#[must_use]
pub fn as_str(data: &[u8]) -> Option<&str> {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    core::str::from_utf8(data.get(..end)?).ok()
}

// --------------------------------------------------------------- io reader

/// Read one element header at the current position of `io`.
///
/// Returns `Ok(None)` at a clean element boundary at end of input, which is
/// not an error: a well-formed file ends exactly there.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed ID or size, [`Error::UnexpectedEof`]
/// when the header itself is truncated, and whatever the transport reports.
pub fn read_header(io: &mut IoContext, caps: Caps) -> Result<Option<Header>> {
    let pos = io.pos();
    let first = match io.r8() {
        Ok(b) => b,
        Err(Error::UnexpectedEof | Error::Eof) => return Ok(None),
        Err(e) => return Err(e),
    };
    let id_len = vint_len(first).ok_or(Error::InvalidData("element id longer than 8 octets"))?;
    if id_len > caps.max_id_len {
        return Err(Error::InvalidData("element id longer than EBMLMaxIDLength"));
    }
    let mut id = u32::from(first);
    for _ in 1..id_len {
        id = (id << 8) | u32::from(io.r8()?);
    }

    let first = io.r8()?;
    let size_len =
        vint_len(first).ok_or(Error::InvalidData("element size longer than 8 octets"))?;
    if size_len > caps.max_size_len {
        return Err(Error::InvalidData(
            "element size longer than EBMLMaxSizeLength",
        ));
    }
    let mut value = u64::from(first & !(0x80u8 >> (size_len - 1)));
    for _ in 1..size_len {
        value = (value << 8) | u64::from(io.r8()?);
    }
    let size = if value == all_ones(size_len) {
        Size::Unknown
    } else {
        Size::Known(value)
    };
    Ok(Some(Header {
        id,
        size,
        pos,
        data_pos: io.pos(),
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::writer::uint;

    #[test]
    fn children_are_yielded_with_their_data_offsets() {
        let mut body = uint(0xB0, 320);
        let width_len = body.len();
        body.extend_from_slice(&uint(0xBA, 240));
        let kids: Vec<_> = Slice::new(&body, Caps::default()).children().collect();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].id, 0xB0);
        assert_eq!(as_uint(kids[0].data), Some(320));
        assert_eq!(kids[1].offset, width_len);
        assert_eq!(as_uint(kids[1].data), Some(240));
        assert!(kids[1].data_offset > kids[1].offset);
    }

    #[test]
    fn a_truncated_tail_ends_the_iteration_instead_of_failing() {
        let mut body = uint(0xB0, 320);
        body.extend_from_slice(&[0x54, 0xB0, 0x88, 0x00]); // header claiming 8 octets
        let kids: Vec<_> = Slice::new(&body, Caps::default()).children().collect();
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[1].data.len(), 1);
    }

    #[test]
    fn a_child_can_never_claim_more_than_its_parent_holds() {
        let body = [0xB0, 0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        for child in Slice::new(&body, Caps::default()).children() {
            assert!(child.data.len() <= body.len());
        }
    }

    #[test]
    fn integers_are_read_at_every_stored_width() {
        assert_eq!(as_uint(&[]), Some(0));
        assert_eq!(as_uint(&[0x01]), Some(1));
        assert_eq!(as_uint(&[0xFF; 8]), Some(u64::MAX));
        assert_eq!(as_uint(&[0; 9]), None);
        assert_eq!(as_int(&[0xFF]), Some(-1));
        assert_eq!(as_int(&[0x80]), Some(-128));
        assert_eq!(as_int(&[0x7F]), Some(127));
        assert_eq!(as_int(&[]), Some(0));
    }

    #[test]
    fn floats_accept_only_the_three_defined_widths() {
        assert_eq!(as_float(&[]), Some(0.0));
        assert_eq!(as_float(&1.5f32.to_be_bytes()), Some(1.5));
        assert_eq!(as_float(&2008.0f64.to_be_bytes()), Some(2008.0));
        assert_eq!(as_float(&[0, 0]), None);
    }

    #[test]
    fn strings_stop_at_their_first_nul() {
        assert_eq!(as_str(b"webm\0\0\0\0"), Some("webm"));
        assert_eq!(as_str(b"matroska"), Some("matroska"));
        assert_eq!(as_str(&[0xFF, 0xFE]), None);
    }
}
