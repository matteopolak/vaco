//! The box grammar: headers, iteration, and the two bounded search helpers.
//!
//! ISO/IEC 14496-12 §4.2. A box is
//!
//! ```text
//! size:u32  type:u32  [largesize:u64 if size == 1]  [usertype:u8[16] if type == 'uuid']  payload
//! ```
//!
//! with `size == 0` meaning "to the end of the enclosing container" and
//! `size == 1` meaning the real size is the following `u64`. A *full box*
//! prefixes its payload with `version:u8` and `flags:u24`.
//!
//! # Why there is no recursive descent here
//!
//! Box parsing is the textbook stack-overflow surface: `moov` inside `moov`
//! inside `moov` for a megabyte costs nothing to write and kills a recursive
//! parser. This module answers that structurally rather than with a depth
//! counter:
//!
//! * [`BoxIter`] is *flat*. It walks one container's direct children and never
//!   descends on its own.
//! * The known tree (`moov ▸ trak ▸ mdia ▸ minf ▸ stbl`) is assembled by nested
//!   `for` loops in [`crate::movie`] — a fixed, compile-time-bounded depth, so
//!   no input can deepen it.
//! * The two generic searches that genuinely need depth, [`find_path`] and
//!   [`walk`], are iterative with an explicit worklist and a hard cap of
//!   [`MAX_DEPTH`].
//!
//! There is therefore no recursive call anywhere in the crate that input can
//! drive. That is worth more than a counter, because a counter has to be
//! correct at every call site and this has to be correct once.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};

use crate::fourcc::FourCc;

/// Bytes in a plain box header.
pub const HEADER_LEN: u64 = 8;
/// Bytes in a box header carrying a 64-bit `largesize`.
pub const HEADER_LEN_LARGE: u64 = 16;
/// Bytes a `uuid` box adds for its extended type.
pub const USERTYPE_LEN: u64 = 16;

/// Depth cap for the generic search helpers.
///
/// Real ISOBMFF nests about eight deep at its worst
/// (`moov ▸ trak ▸ mdia ▸ minf ▸ stbl ▸ stsd ▸ entry ▸ wave ▸ esds`). Sixteen
/// leaves headroom for vendor extensions without letting a crafted file build a
/// deep worklist.
pub const MAX_DEPTH: usize = 16;

/// Fuel charged per box header inspected by [`walk`] and [`find_path`].
///
/// A file made entirely of eight-byte `free` boxes is a legitimate file and a
/// denial-of-service at the same time; the difference is only how many there
/// are. Charging the caller's [`vaco_limits::Budget`] makes the bound
/// deterministic and reproducible rather than wall-clock.
pub const FUEL_PER_BOX: u64 = 1;

/// One parsed box header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxHeader {
    /// The four-character type. `uuid` when [`BoxHeader::usertype`] is set.
    pub kind: FourCc,
    /// Total size of the box including its header, already resolved: a
    /// `size == 0` box reports the distance to the end of its container.
    pub size: u64,
    /// Bytes of header, including `largesize` and `usertype` where present.
    pub header_len: u64,
    /// The extended type of a `uuid` box.
    pub usertype: Option<[u8; 16]>,
    /// True when the box declared `size == 0`, i.e. "to the end of the
    /// container". Preserved because a muxer rewriting the file must know, and
    /// because a `size == 0` box that is *not* last is a corruption signal.
    pub to_end: bool,
}

impl BoxHeader {
    /// Payload length, i.e. `size - header_len`.
    #[must_use]
    pub const fn payload_len(&self) -> u64 {
        self.size.saturating_sub(self.header_len)
    }

    /// Parse a header's fields, resolving `largesize`/`usertype` and the
    /// `size == 0` "to end of container" convention against `available`, but
    /// **not** checking that the resolved `size` actually fits within it —
    /// see [`BoxHeader::parse`] and [`BoxIter`]'s own recovery for the two
    /// different answers callers want to that question.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] when fewer than eight bytes are present, and
    /// [`Error::InvalidData`] when the declared size cannot hold the header
    /// it claims.
    fn parse_raw(data: &[u8], available: u64) -> Result<Self> {
        let mut r = ByteReader::new(data);
        let size32 = u64::from(r.be32());
        let kind = FourCc(<[u8; 4]>::try_from(r.bytes(4)).unwrap_or([0; 4]));
        if r.overrun() {
            return Err(Error::UnexpectedEof);
        }
        let mut header_len = HEADER_LEN;
        let mut to_end = false;
        let mut size = size32;
        if size32 == 1 {
            size = r.be64();
            header_len = HEADER_LEN_LARGE;
            if r.overrun() {
                return Err(Error::UnexpectedEof);
            }
        } else if size32 == 0 {
            to_end = true;
            size = available;
        }
        let usertype = if kind == crate::fourcc::boxes::UUID {
            let bytes = r.bytes(16);
            if r.overrun() {
                return Err(Error::UnexpectedEof);
            }
            header_len = header_len.saturating_add(USERTYPE_LEN);
            Some(<[u8; 16]>::try_from(bytes).unwrap_or([0; 16]))
        } else {
            None
        };
        if size < header_len {
            return Err(Error::InvalidData("isom: box smaller than its header"));
        }
        Ok(Self {
            kind,
            size,
            header_len,
            usertype,
            to_end,
        })
    }

    /// Parse a header out of `data`, where `available` is the number of bytes
    /// remaining in the enclosing container from the box's own start.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] when fewer than eight bytes are present, and
    /// [`Error::InvalidData`] when the declared size cannot hold the header it
    /// claims or overruns the container.
    pub fn parse(data: &[u8], available: u64) -> Result<Self> {
        let h = Self::parse_raw(data, available)?;
        if h.size > available {
            return Err(Error::InvalidData("isom: box extends past its container"));
        }
        Ok(h)
    }

    /// [`BoxHeader::parse_raw`], then — instead of erroring — clamp an
    /// oversized `size` down to `available` and mark the header `to_end`,
    /// exactly the value a legitimately `size == 0` box already carries.
    ///
    /// **Only [`BoxIter`] calls this**, and only because of what `BoxIter`
    /// is: a flat walk over one already-in-memory, already-budgeted
    /// container (`ilst`, `udta`, `trak`, ...), never a multi-gigabyte file.
    /// Clamping there can only ever consume bytes that are already this
    /// container's own — it cannot read past `data`'s end, cannot cross into
    /// a sibling container, and does not scan content looking for a
    /// plausible resync point the way the module doc's original caution
    /// warns against. [`crate::scan::TopLevelScanner`] deliberately does
    /// **not** get this treatment: a corrupted top-level box's size, seeked
    /// over rather than held in memory, could otherwise swallow a real,
    /// unrelated `mdat` into a mis-sized sibling — see the module doc's
    /// "Recovering from an oversized child length" section for the full
    /// reasoning and its one disclosed residual risk.
    ///
    /// # Errors
    /// As [`BoxHeader::parse_raw`] — a header too short to parse at all, or
    /// smaller than its own claimed header length, is still unrecoverable.
    fn parse_clamped(data: &[u8], available: u64) -> Result<Self> {
        let mut h = Self::parse_raw(data, available)?;
        if h.size > available {
            h.size = available;
            h.to_end = true;
        }
        Ok(h)
    }
}

/// A box together with its payload and its absolute position in the file.
#[derive(Debug, Clone, Copy)]
pub struct IsoBox<'a> {
    /// The parsed header.
    pub header: BoxHeader,
    /// The payload, header already stripped. Borrowed from the caller's buffer;
    /// nothing in this crate copies it.
    pub payload: &'a [u8],
    /// Absolute file offset of the box's first header byte.
    ///
    /// Load-bearing: `stco`, `co64` and `tfhd.base_data_offset` are all
    /// **file-absolute**, so a parser that only knows slice-relative positions
    /// cannot resolve a sample offset at all.
    pub offset: u64,
}

impl<'a> IsoBox<'a> {
    /// The box type.
    #[must_use]
    pub const fn kind(&self) -> FourCc {
        self.header.kind
    }

    /// Absolute file offset of the first payload byte.
    #[must_use]
    pub const fn payload_offset(&self) -> u64 {
        self.offset.saturating_add(self.header.header_len)
    }

    /// Interpret the payload as a full box (§4.2), returning version, flags and
    /// the remaining payload.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the payload is shorter than four bytes.
    pub fn full(&self) -> Result<FullBox<'a>> {
        FullBox::parse(self.payload, self.payload_offset())
    }

    /// Iterate this box's direct children, treating the payload as a container.
    #[must_use]
    pub const fn children(&self) -> BoxIter<'a> {
        BoxIter::new(self.payload, self.payload_offset())
    }

    /// Iterate children after skipping `skip` payload bytes.
    ///
    /// `stsd` and `meta` are the two boxes whose children do not start at the
    /// payload — `stsd` has a full-box header plus an entry count, `meta` a
    /// full-box header (and, in some `QuickTime` files, not even that; see
    /// [`crate::movie`]).
    #[must_use]
    pub fn children_after(&self, skip: usize) -> BoxIter<'a> {
        let rest = self.payload.get(skip..).unwrap_or(&[]);
        BoxIter::new(rest, self.payload_offset().saturating_add(skip as u64))
    }
}

/// Version, flags and payload of a full box.
#[derive(Debug, Clone, Copy)]
pub struct FullBox<'a> {
    /// `version` byte.
    pub version: u8,
    /// 24-bit `flags` field.
    pub flags: u32,
    /// Payload after the four-byte version/flags prefix.
    pub body: &'a [u8],
    /// Absolute file offset of `body`'s first byte.
    pub offset: u64,
}

impl<'a> FullBox<'a> {
    /// Split `payload` into version, flags and body.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when fewer than four bytes are present.
    pub fn parse(payload: &'a [u8], offset: u64) -> Result<Self> {
        let head = payload
            .first_chunk::<4>()
            .ok_or(Error::InvalidData("isom: truncated full-box header"))?;
        let body = payload.get(4..).unwrap_or(&[]);
        Ok(Self {
            version: head[0],
            flags: u32::from_be_bytes([0, head[1], head[2], head[3]]),
            body,
            offset: offset.saturating_add(4),
        })
    }

    /// A reader positioned at the first body byte.
    #[must_use]
    pub const fn reader(&self) -> ByteReader<'a> {
        ByteReader::new(self.body)
    }
}

/// Flat iteration over one container's direct children.
///
/// Yields `Err` once and then stops on a child that cannot be recovered —
/// a header too short to read at all, or one claiming to be smaller than
/// its own fixed fields — because a container whose child chain is broken
/// that way has no reliable continuation: skipping ahead to *guess* where
/// the next box starts, by scanning content for something that looks like
/// a plausible header, is how a parser ends up reading a payload as
/// structure.
///
/// # Recovering from an oversized child length
///
/// A child whose declared size *overruns* this container — the shape a
/// single corrupted length field produces, measured directly on
/// `fuzz/seeds/diff/mp4` (an `aware`-mutator campaign corrupting an
/// `ilst ▸ ©too` size to `0x8000_0000`/`0xFFFF_FFFF` loses the *entire*
/// `ilst` walk, including the one real, physically-present `©too ▸ data`
/// entry beneath it, though the reference recovers it) — is different from
/// the case above, and is recovered rather than failed: the declared size
/// is clamped to what is actually left in `data` (marked `to_end`, the
/// same value a legitimately `size == 0` box already carries) instead of
/// erroring. See [`BoxHeader::parse_clamped`] for the exact mechanism and
/// why it does not reopen the "reading a payload as structure" risk this
/// doc's first paragraph warns against: clamping never scans content for a
/// resync point and never reads a byte that is not already part of this
/// same container's own, already-in-memory data.
///
/// **The one residual risk, disclosed rather than hidden**: if the
/// corrupted child was not actually the last legitimate child of this
/// container, clamping folds a real sibling's bytes into the corrupted
/// child's own body, and if that sibling *happens* to look enough like one
/// of the corrupted child's own recognised sub-boxes, it could be
/// misread — bounded to one level of nesting, bounded to bytes physically
/// present in this one container, and never a new scan or a new
/// allocation, so it cannot amplify into a denial of service or escape
/// into a sibling *container's* data the way an unbounded resync could.
/// Weighed and accepted for the class of corruption measured.
#[derive(Debug, Clone)]
pub struct BoxIter<'a> {
    data: &'a [u8],
    pos: usize,
    base: u64,
    done: bool,
}

impl<'a> BoxIter<'a> {
    /// Iterate `data`, whose first byte sits at absolute file offset `base`.
    #[must_use]
    pub const fn new(data: &'a [u8], base: u64) -> Self {
        Self {
            data,
            pos: 0,
            base,
            done: false,
        }
    }

    /// Bytes not yet consumed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// The first direct child of type `kind`, or `None`.
    ///
    /// Stops at the first *unrecoverable* child (too short to parse, or
    /// smaller than its own header) rather than searching past it — an
    /// oversized child recovers instead, per [`BoxIter`]'s own doc, so this
    /// can still find `kind` on the far side of one of those.
    #[must_use]
    pub fn find(self, kind: FourCc) -> Option<IsoBox<'a>> {
        self.flatten().find(|b| b.kind() == kind)
    }
}

impl<'a> Iterator for BoxIter<'a> {
    type Item = Result<IsoBox<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let rest = self.data.get(self.pos..)?;
        if rest.len() < HEADER_LEN as usize {
            self.done = true;
            // A container may legitimately end with fewer than eight bytes of
            // padding; that is not a corruption worth failing the parse over.
            return None;
        }
        // `parse_clamped`, not `parse`: an oversized declared size is
        // recovered here rather than failed — see this type's own doc for
        // why that is safe specifically for a flat, in-memory container
        // walk. A header too short to read, or smaller than its own fixed
        // fields, is still unrecoverable and still stops the walk.
        let header = match BoxHeader::parse_clamped(rest, rest.len() as u64) {
            Ok(h) => h,
            Err(e) => {
                self.done = true;
                return Some(Err(e));
            }
        };
        let size = header.size as usize;
        let hdr = header.header_len as usize;
        let Some(body) = rest.get(hdr..size) else {
            self.done = true;
            return Some(Err(Error::InvalidData("isom: box payload truncated")));
        };
        let offset = self.base.saturating_add(self.pos as u64);
        // `size >= header_len >= 8` is guaranteed by `BoxHeader::parse_clamped`
        // (clamping only ever shrinks `size` down to `available`, never below
        // `header_len`, since `parse_raw` already rejected that), so the
        // position strictly advances and iteration terminates.
        self.pos = self.pos.saturating_add(size);
        Some(Ok(IsoBox {
            header,
            payload: body,
            offset,
        }))
    }
}

/// Follow a fixed path of box types from `root`, e.g.
/// `["moov", "trak", "mdia"]`.
///
/// Iterative, so path length is the only depth and it is the caller's constant.
/// Returns the first match at each level.
#[must_use]
pub fn find_path<'a>(root: BoxIter<'a>, path: &[FourCc]) -> Option<IsoBox<'a>> {
    let mut level = root;
    let mut found = None;
    for (depth, want) in path.iter().enumerate() {
        if depth >= MAX_DEPTH {
            return None;
        }
        let hit = level.find(*want)?;
        level = hit.children();
        found = Some(hit);
    }
    found
}

/// Visit every box reachable from `root`, depth-first, bounded by
/// [`MAX_DEPTH`] and by the fuel `budget` still holds.
///
/// `visit` returns `false` to stop descending into that box, which is how a
/// caller avoids walking a 4 GiB `mdat` looking for structure that is not
/// there.
///
/// # Errors
///
/// Propagates the first malformed box, and [`vaco_core::Error::LimitExceeded`]
/// when the fuel runs out.
pub fn walk<'a, F>(root: BoxIter<'a>, budget: &mut vaco_limits::Budget, mut visit: F) -> Result<()>
where
    F: FnMut(&IsoBox<'a>, usize) -> bool,
{
    // Explicit stack, not the call stack. Depth is capped, so the worklist is
    // capped, so a crafted file cannot grow it without bound.
    let mut stack: Vec<(BoxIter<'a>, usize)> = vec![(root, 0)];
    while let Some((mut level, depth)) = stack.pop() {
        while let Some(item) = level.next() {
            budget.consume_fuel(FUEL_PER_BOX)?;
            let b = item?;
            let descend = visit(&b, depth);
            if descend && depth.saturating_add(1) < MAX_DEPTH {
                stack.push((level, depth));
                stack.push((b.children(), depth.saturating_add(1)));
                break;
            }
        }
    }
    Ok(())
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
    use crate::fourcc::boxes;
    use crate::testutil::{bx, fullbx};

    #[test]
    fn plain_header_round_trips() {
        let data = bx(b"ftyp", &[1, 2, 3, 4]);
        let h = BoxHeader::parse(&data, data.len() as u64).unwrap();
        assert_eq!(h.kind, boxes::FTYP);
        assert_eq!(h.size, 12);
        assert_eq!(h.header_len, 8);
        assert!(!h.to_end);
    }

    #[test]
    fn largesize_header_is_sixteen_bytes() {
        let mut data = vec![0, 0, 0, 1];
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&24u64.to_be_bytes());
        data.extend_from_slice(&[0; 8]);
        let h = BoxHeader::parse(&data, data.len() as u64).unwrap();
        assert_eq!(h.size, 24);
        assert_eq!(h.header_len, 16);
        assert_eq!(h.payload_len(), 8);
    }

    #[test]
    fn size_zero_runs_to_the_end_of_the_container() {
        let mut data = vec![0, 0, 0, 0];
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[7; 40]);
        let h = BoxHeader::parse(&data, data.len() as u64).unwrap();
        assert!(h.to_end);
        assert_eq!(h.size, 48);
    }

    #[test]
    fn uuid_header_carries_its_extended_type() {
        let mut data = vec![0, 0, 0, 32];
        data.extend_from_slice(b"uuid");
        data.extend_from_slice(&[0xAB; 16]);
        data.extend_from_slice(&[0; 8]);
        let h = BoxHeader::parse(&data, data.len() as u64).unwrap();
        assert_eq!(h.header_len, 24);
        assert_eq!(h.usertype, Some([0xAB; 16]));
        assert_eq!(h.payload_len(), 8);
    }

    #[test]
    fn a_size_smaller_than_the_header_is_rejected() {
        let mut data = vec![0, 0, 0, 4];
        data.extend_from_slice(b"free");
        assert!(BoxHeader::parse(&data, 8).is_err());
        // Same for a largesize box whose largesize is under 16.
        let mut big = vec![0, 0, 0, 1];
        big.extend_from_slice(b"free");
        big.extend_from_slice(&12u64.to_be_bytes());
        assert!(BoxHeader::parse(&big, 16).is_err());
    }

    #[test]
    fn a_box_claiming_more_than_its_container_is_rejected() {
        let mut data = vec![0, 0, 0x10, 0];
        data.extend_from_slice(b"moov");
        assert!(BoxHeader::parse(&data, 8).is_err());
    }

    #[test]
    fn iteration_yields_siblings_with_absolute_offsets() {
        let mut file = bx(b"ftyp", b"isom");
        file.extend_from_slice(&bx(b"free", &[]));
        file.extend_from_slice(&bx(b"mdat", &[9; 3]));
        let got: Vec<_> = BoxIter::new(&file, 100).flatten().collect();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].offset, 100);
        assert_eq!(got[1].offset, 112);
        assert_eq!(got[2].offset, 120);
        assert_eq!(got[2].payload, &[9, 9, 9]);
        assert_eq!(got[2].payload_offset(), 128);
    }

    #[test]
    fn iteration_stops_at_a_trailing_stub() {
        let mut file = bx(b"free", &[]);
        file.extend_from_slice(&[0, 0, 0]);
        let got: Vec<_> = BoxIter::new(&file, 0).collect();
        assert_eq!(got.len(), 1);
        assert!(got[0].is_ok());
    }

    /// The behaviour this used to assert (an oversized size fails the whole
    /// walk) is exactly what `parse_clamped` now recovers from instead — see
    /// `BoxIter`'s own doc's "Recovering from an oversized child length"
    /// section. `size=0xFFFFFFFF` is clamped to the 8 bytes actually left,
    /// which is also `junk`'s own `header_len`, so it comes back as a
    /// legitimate-looking empty, `to_end` box rather than an error.
    #[test]
    fn an_oversized_trailing_box_recovers_instead_of_erroring() {
        let mut file = bx(b"free", &[]);
        file.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, b'j', b'u', b'n', b'k']);
        let got: Vec<_> = BoxIter::new(&file, 0).collect();
        assert_eq!(got.len(), 2);
        let junk = got[1].as_ref().unwrap();
        assert_eq!(junk.kind(), FourCc(*b"junk"));
        assert!(junk.header.to_end);
        assert_eq!(junk.payload, &[] as &[u8]);
    }

    /// A header too short to read at all is still unrecoverable — clamping
    /// only ever answers "how much of this box's *payload* is really
    /// there", never "is there even a complete header".
    #[test]
    fn a_header_too_short_to_read_still_stops_the_walk() {
        let mut file = bx(b"free", &[]);
        file.extend_from_slice(&[0, 0, 0]); // 3 bytes: not even one full header
        let got: Vec<_> = BoxIter::new(&file, 0).collect();
        assert_eq!(got.len(), 1);
        assert!(got[0].is_ok());
    }

    /// A size smaller than its own header is still unrecoverable — there is
    /// no "how much is really there" question to answer when the box does
    /// not even claim enough room for the fields it just stated.
    #[test]
    fn a_size_smaller_than_the_header_still_stops_the_walk() {
        let mut file = bx(b"free", &[]);
        file.extend_from_slice(&[0, 0, 0, 4]); // size=4 < header_len=8
        file.extend_from_slice(b"junk");
        let got: Vec<_> = BoxIter::new(&file, 0).collect();
        assert_eq!(got.len(), 2);
        assert!(got[1].is_err());
    }

    /// The actual motivating case (`fuzz/seeds/diff/mp4`, an `aware`-mutator
    /// finding): a real, physically-present `data` sub-box survives its
    /// parent's corrupted length. Before `parse_clamped`, this entire walk
    /// — including the recoverable `data` box — was lost to one bad length
    /// nobody needed to trust in the first place.
    #[test]
    fn a_corrupted_parent_size_still_recovers_its_real_nested_child() {
        let inner_data = bx(b"data", b"Lavf62.12.100");
        let mut too = bx(b"\xa9too", &inner_data);
        // Corrupt `©too`'s own declared size to something wildly oversized,
        // in place, exactly as the `aware` mutator does.
        too[0] = 0x80;
        too[1] = 0;
        too[2] = 0;
        too[3] = 0;
        let children: Vec<_> = BoxIter::new(&too, 0).flatten().collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind(), FourCc(*b"\xa9too"));
        let grandchildren: Vec<_> = children[0].children().flatten().collect();
        assert_eq!(grandchildren.len(), 1);
        assert_eq!(grandchildren[0].kind(), boxes::DATA);
        assert_eq!(grandchildren[0].payload, &inner_data[8..]);
    }

    /// The disclosed residual risk, made concrete: when the corrupted child
    /// is *not* actually last, clamping folds a real sibling's bytes into
    /// its own body. Confirms this stays bounded to that one container —
    /// the sibling's bytes are consumed, not skipped over and rediscovered
    /// — rather than silently doing the right thing by accident.
    #[test]
    fn a_corrupted_non_last_child_consumes_its_sibling_rather_than_skipping_to_it() {
        let mut first = bx(b"free", &[1, 2, 3, 4]);
        first[0] = 0x7F; // oversized: swallows everything after it
        let second = bx(b"free", &[9, 9]);
        let mut file = first;
        file.extend_from_slice(&second);
        let got: Vec<_> = BoxIter::new(&file, 0).flatten().collect();
        // One box, not two: `second` was consumed as part of the first
        // (clamped) box's payload rather than rediscovered as its own
        // sibling.
        assert_eq!(got.len(), 1);
        assert!(got[0].header.to_end);
    }

    #[test]
    fn nested_boxes_never_recurse_into_the_call_stack() {
        // A megabyte of nested `moov`s: fatal to a recursive descent parser,
        // and merely bounded here.
        let mut inner = bx(b"free", &[]);
        for _ in 0..20_000 {
            inner = bx(b"moov", &inner);
        }
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let mut seen = 0usize;
        let r = walk(BoxIter::new(&inner, 0), &mut budget, |_, _| {
            seen += 1;
            true
        });
        assert!(r.is_ok());
        // Capped at MAX_DEPTH, so only the outermost handful are visited.
        assert!(seen <= MAX_DEPTH, "visited {seen}");
    }

    #[test]
    fn walk_charges_fuel_and_stops_when_it_runs_out() {
        let mut file = Vec::new();
        for _ in 0..5000 {
            file.extend_from_slice(&bx(b"free", &[]));
        }
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive().with_fuel(100));
        let r = walk(BoxIter::new(&file, 0), &mut budget, |_, _| false);
        assert!(matches!(r, Err(Error::LimitExceeded { .. })));
    }

    #[test]
    fn find_path_descends_the_known_tree() {
        let stbl = bx(b"stbl", &bx(b"stts", &[0; 8]));
        let minf = bx(b"minf", &stbl);
        let mdia = bx(b"mdia", &minf);
        let trak = bx(b"trak", &mdia);
        let moov = bx(b"moov", &trak);
        let hit = find_path(
            BoxIter::new(&moov, 0),
            &[
                boxes::MOOV,
                boxes::TRAK,
                boxes::MDIA,
                boxes::MINF,
                boxes::STBL,
            ],
        )
        .unwrap();
        assert_eq!(hit.kind(), boxes::STBL);
        assert!(find_path(BoxIter::new(&moov, 0), &[boxes::MOOV, boxes::UDTA]).is_none());
    }

    #[test]
    fn full_box_splits_version_and_flags() {
        let raw = fullbx(b"elst", 1, 0x00_0F_00, &[1, 2, 3]);
        let b = BoxIter::new(&raw, 0).flatten().next().unwrap();
        let f = b.full().unwrap();
        assert_eq!(f.version, 1);
        assert_eq!(f.flags, 0x00_0F_00);
        assert_eq!(f.body, &[1, 2, 3]);
        assert_eq!(f.offset, 12);
    }

    #[test]
    fn a_full_box_with_a_short_payload_is_an_error() {
        let raw = bx(b"elst", &[0, 0]);
        let b = BoxIter::new(&raw, 0).flatten().next().unwrap();
        assert!(b.full().is_err());
    }

    #[test]
    fn fourcc_display_escapes_non_ascii() {
        assert_eq!(FourCc::new(b"moov").to_string(), "moov");
        assert_eq!(FourCc::new(b"qt  ").to_string(), "qt  ");
        assert_eq!(FourCc([0xA9, b'n', b'a', b'm']).to_string(), "\\xa9nam");
        assert!(!FourCc([0xA9, b'n', b'a', b'm']).is_printable());
        assert_eq!(FourCc::new(b"avc1").as_u32_le(), 0x3163_7661);
    }
}
