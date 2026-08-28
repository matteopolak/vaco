//! Marker byte values (ITU-T T.81 Table B.1).
//!
//! `Vaco-Spec-Ref: itu-t-t81-199209`.

pub(crate) const SOI: u8 = 0xD8;
pub(crate) const EOI: u8 = 0xD9;

pub(crate) const SOF0: u8 = 0xC0;
pub(crate) const SOF2: u8 = 0xC2;
pub(crate) const SOF3: u8 = 0xC3;
pub(crate) const DHT: u8 = 0xC4;
pub(crate) const SOF7: u8 = 0xC7;
pub(crate) const SOF9: u8 = 0xC9;
pub(crate) const SOF10: u8 = 0xCA;
pub(crate) const SOF11: u8 = 0xCB;
pub(crate) const DAC: u8 = 0xCC;
pub(crate) const SOF13: u8 = 0xCD;
pub(crate) const SOF14: u8 = 0xCE;
pub(crate) const SOF15: u8 = 0xCF;

pub(crate) const RST0: u8 = 0xD0;
pub(crate) const RST7: u8 = 0xD7;

pub(crate) const SOS: u8 = 0xDA;
pub(crate) const DQT: u8 = 0xDB;
pub(crate) const DRI: u8 = 0xDD;

pub(crate) const APP0: u8 = 0xE0;
pub(crate) const APP14: u8 = 0xEE;

/// Whether `marker` carries no length-prefixed payload at all: `SOI`, `EOI`,
/// `TEM` (`0x01`), the restart markers, and the reserved `0x02..=0xBF`
/// range.
#[must_use]
pub(crate) const fn has_no_payload(marker: u8) -> bool {
    matches!(marker, SOI | EOI | 0x01 | RST0..=RST7) || matches!(marker, 0x02..=0xBF)
}

/// Every `SOF` marker byte (Table B.1) except `DHT`, the reserved `JPG`
/// (`0xC8`) and `DAC`.
#[must_use]
pub(crate) const fn is_sof(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xCF) && !matches!(marker, DHT | 0xC8 | DAC)
}

/// `SOF` markers using arithmetic entropy coding (Annex D) rather than
/// Huffman coding — out of scope: see the crate docs.
#[must_use]
pub(crate) const fn is_arithmetic_sof(marker: u8) -> bool {
    matches!(marker, SOF9 | SOF10 | SOF11 | SOF13 | SOF14 | SOF15)
}

/// Lossless `SOF` markers (Annex H) — out of scope: this crate decodes
/// DCT-based JPEG only.
#[must_use]
pub(crate) const fn is_lossless_sof(marker: u8) -> bool {
    matches!(marker, SOF3 | SOF7 | SOF11)
}

/// Progressive `SOF` markers (Annex G).
#[must_use]
pub(crate) const fn is_progressive_sof(marker: u8) -> bool {
    matches!(marker, SOF2 | SOF10)
}
