//! The RIFF chunk grammar: headers, `RIFF`/`LIST` nesting, and word-alignment
//! padding.
//!
//! Microsoft/IBM *Multimedia Programming Interface and Data Specifications
//! 1.0* (August 1991), the "RIFF Chunks" section. A chunk is
//!
//! ```text
//! ckID:4  ckSize:u32(LE)  ckData[ckSize]  pad:u8 if ckSize is odd
//! ```
//!
//! `ckSize` counts only `ckData`; the pad byte (present so every chunk starts
//! on an even file offset) is not part of it and is not itself counted by
//! anything. A `RIFF` or `LIST` chunk is a chunk whose `ckData` begins with a
//! four-byte form/list type and continues with a nested sequence of chunks —
//! there is no separate "container" grammar, `RIFF` and `LIST` are ordinary
//! chunks that happen to hold more chunks.
//!
//! # Why declared sizes are clamped, not trusted
//!
//! Unlike an ISOBMFF box, a RIFF chunk size is not a promise a well-behaved
//! writer keeps under all conditions: a streaming WAV writer that does not
//! know the final length up front commonly writes `0xFFFFFFFF` (or simply the
//! wrong value) for the outer `RIFF` size and sometimes for `data` too, and
//! expects a reader to fall back to "everything that is actually there".
//! [`ChunkIter`] therefore never rejects a chunk whose declared size overruns
//! its container — it clamps the payload to what is actually available and
//! reports that on [`Chunk::truncated`], the same "declared counts are
//! clamped against the payload actually carried" discipline
//! `vaco-format-isom`'s sample tables use. A file that lies about a chunk
//! size gets a short chunk, never a panic and never an unbounded read.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};

/// Bytes in a chunk header: `ckID` (4) + `ckSize` (4).
pub const HEADER_LEN: u64 = 8;

/// A four-character chunk identifier (`ckID`, list type, or form type).
///
/// Named `ChunkId` rather than the more common `FourCC` spelling because
/// `vaco-format-isom` already defines a `FourCc` type for the unrelated
/// ISOBMFF box-type concept (D19: one definition per concept — these are two
/// different four-byte-code concepts in two sibling crates, not the same
/// concept twice, so they are deliberately not unified into a shared type
/// that neither crate's layer would be the obvious home for).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ChunkId(pub [u8; 4]);

impl ChunkId {
    /// From a literal, e.g. `ChunkId::new(b"RIFF")`.
    #[must_use]
    pub const fn new(v: &[u8; 4]) -> Self {
        Self(*v)
    }

    /// The raw bytes, in file order.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 4] {
        self.0
    }

    /// Whether every byte is printable ASCII, i.e. whether [`core::fmt::Display`]
    /// round-trips.
    #[must_use]
    pub const fn is_printable(self) -> bool {
        let [a, b, c, d] = self.0;
        a.is_ascii_graphic() && b.is_ascii_graphic() && c.is_ascii_graphic() && d.is_ascii_graphic()
    }
}

impl core::fmt::Display for ChunkId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.0 {
            if b.is_ascii_graphic() || b == b' ' {
                write!(f, "{}", char::from(b))?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl core::fmt::Debug for ChunkId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ChunkId({self})")
    }
}

impl From<[u8; 4]> for ChunkId {
    fn from(v: [u8; 4]) -> Self {
        Self(v)
    }
}

/// The well-known chunk and form/list types this crate names.
pub mod ids {
    use super::ChunkId;

    macro_rules! ids {
        ($($name:ident = $lit:literal),* $(,)?) => {
            $(
                #[doc = concat!("`", stringify!($lit), "`.")]
                pub const $name: ChunkId = ChunkId(*$lit);
            )*
        };
    }

    ids! {
        RIFF = b"RIFF", RF64 = b"RF64", LIST = b"LIST", JUNK = b"JUNK", PAD  = b"PAD ",
        DS64 = b"ds64", FMT  = b"fmt ", DATA = b"data", FACT = b"fact",
        WAVE = b"WAVE", AVI_ = b"AVI ", INFO = b"INFO",
    }
}

/// One parsed chunk header: identifier and *declared* size.
///
/// The declared size is carried verbatim; [`ChunkIter`] is what turns it into
/// a payload clamped against what the container actually holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    pub id: ChunkId,
    /// `ckSize` as the file declares it — not yet checked against anything.
    pub declared_size: u32,
}

impl ChunkHeader {
    /// Parse an eight-byte header from the start of `data`.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] when fewer than eight bytes are present.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = ByteReader::new(data);
        let id = ChunkId(read_tag(&mut r));
        let declared_size = r.le32();
        r.check()?;
        Ok(Self { id, declared_size })
    }
}

fn read_tag(r: &mut ByteReader<'_>) -> [u8; 4] {
    let b = r.bytes(4);
    // `ByteReader::bytes` returns a short slice on truncation rather than
    // failing outright; a short tag still round-trips through `r.check()`
    // failing afterwards, so zero-padding here cannot hide a truncation.
    let mut out = [0u8; 4];
    let n = b.len().min(4);
    if let (Some(dst), Some(src)) = (out.get_mut(..n), b.get(..n)) {
        dst.copy_from_slice(src);
    }
    out
}

/// One chunk with its payload resolved against the space actually available
/// in its container.
#[derive(Debug, Clone, Copy)]
pub struct Chunk<'a> {
    pub id: ChunkId,
    /// `ckSize` as declared. May exceed `payload.len()`; see [`Chunk::truncated`].
    pub declared_size: u32,
    /// The payload, clamped to what the container holds. Never longer than
    /// `declared_size`, and never reads past the buffer `ChunkIter` was given.
    pub payload: &'a [u8],
    /// True when `declared_size` claimed more than the container actually had
    /// left (including the all-ones "unknown length" convention some
    /// streaming writers use). The chunk is not an error — it is exactly as
    /// much of it as exists.
    pub truncated: bool,
    /// Byte offset of this chunk's header, relative to the start of the
    /// buffer [`ChunkIter`] was constructed over.
    pub offset: u64,
}

impl<'a> Chunk<'a> {
    /// Whether this chunk's payload is itself a sequence of chunks: a `RIFF`
    /// or `LIST` chunk, whose first four payload bytes are a form/list type
    /// and whose remaining bytes are nested chunks.
    #[must_use]
    pub fn is_container(&self) -> bool {
        self.id == ids::RIFF || self.id == ids::LIST
    }

    /// For a container chunk, the four-byte form/list type and an iterator
    /// over the nested chunks that follow it.
    ///
    /// `None` if the payload is too short to hold the form/list type — a
    /// container chunk is defined to carry it, so a shorter one is malformed
    /// rather than merely empty.
    #[must_use]
    pub fn children(&self) -> Option<(ChunkId, ChunkIter<'a>)> {
        let form = self.payload.get(..4)?;
        let form = ChunkId(<[u8; 4]>::try_from(form).unwrap_or([0; 4]));
        let rest = self.payload.get(4..).unwrap_or(&[]);
        Some((form, ChunkIter::new(rest, self.offset.saturating_add(12))))
    }
}

/// Flat iteration over one container's direct child chunks.
///
/// Mirrors `vaco-format-isom::boxes::BoxIter` in shape (flat, fails once and
/// stops on a broken chain) but not in size-trust policy: see the module
/// documentation for why a RIFF chunk's declared size is clamped rather than
/// rejected.
#[derive(Debug, Clone)]
pub struct ChunkIter<'a> {
    data: &'a [u8],
    pos: usize,
    base: u64,
    done: bool,
}

impl<'a> ChunkIter<'a> {
    /// Iterate `data`, whose first byte sits at offset `base` in whatever
    /// larger buffer the caller is tracking offsets against.
    #[must_use]
    pub const fn new(data: &'a [u8], base: u64) -> Self {
        Self {
            data,
            pos: 0,
            base,
            done: false,
        }
    }

    /// The first direct child with identifier `id`.
    ///
    /// Stops at the first malformed child rather than searching past it, the
    /// same rule `BoxIter::find` uses.
    #[must_use]
    pub fn find(self, id: ChunkId) -> Option<Chunk<'a>> {
        self.flatten().find(|c| c.id == id)
    }
}

impl<'a> Iterator for ChunkIter<'a> {
    type Item = Result<Chunk<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let rest = self.data.get(self.pos..)?;
        if rest.len() < HEADER_LEN as usize {
            self.done = true;
            // A container may legitimately end with a single dangling pad
            // byte; that is not corruption worth failing the walk over.
            return None;
        }
        let header = match ChunkHeader::parse(rest) {
            Ok(h) => h,
            Err(e) => {
                self.done = true;
                return Some(Err(e));
            }
        };
        let avail = rest.len().saturating_sub(HEADER_LEN as usize);
        let want = header.declared_size as usize;
        // `0xFFFF_FFFF` is the documented "length unknown, read to EOF"
        // convention some streaming writers use for `data`/`RIFF`; treat it
        // the same as an ordinary overrun so both end up clamped identically.
        let truncated = header.declared_size == u32::MAX || want > avail;
        let take = want.min(avail);
        let Some(payload) = rest.get(HEADER_LEN as usize..HEADER_LEN as usize + take) else {
            self.done = true;
            return Some(Err(Error::InvalidData("riff: chunk payload truncated")));
        };
        // A pad byte only exists in the stream when the chunk actually ran to
        // its declared (odd) length; a chunk we clamped short has nothing to
        // skip because we are already sitting at the end of the container.
        let pad = usize::from(!truncated && header.declared_size % 2 == 1);
        let offset = self.base.saturating_add(self.pos as u64);
        let consumed = HEADER_LEN as usize + take + pad;
        // `consumed >= HEADER_LEN == 8` unconditionally, so position strictly
        // advances every iteration and the walk terminates.
        self.pos = self.pos.saturating_add(consumed);
        if truncated {
            // Nothing legitimately follows a clamped chunk in this buffer.
            self.done = true;
        }
        Some(Ok(Chunk {
            id: header.id,
            declared_size: header.declared_size,
            payload,
            truncated,
            offset,
        }))
    }
}

/// The outermost `RIFF`/`RF64` header: container id, declared size, and the
/// four-byte form type (`WAVE`, `AVI `, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiffHeader {
    /// `RIFF` for an ordinary file, `RF64` for the 64-bit-size extension
    /// (EBU Tech 3306 / MS `RF64`); see [`crate::rf64`].
    pub container: ChunkId,
    pub declared_size: u32,
    pub form_type: ChunkId,
}

impl RiffHeader {
    /// Bytes in the outermost header: `ckID`(4) + `ckSize`(4) + `formType`(4).
    pub const LEN: usize = 12;

    /// Parse the first twelve bytes of a file.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] when fewer than twelve bytes are present;
    /// [`Error::InvalidData`] when the container id is neither `RIFF` nor
    /// `RF64`.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = ByteReader::new(data);
        let container = ChunkId(read_tag(&mut r));
        let declared_size = r.le32();
        let form_type = ChunkId(read_tag(&mut r));
        r.check()?;
        if container != ids::RIFF && container != ids::RF64 {
            return Err(Error::InvalidData("riff: missing RIFF/RF64 signature"));
        }
        Ok(Self {
            container,
            declared_size,
            form_type,
        })
    }

    /// An iterator over the chunks nested inside this container, given the
    /// full file (or as much of it as is available).
    #[must_use]
    pub fn children<'a>(&self, data: &'a [u8]) -> ChunkIter<'a> {
        let rest = data.get(Self::LEN..).unwrap_or(&[]);
        ChunkIter::new(rest, Self::LEN as u64)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;

    /// Build one raw chunk: id, LE size, data, and the pad byte if needed.
    pub(crate) fn chunk(id: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&id);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    #[test]
    fn header_parses_id_and_size() {
        let data = chunk(*b"fmt ", &[1, 2, 3, 4]);
        let h = ChunkHeader::parse(&data).unwrap();
        assert_eq!(h.id, ids::FMT);
        assert_eq!(h.declared_size, 4);
    }

    #[test]
    fn odd_sized_chunk_gets_a_pad_byte() {
        let data = chunk(*b"data", &[1, 2, 3]);
        assert_eq!(data.len(), 8 + 3 + 1);
        let c = ChunkIter::new(&data, 0).next().unwrap().unwrap();
        assert_eq!(c.payload, &[1, 2, 3]);
        assert!(!c.truncated);
    }

    #[test]
    fn even_sized_chunk_has_no_pad_byte() {
        let data = chunk(*b"data", &[1, 2, 3, 4]);
        assert_eq!(data.len(), 8 + 4);
        let c = ChunkIter::new(&data, 0).next().unwrap().unwrap();
        assert_eq!(c.payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn iteration_yields_siblings_after_padding() {
        let mut file = chunk(*b"fmt ", &[1, 2, 3]); // odd -> padded
        file.extend_from_slice(&chunk(*b"data", &[9, 9]));
        let got: Vec<_> = ChunkIter::new(&file, 100).flatten().collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].offset, 100);
        assert_eq!(got[1].offset, 100 + 8 + 3 + 1);
        assert_eq!(got[1].payload, &[9, 9]);
    }

    #[test]
    fn a_declared_size_past_the_end_is_clamped_not_rejected() {
        let mut data = b"data".to_vec();
        data.extend_from_slice(&1_000_000u32.to_le_bytes());
        data.extend_from_slice(&[1, 2, 3]);
        let c = ChunkIter::new(&data, 0).next().unwrap().unwrap();
        assert!(c.truncated);
        assert_eq!(c.payload, &[1, 2, 3]);
    }

    #[test]
    fn all_ones_size_is_treated_as_unknown_and_clamped() {
        let mut data = b"data".to_vec();
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        data.extend_from_slice(&[7; 5]);
        let c = ChunkIter::new(&data, 0).next().unwrap().unwrap();
        assert!(c.truncated);
        assert_eq!(c.payload, &[7; 5]);
    }

    #[test]
    fn a_truncated_chunk_ends_the_walk() {
        let mut file = b"data".to_vec();
        file.extend_from_slice(&u32::MAX.to_le_bytes());
        file.extend_from_slice(&[7; 5]);
        file.extend_from_slice(&chunk(*b"fmt ", &[1, 2, 3, 4])); // never reached
        let got: Vec<_> = ChunkIter::new(&file, 0).flatten().collect();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn iteration_stops_at_a_trailing_stub() {
        let data = [0u8; 5]; // fewer than HEADER_LEN
        let got: Vec<_> = ChunkIter::new(&data, 0).collect();
        assert!(got.is_empty());
    }

    #[test]
    fn riff_header_round_trips_wave() {
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&36u32.to_le_bytes());
        file.extend_from_slice(b"WAVE");
        let h = RiffHeader::parse(&file).unwrap();
        assert_eq!(h.container, ids::RIFF);
        assert_eq!(h.form_type, ids::WAVE);
        assert_eq!(h.declared_size, 36);
    }

    #[test]
    fn riff_header_rejects_a_bad_signature() {
        let mut file = b"BLAH".to_vec();
        file.extend_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(b"WAVE");
        assert!(RiffHeader::parse(&file).is_err());
    }

    #[test]
    fn riff_header_children_walks_the_top_level() {
        let mut body = chunk(*b"fmt ", &[0; 16]);
        body.extend_from_slice(&chunk(*b"data", &[1, 2]));
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&((4 + body.len()) as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend_from_slice(&body);
        let h = RiffHeader::parse(&file).unwrap();
        let got: Vec<_> = h.children(&file).flatten().collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, ids::FMT);
        assert_eq!(got[1].id, ids::DATA);
    }

    #[test]
    fn list_chunk_exposes_its_list_type_and_children() {
        let inner = chunk(*b"INAM", b"hi");
        let mut list_payload = b"INFO".to_vec();
        list_payload.extend_from_slice(&inner);
        let file = chunk(*b"LIST", &list_payload);
        let c = ChunkIter::new(&file, 0).next().unwrap().unwrap();
        assert!(c.is_container());
        let (form, mut kids) = c.children().unwrap();
        assert_eq!(form, ids::INFO);
        let first = kids.next().unwrap().unwrap();
        assert_eq!(first.id, ChunkId::new(b"INAM"));
        assert_eq!(first.payload, b"hi");
    }

    #[test]
    fn chunk_id_display_escapes_non_ascii() {
        assert_eq!(ChunkId::new(b"data").to_string(), "data");
        assert_eq!(ChunkId([0, b'a', b'b', b'c']).to_string(), "\\x00abc");
    }
}
