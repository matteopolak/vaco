//! The Fraunhofer VBRI header.
//!
//! Unlike Xing/Info, VBRI sits at a **fixed** offset from the frame's own
//! start regardless of channel mode or side-info size, because it does not
//! reuse the side-info gap the way Xing does.
//!
//! No VBRI-writing encoder was available to generate a fixture, so this is
//! transcribed from the format's public documentation rather than confirmed
//! against a real file the way [`crate::xing`] and [`crate::lame`] were.

/// Byte offset of the `"VBRI"` tag from the start of the frame containing it.
pub const FRAME_OFFSET: usize = 36;

#[derive(Debug, Clone)]
pub struct VbriHeader {
    pub version: u16,
    pub quality: u16,
    pub num_bytes: u32,
    pub num_frames: u32,
    pub toc: Vec<u32>,
    pub toc_frames_per_entry: u16,
}

impl VbriHeader {
    /// `data` starts at the tag's own `"VBRI"` magic.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.first_chunk::<4>()? != b"VBRI" {
            return None;
        }
        let version = read_be16(data, 4)?;
        let quality = read_be16(data, 8)?;
        let num_bytes = read_be32(data, 10)?;
        let num_frames = read_be32(data, 14)?;
        let toc_entries = read_be16(data, 18)?;
        let toc_scale = u32::from(read_be16(data, 20)?);
        let toc_entry_bytes = read_be16(data, 22)?;
        let toc_frames_per_entry = read_be16(data, 24)?;

        let mut toc = Vec::new();
        let mut off = 26usize;
        for _ in 0..toc_entries {
            let raw = match toc_entry_bytes {
                2 => u32::from(read_be16(data, off)?),
                4 => read_be32(data, off)?,
                _ => return None,
            };
            toc.push(raw.saturating_mul(toc_scale));
            off = off.saturating_add(usize::from(toc_entry_bytes));
        }

        Some(Self {
            version,
            quality,
            num_bytes,
            num_frames,
            toc,
            toc_frames_per_entry,
        })
    }
}

fn read_be16(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at.saturating_add(2))
        .and_then(|s| s.first_chunk::<2>())
        .map(|c| u16::from_be_bytes(*c))
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

    #[test]
    fn a_synthetic_two_entry_toc_round_trips() {
        let mut data = vec![0u8; 26];
        data[..4].copy_from_slice(b"VBRI");
        data[4..6].copy_from_slice(&1u16.to_be_bytes());
        data[10..14].copy_from_slice(&1000u32.to_be_bytes());
        data[14..18].copy_from_slice(&10u32.to_be_bytes());
        data[18..20].copy_from_slice(&2u16.to_be_bytes());
        data[20..22].copy_from_slice(&1u16.to_be_bytes());
        data[22..24].copy_from_slice(&2u16.to_be_bytes());
        data[24..26].copy_from_slice(&5u16.to_be_bytes());
        data.extend_from_slice(&100u16.to_be_bytes());
        data.extend_from_slice(&200u16.to_be_bytes());

        let vbri = VbriHeader::parse(&data).expect("valid VBRI header");
        assert_eq!(vbri.num_bytes, 1000);
        assert_eq!(vbri.num_frames, 10);
        assert_eq!(vbri.toc, vec![100, 200]);
    }

    #[test]
    fn wrong_magic_is_rejected() {
        assert!(VbriHeader::parse(b"nope").is_none());
    }
}
