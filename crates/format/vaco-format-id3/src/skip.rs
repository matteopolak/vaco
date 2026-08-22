//! Skipping an `ID3v2` tag at the start of a stream.
//!
//! MP3 (and other raw elementary-stream formats a container does not wrap)
//! commonly begin with an `ID3v2` tag before the actual codec data starts —
//! and container/codec probing has to look *past* it, not at it, or every
//! MP3 with a tag attached would fail to identify as MP3 at all. This is a
//! read-only peek: it never trusts a declared size for anything beyond how
//! far to skip, and it works on a forward-only source (a pipe), because it
//! is built on [`vaco_io::IoContext::peek`], which guarantees exactly that.

use vaco_core::Result;
use vaco_io::IoContext;

use crate::header::{Id3v2Header, LEN};

/// Bytes to skip to move past an `ID3v2` tag at `io`'s current position, or
/// `0` if there is none.
///
/// Peeks the header only — it does not need the frames themselves to know
/// how much to skip, so a caller doing pure format probing never has to
/// allocate a copy of the tag body it is not going to look at.
///
/// # Errors
///
/// Propagates a transport failure from the underlying source. A header that
/// fails to parse (missing `"ID3"`, too few bytes available) is not an
/// error here — it means "no tag", and this returns `Ok(0)`.
pub fn detect(io: &mut IoContext) -> Result<u64> {
    let Ok(peeked) = io.peek(LEN) else {
        return Ok(0);
    };
    match Id3v2Header::parse(peeked) {
        Ok(header) => Ok(header.total_len()),
        Err(_) => Ok(0),
    }
}

/// [`detect`], then actually advance `io` past the tag if one was found.
///
/// # Errors
///
/// Propagates a transport failure from [`detect`] or the seek/skip it
/// performs.
pub fn skip(io: &mut IoContext) -> Result<u64> {
    let len = detect(io)?;
    if len > 0 {
        io.skip(len)?;
    }
    Ok(len)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::{IoOptions, MemorySource};

    fn synchsafe_bytes(n: u32) -> [u8; 4] {
        [
            ((n >> 21) & 0x7f) as u8,
            ((n >> 14) & 0x7f) as u8,
            ((n >> 7) & 0x7f) as u8,
            (n & 0x7f) as u8,
        ]
    }

    fn make_stream(tag_body_len: u32, audio: &[u8]) -> Vec<u8> {
        let mut out = b"ID3".to_vec();
        out.push(3);
        out.push(0);
        out.push(0);
        out.extend_from_slice(&synchsafe_bytes(tag_body_len));
        out.extend(std::iter::repeat_n(0u8, tag_body_len as usize));
        out.extend_from_slice(audio);
        out
    }

    #[test]
    fn detects_and_skips_a_tag_on_a_forward_only_source() {
        let stream = make_stream(50, b"\xff\xfbAUDIO");
        let src = MemorySource::forward_only(stream);
        let mut io = IoContext::new(Box::new(src), &IoOptions::default()).unwrap();
        let skipped = skip(&mut io).unwrap();
        assert_eq!(skipped, 10 + 50);
        assert_eq!(&io.peek(4).unwrap()[..4], b"\xff\xfbAU");
    }

    #[test]
    fn no_tag_skips_nothing() {
        let src = MemorySource::forward_only(b"\xff\xfbAUDIO".to_vec());
        let mut io = IoContext::new(Box::new(src), &IoOptions::default()).unwrap();
        let skipped = skip(&mut io).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(&io.peek(4).unwrap()[..4], b"\xff\xfbAU");
    }

    #[test]
    fn a_stream_shorter_than_ten_bytes_skips_nothing() {
        let src = MemorySource::forward_only(b"ID3".to_vec());
        let mut io = IoContext::new(Box::new(src), &IoOptions::default()).unwrap();
        assert_eq!(skip(&mut io).unwrap(), 0);
    }
}
