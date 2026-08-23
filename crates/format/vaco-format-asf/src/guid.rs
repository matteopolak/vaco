//! The 128-bit GUID that tags every ASF object, and the well-known constants
//! [\[ASF\] §10](crate) defines for them.
//!
//! # Binary layout
//!
//! [\[ASF\] §2.1](crate) states that "all ASF objects and structures … are
//! stored in little-endian byte order". A GUID is the classic Microsoft
//! `Data1:u32(LE) Data2:u16(LE) Data3:u16(LE) Data4:u8[8]` layout — the last
//! eight bytes are **not** byte-swapped, which is why the canonical text form
//! `AABBCCDD-EEFF-GGHH-IIJJ-KKLLMMNNOOPP` reads `Data4` left-to-right exactly
//! as the bytes appear on disk while `Data1..Data3` are byte-reversed. Every
//! constant below was transcribed from the numbered GUID tables in [\[ASF\]
//! §10](crate) (revision 01.20.06); see `docs/format/vaco-format-asf.md` for
//! the exact source used.
use core::fmt;

/// A 128-bit ASF object identifier, stored exactly as it appears on disk (16
/// bytes, little-endian `Data1`/`Data2`/`Data3` followed by `Data4` verbatim).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    /// Bytes a GUID occupies on disk.
    pub const LEN: usize = 16;

    /// Wrap raw file-order bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Read the first 16 bytes of `data` as a GUID. `None` if `data` is
    /// shorter than [`Guid::LEN`].
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        data.first_chunk::<16>().copied().map(Self)
    }

    /// The raw file-order bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Build a GUID from its four canonical fields, e.g. as printed in
    /// `AABBCCDD-EEFF-GGHH-IIJJ-KKLLMMNNOOPP` form: `d1` is the first group,
    /// `d2`/`d3` the next two, `d4` the last two groups concatenated into
    /// eight bytes.
    #[must_use]
    pub const fn from_fields(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Self {
        let a = d1.to_le_bytes();
        let b = d2.to_le_bytes();
        let c = d3.to_le_bytes();
        Self([
            a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4],
            d4[5], d4[6], d4[7],
        ])
    }

    /// The well-known name for this GUID, if [`crate::well_known`] has one.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        crate::well_known::name_of(self)
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.name() {
            write!(f, "{name} ({self})")
        } else {
            write!(f, "{self}")
        }
    }
}

impl fmt::Display for Guid {
    /// The canonical `AABBCCDD-EEFF-GGHH-IIJJ-KKLLMMNNOOPP` text form,
    /// upper-case (matching Windows' own `%GUID%` formatting and every ASF
    /// GUID table this crate transcribes).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            b[3],
            b[2],
            b[1],
            b[0],
            b[5],
            b[4],
            b[7],
            b[6],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn from_fields_matches_the_documented_byte_order() {
        // ASF_Header_Object: 75B22630-668E-11CF-A6D9-00AA0062CE6C, measured
        // byte-for-byte against the spec's own GUID table (see
        // crate::well_known).
        let g = Guid::from_fields(
            0x75B2_2630,
            0x668E,
            0x11CF,
            [0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C],
        );
        assert_eq!(
            g.as_bytes(),
            [
                0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62,
                0xCE, 0x6C
            ]
        );
    }

    #[test]
    fn display_round_trips_the_canonical_text_form() {
        let g = Guid::from_fields(
            0x75B2_2630,
            0x668E,
            0x11CF,
            [0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C],
        );
        assert_eq!(g.to_string(), "75B22630-668E-11CF-A6D9-00AA0062CE6C");
    }

    #[test]
    fn parse_reads_the_first_sixteen_bytes_only() {
        let mut data = vec![0xAAu8; 16];
        data.extend_from_slice(b"trailing");
        let g = Guid::parse(&data).unwrap();
        assert_eq!(g.as_bytes(), [0xAA; 16]);
    }

    #[test]
    fn parse_rejects_a_short_buffer() {
        assert!(Guid::parse(&[0; 15]).is_none());
    }
}
