//! The Xing/Info VBR header, written into the first frame right after the
//! Layer III side information.
//!
//! Confirmed byte-for-byte (tag position, flag bits, field order) against a
//! real `ffmpeg -c:a libmp3lame` VBR file before this was written.

use crate::lame::LameTag;

const FLAG_FRAMES: u32 = 1 << 0;
const FLAG_BYTES: u32 = 1 << 1;
const FLAG_TOC: u32 = 1 << 2;
const FLAG_QUALITY: u32 = 1 << 3;
/// LAME only appends its extension when it wrote every optional field; a
/// header with a subset of the flags is some other encoder's plain Xing/Info
/// tag, and the bytes after it are not this format.
const FLAGS_FULL: u32 = FLAG_FRAMES | FLAG_BYTES | FLAG_TOC | FLAG_QUALITY;

pub const TOC_LEN: usize = 100;

/// `Xing`/`Info` plus, when present, the trailing LAME extension.
#[derive(Debug, Clone)]
pub struct XingHeader {
    /// `true` for the `"Xing"` tag (declares VBR), `false` for `"Info"`
    /// (the same layout, written by a CBR encoder).
    pub vbr: bool,
    pub num_frames: Option<u32>,
    pub num_bytes: Option<u32>,
    pub toc: Option<[u8; TOC_LEN]>,
    pub quality: Option<u32>,
    pub lame: Option<LameTag>,
}

impl XingHeader {
    /// `data` starts at the tag's own `"Xing"`/`"Info"` magic.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        let tag = data.first_chunk::<4>()?;
        let vbr = match tag {
            b"Xing" => true,
            b"Info" => false,
            _ => return None,
        };
        let mut off = 4usize;
        let flags = read_be32(data, off)?;
        off = off.saturating_add(4);

        let mut num_frames = None;
        let mut num_bytes = None;
        let mut toc = None;
        let mut quality = None;
        if flags & FLAG_FRAMES != 0 {
            num_frames = Some(read_be32(data, off)?);
            off = off.saturating_add(4);
        }
        if flags & FLAG_BYTES != 0 {
            num_bytes = Some(read_be32(data, off)?);
            off = off.saturating_add(4);
        }
        if flags & FLAG_TOC != 0 {
            toc = data
                .get(off..off.saturating_add(TOC_LEN))?
                .first_chunk::<TOC_LEN>()
                .copied();
            off = off.saturating_add(TOC_LEN);
        }
        if flags & FLAG_QUALITY != 0 {
            quality = Some(read_be32(data, off)?);
            off = off.saturating_add(4);
        }
        let lame = (flags & FLAGS_FULL == FLAGS_FULL)
            .then(|| data.get(off..))
            .flatten()
            .and_then(LameTag::parse);

        Some(Self {
            vbr,
            num_frames,
            num_bytes,
            toc,
            quality,
            lame,
        })
    }
}

fn read_be32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at.saturating_add(4))
        .and_then(|s| s.first_chunk::<4>())
        .map(|c| u32::from_be_bytes(*c))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::lame;

    #[test]
    fn measured_lame_vbr_header() {
        let mut frame = vec![0u8; 4 + 4 + 4 + 4 + TOC_LEN + 4 + lame::LEN];
        frame[..4].copy_from_slice(b"Xing");
        frame[4..8].copy_from_slice(&0x0000_000Fu32.to_be_bytes());
        frame[8..12].copy_from_slice(&78u32.to_be_bytes());
        frame[12..16].copy_from_slice(&9050u32.to_be_bytes());
        let lame_off = 4 + 4 + 4 + 4 + TOC_LEN + 4;
        frame[lame_off..lame_off + 9].copy_from_slice(b"Lavc62.28");
        frame[lame_off + 21] = 0x24;
        frame[lame_off + 22] = 0x04;
        frame[lame_off + 23] = 0x38;

        let xing = XingHeader::parse(&frame).expect("valid Xing header");
        assert!(xing.vbr);
        assert_eq!(xing.num_frames, Some(78));
        assert_eq!(xing.num_bytes, Some(9050));
        assert!(xing.toc.is_some());
        let lame = xing.lame.expect("LAME extension present");
        assert_eq!(lame.encoder_delay, 576);
        assert_eq!(lame.encoder_padding, 1080);
    }

    #[test]
    fn partial_flags_never_claim_a_lame_extension() {
        let mut frame = vec![0u8; 4 + 4 + 4 + lame::LEN + 8];
        frame[..4].copy_from_slice(b"Info");
        frame[4..8].copy_from_slice(&0x0000_0001u32.to_be_bytes());
        frame[8..12].copy_from_slice(&10u32.to_be_bytes());
        let xing = XingHeader::parse(&frame).expect("valid header");
        assert!(!xing.vbr);
        assert!(xing.lame.is_none());
    }

    #[test]
    fn unrelated_bytes_are_not_a_xing_header() {
        assert!(XingHeader::parse(b"junk").is_none());
    }
}
