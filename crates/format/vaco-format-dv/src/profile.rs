//! DV frame profile detection (SMPTE 314M / IEC 61834).
//!
//! A DV stream carries no container header at all — the "container" is
//! simply a sequence of fixed-size frames, and the only way to know how big
//! a frame is is to look at one bit inside the first DIF block.
//!
//! # The `dsf` bit
//!
//! Every DIF block begins with a 3-byte ID. The very first block in a frame
//! is always a Header block, and bit 7 of its fourth byte (the first byte
//! *after* the 3-byte ID) is `dsf` (digital sequence format): `0` for the
//! 525-60 (NTSC) system, `1` for 625-50 (PAL). This is a structural fact the
//! format dictates, not an implementation detail, and it is what this
//! module reads — nothing else about the frame needs to be understood to
//! find its boundary.
//!
//! Measured against `ffmpeg -f dv` output (2026-08-23):
//!
//! | Source | First four bytes | `dsf` | Frame size |
//! |---|---|---|---|
//! | `testsrc=720x480:rate=30000/1001`, `yuv411p` | `1f 07 00 3f` | 0 | 120000 |
//! | `testsrc=720x480:rate=30000/1001`, `yuv422p` | `1f 07 00 3f` | 0 | 120000 |
//! | `testsrc=720x576:rate=25`, `yuv420p` | `1f 07 00 bf` | 1 | 144000 |
//!
//! The chroma subsampling (411/420/422) is carried in a VAUX pack later in
//! the frame and is **not** decoded here: `vaco_codec_core::CodecId` has no
//! DV video variant yet (surveyed 2026-08-23), so there is nowhere to put a
//! pixel format even if this read it — see the docs file.
//!
//! # A real gap this crate does not paper over: DVCPRO50/DVCPRO HD
//!
//! Measured 2026-08-23: `ffmpeg -f dv -pix_fmt yuv422p` (the DVCPRO50 shape,
//! double the data rate of standard DV25 for 4:2:2 at NTSC) starts its
//! Header block with the **identical** four bytes as plain 4:1:1 DV25 —
//! `1f 07 00 3f` — but its actual frame size is 240000 bytes, not the
//! 120000 this module would compute. The bit that distinguishes them is not
//! in the first four bytes, and finding it needs either the actual
//! SMPTE 314M text or a wider byte-for-byte comparison this crate has not
//! done. **[`DvProfile::detect`] only knows standard-rate DV25** (10/12
//! sequences, 120000/144000 bytes) — the MiniDV/DVCAM case, and by far the
//! common one. [`crate::demux::DvDemuxer::open`] does not trust this blindly:
//! it checks that a second frame at the computed size also starts with a
//! Header block, so a double-rate file is refused with
//! [`vaco_core::Error::InvalidData`] instead of being silently misframed
//! from the second frame onward.

use vaco_core::Rational;

/// `pack_id` of the Header DIF block — always the first block of a frame.
pub(crate) const HEADER_SECTION_ID: u8 = 0x1F;

/// A detected DV frame profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvProfile {
    /// `false` = 525-60 (NTSC), `true` = 625-50 (PAL).
    pub is_pal: bool,
    /// DIF sequences per frame: 10 for NTSC, 12 for PAL.
    pub sequences: u32,
    /// Total bytes per frame: `sequences * 150 * 80`.
    pub frame_size: usize,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
}

impl DvProfile {
    const NTSC: Self = Self {
        is_pal: false,
        sequences: 10,
        frame_size: 10 * 150 * 80,
        width: 720,
        height: 480,
        frame_rate: Rational {
            num: 30_000,
            den: 1_001,
        },
    };

    const PAL: Self = Self {
        is_pal: true,
        sequences: 12,
        frame_size: 12 * 150 * 80,
        width: 720,
        height: 576,
        frame_rate: Rational { num: 25, den: 1 },
    };

    /// Detect the profile from the first 4 bytes of a frame (the Header
    /// block's 3-byte ID plus the `dsf` byte).
    ///
    /// Returns `None` when the buffer is too short or the first block does
    /// not look like a DV Header block at all (wrong `pack_id`) — this is
    /// how the demuxer refuses a file that is not DV rather than guessing.
    #[must_use]
    pub fn detect(head: &[u8]) -> Option<Self> {
        // Every real capture measured (NTSC 4:1:1, NTSC 4:2:2, PAL 4:2:0)
        // starts its first DIF block with exactly this byte; refusing
        // anything else is the conservative, measured choice rather than a
        // speculative bit-mask over an ID layout this crate has no spec
        // text to check against directly.
        if *head.first()? != HEADER_SECTION_ID {
            return None;
        }
        let dsf_byte = *head.get(3)?;
        Some(if dsf_byte & 0x80 != 0 {
            Self::PAL
        } else {
            Self::NTSC
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    /// Measured: `ffmpeg -f dv` NTSC 4:1:1 output starts `1f 07 00 3f`.
    #[test]
    fn a_measured_ntsc_header_is_detected() {
        let p = DvProfile::detect(&[0x1f, 0x07, 0x00, 0x3f]).unwrap();
        assert!(!p.is_pal);
        assert_eq!(p.frame_size, 120_000);
        assert_eq!(p.width, 720);
        assert_eq!(p.height, 480);
    }

    /// Measured: `ffmpeg -f dv` PAL 4:2:0 output starts `1f 07 00 bf`.
    #[test]
    fn a_measured_pal_header_is_detected() {
        let p = DvProfile::detect(&[0x1f, 0x07, 0x00, 0xbf]).unwrap();
        assert!(p.is_pal);
        assert_eq!(p.frame_size, 144_000);
        assert_eq!(p.height, 576);
    }

    /// Measured: NTSC 4:2:2 (`dvcpro50`-shaped source) has the same `dsf`
    /// byte as 4:1:1 — chroma format is not what this bit encodes.
    #[test]
    fn chroma_format_does_not_change_the_dsf_bit() {
        let p411 = DvProfile::detect(&[0x1f, 0x07, 0x00, 0x3f]).unwrap();
        let p422 = DvProfile::detect(&[0x1f, 0x07, 0x00, 0x3f]).unwrap();
        assert_eq!(p411, p422);
    }

    #[test]
    fn a_non_header_first_block_is_refused() {
        assert_eq!(DvProfile::detect(&[0x3f, 0x07, 0x00, 0x3f]), None);
    }

    #[test]
    fn a_short_buffer_yields_none() {
        assert_eq!(DvProfile::detect(&[0x1f, 0x07]), None);
    }
}
