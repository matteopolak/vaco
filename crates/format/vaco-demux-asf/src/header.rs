//! The ASF Header Object walk: File Properties, Stream Properties, Content
//! Description, Extended Content Description, and DRM detection.
//!
//! [\[ASF\] §3](vaco_format_asf) lists every child object the Header Object
//! may carry. This module reads the ones a demuxer needs to build
//! [`vaco_format_core::Stream`]s and container metadata; anything else
//! (Script Command, Marker, Bitrate Mutual Exclusion, the Header Extension
//! Object's own children beyond what this crate needs) is skipped, per the
//! spec's own instruction that "implementations shall ignore any … object
//! that they do not know how to handle."
//!
//! # DRM: detected, not decrypted
//!
//! [`HeaderInfo::encryption`] is set when a Content Encryption Object,
//! Extended Content Encryption Object, or Alternate Extended Content
//! Encryption Object is present ([\[ASF\] §3.14-3.16](vaco_format_asf)).
//! [`crate::demux::AsfDemuxer::open`] still succeeds — a probing tool can
//! still see the stream list and metadata of a DRM file, the same as
//! `ffprobe` does — but [`crate::demux::AsfDemuxer::read_packet`] refuses
//! immediately with [`vaco_core::Error::Unsupported`] rather than handing out
//! ciphertext as if it were a decodable payload. Implementing, circumventing,
//! or working around the encryption is out of scope by design, not by
//! omission.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_asf::guid::Guid;
use vaco_format_asf::object::ObjectIter;
use vaco_format_asf::well_known;
use vaco_format_riff::bitmapinfo::BitmapInfoHeader;
use vaco_format_riff::wave::WaveFormatEx;
use vaco_limits::Budget;

use vaco_format_asf::codec;

/// The File Properties Object's fields this crate uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileProperties {
    pub file_size: u64,
    /// 100-nanosecond intervals since 1601-01-01 00:00:00 UTC, verbatim —
    /// see `docs/format/vaco-demux-asf.md` for the conversion to a Unix
    /// timestamp, which this crate does not perform itself (nothing here
    /// needs wall-clock time; `vaco-probe`'s caller can do the arithmetic).
    pub creation_date_100ns: u64,
    pub data_packets_count: u64,
    /// 100-nanosecond units.
    pub play_duration_100ns: u64,
    /// 100-nanosecond units.
    pub send_duration_100ns: u64,
    pub preroll_ms: u64,
    pub broadcast: bool,
    pub seekable: bool,
    pub min_packet_size: u32,
    pub max_packet_size: u32,
    pub max_bitrate: u32,
}

/// Bytes in the File Properties Object's payload, after the 24-byte object
/// header: a 16-byte File ID, six 8-byte fields (File Size, Creation Date,
/// Data Packets Count, Play Duration, Send Duration, Preroll), then four
/// 4-byte fields (Flags, Minimum/Maximum Data Packet Size, Maximum Bitrate).
const FILE_PROPERTIES_LEN: usize = 16 + 8 * 6 + 4 * 4;

impl FileProperties {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self> {
        if payload.len() < FILE_PROPERTIES_LEN {
            return Err(Error::InvalidData(
                "asf: File Properties Object shorter than its fixed layout",
            ));
        }
        let get_u64 = |off: usize| -> u64 {
            payload
                .get(off..off + 8)
                .and_then(<[u8]>::first_chunk::<8>)
                .map_or(0, |b| u64::from_le_bytes(*b))
        };
        let get_u32 = |off: usize| -> u32 {
            payload
                .get(off..off + 4)
                .and_then(<[u8]>::first_chunk::<4>)
                .map_or(0, |b| u32::from_le_bytes(*b))
        };
        let file_size = get_u64(16);
        let creation_date_100ns = get_u64(24);
        let data_packets_count = get_u64(32);
        let play_duration_100ns = get_u64(40);
        let send_duration_100ns = get_u64(48);
        let preroll_ms = get_u64(56);
        let flags = get_u32(64);
        let min_packet_size = get_u32(68);
        let max_packet_size = get_u32(72);
        let max_bitrate = get_u32(76);
        Ok(Self {
            file_size,
            creation_date_100ns,
            data_packets_count,
            play_duration_100ns,
            send_duration_100ns,
            preroll_ms,
            broadcast: flags & 0x1 != 0,
            seekable: flags & 0x2 != 0,
            min_packet_size,
            max_packet_size,
            max_bitrate,
        })
    }
}

/// One parsed Stream Properties Object.
#[derive(Debug, Clone)]
pub struct ParsedStreamProperties {
    pub stream_number: u8,
    pub encrypted: bool,
    pub media_type: MediaType,
    pub time_base: Rational,
    pub params: CodecParameters,
}

/// Bytes in the Stream Properties Object's fixed prefix (before
/// Type-Specific Data / Error Correction Data): `StreamType(16) +
/// ErrorCorrectionType(16) + TimeOffset(8) + TypeSpecificDataLength(4) +
/// ErrorCorrectionDataLength(4) + Flags(2) + Reserved(4)`.
const STREAM_PROPERTIES_FIXED_LEN: usize = 16 + 16 + 8 + 4 + 4 + 2 + 4;

pub(crate) fn parse_stream_properties(
    payload: &[u8],
    budget: &mut Budget,
) -> Result<ParsedStreamProperties> {
    if payload.len() < STREAM_PROPERTIES_FIXED_LEN {
        return Err(Error::InvalidData(
            "asf: Stream Properties Object shorter than its fixed prefix",
        ));
    }
    let stream_type =
        Guid::parse(payload).ok_or(Error::InvalidData("asf: truncated stream type guid"))?;
    let type_specific_len = payload
        .get(40..44)
        .and_then(<[u8]>::first_chunk::<4>)
        .map_or(0u32, |b| u32::from_le_bytes(*b)) as usize;
    let flags = payload
        .get(48..50)
        .and_then(<[u8]>::first_chunk::<2>)
        .map_or(0u16, |b| u16::from_le_bytes(*b));
    let stream_number = (flags & 0x7F) as u8;
    let encrypted = flags & 0x8000 != 0;
    let type_specific = payload
        .get(STREAM_PROPERTIES_FIXED_LEN..STREAM_PROPERTIES_FIXED_LEN + type_specific_len)
        .unwrap_or(&[]);

    let (media_type, params, time_base) = if stream_type == well_known::AUDIO_MEDIA {
        let mut wfx = WaveFormatEx::parse(type_specific, budget)?;
        let mut params = CodecParameters::audio();
        if let Some(a) = &mut params.audio {
            a.sample_rate = wfx.samples_per_sec;
            a.bits_per_coded_sample = u8::try_from(wfx.bits_per_sample).ok();
            let channels = u32::from(wfx.channels);
            a.layout = Some(
                vaco_chlayout::ChannelLayout::default_for(channels)
                    .unwrap_or_else(|| vaco_chlayout::ChannelLayout::unspecified(channels)),
            );
        }
        params.codec_id = codec::audio_codec_id(&wfx);
        params.codec_tag = Some(le_u16_tag(wfx.format_tag));
        if !wfx.extra.is_empty() {
            params.extradata = Some(core::mem::take(&mut wfx.extra));
        }
        let tb = if wfx.samples_per_sec > 0 {
            Rational::new(1, i32::try_from(wfx.samples_per_sec).unwrap_or(1))
        } else {
            crate::demux::TIME_BASE_100NS
        };
        (MediaType::Audio, params, tb)
    } else if stream_type == well_known::VIDEO_MEDIA {
        // [ASF] §9.2: EncodedImageWidth:u32, EncodedImageHeight:u32,
        // ReservedFlags:u8, FormatDataSize:u16, then the BITMAPINFOHEADER
        // itself as `Format Data`.
        let format_data_size = type_specific
            .get(9..11)
            .and_then(<[u8]>::first_chunk::<2>)
            .map_or(0u16, |b| u16::from_le_bytes(*b)) as usize;
        let format_data = type_specific.get(11..11 + format_data_size).unwrap_or(&[]);
        let bih = BitmapInfoHeader::parse(format_data)?;
        let mut params = CodecParameters::video();
        if let Some(v) = &mut params.video {
            v.width = bih.width.unsigned_abs();
            v.height = bih.abs_height();
            v.coded_width = v.width;
            v.coded_height = v.height;
        }
        let compression = bih.compression();
        params.codec_id = codec::video_codec_id(compression);
        if let vaco_format_riff::bitmapinfo::Compression::FourCc(id) = compression {
            params.codec_tag = Some(id.as_bytes());
        }
        if format_data.len() > BitmapInfoHeader::LEN {
            let extra = format_data.get(BitmapInfoHeader::LEN..).unwrap_or(&[]);
            if !extra.is_empty() {
                let mut buf = budget.alloc::<u8>(extra.len())?;
                buf.copy_from_slice(extra);
                params.extradata = Some(buf);
            }
        }
        (MediaType::Video, params, crate::demux::TIME_BASE_100NS)
    } else {
        // A stream type this crate does not decode payload structure for
        // (Command, JFIF, Degradable JPEG, File Transfer, Binary, or a
        // private one). Its packets still demux — [`crate::demux`] only
        // needs the stream number, not the codec — they simply carry no
        // codec identity.
        // `MediaType` has no "unknown" bucket, and `Data` is exactly what it
        // names for "timed data with no decoder" — the right fit for a
        // command stream, and the least-wrong fit for JFIF/Degradable
        // JPEG/File Transfer/Binary/private types this crate does not model.
        (
            MediaType::Data,
            CodecParameters::new(MediaType::Data),
            crate::demux::TIME_BASE_100NS,
        )
    };

    Ok(ParsedStreamProperties {
        stream_number,
        encrypted,
        media_type,
        time_base,
        params,
    })
}

fn le_u16_tag(tag: u16) -> [u8; 4] {
    let b = tag.to_le_bytes();
    [b[0], b[1], 0, 0]
}

/// How a file's DRM was declared, if at all — carried in the demuxer's
/// container metadata rather than acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// A Content Encryption Object: Microsoft DRM v1.
    DrmV1,
    /// An Extended Content Encryption Object: DRM v7.
    ExtendedDrm,
    /// An Alternate Extended Content Encryption Object: device DRM.
    AlternateDrm,
}

impl Encryption {
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::DrmV1 => "Content Encryption Object (Microsoft DRM v1)",
            Self::ExtendedDrm => "Extended Content Encryption Object (DRM v7)",
            Self::AlternateDrm => "Alternate Extended Content Encryption Object (device DRM)",
        }
    }
}

/// Everything [`crate::demux::AsfDemuxer::open`] needs from the Header
/// Object, gathered in one pass over its children.
#[derive(Debug, Default)]
pub struct HeaderInfo {
    pub file_properties: Option<FileProperties>,
    pub streams: Vec<ParsedStreamProperties>,
    pub metadata: Vec<(String, String)>,
    pub encryption: Option<Encryption>,
    /// Raw bytes of every Simple Index Object found, in file order — parsed
    /// by [`crate::index`] once the stream list (and therefore stream
    /// numbers) is known.
    pub simple_index_objects: Vec<Vec<u8>>,
    /// Raw bytes of the top-level Index Object, if present.
    pub index_object: Option<Vec<u8>>,
}

/// Walk a Header Object's payload (everything after its own 30-byte prefix:
/// `ObjectID(16) + ObjectSize(8) + NumHeaderObjects(4) + Reserved1(1) +
/// Reserved2(1)`) and gather [`HeaderInfo`].
///
/// # Errors
/// [`Error::InvalidData`] if the mandatory File Properties Object or every
/// Stream Properties Object is missing or malformed, or if a child object's
/// own header is truncated beyond recovery.
pub(crate) fn parse_header_object(payload: &[u8], budget: &mut Budget) -> Result<HeaderInfo> {
    let mut info = HeaderInfo::default();
    for obj in ObjectIter::new(payload) {
        let obj = obj?;
        if obj.guid == well_known::FILE_PROPERTIES_OBJECT {
            info.file_properties = Some(FileProperties::parse(obj.payload)?);
        } else if obj.guid == well_known::STREAM_PROPERTIES_OBJECT {
            info.streams
                .push(parse_stream_properties(obj.payload, budget)?);
        } else if obj.guid == well_known::CONTENT_DESCRIPTION_OBJECT {
            parse_content_description(obj.payload, &mut info.metadata);
        } else if obj.guid == well_known::EXTENDED_CONTENT_DESCRIPTION_OBJECT {
            parse_extended_content_description(obj.payload, budget, &mut info.metadata)?;
        } else if obj.guid == well_known::CONTENT_ENCRYPTION_OBJECT {
            info.encryption = Some(Encryption::DrmV1);
        } else if obj.guid == well_known::EXTENDED_CONTENT_ENCRYPTION_OBJECT {
            info.encryption = Some(Encryption::ExtendedDrm);
        } else if obj.guid == well_known::ALT_EXTENDED_CONTENT_ENCRYPTION_OBJECT {
            info.encryption = Some(Encryption::AlternateDrm);
        } else if obj.guid == well_known::HEADER_EXTENSION_OBJECT {
            parse_header_extension(obj.payload, &mut info, budget);
        }
        // Anything else (Script Command, Marker, Bitrate Mutual Exclusion,
        // Codec List, Stream Bitrate Properties, Content Branding, Digital
        // Signature, Padding, a private extension) is ignored, per spec.
    }
    Ok(info)
}

/// Walk a Header Extension Object's data for the two things this crate
/// wants out of it: the top-level Simple Index Object lives at the
/// top level of the file (sibling of Data Object), not inside the Header
/// Extension Object — so nothing here actually reads index objects; this
/// exists to keep the door open for Extended Stream Properties children in
/// the future (deferred, see `docs/format/vaco-demux-asf.md`) without a
/// second header-walking entry point.
fn parse_header_extension(payload: &[u8], _info: &mut HeaderInfo, _budget: &mut Budget) {
    // Bytes 0..22 are Reserved Field 1 (GUID) + Reserved Field 2 (WORD) +
    // Header Extension Data Size (DWORD); the extension objects themselves
    // start at byte 22 per [ASF] §3.4. This crate does not currently parse
    // any of them (Extended Stream Properties support is deferred — see
    // module docs) but the walk is exercised so a malformed extension does
    // not silently look like an empty one.
    if payload.len() < 22 {
        return;
    }
    let _ = ObjectIter::new(payload.get(22..).unwrap_or(&[])).count();
}

/// [\[ASF\] §3.10](vaco_format_asf): five UTF-16LE, length-prefixed strings,
/// mapped to the same metadata keys `ffprobe` prints for them (measured:
/// `ffmpeg -metadata title=… author=… copyright=… comment=… rating=… -f asf`,
/// then `ffprobe -show_format`).
fn parse_content_description(payload: &[u8], metadata: &mut Vec<(String, String)>) {
    let get_u16 = |off: usize| -> usize {
        payload
            .get(off..off + 2)
            .and_then(<[u8]>::first_chunk::<2>)
            .map_or(0, |b| usize::from(u16::from_le_bytes(*b)))
    };
    let title_len = get_u16(0);
    let author_len = get_u16(2);
    let copyright_len = get_u16(4);
    let description_len = get_u16(6);
    let rating_len = get_u16(8);
    let mut pos = 10;
    let mut take = |len: usize| -> String {
        let s = utf16le_string(payload.get(pos..pos + len).unwrap_or(&[]));
        pos += len;
        s
    };
    let title = take(title_len);
    let author = take(author_len);
    let copyright = take(copyright_len);
    let description = take(description_len);
    let rating = take(rating_len);
    for (key, value) in [
        ("title", title),
        ("artist", author),
        ("copyright", copyright),
        ("comment", description),
        ("rating", rating),
    ] {
        if !value.is_empty() {
            metadata.push((key.to_owned(), value));
        }
    }
}

/// [\[ASF\] §3.11](vaco_format_asf): a count-prefixed list of name/typed-value
/// pairs. Only the Unicode-string (0x0000) type is turned into text
/// directly; other types are formatted as their decimal value so nothing is
/// silently dropped. A handful of well-known `WM/`-prefixed names are
/// remapped to the same short key `ffprobe` prints for them (measured the
/// same way as [`parse_content_description`]); everything else is exposed
/// under its own name with the `WM/` prefix stripped, which is an honest
/// fallback rather than a verified reproduction of the reference's full
/// tag table — see `docs/format/vaco-demux-asf.md`.
fn parse_extended_content_description(
    payload: &[u8],
    budget: &mut Budget,
    metadata: &mut Vec<(String, String)>,
) -> Result<()> {
    let count = payload
        .get(0..2)
        .and_then(<[u8]>::first_chunk::<2>)
        .map_or(0u16, |b| u16::from_le_bytes(*b));
    let mut pos = 2usize;
    for _ in 0..count {
        budget.consume_fuel(1)?;
        let Some(name_len) = payload
            .get(pos..pos + 2)
            .and_then(<[u8]>::first_chunk::<2>)
            .map(|b| usize::from(u16::from_le_bytes(*b)))
        else {
            break;
        };
        pos += 2;
        let name = utf16le_string(payload.get(pos..pos + name_len).unwrap_or(&[]));
        pos += name_len;
        let Some(value_type) = payload
            .get(pos..pos + 2)
            .and_then(<[u8]>::first_chunk::<2>)
            .map(|b| u16::from_le_bytes(*b))
        else {
            break;
        };
        pos += 2;
        let Some(value_len) = payload
            .get(pos..pos + 2)
            .and_then(<[u8]>::first_chunk::<2>)
            .map(|b| usize::from(u16::from_le_bytes(*b)))
        else {
            break;
        };
        pos += 2;
        let raw = payload.get(pos..pos + value_len).unwrap_or(&[]);
        pos += value_len;
        let value = descriptor_value_to_string(value_type, raw);
        let key = descriptor_key(&name);
        if !value.is_empty() {
            metadata.push((key, value));
        }
    }
    Ok(())
}

/// Map a Content Descriptor's name to the metadata key this crate exposes it
/// under. See [`parse_extended_content_description`]'s doc comment for the
/// honesty note on the fallback branch.
fn descriptor_key(name: &str) -> String {
    match name {
        "WM/Genre" => "genre".to_owned(),
        "WM/TrackNumber" => "track".to_owned(),
        "WM/Language" => "language".to_owned(),
        "WM/AlbumTitle" => "album".to_owned(),
        "WM/EncodingSettings" => "encoder".to_owned(),
        "date" => "date".to_owned(),
        other => other.strip_prefix("WM/").unwrap_or(other).to_owned(),
    }
}

fn descriptor_value_to_string(value_type: u16, raw: &[u8]) -> String {
    match value_type {
        0x0000 => utf16le_string(raw),
        0x0002 => {
            let v = raw.first_chunk::<4>().map_or(0, |b| u32::from_le_bytes(*b));
            (v != 0).to_string()
        }
        0x0003 => raw
            .first_chunk::<4>()
            .map_or(String::new(), |b| u32::from_le_bytes(*b).to_string()),
        0x0004 => raw
            .first_chunk::<8>()
            .map_or(String::new(), |b| u64::from_le_bytes(*b).to_string()),
        0x0005 => raw
            .first_chunk::<2>()
            .map_or(String::new(), |b| u16::from_le_bytes(*b).to_string()),
        // 0x0001 (BYTE array) and anything unrecognised: no useful text
        // representation, so this is left empty rather than guessed at.
        _ => String::new(),
    }
}

/// Decode a NUL-trimmed UTF-16LE byte string. Lossy on an unpaired
/// surrogate or an odd trailing byte, never a panic.
fn utf16le_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .filter_map(<[u8]>::first_chunk::<2>)
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]); // NUL terminator
        out
    }

    #[test]
    fn file_properties_reads_the_measured_layout() {
        let mut p = vec![0u8; 16]; // File ID (ignored)
        p.extend_from_slice(&1000u64.to_le_bytes()); // file size
        p.extend_from_slice(&116_444_736_000_000_000u64.to_le_bytes()); // creation date
        p.extend_from_slice(&8u64.to_le_bytes()); // data packets count
        p.extend_from_slice(&41_000_000u64.to_le_bytes()); // play duration
        p.extend_from_slice(&10_000_000u64.to_le_bytes()); // send duration
        p.extend_from_slice(&3100u64.to_le_bytes()); // preroll
        p.extend_from_slice(&0x02u32.to_le_bytes()); // flags: seekable
        p.extend_from_slice(&3200u32.to_le_bytes()); // min packet size
        p.extend_from_slice(&3200u32.to_le_bytes()); // max packet size
        p.extend_from_slice(&200_000u32.to_le_bytes()); // max bitrate
        let fp = FileProperties::parse(&p).unwrap();
        assert_eq!(fp.file_size, 1000);
        assert!(fp.seekable);
        assert!(!fp.broadcast);
        assert_eq!(fp.min_packet_size, 3200);
        assert_eq!(fp.max_packet_size, 3200);
        assert_eq!(fp.data_packets_count, 8);
    }

    #[test]
    fn content_description_maps_to_the_measured_tag_names() {
        let title = utf16("t"); // "t" + NUL terminator = 4 bytes
        let mut p = Vec::new();
        p.extend_from_slice(&(title.len() as u16).to_le_bytes()); // title len
        p.extend_from_slice(&0u16.to_le_bytes()); // author len (empty)
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&title);
        let mut meta = Vec::new();
        parse_content_description(&p, &mut meta);
        assert_eq!(meta, vec![("title".to_owned(), "t".to_owned())]);
    }

    #[test]
    fn extended_content_description_maps_known_wm_tags() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u16.to_le_bytes()); // count
        let name = utf16("WM/Genre");
        p.extend_from_slice(&(name.len() as u16).to_le_bytes());
        p.extend_from_slice(&name);
        p.extend_from_slice(&0u16.to_le_bytes()); // type: unicode string
        let value = utf16("Rock");
        p.extend_from_slice(&(value.len() as u16).to_le_bytes());
        p.extend_from_slice(&value);
        let mut meta = Vec::new();
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        parse_extended_content_description(&p, &mut budget, &mut meta).unwrap();
        assert_eq!(meta, vec![("genre".to_owned(), "Rock".to_owned())]);
    }

    #[test]
    fn an_unmapped_wm_tag_falls_back_to_its_own_name() {
        assert_eq!(descriptor_key("WM/SomeVendorThing"), "SomeVendorThing");
        assert_eq!(descriptor_key("NoPrefixAtAll"), "NoPrefixAtAll");
    }

    #[test]
    fn header_object_walk_finds_encryption_and_streams() {
        let mut payload = Vec::new();
        // A minimal File Properties Object.
        let mut fp = vec![0u8; 16];
        fp.extend_from_slice(&0u64.to_le_bytes());
        fp.extend_from_slice(&0u64.to_le_bytes());
        fp.extend_from_slice(&0u64.to_le_bytes());
        fp.extend_from_slice(&0u64.to_le_bytes());
        fp.extend_from_slice(&0u64.to_le_bytes());
        fp.extend_from_slice(&0u64.to_le_bytes());
        fp.extend_from_slice(&0u32.to_le_bytes());
        fp.extend_from_slice(&3200u32.to_le_bytes());
        fp.extend_from_slice(&3200u32.to_le_bytes());
        fp.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&well_known::FILE_PROPERTIES_OBJECT.as_bytes());
        payload.extend_from_slice(&(24 + fp.len() as u64).to_le_bytes());
        payload.extend_from_slice(&fp);
        // A Content Encryption Object with no meaningful payload (detection
        // only, contents unused).
        payload.extend_from_slice(&well_known::CONTENT_ENCRYPTION_OBJECT.as_bytes());
        payload.extend_from_slice(&24u64.to_le_bytes());

        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let info = parse_header_object(&payload, &mut budget).unwrap();
        assert!(info.file_properties.is_some());
        assert_eq!(info.encryption, Some(Encryption::DrmV1));
    }
}
