//! The Opus comment header. RFC 7845 §5.2.
//!
//! The same Vorbis-style tag list Vorbis and FLAC use, wrapped in an `OpusTags`
//! magic. Everything here borrows the input: a comment header is metadata read
//! once and copied by whoever wants to keep it, so allocating a `String` per tag
//! inside the parser would be a cost paid by every caller including the ones
//! that only want the vendor string.
//!
//! ```text
//!   'O','p','u','s','T','a','g','s'
//!   vendor string length (32, little-endian)   vendor string (UTF-8)
//!   user comment list length (32, little-endian)
//!     for each: length (32, little-endian), 'TAG=value' (UTF-8)
//!   [trailing bytes: padding, or RFC 7845 §5.2.1 channel-mapping extensions]
//! ```

use vaco_core::{Error, Result};

/// The eight magic bytes a comment header opens with.
pub const MAGIC: &[u8; 8] = b"OpusTags";

/// A parsed comment header, borrowing its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentHeader<'a> {
    /// The vendor string — the encoder's own identification.
    pub vendor: &'a str,
    /// The raw `TAG=value` list, unvalidated beyond being UTF-8.
    comments: &'a [u8],
    /// How many entries the list header declared.
    count: u32,
    /// Bytes after the comment list: padding, or the §5.2.1 extension.
    pub trailing: &'a [u8],
}

impl<'a> CommentHeader<'a> {
    /// Parse an `OpusTags` packet, magic included.
    ///
    /// A declared length that runs past the end of the packet is an error
    /// rather than a truncation: a comment header is not a stream, so a short
    /// read means the packet is malformed.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the magic is absent, a length overruns the
    /// packet, or a string is not UTF-8.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let Some(rest) = data.strip_prefix(MAGIC.as_slice()) else {
            return Err(Error::InvalidData("missing OpusTags magic"));
        };
        let (vendor, rest) = take_string(rest)?;
        let (count, mut rest) = take_u32(rest)?;

        // Walk the list once to find where it ends, bounding the walk by the
        // bytes that actually exist: a four-byte count can claim four billion
        // comments in a twenty-byte packet.
        let list_start = rest;
        let mut consumed = 0usize;
        for _ in 0..count {
            let (_, tail) = take_string(rest)?;
            consumed = list_start.len().saturating_sub(tail.len());
            rest = tail;
        }
        let comments = list_start.get(..consumed).unwrap_or_default();

        Ok(Self {
            vendor,
            comments,
            count,
            trailing: rest,
        })
    }

    /// How many comments the header declares, which is also how many
    /// [`CommentHeader::iter`] yields.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.count
    }

    /// Whether the comment list is empty. The vendor string is not a comment.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The comments, in order, as raw `TAG=value` strings.
    #[must_use]
    pub const fn iter(&self) -> CommentIter<'a> {
        CommentIter {
            rest: self.comments,
        }
    }

    /// The comments split at the first `=`, with the field name left as written.
    ///
    /// RFC 7845 §5.2 makes field names case-insensitive ASCII; case folding is
    /// the caller's business because a metadata dictionary has its own opinion
    /// about key spelling.
    pub fn pairs(&self) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.iter().filter_map(|c| c.split_once('='))
    }

    /// The first value whose field name matches `name`, case-insensitively.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'a str> {
        self.pairs()
            .find(|&(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    /// The `R128_TRACK_GAIN` tag, in Q7.8 dB, if present and well formed.
    ///
    /// RFC 7845 §5.2.1 defines it as the track gain to apply on top of the
    /// identification header's output gain.
    #[must_use]
    pub fn r128_track_gain(&self) -> Option<i16> {
        self.get("R128_TRACK_GAIN")?.trim().parse().ok()
    }

    /// The `R128_ALBUM_GAIN` tag, in Q7.8 dB.
    #[must_use]
    pub fn r128_album_gain(&self) -> Option<i16> {
        self.get("R128_ALBUM_GAIN")?.trim().parse().ok()
    }
}

impl<'a> IntoIterator for &CommentHeader<'a> {
    type Item = &'a str;
    type IntoIter = CommentIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterates the `TAG=value` strings of a [`CommentHeader`].
#[derive(Debug, Clone)]
pub struct CommentIter<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for CommentIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        // The whole list was validated by `CommentHeader::parse`, so a failure
        // here can only mean the iterator was handed something else. Stop
        // rather than loop.
        let (s, rest) = take_string(self.rest).ok()?;
        self.rest = rest;
        Some(s)
    }
}

fn take_u32(data: &[u8]) -> Result<(u32, &[u8])> {
    let Some((head, rest)) = data.split_at_checked(4) else {
        return Err(Error::InvalidData("truncated OpusTags length field"));
    };
    let Some(bytes) = head.first_chunk::<4>() else {
        return Err(Error::InvalidData("truncated OpusTags length field"));
    };
    Ok((u32::from_le_bytes(*bytes), rest))
}

fn take_string(data: &[u8]) -> Result<(&str, &[u8])> {
    let (len, rest) = take_u32(data)?;
    let len = usize::try_from(len).map_err(|_| Error::InvalidData("OpusTags string too long"))?;
    let Some((head, tail)) = rest.split_at_checked(len) else {
        return Err(Error::InvalidData("OpusTags string overruns the packet"));
    };
    let text =
        str::from_utf8(head).map_err(|_| Error::InvalidData("OpusTags string is not UTF-8"))?;
    Ok((text, tail))
}
