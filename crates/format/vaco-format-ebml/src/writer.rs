//! Building elements.
//!
//! Two shapes are offered, matching the two readers in [`crate::reader`]: the
//! functions here build whole elements into an owned `Vec<u8>`, for masters
//! small enough to assemble before their length is written (an `Info` or
//! `Tracks` element, e.g.); [`write_header`] and [`write_header_unknown`]
//! write just the ID and size octets to an [`IoWriter`], for a master too
//! large to buffer — a `Segment` or a `Cluster` — whose caller streams the
//! body directly and comes back to patch the size, or leaves it unknown.
//!
//! Nothing here validates that an ID is legal where it is placed, or that a
//! caller-supplied value fits the element kind it claims. That is schema
//! knowledge this crate deliberately does not have — see the module docs at
//! the crate root — so a caller building a real Matroska file gets that
//! checking from its own element table, not from here.

use vaco_core::Result;
use vaco_io::IoWriter;

use crate::vint::{id_bytes, vint_min, vint_unknown};

/// One complete element: ID, shortest size, body.
#[must_use]
pub fn element(id: u32, body: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend_from_slice(&vint_min(body.len() as u64));
    out.extend_from_slice(body);
    out
}

/// An element whose size field is the unknown-size marker (RFC 8794 section
/// 6.2), at the widest VINT width so a later patch — if the caller ever wants
/// one — has room to write a real size in the same octets.
#[must_use]
pub fn element_unknown_size(id: u32, body: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend_from_slice(&vint_unknown(8));
    out.extend_from_slice(body);
    out
}

/// An unsigned-integer element, in the fewest octets that hold `value`.
#[must_use]
pub fn uint(id: u32, value: u64) -> Vec<u8> {
    let mut bytes = value.to_be_bytes().to_vec();
    while bytes.len() > 1 && bytes.first() == Some(&0) {
        bytes.remove(0);
    }
    element(id, &bytes)
}

/// A signed-integer element, in the full eight octets — RFC 8794 section 7.2
/// does not require the shortest encoding, and eight octets is simplest to
/// get right for a value that may be negative.
#[must_use]
pub fn int(id: u32, value: i64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

/// An eight-octet float element.
#[must_use]
pub fn float(id: u32, value: f64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

/// A string element. Callers wanting an `Utf8`-kind element (RFC 8794 section
/// 7.5) use the same encoding; the distinction is in the schema, not the
/// bytes.
#[must_use]
pub fn string(id: u32, value: &str) -> Vec<u8> {
    element(id, value.as_bytes())
}

/// A binary element: the payload verbatim.
#[must_use]
pub fn binary(id: u32, value: &[u8]) -> Vec<u8> {
    element(id, value)
}

/// Write an element header (ID plus a known size) directly to `w`, without
/// buffering the body.
///
/// # Errors
///
/// Propagates transport failure.
pub fn write_header(w: &mut IoWriter, id: u32, size: u64) -> Result<()> {
    w.write(&id_bytes(id))?;
    w.write(&vint_min(size))
}

/// Write an element header with the unknown-size marker, at the widest VINT
/// width — the shape a caller patches later if the sink turns out to be
/// seekable, and the shape RFC 8794 section 6.2 requires when it does not.
///
/// # Errors
///
/// Propagates transport failure.
pub fn write_header_unknown(w: &mut IoWriter, id: u32) -> Result<()> {
    w.write(&id_bytes(id))?;
    w.write(&vint_unknown(8))
}

/// Overwrite an unknown-size marker written by [`write_header_unknown`] with
/// a real size, at the same eight-octet width so nothing after it moves.
///
/// The caller is responsible for seeking `w` to the size field first; this
/// only encodes and writes the replacement bytes.
///
/// # Errors
///
/// Propagates transport failure.
pub fn patch_known_size(w: &mut IoWriter, size: u64) -> Result<()> {
    w.write(&crate::vint::vint(size, 8))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::element::Caps;
    use crate::reader::{Slice, as_uint};

    #[test]
    fn uint_round_trips_through_the_reader() {
        for v in [0u64, 1, 255, 256, u64::from(u32::MAX), u64::MAX] {
            let bytes = uint(0xB0, v);
            let kids: Vec<_> = Slice::new(&bytes, Caps::default()).children().collect();
            // `uint` writes one whole element; walking its own bytes as if
            // they were a master's children recovers exactly it.
            assert_eq!(kids.len(), 1);
            assert_eq!(kids[0].id, 0xB0);
            assert_eq!(as_uint(kids[0].data), Some(v));
        }
    }

    #[test]
    fn a_zero_value_still_writes_one_octet() {
        assert_eq!(uint(0xB0, 0).len(), 3); // 1-byte id + 1-byte size + 1-byte body
    }
}
