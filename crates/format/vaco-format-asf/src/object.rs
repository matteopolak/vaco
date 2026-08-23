//! The ASF object header walk: every object in the file — Header Object
//! children, Header Extension Object children, Data Object, index objects —
//! shares the same 24-byte prefix, per [\[ASF\] §2.1](crate):
//!
//! ```text
//! Object ID:   GUID    128 bits
//! Object Size: QWORD    64 bits (LE) -- includes this 24-byte prefix
//! Object Data: BYTE[Object Size - 24]
//! ```
//!
//! [`ObjectHeader::parse`] reads the prefix; [`ObjectIter`] walks a
//! concatenated run of such objects (a Header Object's payload, a Header
//! Extension Object's data) the same way `vaco-format-riff`'s `ChunkIter`
//! walks RIFF chunks — declared sizes are clamped to what is actually
//! present rather than trusted, so a lying `Object Size` yields a short
//! object, never a panic or an unbounded read.

use vaco_core::{Error, Result};

use crate::guid::Guid;

/// Bytes in the shared object prefix: `Object ID` (16) + `Object Size` (8).
pub const HEADER_LEN: u64 = 24;

/// A parsed object prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHeader {
    pub guid: Guid,
    /// The full declared size, including this 24-byte prefix.
    pub size: u64,
}

impl ObjectHeader {
    /// Parse the 24-byte prefix from the start of `data`.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `data` is shorter than [`HEADER_LEN`], or if
    /// the declared size is smaller than the prefix it must at least contain.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let guid_bytes = data
            .get(0..16)
            .ok_or(Error::InvalidData("asf: object header shorter than a GUID"))?;
        let guid = Guid::parse(guid_bytes).ok_or(Error::InvalidData("asf: truncated GUID"))?;
        let size_bytes = data
            .get(16..24)
            .and_then(|s| <[u8; 8]>::try_from(s).ok())
            .ok_or(Error::InvalidData(
                "asf: object header shorter than 24 bytes",
            ))?;
        let size = u64::from_le_bytes(size_bytes);
        if size < HEADER_LEN {
            return Err(Error::InvalidData(
                "asf: object size smaller than its own header",
            ));
        }
        Ok(Self { guid, size })
    }

    /// The payload length: `size - 24`, clamped to what `available` bytes can
    /// actually supply.
    #[must_use]
    pub fn payload_len(&self, available: u64) -> u64 {
        self.size.saturating_sub(HEADER_LEN).min(available)
    }
}

/// One decoded object: its header plus the payload bytes actually present
/// (which may be shorter than the header's declared size, per the module
/// docs).
#[derive(Debug, Clone, Copy)]
pub struct Object<'a> {
    pub guid: Guid,
    /// The declared `Object Size`, untruncated — a caller that needs to
    /// detect a lying size compares this against `payload.len() + 24`.
    pub declared_size: u64,
    pub payload: &'a [u8],
}

/// Walks a byte slice as a run of concatenated ASF objects.
///
/// Used for a Header Object's own children and for a Header Extension
/// Object's `Header Extension Data` — both are, per the spec, "0 or more …
/// objects stored consecutively within the array of bytes. No empty space,
/// padding, or leading or trailing bytes are allowed."
#[derive(Debug, Clone)]
pub struct ObjectIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ObjectIter<'a> {
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl<'a> Iterator for ObjectIter<'a> {
    type Item = Result<Object<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.data.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        let header = match ObjectHeader::parse(rest) {
            Ok(h) => h,
            Err(e) => {
                // Stop rather than loop forever on a header this short cannot
                // be recovered from; the caller sees the one error and the
                // walk ends, exactly like `ChunkIter`'s truncated-tail
                // handling.
                self.pos = self.data.len();
                return Some(Err(e));
            }
        };
        let available = (rest.len() as u64).saturating_sub(HEADER_LEN);
        let payload_len = header.payload_len(available);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "payload_len was just clamped to `available`, itself derived from a slice length"
        )]
        let payload_len = payload_len as usize;
        let payload = rest.get(24..24 + payload_len).unwrap_or(&[]);
        // Advance by the *declared* size when it fits, so a well-formed file
        // walks correctly; when it does not fit, advance to the end so the
        // next call terminates the iterator rather than re-reading the same
        // truncated tail forever.
        let declared = usize::try_from(header.size).unwrap_or(usize::MAX);
        self.pos = self.pos.saturating_add(declared.min(rest.len()).max(24));
        if declared > rest.len() {
            // A declared size that overruns what is left: yield the
            // truncated object once, then stop.
            self.pos = self.data.len();
        }
        Some(Ok(Object {
            guid: header.guid,
            declared_size: header.size,
            payload,
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn obj(guid: Guid, payload: &[u8]) -> Vec<u8> {
        let mut out = guid.as_bytes().to_vec();
        out.extend_from_slice(&(24 + payload.len() as u64).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn header_parses_guid_and_size() {
        let g = crate::well_known::FILE_PROPERTIES_OBJECT;
        let bytes = obj(g, &[1, 2, 3, 4]);
        let h = ObjectHeader::parse(&bytes).unwrap();
        assert_eq!(h.guid, g);
        assert_eq!(h.size, 28);
    }

    #[test]
    fn a_size_smaller_than_the_header_itself_is_rejected() {
        let mut bytes = crate::well_known::PADDING_OBJECT.as_bytes().to_vec();
        bytes.extend_from_slice(&10u64.to_le_bytes());
        assert!(ObjectHeader::parse(&bytes).is_err());
    }

    #[test]
    fn iter_walks_several_objects_back_to_back() {
        let mut data = obj(crate::well_known::FILE_PROPERTIES_OBJECT, &[1; 10]);
        data.extend_from_slice(&obj(crate::well_known::STREAM_PROPERTIES_OBJECT, &[2; 5]));
        let objs: Vec<_> = ObjectIter::new(&data).collect::<Result<_>>().unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].guid, crate::well_known::FILE_PROPERTIES_OBJECT);
        assert_eq!(objs[0].payload, [1u8; 10]);
        assert_eq!(objs[1].guid, crate::well_known::STREAM_PROPERTIES_OBJECT);
        assert_eq!(objs[1].payload, [2u8; 5]);
    }

    #[test]
    fn a_declared_size_past_the_buffer_yields_a_clamped_object_then_stops() {
        let mut data = crate::well_known::PADDING_OBJECT.as_bytes().to_vec();
        data.extend_from_slice(&1_000_000u64.to_le_bytes()); // lies: claims 1MB
        data.extend_from_slice(&[9; 4]); // only 4 real payload bytes
        let objs: Vec<_> = ObjectIter::new(&data).collect();
        assert_eq!(objs.len(), 1);
        let o = objs[0].as_ref().unwrap();
        assert_eq!(o.payload, [9u8; 4]);
    }

    #[test]
    fn empty_input_yields_no_objects() {
        assert!(ObjectIter::new(&[]).next().is_none());
    }
}
