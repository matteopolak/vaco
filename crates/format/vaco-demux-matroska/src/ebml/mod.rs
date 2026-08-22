//! The EBML layer: variable-length integers, the element grammar, the schema
//! table, and the unknown-size termination rule.
//!
//! This module knows nothing about Matroska semantics — only about elements,
//! their IDs, their sizes and which element may legally contain which. It is
//! kept behind a module boundary with no dependency on the rest of the crate so
//! that it can be promoted to `vaco-format-ebml` unchanged if a Matroska muxer
//! or another EBML-based format wants it.
//!
//! # Specification
//!
//! RFC 8794 sections 4 (element structure), 5 (element ID and data size VINTs),
//! 6.2 (unknown data size) and 11.2 (the EBML header elements). The Matroska
//! rows of [`schema`] come from RFC 9559 section 5.
//!
//! # The three readers, and why there are three
//!
//! | Reader | Input | Used for |
//! |---|---|---|
//! | [`Slice`] | `&[u8]` already in memory | every bounded master: `Info`, `Tracks`, `Cues`, `Tags`, … |
//! | [`read_header`] | [`IoContext`] | one element header at the current stream position |
//! | [`Stack`] | — | the open-element stack, which is what makes unknown sizes terminable |
//!
//! Bounded masters are read whole and walked in memory because that is both
//! simpler and faster; the streaming path exists because a `Cluster` may be of
//! unknown size and arbitrarily large, and because a live `WebM` stream is not
//! seekable at all.
//!
//! # Bounds
//!
//! Everything here is driven by attacker-controlled byte counts:
//!
//! * `EBMLMaxIDLength` and `EBMLMaxSizeLength` are capped at [`MAX_ID_LEN`] and
//!   [`MAX_SIZE_LEN`] regardless of what the header declares, and a declaration
//!   *larger* than the cap is rejected rather than clamped.
//! * [`Slice::children`] is a flat iterator; nesting is expressed by the caller
//!   recursing with an explicit `depth` checked against [`MAX_DEPTH`].
//! * [`Stack`] has a fixed frame ceiling and cannot grow past it.

use vaco_core::{Error, Result};
use vaco_io::IoContext;

pub mod schema;

/// Longest element ID this implementation will read, in octets.
///
/// RFC 8794 section 11.2.4 gives `EBMLMaxIDLength` a default and a minimum of 4,
/// and no Matroska element uses more. A header declaring more is rejected: the
/// value is attacker-controlled and widening it buys nothing.
pub const MAX_ID_LEN: u8 = 4;

/// Longest element data size this implementation will read, in octets.
///
/// RFC 8794 section 6.3: eight octets expresses up to `2^56 - 2`, which is
/// already 72 PB. This is also the ceiling `EBMLMaxSizeLength` may declare.
pub const MAX_SIZE_LEN: u8 = 8;

/// How deeply a master element may nest before the parse is abandoned.
///
/// Matroska's deepest defined path is seven levels
/// (`Segment\Tracks\TrackEntry\Video\Colour\MasteringMetadata\LuminanceMax`),
/// but `SimpleTag` and `ChapterAtom` are recursive, so a file can nominate an
/// arbitrary depth. Sixteen leaves room for legal nesting and turns the
/// pathological case into an error instead of stack growth.
pub const MAX_DEPTH: u8 = 16;

/// The EBML value types RFC 8794 section 7 defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    Master,
    UInt,
    Int,
    Float,
    Str,
    Utf8,
    Binary,
    Date,
}

/// One row of the schema.
#[derive(Debug, Clone, Copy)]
pub struct ElementDef {
    pub id: u32,
    pub name: &'static str,
    pub kind: ElementKind,
    /// The ID of the only element this one may appear inside;
    /// [`schema::ROOT`] for a root element and [`schema::GLOBAL`] for a global.
    pub parent: u32,
    /// Whether the element may also appear inside itself, as `SimpleTag` and
    /// `ChapterAtom` do.
    pub recursive: bool,
    /// Whether RFC 8794 section 11.1.6.10's `unknownsizeallowed` is set, which
    /// only `Segment` and `Cluster` have.
    pub unknown_size_ok: bool,
}

/// Look up `id` in the schema.
#[must_use]
pub fn lookup(id: u32) -> Option<&'static ElementDef> {
    schema::ELEMENTS
        .binary_search_by_key(&id, |d| d.id)
        .ok()
        .and_then(|i| schema::ELEMENTS.get(i))
}

/// Whether `id` is legal directly inside `parent`.
///
/// Global elements are legal inside every master (RFC 8794 section 11.1.6.5),
/// and an ID the schema does not know is not legal anywhere — which is what
/// makes it unable to terminate an unknown-size element.
#[must_use]
pub fn is_child_of(id: u32, parent: u32) -> bool {
    match lookup(id) {
        Some(def) => {
            def.parent == schema::GLOBAL
                || def.parent == parent
                || (def.recursive && def.id == parent)
        }
        None => false,
    }
}

/// Whether `id` is a root element, which terminates any open element.
#[must_use]
pub fn is_root(id: u32) -> bool {
    lookup(id).is_some_and(|d| d.parent == schema::ROOT)
}

/// An element's data size, which RFC 8794 section 6.2 allows to be unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Known(u64),
    Unknown,
}

impl Size {
    /// The size in octets, or `None` when unknown.
    #[must_use]
    pub const fn known(self) -> Option<u64> {
        match self {
            Self::Known(n) => Some(n),
            Self::Unknown => None,
        }
    }
}

/// An element header: everything before the element data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub id: u32,
    pub size: Size,
    /// Byte offset of the first octet of the element ID.
    pub pos: u64,
    /// Byte offset of the first octet of the element data.
    pub data_pos: u64,
}

impl Header {
    /// One past the last octet of the element data, when the size is known.
    #[must_use]
    pub fn end(&self) -> Option<u64> {
        self.size.known().and_then(|n| self.data_pos.checked_add(n))
    }
}

/// The two length caps, as the EBML header declared them.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub max_id_len: u8,
    pub max_size_len: u8,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            max_id_len: MAX_ID_LEN,
            max_size_len: MAX_SIZE_LEN,
        }
    }
}

impl Caps {
    /// Adopt the header's declared lengths, rejecting anything above our own
    /// ceiling.
    ///
    /// RFC 8794 section 11.2.4 sets the minimum of `EBMLMaxIDLength` at 4, so a
    /// smaller declaration is treated as 4 rather than honoured — honouring it
    /// would make us reject `Segment`, whose ID is four octets, on a file every
    /// other implementation reads.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when the file asks for more than we will read.
    pub fn adopt(&mut self, max_id_len: u64, max_size_len: u64) -> Result<()> {
        if max_id_len > u64::from(MAX_ID_LEN) {
            return Err(Error::Unsupported("EBMLMaxIDLength above 4"));
        }
        if max_size_len > u64::from(MAX_SIZE_LEN) {
            return Err(Error::Unsupported("EBMLMaxSizeLength above 8"));
        }
        self.max_id_len = MAX_ID_LEN;
        self.max_size_len = if max_size_len == 0 {
            MAX_SIZE_LEN
        } else {
            max_size_len as u8
        };
        Ok(())
    }
}

// ------------------------------------------------------------------- VINTs

/// Octet length of a VINT from its leading octet, or `None` when the octet is
/// zero and the length would exceed eight.
#[must_use]
pub const fn vint_len(first: u8) -> Option<u8> {
    if first == 0 {
        None
    } else {
        Some(first.leading_zeros() as u8 + 1)
    }
}

/// Decode an element ID from `buf`, returning it and the octets consumed.
///
/// The ID keeps its length marker — RFC 8794 section 5: "the `VINT_MARKER` and
/// `VINT_DATA` of the Element ID are used together" — so `0x1A45DFA3` is the
/// stored value, not a stripped one.
///
/// # Errors
///
/// [`Error::InvalidData`] for a zero leading octet or a length above
/// `max_id_len`, and [`Error::UnexpectedEof`] when `buf` is too short.
pub fn read_id(buf: &[u8], max_id_len: u8) -> Result<(u32, usize)> {
    let first = *buf.first().ok_or(Error::UnexpectedEof)?;
    let len = vint_len(first).ok_or(Error::InvalidData("element id longer than 8 octets"))?;
    if len > max_id_len {
        return Err(Error::InvalidData("element id longer than EBMLMaxIDLength"));
    }
    let bytes = buf
        .get(..len as usize)
        .ok_or(Error::UnexpectedEof)?
        .iter()
        .fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
    Ok((bytes, len as usize))
}

/// Decode an element data size from `buf`, returning it and the octets consumed.
///
/// # Errors
///
/// As [`read_id`], against `max_size_len`.
pub fn read_size(buf: &[u8], max_size_len: u8) -> Result<(Size, usize)> {
    let first = *buf.first().ok_or(Error::UnexpectedEof)?;
    let len = vint_len(first).ok_or(Error::InvalidData("element size longer than 8 octets"))?;
    if len > max_size_len {
        return Err(Error::InvalidData(
            "element size longer than EBMLMaxSizeLength",
        ));
    }
    let slice = buf.get(..len as usize).ok_or(Error::UnexpectedEof)?;
    // Strip the marker bit out of the leading octet; the rest is big-endian.
    let mut value = u64::from(first & !(0x80u8 >> (len - 1)));
    for &b in slice.iter().skip(1) {
        value = (value << 8) | u64::from(b);
    }
    // All VINT_DATA bits set is the unknown-size marker (RFC 8794 section 6.2).
    let data_bits = 7u32 * u32::from(len);
    let all_ones = if data_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << data_bits) - 1
    };
    let size = if value == all_ones {
        Size::Unknown
    } else {
        Size::Known(value)
    };
    Ok((size, len as usize))
}

/// Decode the signed VINT that EBML lacing uses for its size deltas.
///
/// RFC 9559 section 10.3.3: the unsigned value is read as a normal VINT and then
/// `2^((7n)-1) - 1` is subtracted, where `n` is the octet length.
///
/// # Errors
///
/// As [`read_size`]; an unknown-size marker here is [`Error::InvalidData`].
pub fn read_signed_vint(buf: &[u8]) -> Result<(i64, usize)> {
    let (size, used) = read_size(buf, MAX_SIZE_LEN)?;
    let raw = match size {
        Size::Known(v) => v,
        Size::Unknown => return Err(Error::InvalidData("lace size delta is the unknown marker")),
    };
    // 7*n - 1 <= 55 for n <= 8, so the shift and the cast are both in range.
    let bias = (1i64 << (7 * used as u32 - 1)) - 1;
    Ok((raw.cast_signed().wrapping_sub(bias), used))
}

// -------------------------------------------------------------- slice reader

/// A cursor over one master element's data, already in memory.
///
/// Yields direct children only. Nesting is the caller's business, which is what
/// keeps recursion explicit and countable — see [`MAX_DEPTH`].
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
    ///
    /// Carried rather than recomputed because `Packet::pos` must be the block
    /// element's data offset, and a `Block` inside a `BlockGroup` is only
    /// reachable through this iterator.
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
    /// truncated `Tags` element should still yield the tags that were complete,
    /// which is what every other implementation does and what a partially
    /// written file needs.
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
/// own cursor is a trap in a parser, where "iterate the children" and "iterate
/// them again from where I stopped" look identical at the call site.
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
        // master: there is nothing after it to terminate against. Only `Segment`
        // and `Cluster` are ever read this way, and neither is read in memory.
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
/// Returns `Ok(None)` at a clean element boundary at end of input, which is not
/// an error: a Matroska file ends exactly there.
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
    let data_bits = 7u32 * u32::from(size_len);
    let all_ones = if data_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << data_bits) - 1
    };
    let size = if value == all_ones {
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

// ------------------------------------------------------------ element stack

/// One open master element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub id: u32,
    /// One past the last octet of this element's data, when known.
    pub end: Option<u64>,
}

/// The stack of open master elements, and with it RFC 8794 section 6.2.
///
/// The rule the whole streaming parser turns on: an unknown-size element ends at
/// the first element that is not one of its legal children. Deciding that needs
/// two things — the schema, and knowing what is currently open — and this type
/// is the second.
#[derive(Debug, Default, Clone)]
pub struct Stack {
    frames: Vec<Frame>,
}

impl Stack {
    /// Deepest nesting the stack will hold. Matroska needs five.
    pub const MAX_FRAMES: usize = MAX_DEPTH as usize;

    #[must_use]
    pub const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// Open a master element.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] past [`Stack::MAX_FRAMES`].
    pub fn push(&mut self, id: u32, end: Option<u64>) -> Result<()> {
        if self.frames.len() >= Self::MAX_FRAMES {
            return Err(Error::LimitExceeded {
                limit: "ebml_depth",
                requested: self.frames.len() as u64 + 1,
                cap: Self::MAX_FRAMES as u64,
            });
        }
        self.frames.push(Frame { id, end });
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    #[must_use]
    pub fn top(&self) -> Option<Frame> {
        self.frames.last().copied()
    }

    /// The ID of the innermost open element, or [`schema::ROOT`].
    #[must_use]
    pub fn open_id(&self) -> u32 {
        self.frames.last().map_or(schema::ROOT, |f| f.id)
    }

    /// The nearest known end above `pos`, which bounds any read.
    #[must_use]
    pub fn bound(&self) -> Option<u64> {
        self.frames.iter().filter_map(|f| f.end).min()
    }

    /// Pop every frame whose known end is at or before `pos`.
    ///
    /// Returns how many were closed.
    pub fn close_finished(&mut self, pos: u64) -> usize {
        let mut n = 0;
        while self
            .frames
            .last()
            .is_some_and(|f| f.end.is_some_and(|e| pos >= e))
        {
            self.frames.pop();
            n += 1;
        }
        n
    }

    /// How many unknown-size frames an element with ID `id` terminates.
    ///
    /// RFC 8794 section 6.2, transcribed directly: walk outward from the
    /// innermost open element; each frame whose children do not admit `id` is
    /// ended by it. Only unknown-size frames may be ended this way — a frame
    /// with a known size ends where its size says it does, so an unexpected ID
    /// inside one is a corrupt child to skip, not a terminator.
    ///
    /// Returns `None` when `id` cannot be placed at all, which happens for an
    /// unknown ID or one that is only legal deeper in the tree.
    #[must_use]
    pub fn terminations_for(&self, id: u32) -> Option<usize> {
        let mut popped = 0usize;
        loop {
            let idx = self.frames.len().checked_sub(popped)?;
            let parent = if idx == 0 {
                schema::ROOT
            } else {
                self.frames.get(idx - 1)?.id
            };
            if is_child_of(id, parent) {
                return Some(popped);
            }
            // A root element ends everything, however deep.
            if idx == 0 {
                return is_root(id).then_some(popped);
            }
            let frame = self.frames.get(idx - 1)?;
            if frame.end.is_some() {
                // Known size: `id` is not a legal child, but the frame does not
                // end here. The caller skips the element instead.
                return None;
            }
            popped = popped.checked_add(1)?;
        }
    }

    pub fn truncate_by(&mut self, n: usize) {
        let keep = self.frames.len().saturating_sub(n);
        self.frames.truncate(keep);
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

#[cfg(test)]
mod tests;
