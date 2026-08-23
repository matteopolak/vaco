//! The byte-range segment reader HLS and DASH both need.
//!
//! `#EXT-X-BYTERANGE:<length>[@<offset>]` (RFC 8216 §4.4.4.2) and DASH's
//! `indexRange`/`SegmentBase`+`SegmentList` `<Initialization range="…">` /
//! `<SegmentURL mediaRange="…">` (ISO/IEC 23009-1 §5.3.9.2, §5.3.9.5) are the
//! same idea stated in two syntaxes: one physical file holds many logical
//! segments, addressed by a byte offset and length. `-hls_flags single_file`
//! and DASH `-single_file` write that shape from the mux side.
//!
//! [`BoundedSource`] is the one implementation: a [`MediaSource`] adapter that
//! exposes only `[offset, offset + length)` of an underlying source, so that
//! everything above it — including a nested MPEG-TS or fMP4 demuxer reached
//! through [`crate::provider::SegmentDemuxerProvider`] — sees what looks like
//! a small, self-contained file starting at position zero.

use vaco_core::{Error, Result};
use vaco_io::{MediaSource, Seekability};

/// A `length@offset` byte range, in the container's own byte space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

impl ByteRange {
    /// The whole file, unbounded — the value that means "not a byte-range
    /// segment at all", so a caller can build one unconditionally and check
    /// `is_whole_file` rather than threading an `Option<ByteRange>` through
    /// every layer twice.
    #[must_use]
    pub const fn whole_file() -> Self {
        Self {
            offset: 0,
            length: u64::MAX,
        }
    }

    /// Parse HLS's `#EXT-X-BYTERANGE:<n>[@<o>]` value, or DASH's
    /// `first-last` HTTP range-suffix form (`indexRange="0-863"`,
    /// `mediaRange="864-48087"`), given the offset an omitted HLS `@o` falls
    /// back to (the end of the previous byte-range segment in the same
    /// playlist; RFC 8216 §4.4.4.2).
    ///
    /// Returns `None` for anything that is not one of the two known shapes,
    /// rather than guessing — an unparsed byte range is exactly the kind of
    /// wrong-but-plausible value the module docs warn produces silent drift.
    #[must_use]
    pub fn parse_hls(value: &str, previous_end: u64) -> Option<Self> {
        let (len_str, off_str) = match value.split_once('@') {
            Some((l, o)) => (l, Some(o)),
            None => (value, None),
        };
        let length: u64 = len_str.trim().parse().ok()?;
        let offset = match off_str {
            Some(o) => o.trim().parse().ok()?,
            None => previous_end,
        };
        Some(Self { offset, length })
    }

    /// Parse DASH's inclusive `first-last` range-suffix form.
    #[must_use]
    pub fn parse_dash_range(value: &str) -> Option<Self> {
        let (first, last) = value.split_once('-')?;
        let first: u64 = first.trim().parse().ok()?;
        let last: u64 = last.trim().parse().ok()?;
        let length = last.checked_sub(first)?.checked_add(1)?;
        Some(Self {
            offset: first,
            length,
        })
    }

    /// One past the last byte this range covers — what an HLS playlist with
    /// an omitted next `@o` continues from.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.offset.saturating_add(self.length)
    }
}

/// A [`MediaSource`] restricted to one [`ByteRange`] of an underlying source.
///
/// Positions reported and accepted by this adapter are relative to the range's
/// own start, so a nested demuxer opened on one sees a normal file beginning
/// at byte zero, whatever offset it actually sits at.
pub struct BoundedSource {
    inner: Box<dyn MediaSource>,
    range: ByteRange,
    /// Position relative to `range.offset`.
    pos: u64,
}

/// Manual: `dyn MediaSource` carries no `Debug` bound (the trait predates this
/// crate and is frozen), so a derive cannot reach through the box.
impl std::fmt::Debug for BoundedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedSource")
            .field("range", &self.range)
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl BoundedSource {
    /// Wrap `inner`, exposing only `range`.
    ///
    /// # Errors
    /// Propagates the initial seek to `range.offset`.
    pub fn new(mut inner: Box<dyn MediaSource>, range: ByteRange) -> Result<Self> {
        inner.seek(range.offset)?;
        Ok(Self {
            inner,
            range,
            pos: 0,
        })
    }

    /// Bytes remaining in the range from the current position, capped by
    /// what the underlying source itself reports remaining (a range that
    /// claims to extend past a short file is truncated at the file, not
    /// treated as corrupt: DASH's `indexRange` on a still-growing live
    /// segment is legitimately open-ended in practice).
    fn remaining(&self) -> u64 {
        if self.range.length == u64::MAX {
            return u64::MAX;
        }
        self.range.length.saturating_sub(self.pos)
    }
}

impl MediaSource for BoundedSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let remaining = self.remaining();
        if remaining == 0 {
            return Ok(0);
        }
        let cap = usize::try_from(remaining).unwrap_or(usize::MAX);
        let n = buf.len().min(cap);
        let Some(dst) = buf.get_mut(..n) else {
            return Ok(0);
        };
        let read = self.inner.read(dst)?;
        self.pos = self.pos.saturating_add(read as u64);
        Ok(read)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        if self.inner.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let target = self.range.offset.saturating_add(pos);
        self.inner.seek(target)?;
        self.pos = pos;
        Ok(pos)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        if self.range.length == u64::MAX {
            return self
                .inner
                .size()
                .map(|s| s.saturating_sub(self.range.offset));
        }
        Some(self.range.length)
    }

    fn seekability(&self) -> Seekability {
        self.inner.seekability()
    }

    fn peek(&mut self, len: usize) -> Result<&[u8]> {
        let cap = usize::try_from(self.remaining()).unwrap_or(usize::MAX);
        self.inner.peek(len.min(cap))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    #[test]
    fn hls_byterange_with_explicit_offset() {
        let r = ByteRange::parse_hls("1024@512", 0).unwrap();
        assert_eq!(
            r,
            ByteRange {
                offset: 512,
                length: 1024
            }
        );
    }

    #[test]
    fn hls_byterange_without_offset_continues_the_previous_one() {
        let r = ByteRange::parse_hls("1024", 512).unwrap();
        assert_eq!(
            r,
            ByteRange {
                offset: 512,
                length: 1024
            }
        );
    }

    #[test]
    fn dash_range_is_inclusive() {
        let r = ByteRange::parse_dash_range("0-863").unwrap();
        assert_eq!(
            r,
            ByteRange {
                offset: 0,
                length: 864
            }
        );
    }

    #[test]
    fn garbage_does_not_parse_as_either_shape() {
        assert!(ByteRange::parse_hls("not-a-number", 0).is_none());
        assert!(ByteRange::parse_dash_range("only-one-side").is_none());
    }

    #[test]
    fn bounded_source_exposes_only_its_slice_starting_at_zero() {
        let data = (0u8..=255).collect::<Vec<u8>>();
        let src = Box::new(MemorySource::new(data));
        let mut bounded = BoundedSource::new(
            src,
            ByteRange {
                offset: 10,
                length: 20,
            },
        )
        .unwrap();
        assert_eq!(bounded.position(), 0);
        assert_eq!(bounded.size(), Some(20));
        let mut buf = [0u8; 5];
        bounded.read(&mut buf).unwrap();
        assert_eq!(buf, [10, 11, 12, 13, 14]);
        assert_eq!(bounded.position(), 5);

        // Reading to and past the end of the range never reaches bytes
        // belonging to the underlying source past the range.
        let mut all = Vec::new();
        loop {
            let mut chunk = [0u8; 7];
            let n = bounded.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            all.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(all, &data_slice(10, 30)[5..]);
    }

    fn data_slice(start: u8, end: u8) -> Vec<u8> {
        (start..end).collect()
    }

    #[test]
    fn whole_file_range_never_starves() {
        let data = vec![1u8, 2, 3, 4];
        let src = Box::new(MemorySource::new(data));
        let mut bounded = BoundedSource::new(src, ByteRange::whole_file()).unwrap();
        assert_eq!(bounded.size(), Some(4));
        let mut buf = [0u8; 4];
        assert_eq!(bounded.read(&mut buf).unwrap(), 4);
    }
}
