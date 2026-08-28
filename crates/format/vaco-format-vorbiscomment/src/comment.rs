//! The Vorbis comment tag list: `vendor_length + vendor + user_comment_list`.
//!
//! Xiph Vorbis I specification §5.2.2 defines this layout as the content of
//! the comment header; the FLAC format states its `VORBIS_COMMENT` metadata
//! block is "the metadata format defined in the Vorbis specification,
//! section 5.2, without the framing bit" — i.e. the same bytes, minus the
//! wrapper. One reader serves both:
//!
//! ```text
//! [packet_type='\x03' 'vorbis']    <- native Vorbis header only
//!   vendor_length (u32, LE)   vendor_string (UTF-8)
//!   user_comment_list_length (u32, LE)
//!     for each: length (u32, LE), 'TAG=value' (UTF-8)
//! [framing_bit: one byte, low bit set]   <- native Vorbis header only
//! ```
//!
//! Verified against the actual bytes a real encoder writes, not transcribed
//! from the specification text alone: `Vaco-Spec-Ref` on the parsing
//! functions below names both.

use vaco_core::{Error, Result};

/// The eight bytes a native Vorbis comment header packet opens with:
/// `packet_type = 3` then the `"vorbis"` magic.
pub const VORBIS_MAGIC: &[u8; 7] = b"\x03vorbis";

/// A parsed comment list, borrowing its input.
///
/// Mirrors `vaco-parse-opus`'s `CommentHeader` in shape — both formats share
/// this exact vendor-plus-list layout — but that crate is not this one's to
/// depend on or to edit, so this is a second, independent reader over the
/// same wire format rather than a shared one. See the crate-level
/// discussion of why: Opus's `OpusTags` shipped before this work package and
/// is out of scope to refactor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VorbisComment<'a> {
    pub vendor: &'a str,
    comments: &'a [u8],
    count: u32,
    /// Bytes after the comment list. For a native header this is exactly the
    /// one framing byte (already checked by [`VorbisComment::parse`]); for a
    /// raw FLAC block it is normally empty.
    pub trailing: &'a [u8],
}

impl<'a> VorbisComment<'a> {
    /// Parse the vendor-plus-list content alone: no packet-type byte, no
    /// `"vorbis"` magic, no framing bit. This is what a FLAC `VORBIS_COMMENT`
    /// metadata block's payload *is*, in full.
    ///
    /// `Vaco-Spec-Ref: rfc-9639` `METADATA_BLOCK_VORBIS_COMMENT`; measured
    /// against a real `ffmpeg -c:a flac` file's block 4.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when a declared length overruns the packet or a
    /// string is not UTF-8. A declared length that runs past the end of the
    /// input is an error rather than a truncation, matching
    /// `vaco-parse-opus::CommentHeader`.
    pub fn parse_raw(data: &'a [u8]) -> Result<Self> {
        let (vendor, rest) = take_string(data)?;
        let (count, mut rest) = take_u32(rest)?;
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

    /// Parse a native Vorbis comment header packet: `packet_type=3`,
    /// `"vorbis"`, the vendor-plus-list content, then a one-byte framing
    /// field whose low bit must be set.
    ///
    /// `Vaco-Spec-Ref: xiph-vorbis-i` §5.2; measured against a real
    /// `ffmpeg -c:a vorbis` encode's second Ogg packet.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the magic is absent, [`Self::parse_raw`]'s
    /// errors, or the framing byte is missing or has its low bit clear.
    pub fn parse_native(data: &'a [u8]) -> Result<Self> {
        let Some(rest) = data.strip_prefix(VORBIS_MAGIC.as_slice()) else {
            return Err(Error::InvalidData("missing Vorbis comment header magic"));
        };
        let parsed = Self::parse_raw(rest)?;
        let Some((&framing, after)) = parsed.trailing.split_first() else {
            return Err(Error::InvalidData(
                "Vorbis comment header is missing its framing byte",
            ));
        };
        if framing & 1 == 0 {
            return Err(Error::InvalidData(
                "Vorbis comment header framing bit is not set",
            ));
        }
        Ok(Self {
            trailing: after,
            ..parsed
        })
    }

    /// How many comments this header declares.
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

    /// The comments split at the first `=`, field name as written.
    ///
    /// RFC-adjacent convention (Vorbis I §5.2, "the field name is
    /// case-insensitive"): case folding is the caller's business, since a
    /// metadata dictionary has its own opinion about key spelling.
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
}

impl<'a> IntoIterator for &VorbisComment<'a> {
    type Item = &'a str;
    type IntoIter = CommentIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterates the `TAG=value` strings of a [`VorbisComment`].
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
        let (s, rest) = take_string(self.rest).ok()?;
        self.rest = rest;
        Some(s)
    }
}

fn take_u32(data: &[u8]) -> Result<(u32, &[u8])> {
    let Some((head, rest)) = data.split_at_checked(4) else {
        return Err(Error::InvalidData("truncated Vorbis comment length field"));
    };
    let Some(bytes) = head.first_chunk::<4>() else {
        return Err(Error::InvalidData("truncated Vorbis comment length field"));
    };
    Ok((u32::from_le_bytes(*bytes), rest))
}

fn take_string(data: &[u8]) -> Result<(&str, &[u8])> {
    let (len, rest) = take_u32(data)?;
    let len =
        usize::try_from(len).map_err(|_| Error::InvalidData("Vorbis comment string too long"))?;
    let Some((head, tail)) = rest.split_at_checked(len) else {
        return Err(Error::InvalidData("Vorbis comment string overruns the packet"));
    };
    let text = str::from_utf8(head)
        .map_err(|_| Error::InvalidData("Vorbis comment string is not UTF-8"))?;
    Ok((text, tail))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    fn le32(n: u32) -> [u8; 4] {
        n.to_le_bytes()
    }

    fn raw_block(vendor: &str, comments: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&le32(vendor.len() as u32));
        out.extend_from_slice(vendor.as_bytes());
        out.extend_from_slice(&le32(comments.len() as u32));
        for c in comments {
            out.extend_from_slice(&le32(c.len() as u32));
            out.extend_from_slice(c.as_bytes());
        }
        out
    }

    #[test]
    fn parse_raw_reads_the_flac_block_shape() {
        let data = raw_block("Lavf62.12.100", &["encoder=Lavf62.12.100"]);
        let comment = VorbisComment::parse_raw(&data).expect("valid block");
        assert_eq!(comment.vendor, "Lavf62.12.100");
        assert_eq!(comment.len(), 1);
        assert_eq!(comment.get("encoder"), Some("Lavf62.12.100"));
        assert!(comment.trailing.is_empty());
    }

    #[test]
    fn parse_native_requires_magic_and_framing_bit() {
        let mut data = VORBIS_MAGIC.to_vec();
        data.extend(raw_block("V", &["title=T", "artist=A"]));
        data.push(1); // framing bit set
        let comment = VorbisComment::parse_native(&data).expect("valid native header");
        assert_eq!(comment.get("title"), Some("T"));
        assert_eq!(comment.get("TITLE"), Some("T"), "case-insensitive");
        assert_eq!(comment.get("artist"), Some("A"));
        assert!(comment.trailing.is_empty());

        // No magic at all.
        assert!(VorbisComment::parse_native(&raw_block("V", &[])).is_err());

        // Magic present, framing bit clear.
        let mut bad = VORBIS_MAGIC.to_vec();
        bad.extend(raw_block("V", &[]));
        bad.push(0);
        assert!(VorbisComment::parse_native(&bad).is_err());

        // Magic present, framing byte missing entirely.
        let mut short = VORBIS_MAGIC.to_vec();
        short.extend(raw_block("V", &[]));
        assert!(VorbisComment::parse_native(&short).is_err());
    }

    #[test]
    fn a_declared_length_past_the_end_is_an_error_not_a_panic() {
        assert!(VorbisComment::parse_raw(&[]).is_err());
        assert!(VorbisComment::parse_raw(&[0xff, 0xff, 0xff, 0xff]).is_err());
        let mut data = le32(1_000_000).to_vec();
        data.extend_from_slice(b"short");
        assert!(VorbisComment::parse_raw(&data).is_err());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..64usize {
            let data = vec![0xffu8; len];
            let _ = VorbisComment::parse_raw(&data);
            let _ = VorbisComment::parse_native(&data);
        }
    }
}
