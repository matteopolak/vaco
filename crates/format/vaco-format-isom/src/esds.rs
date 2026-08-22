//! `esds` — the MPEG-4 elementary stream descriptor (ISO/IEC 14496-1 §7.2.6).
//!
//! An `esds` is not a box tree; it is a *descriptor* tree with its own length
//! encoding, and that difference is where parsers go wrong. Each descriptor is
//!
//! ```text
//! tag:u8  size:expandable  payload
//! ```
//!
//! where `expandable` is base-128, seven bits per byte, continuation in bit 7,
//! **at most four bytes** (§8.3.3). A five-byte length is not a longer number,
//! it is a malformed file, and accepting it is how a parser ends up with a
//! length that does not fit its own arithmetic.
//!
//! The payload this crate actually wants is the `DecoderSpecificInfo` (tag
//! `0x05`) — the `AudioSpecificConfig` for AAC, the VOS header for MPEG-4
//! Visual — plus the object type indication that says which codec it belongs
//! to. Everything else is read to be skipped correctly.

use vaco_codec_core::CodecId;
use vaco_core::{Error, Result};

use crate::boxes::FullBox;

/// `ES_Descriptor`.
pub const TAG_ES: u8 = 0x03;
/// `DecoderConfigDescriptor`.
pub const TAG_DECODER_CONFIG: u8 = 0x04;
/// `DecoderSpecificInfo`.
pub const TAG_DECODER_SPECIFIC: u8 = 0x05;
/// `SLConfigDescriptor`.
pub const TAG_SL_CONFIG: u8 = 0x06;

/// Descriptors inspected before giving up, so a crafted chain cannot spin.
pub const MAX_DESCRIPTORS: usize = 64;

/// What an `esds` says about its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EsDescriptor<'a> {
    /// `ES_ID`.
    pub es_id: u16,
    /// `objectTypeIndication` from the `DecoderConfigDescriptor`.
    pub object_type: u8,
    /// `streamType`, six bits.
    pub stream_type: u8,
    /// `bufferSizeDB`.
    pub buffer_size: u32,
    /// `maxBitrate`, bits per second. Zero means unstated.
    pub max_bitrate: u32,
    /// `avgBitrate`, bits per second. Zero means unstated.
    pub avg_bitrate: u32,
    /// `DecoderSpecificInfo` — the codec extradata, borrowed.
    pub decoder_specific: Option<&'a [u8]>,
}

impl<'a> EsDescriptor<'a> {
    /// Parse an `esds` full box.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the outer descriptor is not an
    /// `ES_Descriptor` or its length encoding is malformed.
    pub fn parse(full: &FullBox<'a>) -> Result<Self> {
        let (tag, body, _) = read_descriptor(full.body)
            .ok_or(Error::InvalidData("isom: malformed esds descriptor"))?;
        if tag != TAG_ES {
            return Err(Error::InvalidData(
                "isom: esds does not hold an ES_Descriptor",
            ));
        }
        Self::parse_es(body)
    }

    /// Parse an `ES_Descriptor` payload directly, for callers that already
    /// stripped the tag (`wave ▸ esds`, and MPEG-2 systems).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the fixed header does not fit.
    pub fn parse_es(body: &'a [u8]) -> Result<Self> {
        let mut r = vaco_bitstream::ByteReader::new(body);
        let es_id = r.be16();
        let flags = r.u8();
        if flags & 0x80 != 0 {
            let _depends_on = r.be16();
        }
        if flags & 0x40 != 0 {
            let n = usize::from(r.u8());
            let _url = r.bytes(n);
        }
        if flags & 0x20 != 0 {
            let _ocr = r.be16();
        }
        r.check()
            .map_err(|_| Error::InvalidData("isom: truncated ES_Descriptor"))?;

        let mut me = Self {
            es_id,
            ..Self::default()
        };
        let mut rest = body.get(r.pos()..).unwrap_or(&[]);
        let mut seen = 0usize;
        while let Some((tag, payload, consumed)) = read_descriptor(rest) {
            seen = seen.saturating_add(1);
            if seen > MAX_DESCRIPTORS {
                break;
            }
            if tag == TAG_DECODER_CONFIG {
                me.read_decoder_config(payload);
            }
            let Some(next) = rest.get(consumed..) else {
                break;
            };
            if consumed == 0 {
                break;
            }
            rest = next;
        }
        Ok(me)
    }

    fn read_decoder_config(&mut self, payload: &'a [u8]) {
        let mut r = vaco_bitstream::ByteReader::new(payload);
        self.object_type = r.u8();
        let packed = r.u8();
        self.stream_type = packed >> 2;
        self.buffer_size = r.be24();
        self.max_bitrate = r.be32();
        self.avg_bitrate = r.be32();
        if r.overrun() {
            return;
        }
        let mut rest = payload.get(r.pos()..).unwrap_or(&[]);
        let mut seen = 0usize;
        while let Some((tag, body, consumed)) = read_descriptor(rest) {
            seen = seen.saturating_add(1);
            if seen > MAX_DESCRIPTORS {
                return;
            }
            if tag == TAG_DECODER_SPECIFIC {
                self.decoder_specific = Some(body);
                return;
            }
            let Some(next) = rest.get(consumed..) else {
                return;
            };
            if consumed == 0 {
                return;
            }
            rest = next;
        }
    }

    /// The codec the object type indication names, where this workspace has an
    /// identifier for it.
    #[must_use]
    pub fn codec(&self) -> Option<CodecId> {
        object_type_codec(self.object_type)
    }
}

/// Read one descriptor: its tag, its payload, and the bytes it occupied.
///
/// `None` for a truncated header, a length that does not fit, or an expandable
/// size longer than the four bytes §8.3.3 permits.
#[must_use]
pub fn read_descriptor(data: &[u8]) -> Option<(u8, &[u8], usize)> {
    let tag = data.first().copied()?;
    let mut len = 0u32;
    let mut at = 1usize;
    for _ in 0..4 {
        let b = data.get(at).copied()?;
        at = at.checked_add(1)?;
        len = len.checked_mul(128)?.checked_add(u32::from(b & 0x7F))?;
        if b & 0x80 == 0 {
            let end = at.checked_add(len as usize)?;
            let payload = data.get(at..end)?;
            return Some((tag, payload, end));
        }
    }
    None
}

/// Encode an expandable size, for fixture construction and for the muxer that
/// will eventually live next door.
///
/// Always emits four bytes, which is legal (§8.3.3 permits redundant
/// continuation bytes) and is what most writers do so that a size can be
/// patched in place.
#[must_use]
pub fn write_expandable(len: u32) -> [u8; 4] {
    [
        (((len >> 21) & 0x7F) as u8) | 0x80,
        (((len >> 14) & 0x7F) as u8) | 0x80,
        (((len >> 7) & 0x7F) as u8) | 0x80,
        (len & 0x7F) as u8,
    ]
}

/// MPEG-4 object type indications (ISO/IEC 14496-1 Table 5, extended by the
/// MP4 Registration Authority) mapped to this workspace's codec identifiers.
///
/// Only the values `vaco-codec-core` can name are listed. The rest are
/// deliberately `None` rather than approximated — a demuxer that gets `None`
/// still has the raw object type and the sample entry's four-character code to
/// report, and inventing a wrong `CodecId` would be worse than reporting
/// nothing.
#[must_use]
pub fn object_type_codec(oti: u8) -> Option<CodecId> {
    match oti {
        0x21 => Some(CodecId::H264),
        0x23 => Some(CodecId::Hevc),
        // 0x40 MPEG-4 Audio, 0x66/0x67/0x68 MPEG-2 AAC main/LC/SSR.
        0x40 | 0x66 | 0x67 | 0x68 => Some(CodecId::Aac),
        // 0x69 MPEG-2 Audio (layer 3), 0x6B MPEG-1 Audio.
        0x69 | 0x6B => Some(CodecId::Mp3),
        0x6C => Some(CodecId::Jpeg),
        0x6D => Some(CodecId::Png),
        0xAD => Some(CodecId::Opus),
        0xDD => Some(CodecId::Vorbis),
        _ => None,
    }
}

/// The `streamType` values §7.2.6.6 defines, for the two that matter here.
pub mod stream_type {
    /// Visual stream.
    pub const VISUAL: u8 = 0x04;
    /// Audio stream.
    pub const AUDIO: u8 = 0x05;
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::testutil::{first_box, fullbx};

    fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend_from_slice(&write_expandable(u32::try_from(payload.len()).unwrap()));
        out.extend_from_slice(payload);
        out
    }

    /// An `esds` for AAC-LC 44.1 kHz mono: `AudioSpecificConfig` `0x12 0x08`.
    fn aac_esds() -> Vec<u8> {
        let dsi = descriptor(TAG_DECODER_SPECIFIC, &[0x12, 0x08]);
        let mut dcd = vec![0x40, 0x15];
        dcd.extend_from_slice(&[0, 0, 0]); // bufferSizeDB
        dcd.extend_from_slice(&96_000u32.to_be_bytes());
        dcd.extend_from_slice(&69_655u32.to_be_bytes());
        dcd.extend_from_slice(&dsi);
        let mut es = vec![0x00, 0x02, 0x00];
        es.extend_from_slice(&descriptor(TAG_DECODER_CONFIG, &dcd));
        es.extend_from_slice(&descriptor(TAG_SL_CONFIG, &[0x02]));
        fullbx(b"esds", 0, 0, &descriptor(TAG_ES, &es))
    }

    #[test]
    fn an_aac_esds_yields_its_audio_specific_config() {
        let raw = aac_esds();
        let d = EsDescriptor::parse(&first_box(&raw).full().unwrap()).unwrap();
        assert_eq!(d.es_id, 2);
        assert_eq!(d.object_type, 0x40);
        assert_eq!(d.stream_type, stream_type::AUDIO);
        assert_eq!(d.max_bitrate, 96_000);
        assert_eq!(d.avg_bitrate, 69_655);
        assert_eq!(d.decoder_specific, Some(&[0x12u8, 0x08][..]));
        assert_eq!(d.codec(), Some(CodecId::Aac));
    }

    #[test]
    fn expandable_sizes_round_trip_at_every_width() {
        for len in [0u32, 1, 127, 128, 16_383, 16_384] {
            let payload = vec![0u8; 0];
            let mut d = vec![TAG_DECODER_SPECIFIC];
            d.extend_from_slice(&write_expandable(len));
            d.extend_from_slice(&payload);
            // Only the length decoding is under test, so give it the bytes it
            // asks for.
            d.resize(5usize.saturating_add(len as usize), 0);
            let (tag, body, used) = read_descriptor(&d).unwrap();
            assert_eq!(tag, TAG_DECODER_SPECIFIC);
            assert_eq!(body.len(), len as usize);
            assert_eq!(used, 5 + len as usize);
        }
    }

    #[test]
    fn a_short_form_length_is_accepted() {
        let d = [TAG_DECODER_SPECIFIC, 0x02, 0xAA, 0xBB];
        let (tag, body, used) = read_descriptor(&d).unwrap();
        assert_eq!(tag, TAG_DECODER_SPECIFIC);
        assert_eq!(body, &[0xAA, 0xBB]);
        assert_eq!(used, 4);
    }

    #[test]
    fn a_five_byte_expandable_size_is_rejected() {
        let d = [TAG_ES, 0x80, 0x80, 0x80, 0x80, 0x01];
        assert!(read_descriptor(&d).is_none());
    }

    #[test]
    fn a_length_past_the_end_is_rejected_rather_than_clamped() {
        let d = [TAG_DECODER_SPECIFIC, 0x40, 0x01];
        assert!(read_descriptor(&d).is_none());
    }

    #[test]
    fn an_esds_that_is_not_an_es_descriptor_is_an_error() {
        let raw = fullbx(b"esds", 0, 0, &descriptor(TAG_DECODER_CONFIG, &[0; 13]));
        assert!(EsDescriptor::parse(&first_box(&raw).full().unwrap()).is_err());
    }

    #[test]
    fn the_optional_es_header_fields_are_skipped_correctly() {
        // All three flags set: depends_on, URL, OCR.
        let dsi = descriptor(TAG_DECODER_SPECIFIC, &[0x11, 0x90]);
        let mut dcd = vec![0x40, 0x15, 0, 0, 0];
        dcd.extend_from_slice(&[0; 8]);
        dcd.extend_from_slice(&dsi);
        let mut es = vec![0x00, 0x01, 0xE0];
        es.extend_from_slice(&[0x00, 0x07]); // dependsOn_ES_ID
        es.push(3); // URL length
        es.extend_from_slice(b"a/b");
        es.extend_from_slice(&[0x00, 0x09]); // OCR_ES_Id
        es.extend_from_slice(&descriptor(TAG_DECODER_CONFIG, &dcd));
        let raw = fullbx(b"esds", 0, 0, &descriptor(TAG_ES, &es));
        let d = EsDescriptor::parse(&first_box(&raw).full().unwrap()).unwrap();
        assert_eq!(d.decoder_specific, Some(&[0x11u8, 0x90][..]));
    }

    #[test]
    fn a_truncated_es_descriptor_is_an_error_not_a_panic() {
        let raw = fullbx(b"esds", 0, 0, &descriptor(TAG_ES, &[0x00]));
        assert!(EsDescriptor::parse(&first_box(&raw).full().unwrap()).is_err());
    }

    #[test]
    fn an_esds_with_no_decoder_specific_info_still_parses() {
        let dcd = vec![0x21, 0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut es = vec![0x00, 0x03, 0x00];
        es.extend_from_slice(&descriptor(TAG_DECODER_CONFIG, &dcd));
        let raw = fullbx(b"esds", 0, 0, &descriptor(TAG_ES, &es));
        let d = EsDescriptor::parse(&first_box(&raw).full().unwrap()).unwrap();
        assert_eq!(d.decoder_specific, None);
        assert_eq!(d.codec(), Some(CodecId::H264));
    }

    #[test]
    fn unknown_object_types_map_to_nothing_rather_than_a_guess() {
        assert_eq!(object_type_codec(0x00), None);
        assert_eq!(object_type_codec(0xFF), None);
        assert_eq!(object_type_codec(0xA5), None); // AC-3: no CodecId yet
        assert_eq!(object_type_codec(0x6B), Some(CodecId::Mp3));
    }
}
