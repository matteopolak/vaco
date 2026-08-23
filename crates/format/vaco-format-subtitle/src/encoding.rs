//! Byte-order-mark sniffing, matched to the reference's measured behaviour.
//!
//! Measured against `ffmpeg 8.1` (`ffprobe -f srt -show_packets -show_data`,
//! D17), a text-subtitle demuxer does exactly three things with encoding,
//! never a fourth:
//!
//! 1. A UTF-8 BOM (`EF BB BF`) at the very start of the file is stripped; the
//!    rest of the file is treated as UTF-8 bytes, unvalidated.
//! 2. A UTF-16LE (`FF FE`) or UTF-16BE (`FE FF`) BOM at the start of the file
//!    triggers a full conversion to UTF-8, and the BOM does not appear in the
//!    output.
//! 3. Anything else — no BOM, or a legacy single-byte encoding such as
//!    Latin-1 — passes through **completely unchanged**. A raw `0xE9` (Latin-1
//!    `é`) or a stray `0xFF 0xFE` in the middle of a line survive byte-for-byte
//!    into the packet payload. There is no auto-detection of legacy encodings
//!    without an explicit hint (the reference's `-sub_charenc`, which nothing
//!    in this crate reaches for — a demuxer that guessed would silently
//!    mis-decode a file the reference passes through faithfully).
//!
//! So this module's whole job is: sniff a BOM, strip or convert it, and
//! otherwise get out of the way.

/// Which byte-order mark, if any, was found and consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedEncoding {
    /// No BOM. The bytes are passed through exactly as read.
    Unmarked,
    /// A UTF-8 BOM was present and stripped.
    Utf8,
    /// A UTF-16LE BOM was present; the file was converted to UTF-8.
    Utf16Le,
    /// A UTF-16BE BOM was present; the file was converted to UTF-8.
    Utf16Be,
}

const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const UTF16LE_BOM: [u8; 2] = [0xFF, 0xFE];
const UTF16BE_BOM: [u8; 2] = [0xFE, 0xFF];

/// Sniff a BOM at the start of `input` and return UTF-8 bytes plus what was
/// found.
///
/// This is the one function every demuxer in `vaco-subtitle-text` calls
/// before doing anything else with its input. It never fails: a UTF-16 file
/// whose length is odd, or that contains an unpaired surrogate, is decoded
/// with `\u{FFFD}` in place of the bad unit rather than rejected — a
/// corrupted subtitle file is exactly the case a demuxer needs to stay
/// lenient about (see `planning/AGENT-CONSTRAINTS.md`, "Detection and
/// demuxing ask different questions").
#[must_use]
pub fn decode_to_utf8_bytes(input: &[u8]) -> (Vec<u8>, DetectedEncoding) {
    if let Some(rest) = input.strip_prefix(&UTF8_BOM) {
        return (rest.to_vec(), DetectedEncoding::Utf8);
    }
    if let Some(rest) = input.strip_prefix(&UTF16LE_BOM) {
        return (
            utf16_to_utf8(rest, u16::from_le_bytes),
            DetectedEncoding::Utf16Le,
        );
    }
    if let Some(rest) = input.strip_prefix(&UTF16BE_BOM) {
        return (
            utf16_to_utf8(rest, u16::from_be_bytes),
            DetectedEncoding::Utf16Be,
        );
    }
    (input.to_vec(), DetectedEncoding::Unmarked)
}

/// Decode `bytes` as UTF-16 code units assembled by `unit_from`, lossily.
///
/// A trailing odd byte (a truncated code unit) is dropped rather than panicking
/// — `chunks_exact(2)` already excludes it, so there is nothing further to do.
fn utf16_to_utf8(bytes: &[u8], unit_from: fn([u8; 2]) -> u16) -> Vec<u8> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            // `chunks_exact(2)` guarantees exactly two bytes per chunk.
            let arr: [u8; 2] = [
                pair.first().copied().unwrap_or(0),
                pair.get(1).copied().unwrap_or(0),
            ];
            unit_from(arr)
        })
        .collect();
    String::from_utf16_lossy(&units).into_bytes()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn utf8_bom_is_stripped() {
        let (out, enc) = decode_to_utf8_bytes(b"\xEF\xBB\xBFhello");
        assert_eq!(out, b"hello");
        assert_eq!(enc, DetectedEncoding::Utf8);
    }

    #[test]
    fn utf16le_bom_converts() {
        let mut input = vec![0xFF, 0xFE];
        input.extend("hi".encode_utf16().flat_map(u16::to_le_bytes));
        let (out, enc) = decode_to_utf8_bytes(&input);
        assert_eq!(out, b"hi");
        assert_eq!(enc, DetectedEncoding::Utf16Le);
    }

    #[test]
    fn utf16be_bom_converts() {
        let mut input = vec![0xFE, 0xFF];
        input.extend("hi".encode_utf16().flat_map(u16::to_be_bytes));
        let (out, enc) = decode_to_utf8_bytes(&input);
        assert_eq!(out, b"hi");
        assert_eq!(enc, DetectedEncoding::Utf16Be);
    }

    #[test]
    fn unmarked_bytes_pass_through_verbatim_including_invalid_utf8() {
        let input: &[u8] = &[b'H', b'i', 0xFF, 0xFE, b' ', 0xE9];
        let (out, enc) = decode_to_utf8_bytes(input);
        assert_eq!(out, input);
        assert_eq!(enc, DetectedEncoding::Unmarked);
    }

    #[test]
    fn truncated_utf16_drops_the_dangling_byte_without_panicking() {
        let input: &[u8] = &[0xFF, 0xFE, b'h', 0, b'i', 0, 0x41];
        let (out, _) = decode_to_utf8_bytes(input);
        assert_eq!(out, b"hi");
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..64 {
            let buf: Vec<u8> = (0..len).map(|i| (i * 37 % 256) as u8).collect();
            let _ = decode_to_utf8_bytes(&buf);
        }
    }
}
