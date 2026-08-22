//! Walking the top level of a file over [`IoContext`], without reading it.
//!
//! The rest of this crate parses from a `&[u8]`, which is the right shape for
//! `moov` (small, needed whole, wanted zero-copy) and exactly the wrong shape
//! for `mdat` (usually the entire file). [`TopLevelScanner`] bridges the two:
//! it reads eight-byte headers and seeks past payloads, so finding a `moov` at
//! the end of a two-gigabyte file costs a handful of reads.
//!
//! It is also where the answer to the single most common MP4 question lives.
//! A `moov` after `mdat` cannot be streamed, and
//! [`ScanError::MoovAfterMediaData`] says so in those words rather than
//! reporting a generic failure — because the user's next action is
//! `-movflags +faststart`, and only an error that names it gets them there.

use vaco_core::{Error, Result};
use vaco_io::{IoContext, Seekability};
use vaco_limits::Budget;

use crate::boxes::{BoxHeader, HEADER_LEN};
use crate::fourcc::{FourCc, boxes};

/// One top-level box, located but not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxSpan {
    /// The box type.
    pub kind: FourCc,
    /// Absolute offset of the box's first header byte.
    pub offset: u64,
    /// Total size including the header.
    pub size: u64,
    /// Header length, including `largesize` and `usertype`.
    pub header_len: u64,
    /// The `uuid` extended type, when present.
    pub usertype: Option<[u8; 16]>,
    /// Whether the box declared `size == 0`.
    pub to_end: bool,
}

impl BoxSpan {
    /// Absolute offset of the payload.
    #[must_use]
    pub const fn payload_offset(&self) -> u64 {
        self.offset.saturating_add(self.header_len)
    }

    /// Payload length.
    #[must_use]
    pub const fn payload_len(&self) -> u64 {
        self.size.saturating_sub(self.header_len)
    }

    /// Absolute offset one past the box.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.offset.saturating_add(self.size)
    }
}

/// Why a scan could not produce what the caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanError {
    /// No `moov` anywhere in the file.
    NoMovie,
    /// The `moov` follows the `mdat` and the source cannot seek.
    ///
    /// The file is well-formed; it just cannot be *streamed*. Remuxing it with
    /// `-movflags +faststart` moves the `moov` to the front and fixes it.
    MoovAfterMediaData,
}

impl ScanError {
    /// The message this crate reports for the condition.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NoMovie => "isom: no moov box in the file",
            Self::MoovAfterMediaData => {
                "isom: moov follows mdat and the input cannot seek; remux with -movflags +faststart"
            }
        }
    }
}

impl From<ScanError> for Error {
    fn from(e: ScanError) -> Self {
        Self::InvalidData(e.message())
    }
}

/// Top-level box types that make a file plausibly ISOBMFF.
pub const TOP_LEVEL_TYPES: &[FourCc] = &[
    boxes::FTYP,
    boxes::STYP,
    boxes::MOOV,
    boxes::MOOF,
    boxes::MDAT,
    boxes::FREE,
    boxes::SKIP,
    boxes::WIDE,
    boxes::JUNK,
    boxes::PNOT,
    boxes::SIDX,
    boxes::SSIX,
    boxes::MFRA,
    boxes::META,
    boxes::UUID,
    boxes::PRFT,
    boxes::EMSG,
    boxes::PSSH,
];

/// Top-level boxes inspected before a scan gives up.
///
/// A file of eight-byte `free` boxes is legal and is also a way to make a
/// scanner do a million seeks. Fuel from the caller's [`Budget`] bounds it
/// deterministically; this constant bounds it even for a caller that handed
/// over an unlimited budget.
pub const MAX_TOP_LEVEL_BOXES: u32 = 1 << 20;

/// Iterates the top level of a source, reading headers and seeking payloads.
#[derive(Debug)]
pub struct TopLevelScanner<'io> {
    io: &'io mut IoContext,
    pos: u64,
    end: u64,
    seen: u32,
    done: bool,
}

impl<'io> TopLevelScanner<'io> {
    /// Scan from the current position to the end of the source.
    pub fn new(io: &'io mut IoContext) -> Self {
        let pos = io.pos();
        let end = io.size().unwrap_or(u64::MAX);
        Self {
            io,
            pos,
            end,
            seen: 0,
            done: false,
        }
    }

    /// Scan a bounded range.
    pub fn range(io: &'io mut IoContext, start: u64, end: u64) -> Self {
        Self {
            io,
            pos: start,
            end,
            seen: 0,
            done: false,
        }
    }

    /// The position the scanner will read from next.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.pos
    }

    /// Read the next box header, leaving the source positioned at its payload.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed header,
    /// [`vaco_core::Error::LimitExceeded`] when the fuel runs out, and whatever
    /// the transport reports.
    pub fn next_box(&mut self, budget: &mut Budget) -> Result<Option<BoxSpan>> {
        if self.done || self.pos >= self.end {
            return Ok(None);
        }
        self.seen = self.seen.saturating_add(1);
        if self.seen > MAX_TOP_LEVEL_BOXES {
            self.done = true;
            return Ok(None);
        }
        budget.consume_fuel(crate::boxes::FUEL_PER_BOX)?;
        if self.io.pos() != self.pos {
            self.io.seek(self.pos)?;
        }
        let mut head = [0u8; 16];
        let available = self.end.saturating_sub(self.pos);
        if available < HEADER_LEN {
            self.done = true;
            return Ok(None);
        }
        // Read only what the header needs: eight bytes, then eight more for a
        // `largesize`, then sixteen for a `uuid`. Peeking is cheaper than
        // seeking back.
        let peek_len = usize::try_from(available.min(24)).unwrap_or(24);
        let peeked = match self.io.peek(peek_len) {
            Ok(b) => b,
            Err(Error::UnexpectedEof) => {
                self.done = true;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let n = peeked.len().min(16);
        head.get_mut(..n)
            .and_then(|d| peeked.get(..n).map(|s| d.copy_from_slice(s)))
            .ok_or(Error::UnexpectedEof)?;
        let header = BoxHeader::parse(head.get(..n).unwrap_or(&[]), available)?;
        let span = BoxSpan {
            kind: header.kind,
            offset: self.pos,
            size: header.size,
            header_len: header.header_len,
            usertype: header.usertype,
            to_end: header.to_end,
        };
        self.io.seek(span.payload_offset())?;
        // `size >= header_len >= 8` is guaranteed, so the scan advances.
        self.pos = span.end();
        Ok(Some(span))
    }

    /// Read a located box's payload into a budgeted buffer.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] when the payload exceeds the budget,
    /// and whatever the transport reports.
    pub fn read_payload(&mut self, span: BoxSpan, budget: &mut Budget) -> Result<Vec<u8>> {
        let len = usize::try_from(span.payload_len())
            .map_err(|_| Error::InvalidData("isom: box payload too large for this platform"))?;
        let mut buf = budget.alloc::<u8>(len)?;
        self.io.seek(span.payload_offset())?;
        self.io.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Find the `moov`, reading no media data.
    ///
    /// Returns the span and whether an `mdat` was seen first. On a source that
    /// cannot seek, a `moov` after `mdat` is
    /// [`ScanError::MoovAfterMediaData`] — unless `mvex` will make the file
    /// readable anyway, which only the caller knows, so the distinction is
    /// reported rather than decided here.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed top-level chain, and whatever the
    /// transport reports.
    pub fn find_movie(
        &mut self,
        budget: &mut Budget,
    ) -> Result<core::result::Result<BoxSpan, ScanError>> {
        let mut saw_mdat = false;
        while let Some(span) = self.next_box(budget)? {
            match span.kind {
                boxes::MOOV => {
                    if saw_mdat && self.io.seekability() == Seekability::None {
                        return Ok(Err(ScanError::MoovAfterMediaData));
                    }
                    return Ok(Ok(span));
                }
                boxes::MDAT => saw_mdat = true,
                _ => {}
            }
        }
        Ok(Err(if saw_mdat {
            ScanError::MoovAfterMediaData
        } else {
            ScanError::NoMovie
        }))
    }
}

/// Whether `kind` is a box type a file may legitimately start with.
#[must_use]
pub fn is_top_level_type(kind: FourCc) -> bool {
    TOP_LEVEL_TYPES.contains(&kind)
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
    use crate::testutil::bx;
    use vaco_io::{IoOptions, MediaSource, MemorySource};
    use vaco_limits::Limits;

    fn ctx(data: Vec<u8>) -> IoContext {
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data));
        IoContext::new(src, &IoOptions::default()).unwrap()
    }

    fn pipe(data: Vec<u8>) -> IoContext {
        let src: Box<dyn MediaSource> = Box::new(MemorySource::forward_only(data));
        IoContext::new(src, &IoOptions::default()).unwrap()
    }

    fn budget() -> Budget {
        Budget::new(Limits::permissive())
    }

    #[test]
    fn a_scan_visits_every_top_level_box_without_reading_payloads() {
        let mut file = bx(b"ftyp", b"isom\0\0\x02\0");
        file.extend_from_slice(&bx(b"mdat", &[0u8; 4096]));
        file.extend_from_slice(&bx(b"moov", &[0; 32]));
        let mut io = ctx(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        let kinds: Vec<FourCc> = core::iter::from_fn(|| s.next_box(&mut b).ok().flatten())
            .map(|x| x.kind)
            .collect();
        assert_eq!(kinds, vec![boxes::FTYP, boxes::MDAT, boxes::MOOV]);
    }

    #[test]
    fn find_movie_locates_a_trailing_moov_on_a_seekable_source() {
        let mut file = bx(b"ftyp", b"isom\0\0\x02\0");
        file.extend_from_slice(&bx(b"mdat", &[7u8; 1000]));
        file.extend_from_slice(&bx(b"moov", &[1; 16]));
        let mut io = ctx(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        let span = s.find_movie(&mut b).unwrap().unwrap();
        assert_eq!(span.kind, boxes::MOOV);
        assert_eq!(span.payload_len(), 16);
        let payload = s.read_payload(span, &mut b).unwrap();
        assert_eq!(payload, vec![1; 16]);
    }

    #[test]
    fn a_trailing_moov_on_a_pipe_names_the_fix() {
        let mut file = bx(b"ftyp", b"isom\0\0\x02\0");
        file.extend_from_slice(&bx(b"mdat", &[7u8; 64]));
        file.extend_from_slice(&bx(b"moov", &[1; 16]));
        let mut io = pipe(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        let e = s.find_movie(&mut b).unwrap().unwrap_err();
        assert_eq!(e, ScanError::MoovAfterMediaData);
        assert!(e.message().contains("faststart"));
    }

    #[test]
    fn a_file_with_no_moov_says_so() {
        let file = bx(b"ftyp", b"isom\0\0\x02\0");
        let mut io = ctx(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        assert_eq!(
            s.find_movie(&mut b).unwrap().unwrap_err(),
            ScanError::NoMovie
        );
    }

    #[test]
    fn a_size_zero_box_ends_the_scan_at_the_file_end() {
        let mut file = bx(b"ftyp", b"isom\0\0\x02\0");
        file.extend_from_slice(&[0, 0, 0, 0]);
        file.extend_from_slice(b"mdat");
        file.extend_from_slice(&[9; 100]);
        let mut io = ctx(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        let first = s.next_box(&mut b).unwrap().unwrap();
        assert_eq!(first.kind, boxes::FTYP);
        let second = s.next_box(&mut b).unwrap().unwrap();
        assert!(second.to_end);
        assert_eq!(second.payload_len(), 100);
        assert!(s.next_box(&mut b).unwrap().is_none());
    }

    #[test]
    fn a_largesize_box_is_located_correctly() {
        let mut file = vec![0, 0, 0, 1];
        file.extend_from_slice(b"mdat");
        file.extend_from_slice(&(16u64 + 8).to_be_bytes());
        file.extend_from_slice(&[3; 8]);
        file.extend_from_slice(&bx(b"moov", &[0; 8]));
        let mut io = ctx(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        let first = s.next_box(&mut b).unwrap().unwrap();
        assert_eq!(first.header_len, 16);
        assert_eq!(first.payload_len(), 8);
        assert_eq!(s.next_box(&mut b).unwrap().unwrap().kind, boxes::MOOV);
    }

    #[test]
    fn a_box_claiming_more_than_the_file_is_rejected() {
        let mut file = vec![0xFF, 0xFF, 0xFF, 0xFF];
        file.extend_from_slice(b"mdat");
        let mut io = ctx(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        assert!(s.next_box(&mut b).is_err());
    }

    #[test]
    fn a_scan_charges_fuel() {
        let mut file = Vec::new();
        for _ in 0..1000 {
            file.extend_from_slice(&bx(b"free", &[]));
        }
        let mut io = ctx(file);
        let mut b = Budget::new(Limits::permissive().with_fuel(10));
        let mut s = TopLevelScanner::new(&mut io);
        let mut n = 0;
        loop {
            match s.next_box(&mut b) {
                Ok(Some(_)) => n += 1,
                Ok(None) => panic!("should have run out of fuel"),
                Err(Error::LimitExceeded { .. }) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(n, 10);
    }

    #[test]
    fn reading_a_payload_is_charged_to_the_budget() {
        let file = bx(b"moov", &[0u8; 4096]);
        let mut io = ctx(file);
        let mut b = Budget::new(Limits::strict().with_alloc_single(1024));
        let mut s = TopLevelScanner::new(&mut io);
        let span = s.next_box(&mut b).unwrap().unwrap();
        assert!(matches!(
            s.read_payload(span, &mut b),
            Err(Error::LimitExceeded { .. })
        ));
    }

    #[test]
    fn a_trailing_stub_shorter_than_a_header_ends_the_scan() {
        let mut file = bx(b"free", &[]);
        file.extend_from_slice(&[0, 0, 0]);
        let mut io = ctx(file);
        let mut b = budget();
        let mut s = TopLevelScanner::new(&mut io);
        assert!(s.next_box(&mut b).unwrap().is_some());
        assert!(s.next_box(&mut b).unwrap().is_none());
    }

    #[test]
    fn top_level_type_membership() {
        assert!(is_top_level_type(boxes::FTYP));
        assert!(is_top_level_type(boxes::MOOF));
        assert!(!is_top_level_type(boxes::STSD));
    }
}
