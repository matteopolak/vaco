//! The packed language code of `mdhd` and the 3GPP `udta` boxes.
//!
//! ISO/IEC 14496-12 §8.4.2.3: the 16-bit field is one pad bit followed by three
//! 5-bit values, each the ISO-639-2/T letter minus `0x60`. So `und` packs as
//! `0x55C4`.
//!
//! Two values mean "unspecified" and both occur in the wild: literal zero, and
//! the packed form of `und`. A field whose **top bit is set** is not a packed
//! ISO-639 code at all — it is a classic Macintosh language code, and `QuickTime`
//! files from before 2005 are full of them.

/// A media language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// A three-letter ISO-639-2/T code.
    Iso639([u8; 3]),
    /// A Macintosh language code, which the packed field cannot express.
    ///
    /// Kept as the raw value rather than mapped: the legacy table is large,
    /// only partially agreed on between sources, and the demuxer is where the
    /// policy for an unmappable code belongs.
    Macintosh(u16),
    /// Unspecified — zero, or `und`. The default, because that is what a track
    /// with no `mdhd` at all reports.
    #[default]
    Undefined,
}

/// The packed value the reference writes for an unspecified language.
pub const PACKED_UND: u16 = 0x55C4;

impl Language {
    /// Unpack an `mdhd`/`udta` language field.
    #[must_use]
    pub fn unpack(packed: u16) -> Self {
        if packed == 0 || packed == PACKED_UND {
            return Self::Undefined;
        }
        if packed & 0x8000 != 0 {
            return Self::Macintosh(packed);
        }
        let mut out = [0u8; 3];
        for (i, slot) in out.iter_mut().enumerate() {
            let shift = 10u16.saturating_sub(5u16.saturating_mul(i as u16));
            let v = ((packed >> shift) & 0x1F) as u8;
            let ch = v.saturating_add(0x60);
            if !ch.is_ascii_lowercase() {
                // A component outside a-z means this was never a packed
                // ISO-639 code; do not invent letters for it.
                return Self::Macintosh(packed);
            }
            *slot = ch;
        }
        Self::Iso639(out)
    }

    /// Pack an ISO-639-2/T code, or [`PACKED_UND`] for anything else.
    #[must_use]
    pub fn pack(self) -> u16 {
        match self {
            Self::Iso639(c) => {
                let mut out = 0u16;
                for ch in c {
                    let v = u16::from(ch.saturating_sub(0x60)) & 0x1F;
                    out = (out << 5) | v;
                }
                out
            }
            Self::Macintosh(v) => v,
            Self::Undefined => PACKED_UND,
        }
    }

    /// The three-letter code, when there is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Iso639(c) => core::str::from_utf8(c).ok(),
            _ => None,
        }
    }

    /// The value `ffprobe` prints for the `language` stream tag.
    ///
    /// Unspecified prints as `und`, which is why [`Language::Undefined`] does
    /// not simply produce nothing.
    #[must_use]
    pub fn tag(&self) -> &str {
        match self {
            Self::Iso639(c) => core::str::from_utf8(c).unwrap_or("und"),
            _ => "und",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn eng_round_trips() {
        let l = Language::unpack(0x15C7);
        assert_eq!(l, Language::Iso639(*b"eng"));
        assert_eq!(l.pack(), 0x15C7);
        assert_eq!(l.as_str(), Some("eng"));
    }

    #[test]
    fn und_and_zero_are_both_unspecified() {
        assert_eq!(Language::unpack(PACKED_UND), Language::Undefined);
        assert_eq!(Language::unpack(0), Language::Undefined);
        assert_eq!(Language::Undefined.tag(), "und");
        assert_eq!(Language::Undefined.pack(), PACKED_UND);
    }

    #[test]
    fn the_top_bit_marks_a_macintosh_code() {
        assert_eq!(Language::unpack(0x8000), Language::Macintosh(0x8000));
        assert_eq!(Language::unpack(0x8000).tag(), "und");
        assert_eq!(Language::unpack(0x8000).as_str(), None);
        assert_eq!(Language::Macintosh(0x8123).pack(), 0x8123);
    }

    #[test]
    fn a_component_outside_a_to_z_is_not_iso639() {
        // 0x0000 is caught as undefined, so use a value whose middle component
        // decodes to `0x60`, which is not a letter.
        let packed = 0b0_00001_00000_00001u16;
        assert!(matches!(Language::unpack(packed), Language::Macintosh(_)));
    }

    #[test]
    fn every_three_letter_code_round_trips() {
        for a in b'a'..=b'z' {
            for b in b'a'..=b'z' {
                for c in b'a'..=b'z' {
                    let l = Language::Iso639([a, b, c]);
                    let packed = l.pack();
                    if packed == PACKED_UND || packed == 0 {
                        continue;
                    }
                    assert_eq!(Language::unpack(packed), l, "{a} {b} {c}");
                }
            }
        }
    }
}
