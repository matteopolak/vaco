//! The LAME/Lavc extension appended after a full Xing/Info header.
//!
//! Layout and the delay/padding bit-packing were confirmed byte-for-byte
//! against `ffmpeg -c:a libmp3lame`'s own output (its encoder-id string reads
//! `"Lavc"`, not `"LAME"`, when `ffmpeg`'s own wrapper writes it) before this
//! was written, not recalled from memory.

/// Byte offset of the 3-byte encoder delay/padding field from the start of
/// the extension. Confirmed against a real `ffmpeg -c:a libmp3lame` file.
const DELAY_PADDING_OFFSET: usize = 21;

/// Total extension length, for a caller computing how much of the frame the
/// Xing+LAME block occupies.
pub const LEN: usize = 36;

/// The fields gapless playback needs: encoder priming samples to drop from
/// the front, and padding samples to drop from the back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LameTag {
    pub encoder: [u8; 9],
    pub encoder_delay: u16,
    pub encoder_padding: u16,
}

impl LameTag {
    /// `data` starts right after the Xing/Info header's last field (quality).
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        let encoder = *data.first_chunk::<9>()?;
        let region = data.get(..DELAY_PADDING_OFFSET.saturating_add(3))?;
        let b0 = *region.get(DELAY_PADDING_OFFSET)?;
        let b1 = *region.get(DELAY_PADDING_OFFSET.saturating_add(1))?;
        let b2 = *region.get(DELAY_PADDING_OFFSET.saturating_add(2))?;
        let encoder_delay = (u16::from(b0) << 4) | (u16::from(b1) >> 4);
        let encoder_padding = (u16::from(b1 & 0x0F) << 8) | u16::from(b2);
        Some(Self {
            encoder,
            encoder_delay,
            encoder_padding,
        })
    }
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

    /// Bytes 0..27 of the LAME extension exactly as read from a real
    /// `ffmpeg -c:a libmp3lame -q:a 4` file's first frame.
    const MEASURED_LAME_PREFIX: [u8; 27] = [
        0x4c, 0x61, 0x76, 0x63, 0x36, 0x32, 0x2e, 0x32, 0x38, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x24, 0x04, 0x38, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn measured_lavc_tag_decodes_delay_and_padding() {
        let mut data = [0u8; LEN];
        data[..27].copy_from_slice(&MEASURED_LAME_PREFIX);
        let tag = LameTag::parse(&data).expect("valid tag");
        assert_eq!(&tag.encoder, b"Lavc62.28");
        assert_eq!(tag.encoder_delay, 576);
        assert_eq!(tag.encoder_padding, 1080);
    }
}
