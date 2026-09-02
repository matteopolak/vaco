//! `frame_header()` field extraction — RDD 36 §5.1.1 — stopped the instant
//! `alpha_channel_type` is read.
//!
//! # Why not just call `vaco-codec-prores`
//!
//! Same layering `vaco-parse-h264` already draws against `vaco-codec-h264`
//! (checked before writing this: no `vaco-parse-*` crate in this workspace
//! depends on its `vaco-codec-*` decoder counterpart) — a parser has to work
//! in a `parse-prores`-without-`codec-prores` build, so it cannot name the
//! decoder crate at all. This transcribes the same handful of `frame_header()`
//! fields `vaco-codec-prores::header::parse_frame_header` reads, from the same
//! published RDD 36 field order, and stops several fields earlier: nothing
//! past `alpha_channel_type` (not `load_luma_quantization_matrix`, not either
//! quantization matrix) is read, because nothing past it bears on pixel
//! format. Both copies answer to the same spec table, which is what keeps
//! them from drifting apart even though they are not the same code.
//!
//! # Byte layout consulted (offsets from the start of `frame_header()`,
//! i.e. from the two-byte `frame_header_size` field itself, matching
//! `vaco-codec-prores::decoder::decode_frame_payload`'s own slicing)
//!
//! | Bytes | Field |
//! |---|---|
//! | 0-1 | `frame_header_size` (`u16`) |
//! | 2 | reserved |
//! | 3 | `bitstream_version` |
//! | 4-7 | `encoder_identifier` |
//! | 8-9 | `horizontal_size` |
//! | 10-11 | `vertical_size` |
//! | 12 | `chroma_format`(2) / reserved(2) / `interlace_mode`(2) / reserved(2) |
//! | 13 | `aspect_ratio_information`(4) / `frame_rate_code`(4) |
//! | 14 | `color_primaries` |
//! | 15 | `transfer_characteristic` |
//! | 16 | `matrix_coefficients` |
//! | 17 | reserved(4) / `alpha_channel_type`(4) — **stop here** |
//!
//! 18 bytes, against a coded frame that is at minimum a slice header plus one
//! macroblock's worth of coefficients — reading this and nothing else is the
//! entire reason a parser can answer "what pixel format" without decoding a
//! sample.

use vaco_bitstream::BitReader;
use vaco_pixfmt::PixFmt;

/// `frame_identifier`, RDD 36 §5.1: every `ProRes` sample this workspace's
/// demuxers hand a parser starts with the four-byte size this crate does not
/// need, then this tag, then `frame_header()`.
const FRAME_IDENTIFIER: &[u8; 4] = b"icpf";

/// What this crate exists to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameInfo {
    pub(crate) format: PixFmt,
    /// 10 for 4:2:2, 12 for 4:4:4 — not itself a bitstream syntax element (RDD
    /// 36 does not state a sample bit depth), the same derived-from-
    /// `chroma_format` rule `vaco-codec-prores::header::FrameHeader::bit_depth`
    /// documents having measured against real `ffmpeg -c:v prores_ks` output
    /// for every profile: 4:2:2 is always 10-bit, 4:4:4 always 12, with or
    /// without alpha.
    pub(crate) bits_per_raw_sample: u8,
}

/// Read `frame_header()` out of one whole `ProRes` sample (`payload[0..4]` is
/// the sample's own `frame_size`, `payload[4..8]` must be `"icpf"`,
/// `payload[8..]` is `frame_header()`) and resolve the pixel format.
///
/// `None` for anything this does not recognise — truncated input, a missing
/// `"icpf"` tag, a `frame_header_size` too small to hold what is read, a
/// reserved `chroma_format`/`alpha_channel_type` code, or `bitstream_version`
/// 0 paired with 4:4:4 or alpha (RDD 36 §5.1.1 forbids that combination,
/// exactly as `vaco-codec-prores::header::parse_frame_header` rejects it).
/// Every one of those is "this sample told this parser nothing", not an
/// error: a malformed or short sample is not a reason to stop reporting
/// whatever the container itself already knows, the same contract every
/// other `Parser::parse` in this workspace follows for a bad header.
pub(crate) fn parse(payload: &[u8]) -> Option<FrameInfo> {
    if payload.get(4..8) != Some(FRAME_IDENTIFIER) {
        return None;
    }
    let header_bytes = payload.get(8..)?;
    let mut r = BitReader::new(header_bytes);
    let frame_header_size = r.try_get(16).ok()?;
    if frame_header_size < 20 {
        return None;
    }
    let _reserved = r.get(8);
    let bitstream_version = r.get(8);
    if bitstream_version > 1 {
        return None;
    }
    let _encoder_identifier = r.get(32);
    let _horizontal_size = r.get(16);
    let _vertical_size = r.get(16);
    let chroma_format_code = r.get(2);
    let _reserved = r.get(2);
    let _interlace_code = r.get(2);
    let _reserved = r.get(2);
    let _aspect_ratio_information = r.get(4);
    let _frame_rate_code = r.get(4);
    let _color_primaries = r.get(8);
    let _transfer_characteristic = r.get(8);
    let _matrix_coefficients = r.get(8);
    let _reserved = r.get(4);
    let alpha_channel_type = r.get(4);
    // `BitReader::get` past a truncated buffer returns a sticky zero rather
    // than panicking (see its own docs), which would silently misread a
    // genuinely truncated `frame_header()` as "no alpha, 4:2:2" — checked
    // once at the end, the same shape every reader in this workspace uses,
    // rather than threading `Result` through each field above.
    if r.overrun() {
        return None;
    }
    if alpha_channel_type > 2 {
        return None;
    }
    let has_alpha = alpha_channel_type != 0;
    let format = match (chroma_format_code, has_alpha) {
        (2, false) => PixFmt::Yuv422p10le,
        (2, true) => PixFmt::Yuva422p10le,
        (3, false) => PixFmt::Yuv444p12le,
        (3, true) => PixFmt::Yuva444p12le,
        _ => return None, // reserved chroma_format
    };
    if bitstream_version == 0 && (chroma_format_code != 2 || has_alpha) {
        return None;
    }
    let bits_per_raw_sample = if chroma_format_code == 2 { 10 } else { 12 };
    Some(FrameInfo {
        format,
        bits_per_raw_sample,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    /// Builds a minimal, valid `icpf` sample: 4-byte frame size (unread),
    /// `"icpf"`, then just enough of `frame_header()` for [`parse`] to reach
    /// `alpha_channel_type` — the same 18 bytes the module doc's table lists,
    /// padded to a legal `frame_header_size` of 20.
    fn sample(chroma_format: u8, alpha_channel_type: u8, bitstream_version: u8) -> Vec<u8> {
        let mut header = vec![0u8; 20];
        header[0..2].copy_from_slice(&20u16.to_be_bytes());
        header[3] = bitstream_version;
        header[12] = chroma_format << 6;
        header[17] = alpha_channel_type;
        let mut sample = vec![0u8; 4];
        sample.extend_from_slice(FRAME_IDENTIFIER);
        sample.extend_from_slice(&header);
        sample
    }

    #[test]
    fn a_422_sample_with_no_alpha_resolves_10_bit_422() {
        let info = parse(&sample(2, 0, 1)).unwrap();
        assert_eq!(info.format, PixFmt::Yuv422p10le);
        assert_eq!(info.bits_per_raw_sample, 10);
    }

    #[test]
    fn a_444_sample_with_alpha_resolves_12_bit_4444() {
        let info = parse(&sample(3, 1, 1)).unwrap();
        assert_eq!(info.format, PixFmt::Yuva444p12le);
        assert_eq!(info.bits_per_raw_sample, 12);
    }

    #[test]
    fn a_444_sample_with_16_bit_alpha_still_resolves() {
        let info = parse(&sample(3, 2, 1)).unwrap();
        assert_eq!(info.format, PixFmt::Yuva444p12le);
    }

    #[test]
    fn version_zero_with_444_is_rejected() {
        assert!(parse(&sample(3, 0, 0)).is_none());
    }

    #[test]
    fn version_zero_with_alpha_is_rejected() {
        assert!(parse(&sample(2, 1, 0)).is_none());
    }

    #[test]
    fn version_zero_plain_422_is_accepted() {
        let info = parse(&sample(2, 0, 0)).unwrap();
        assert_eq!(info.format, PixFmt::Yuv422p10le);
    }

    #[test]
    fn a_reserved_chroma_format_resolves_nothing() {
        assert!(parse(&sample(0, 0, 1)).is_none());
        assert!(parse(&sample(1, 0, 1)).is_none());
    }

    #[test]
    fn a_reserved_alpha_channel_type_resolves_nothing() {
        assert!(parse(&sample(2, 3, 1)).is_none());
    }

    #[test]
    fn a_missing_icpf_tag_resolves_nothing() {
        let mut s = sample(2, 0, 1);
        s[4] = b'x';
        assert!(parse(&s).is_none());
    }

    #[test]
    fn a_truncated_sample_resolves_nothing() {
        let s = sample(2, 0, 1);
        assert!(parse(&s[..10]).is_none());
    }

    #[test]
    fn too_small_a_frame_header_size_resolves_nothing() {
        let mut s = sample(2, 0, 1);
        s[8..10].copy_from_slice(&19u16.to_be_bytes());
        assert!(parse(&s).is_none());
    }

    #[test]
    fn empty_input_resolves_nothing() {
        assert!(parse(&[]).is_none());
    }
}
