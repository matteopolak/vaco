//! Descriptors (ISO/IEC 13818-1 §2.6, ETSI EN 300 468 §6, ATSC A/65 §6).
//!
//! A descriptor loop is a `tag, length, payload` sequence. The generic shape is
//! parsed here; the handful whose contents actually change what a demuxer
//! reports get typed accessors, and the rest stay bytes.
//!
//! # The rule that decides what gets a parser
//!
//! A descriptor earns a typed accessor when it changes an *observable* field:
//! the codec a `stream_type` resolves to, a stream's language, a disposition
//! flag, or — for teletext and DVB subtitling — **how many streams one PID
//! produces**. Everything else is left as `data`, because parsing a structure
//! nothing reads is attack surface bought with no output.

/// `registration_descriptor`, 13818-1 §2.6.8. Carries a four-character
/// format identifier which overrides an ambiguous `stream_type`.
pub const TAG_REGISTRATION: u8 = 0x05;
/// `CA_descriptor`, §2.6.16.
pub const TAG_CA: u8 = 0x09;
/// `ISO_639_language_descriptor`, §2.6.18.
pub const TAG_ISO639_LANGUAGE: u8 = 0x0A;
/// `maximum_bitrate_descriptor`, §2.6.26.
pub const TAG_MAXIMUM_BITRATE: u8 = 0x0E;
/// `AVC_video_descriptor`, §2.6.64.
pub const TAG_AVC_VIDEO: u8 = 0x28;
/// `SVC_extension_descriptor`, §2.6.76.
pub const TAG_SVC_EXTENSION: u8 = 0x30;
/// `HEVC_video_descriptor`, §2.6.95.
pub const TAG_HEVC_VIDEO: u8 = 0x38;
/// `extension_descriptor` in the 13818-1 range, §2.6.90.
pub const TAG_EXTENSION: u8 = 0x3F;

// --- DVB, EN 300 468 §6.1 -------------------------------------------------

/// `network_name_descriptor`.
pub const TAG_NETWORK_NAME: u8 = 0x40;
/// `service_descriptor`: the service type, provider name and service name an
/// SDT carries.
pub const TAG_SERVICE: u8 = 0x48;
/// `stream_identifier_descriptor`: the `component_tag`.
pub const TAG_STREAM_IDENTIFIER: u8 = 0x52;
/// `teletext_descriptor`. One descriptor declares several logical pages.
pub const TAG_TELETEXT: u8 = 0x56;
/// `subtitling_descriptor`. Likewise several logical subtitle streams.
pub const TAG_SUBTITLING: u8 = 0x59;
/// `AC-3_descriptor`.
pub const TAG_DVB_AC3: u8 = 0x6A;
/// `VBI_teletext_descriptor`, same body shape as `teletext_descriptor`.
pub const TAG_VBI_TELETEXT: u8 = 0x46;
/// `enhanced_AC-3_descriptor`.
pub const TAG_DVB_EAC3: u8 = 0x7A;
/// `DTS_descriptor`.
pub const TAG_DVB_DTS: u8 = 0x7B;
/// `AAC_descriptor`.
pub const TAG_DVB_AAC: u8 = 0x7C;
/// DVB `extension_descriptor`; the first payload byte selects the extension.
pub const TAG_DVB_EXTENSION: u8 = 0x7F;

/// DVB extension tag for `supplementary_audio_descriptor`.
pub const EXT_SUPPLEMENTARY_AUDIO: u8 = 0x06;

// --- ATSC / private range -------------------------------------------------

/// ATSC A/52 `AC-3_audio_descriptor`, in the user-private range.
pub const TAG_ATSC_AC3: u8 = 0x81;
/// ATSC A/65 `caption_service_descriptor`.
pub const TAG_ATSC_CAPTION_SERVICE: u8 = 0x86;
/// ATSC `enhanced_AC-3_audio_descriptor`.
pub const TAG_ATSC_EAC3: u8 = 0xCC;

/// One `tag, length, payload` triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor<'a> {
    pub tag: u8,
    pub data: &'a [u8],
}

impl<'a> Descriptor<'a> {
    /// The four-character format identifier of a `registration_descriptor`.
    ///
    /// This is the field that turns `stream_type 0x06` — "PES packets
    /// containing private data", which is to say "anything" — into a codec.
    #[must_use]
    pub fn registration_format(&self) -> Option<[u8; 4]> {
        if self.tag != TAG_REGISTRATION {
            return None;
        }
        let b = self.data.get(..4)?;
        Some([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?])
    }

    /// The DVB `extension_descriptor_tag`, for tag `0x7F`.
    #[must_use]
    pub fn dvb_extension_tag(&self) -> Option<u8> {
        if self.tag != TAG_DVB_EXTENSION {
            return None;
        }
        self.data.first().copied()
    }

    /// `ISO_639_language_descriptor` entries.
    ///
    /// Several are permitted in one descriptor. The first is the one a
    /// demuxer reports as the stream's `language` tag.
    #[must_use]
    pub fn iso639_languages(&self) -> Iso639Iter<'a> {
        Iso639Iter {
            rest: if self.tag == TAG_ISO639_LANGUAGE {
                self.data
            } else {
                &[]
            },
        }
    }

    /// `teletext_descriptor` / `VBI_teletext_descriptor` entries.
    #[must_use]
    pub fn teletext_pages(&self) -> TeletextIter<'a> {
        TeletextIter {
            rest: if self.tag == TAG_TELETEXT || self.tag == TAG_VBI_TELETEXT {
                self.data
            } else {
                &[]
            },
        }
    }

    /// `subtitling_descriptor` entries.
    #[must_use]
    pub fn subtitling_entries(&self) -> SubtitlingIter<'a> {
        SubtitlingIter {
            rest: if self.tag == TAG_SUBTITLING {
                self.data
            } else {
                &[]
            },
        }
    }

    /// `maximum_bitrate_descriptor`, in bits per second.
    ///
    /// The field counts 50-byte-per-second units, so the conversion is
    /// `value * 50 * 8`.
    #[must_use]
    pub fn maximum_bitrate(&self) -> Option<u64> {
        if self.tag != TAG_MAXIMUM_BITRATE {
            return None;
        }
        let b = self.data.get(..3)?;
        let raw = (u64::from(*b.first()? & 0x3F) << 16)
            | (u64::from(*b.get(1)?) << 8)
            | u64::from(*b.get(2)?);
        Some(raw.saturating_mul(400))
    }

    /// `service_descriptor`: `(service_type, provider_name, service_name)`.
    ///
    /// Both names are DVB-encoded text; see [`crate::text::decode`].
    #[must_use]
    pub fn service(&self) -> Option<ServiceDescriptor<'a>> {
        if self.tag != TAG_SERVICE {
            return None;
        }
        let service_type = *self.data.first()?;
        let provider_len = usize::from(*self.data.get(1)?);
        let provider_end = 2usize.checked_add(provider_len)?;
        let provider = self.data.get(2..provider_end)?;
        let name_len = usize::from(*self.data.get(provider_end)?);
        let name_start = provider_end.checked_add(1)?;
        let name = self
            .data
            .get(name_start..name_start.checked_add(name_len)?)?;
        Some(ServiceDescriptor {
            service_type,
            provider,
            name,
        })
    }

    /// `stream_identifier_descriptor`'s `component_tag`.
    #[must_use]
    pub fn component_tag(&self) -> Option<u8> {
        if self.tag != TAG_STREAM_IDENTIFIER {
            return None;
        }
        self.data.first().copied()
    }
}

/// A DVB `service_descriptor`, names still in their transmitted encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceDescriptor<'a> {
    /// EN 300 468 Table 87: `0x01` digital TV, `0x02` digital radio, and so on.
    pub service_type: u8,
    pub provider: &'a [u8],
    pub name: &'a [u8],
}

/// One `ISO_639_language_descriptor` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Iso639Entry {
    /// Three-character ISO 639-2 code, as transmitted.
    pub language: [u8; 3],
    /// `0` undefined, `1` clean effects, `2` hearing impaired,
    /// `3` visual impaired commentary.
    pub audio_type: u8,
}

impl Iso639Entry {
    /// The language code as text, or `None` when it is not three printable
    /// ASCII letters.
    ///
    /// Rejecting anything else matters: an all-zero code is common padding and
    /// reporting `"\0\0\0"` as a language would put three NULs into
    /// `vaco-probe`'s output.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        if self.language.iter().any(|b| !b.is_ascii_alphabetic()) {
            return None;
        }
        core::str::from_utf8(&self.language).ok()
    }
}

/// Iterator over `ISO_639_language_descriptor` entries.
#[derive(Debug, Clone)]
pub struct Iso639Iter<'a> {
    rest: &'a [u8],
}

impl Iterator for Iso639Iter<'_> {
    type Item = Iso639Entry;

    fn next(&mut self) -> Option<Self::Item> {
        let e = self.rest.get(..4)?;
        let entry = Iso639Entry {
            language: [*e.first()?, *e.get(1)?, *e.get(2)?],
            audio_type: *e.get(3)?,
        };
        self.rest = self.rest.get(4..).unwrap_or(&[]);
        Some(entry)
    }
}

/// One page of a `teletext_descriptor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeletextEntry {
    pub language: [u8; 3],
    /// `1` initial page, `2` subtitle, `3` additional information,
    /// `4` programme schedule, `5` subtitle for the hearing impaired.
    pub teletext_type: u8,
    pub magazine: u8,
    /// Two BCD digits, as transmitted.
    pub page_number: u8,
}

impl TeletextEntry {
    /// The page as broadcasters write it: magazine 1 page 0x88 is 188.
    ///
    /// Magazine zero means 8, which is the convention the numbering has always
    /// used and the reason this is not a plain concatenation.
    #[must_use]
    pub const fn page(&self) -> u32 {
        let magazine = if self.magazine == 0 { 8 } else { self.magazine };
        (magazine as u32)
            .saturating_mul(100)
            .saturating_add(((self.page_number >> 4) & 0x0F) as u32 * 10)
            .saturating_add((self.page_number & 0x0F) as u32)
    }

    /// Whether this page is a subtitle page rather than a data page.
    #[must_use]
    pub const fn is_subtitle(&self) -> bool {
        self.teletext_type == 2 || self.teletext_type == 5
    }

    /// Whether this page is meant for the hearing impaired.
    #[must_use]
    pub const fn is_hearing_impaired(&self) -> bool {
        self.teletext_type == 5
    }

    /// The language code as text, when it is three ASCII letters.
    #[must_use]
    pub fn language_str(&self) -> Option<&str> {
        if self.language.iter().any(|b| !b.is_ascii_alphabetic()) {
            return None;
        }
        core::str::from_utf8(&self.language).ok()
    }
}

/// Iterator over `teletext_descriptor` pages.
#[derive(Debug, Clone)]
pub struct TeletextIter<'a> {
    rest: &'a [u8],
}

impl Iterator for TeletextIter<'_> {
    type Item = TeletextEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let e = self.rest.get(..5)?;
        let flags = *e.get(3)?;
        let entry = TeletextEntry {
            language: [*e.first()?, *e.get(1)?, *e.get(2)?],
            teletext_type: flags >> 3,
            magazine: flags & 0x07,
            page_number: *e.get(4)?,
        };
        self.rest = self.rest.get(5..).unwrap_or(&[]);
        Some(entry)
    }
}

/// One entry of a `subtitling_descriptor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtitlingEntry {
    pub language: [u8; 3],
    /// EN 300 468 Table 90. `0x20`+ are DVB subtitles; `0x10`-`0x14` and
    /// `0x20`-`0x24` differ in aspect ratio, `0x24`/`0x14` and the `0x2x`
    /// hearing-impaired variants set the disposition.
    pub subtitling_type: u8,
    pub composition_page_id: u16,
    pub ancillary_page_id: u16,
}

impl SubtitlingEntry {
    /// Whether this entry declares subtitles for the hard of hearing.
    ///
    /// Table 90 puts the hard-of-hearing variants at `0x20`-`0x24`, mirroring
    /// the ordinary `0x10`-`0x14`.
    #[must_use]
    pub const fn is_hearing_impaired(&self) -> bool {
        self.subtitling_type >= 0x20 && self.subtitling_type <= 0x24
    }

    /// The language code as text, when it is three ASCII letters.
    #[must_use]
    pub fn language_str(&self) -> Option<&str> {
        if self.language.iter().any(|b| !b.is_ascii_alphabetic()) {
            return None;
        }
        core::str::from_utf8(&self.language).ok()
    }
}

/// Iterator over `subtitling_descriptor` entries.
#[derive(Debug, Clone)]
pub struct SubtitlingIter<'a> {
    rest: &'a [u8],
}

impl Iterator for SubtitlingIter<'_> {
    type Item = SubtitlingEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let e = self.rest.get(..8)?;
        let entry = SubtitlingEntry {
            language: [*e.first()?, *e.get(1)?, *e.get(2)?],
            subtitling_type: *e.get(3)?,
            composition_page_id: u16::from_be_bytes([*e.get(4)?, *e.get(5)?]),
            ancillary_page_id: u16::from_be_bytes([*e.get(6)?, *e.get(7)?]),
        };
        self.rest = self.rest.get(8..).unwrap_or(&[]);
        Some(entry)
    }
}

/// Walk a descriptor loop.
///
/// Stops at the first truncated descriptor rather than trying to resynchronise:
/// a descriptor loop has no self-synchronising structure, so once the length
/// bytes stop lining up nothing after them means anything.
#[derive(Debug, Clone)]
pub struct DescriptorIter<'a> {
    rest: &'a [u8],
}

impl<'a> DescriptorIter<'a> {
    /// Iterate the descriptors in `data`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { rest: data }
    }

    /// The first descriptor with `tag`.
    #[must_use]
    pub fn find_tag(&self, tag: u8) -> Option<Descriptor<'a>> {
        self.clone().find(|d| d.tag == tag)
    }

    /// Whether any descriptor carries `tag`.
    #[must_use]
    pub fn has_tag(&self, tag: u8) -> bool {
        self.find_tag(tag).is_some()
    }

    /// The four-character identifier of the first `registration_descriptor`.
    #[must_use]
    pub fn registration(&self) -> Option<[u8; 4]> {
        self.clone().find_map(|d| d.registration_format())
    }
}

impl<'a> Iterator for DescriptorIter<'a> {
    type Item = Descriptor<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let tag = *self.rest.first()?;
        let len = usize::from(*self.rest.get(1)?);
        let end = 2usize.checked_add(len)?;
        let data = self.rest.get(2..end)?;
        self.rest = self.rest.get(end..).unwrap_or(&[]);
        Some(Descriptor { tag, data })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn walks_a_loop_of_three() {
        let data = [
            0x05, 4, b'A', b'C', b'-', b'3', 0x0A, 4, b'e', b'n', b'g', 0, 0x52, 1, 7,
        ];
        let tags: Vec<u8> = DescriptorIter::new(&data).map(|d| d.tag).collect();
        assert_eq!(tags, vec![0x05, 0x0A, 0x52]);
        assert_eq!(DescriptorIter::new(&data).registration(), Some(*b"AC-3"));
        assert_eq!(
            DescriptorIter::new(&data)
                .find_tag(TAG_STREAM_IDENTIFIER)
                .unwrap()
                .component_tag(),
            Some(7)
        );
    }

    #[test]
    fn a_truncated_descriptor_ends_the_walk() {
        let data = [0x05, 4, b'A', b'C'];
        assert_eq!(DescriptorIter::new(&data).count(), 0);
        let data = [0x52, 1, 7, 0x05, 40, 1];
        assert_eq!(DescriptorIter::new(&data).count(), 1);
    }

    #[test]
    fn empty_and_odd_inputs_terminate() {
        assert_eq!(DescriptorIter::new(&[]).count(), 0);
        assert_eq!(DescriptorIter::new(&[0x05]).count(), 0);
        // A zero-length descriptor is legal and must not stall the iterator.
        assert_eq!(DescriptorIter::new(&[0x05, 0, 0x06, 0]).count(), 2);
    }

    #[test]
    fn language_entries() {
        let d = Descriptor {
            tag: TAG_ISO639_LANGUAGE,
            data: b"eng\x00fra\x02",
        };
        let v: Vec<_> = d.iso639_languages().collect();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].as_str(), Some("eng"));
        assert_eq!(v[1].audio_type, 2);
    }

    #[test]
    fn a_padded_language_code_is_rejected() {
        let d = Descriptor {
            tag: TAG_ISO639_LANGUAGE,
            data: &[0, 0, 0, 0],
        };
        assert_eq!(d.iso639_languages().next().unwrap().as_str(), None);
    }

    #[test]
    fn teletext_pages_number_the_way_broadcasters_do() {
        // eng, type 2 (subtitle), magazine 1, page 0x88 -> 188.
        let d = Descriptor {
            tag: TAG_TELETEXT,
            data: &[b'e', b'n', b'g', (2 << 3) | 1, 0x88],
        };
        let e = d.teletext_pages().next().unwrap();
        assert_eq!(e.page(), 188);
        assert!(e.is_subtitle());
        assert!(!e.is_hearing_impaired());
        // Magazine zero means eight.
        let d = Descriptor {
            tag: TAG_TELETEXT,
            data: &[b'e', b'n', b'g', 5 << 3, 0x01],
        };
        let e = d.teletext_pages().next().unwrap();
        assert_eq!(e.page(), 801);
        assert!(e.is_hearing_impaired());
    }

    #[test]
    fn one_teletext_descriptor_can_declare_five_streams() {
        let mut data = Vec::new();
        for (lang, page) in [
            (b"eng", 0x01u8),
            (b"fra", 0x02),
            (b"deu", 0x03),
            (b"spa", 0x04),
            (b"ita", 0x05),
        ] {
            data.extend_from_slice(lang);
            data.push((2 << 3) | 1);
            data.push(page);
        }
        let d = Descriptor {
            tag: TAG_TELETEXT,
            data: &data,
        };
        assert_eq!(d.teletext_pages().count(), 5);
    }

    #[test]
    fn subtitling_entries_carry_page_ids() {
        let d = Descriptor {
            tag: TAG_SUBTITLING,
            data: &[b'e', b'n', b'g', 0x20, 0x00, 0x01, 0x00, 0x02],
        };
        let e = d.subtitling_entries().next().unwrap();
        assert_eq!(e.composition_page_id, 1);
        assert_eq!(e.ancillary_page_id, 2);
        assert!(e.is_hearing_impaired());
        assert_eq!(e.language_str(), Some("eng"));
    }

    #[test]
    fn service_descriptor_splits_two_length_prefixed_names() {
        let mut data = vec![0x01, 6];
        data.extend_from_slice(b"FFmpeg");
        data.push(9);
        data.extend_from_slice(b"Service01");
        let d = Descriptor {
            tag: TAG_SERVICE,
            data: &data,
        };
        let s = d.service().unwrap();
        assert_eq!(s.service_type, 0x01);
        assert_eq!(s.provider, b"FFmpeg");
        assert_eq!(s.name, b"Service01");
    }

    #[test]
    fn a_service_descriptor_with_a_lying_length_is_rejected() {
        let d = Descriptor {
            tag: TAG_SERVICE,
            data: &[0x01, 200, b'x'],
        };
        assert!(d.service().is_none());
    }

    #[test]
    fn maximum_bitrate_converts_units() {
        let d = Descriptor {
            tag: TAG_MAXIMUM_BITRATE,
            data: &[0xC0, 0x00, 0x0A],
        };
        // Reserved top two bits masked off; 10 * 50 * 8 = 4000.
        assert_eq!(d.maximum_bitrate(), Some(4000));
    }

    #[test]
    fn accessors_refuse_the_wrong_tag() {
        let d = Descriptor {
            tag: 0x99,
            data: b"AC-3",
        };
        assert!(d.registration_format().is_none());
        assert!(d.service().is_none());
        assert_eq!(d.iso639_languages().count(), 0);
        assert_eq!(d.teletext_pages().count(), 0);
        assert_eq!(d.subtitling_entries().count(), 0);
    }
}
