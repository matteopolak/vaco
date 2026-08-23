//! `stsd` — sample descriptions, and the configuration boxes hanging off them.
//!
//! ISO/IEC 14496-12 §8.5.2, plus §8.5.2.2's `VisualSampleEntry` and
//! `AudioSampleEntry`, plus the `QuickTime` sound description versions 1 and 2.
//!
//! # The rule that catches people
//!
//! **Which body a sample entry has is decided by the track's handler, not by
//! the entry's four-character code.** `mp4a` in a `vide` track is not an audio
//! entry; `rtp ` in a hint track is neither. Parsing by fourcc means keeping a
//! list of every code in existence and being wrong about the next one, so
//! [`SampleEntry::parse`] takes the handler and the fourcc stays what it is: an
//! identifier to report and to look up in a table.
//!
//! # Where extradata comes from
//!
//! A sample entry's fixed fields are followed by extension boxes, and one of
//! them carries the codec's out-of-band configuration. Which one, and what the
//! bytes mean, differs per codec — so [`CodecConfig`] reports the *flavour*
//! alongside the raw payload and stops there. Turning an `esds` into an
//! `AudioSpecificConfig`, or a `dfLa` into a `STREAMINFO`, is a decision about
//! what a decoder wants and belongs to the demuxer, not to the box layer.

use vaco_codec_core::CodecId;
use vaco_core::{MediaType, Rational, Result};

use crate::boxes::{BoxIter, IsoBox};
use crate::fixed::fp16u;
use crate::fourcc::{FourCc, boxes};

/// Bytes of `SampleEntry` before any body: six reserved plus
/// `data_reference_index`.
const SAMPLE_ENTRY_LEN: usize = 8;
/// Bytes of `VisualSampleEntry` body before the extension boxes.
const VISUAL_BODY_LEN: usize = 70;
/// Bytes of `AudioSampleEntry` version 0 body.
const AUDIO_BODY_V0: usize = 20;
/// Additional bytes in a `QuickTime` sound description version 1.
const AUDIO_EXTRA_V1: usize = 16;
/// Additional bytes in a `QuickTime` sound description version 2.
const AUDIO_EXTRA_V2: usize = 36;

/// The fixed fields of a `VisualSampleEntry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisualSampleEntry {
    /// Coded width in pixels.
    pub width: u16,
    /// Coded height in pixels.
    pub height: u16,
    /// Horizontal resolution, 16.16 dpi. Conventionally 72.
    pub horiz_resolution: Rational,
    /// Vertical resolution, 16.16 dpi.
    pub vert_resolution: Rational,
    /// Samples per frame; 1 for everything that is not an old `QuickTime` file.
    pub frame_count: u16,
    /// Bit depth. `0x18` is 24-bit colour; values from `0x21` up mean greyscale
    /// with a palette, per `QuickTime`.
    pub depth: u16,
    /// The 32-byte Pascal `compressorname`, trimmed to its declared length.
    pub compressor_name: [u8; 31],
    /// How many of [`VisualSampleEntry::compressor_name`] are meaningful.
    pub compressor_name_len: u8,
}

impl VisualSampleEntry {
    /// The compressor name as text, when it is valid UTF-8.
    #[must_use]
    pub fn compressor(&self) -> Option<&str> {
        let n = usize::from(self.compressor_name_len).min(31);
        core::str::from_utf8(self.compressor_name.get(..n)?).ok()
    }
}

/// The fixed fields of an `AudioSampleEntry`, across all three versions.
///
/// Not `Eq`: version 2 carries an IEEE-754 sample rate, and a `NaN` in that
/// field would break the reflexivity `Eq` promises. `PartialEq` is enough for
/// every use here.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AudioSampleEntry {
    /// `QuickTime` sound description version: 0, 1 or 2.
    pub version: u16,
    /// Channels. In version 2 this comes from `numAudioChannels`, which is
    /// 32-bit and therefore clamped here.
    pub channel_count: u16,
    /// Bits per sample.
    pub sample_size: u16,
    /// The 16.16 `samplerate` field, exactly.
    ///
    /// Kept as a rational because the field only has 16 integer bits: a
    /// 96 000 Hz track cannot be expressed and is written as 0 or as a
    /// wrapped value, with the true rate in a `srat` box or in version 2's
    /// `audioSampleRate` double. Truncating here would lose the evidence.
    pub sample_rate: Rational,
    /// Version 2's `audioSampleRate`, an IEEE-754 double.
    pub sample_rate_f64: Option<f64>,
    /// Version 1's `samples_per_packet`.
    pub samples_per_packet: u32,
    /// Version 1's `bytes_per_packet`.
    pub bytes_per_packet: u32,
    /// Version 1's `bytes_per_frame`.
    pub bytes_per_frame: u32,
    /// Version 1's `bytes_per_sample`.
    pub bytes_per_sample: u32,
}

impl AudioSampleEntry {
    /// The sample rate in whole hertz, preferring version 2's double.
    #[must_use]
    pub fn rate_hz(&self) -> u32 {
        if let Some(f) = self.sample_rate_f64
            && f.is_finite()
            && f > 0.0
            && f < f64::from(u32::MAX)
        {
            return f as u32;
        }
        let v = self.sample_rate.to_f64();
        if v.is_finite() && v > 0.0 && v < f64::from(u32::MAX) {
            v as u32
        } else {
            0
        }
    }
}

/// What a configuration box's bytes are.
///
/// The flavour is what tells a demuxer how to read [`CodecConfig::data`]; the
/// same four bytes mean different things in an `avcC` and a `dOps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFlavour {
    /// `avcC` — `AVCDecoderConfigurationRecord` (ISO/IEC 14496-15 §5.3.3.1).
    Avcc,
    /// `hvcC` — `HEVCDecoderConfigurationRecord` (14496-15 §8.3.3.1).
    Hvcc,
    /// `vvcC` — `VvcDecoderConfigurationRecord`.
    Vvcc,
    /// `av1C` — AV1 codec configuration record.
    Av1c,
    /// `vpcC` — VP codec configuration record (a full box).
    Vpcc,
    /// `esds` — an MPEG-4 elementary stream descriptor; the extradata is the
    /// `DecoderSpecificInfo` inside, not these bytes.
    Esds,
    /// `dOps` — Opus specific box; the payload is `OpusHead` minus its magic.
    Dops,
    /// `dfLa` — FLAC specific box (a full box) holding metadata blocks.
    Dfla,
    /// `dac3` or `dec3` — AC-3 / E-AC-3 specific box.
    Dac3,
    /// `alac` — the Apple Lossless magic cookie.
    Alac,
    /// `glbl` — a raw global header, as some `QuickTime` writers emit.
    Glbl,
}

/// One configuration box, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct CodecConfig<'a> {
    /// What the bytes are.
    pub flavour: ConfigFlavour,
    /// The box type they came from.
    pub kind: FourCc,
    /// The box payload, header stripped. For a full box the version and flags
    /// are still present, because they are part of the record.
    pub data: &'a [u8],
}

/// One entry of `stsd`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SampleEntry<'a> {
    /// The entry's four-character code, as stored.
    pub format: FourCc,
    /// `data_reference_index`, one-based into `dref`.
    pub data_reference_index: u16,
    /// The visual body, when the handler is `vide`.
    pub visual: Option<VisualSampleEntry>,
    /// The audio body, when the handler is `soun`.
    pub audio: Option<AudioSampleEntry>,
    /// The extension boxes following the fixed fields.
    pub extensions: &'a [u8],
    /// Absolute file offset of [`SampleEntry::extensions`].
    pub extensions_offset: u64,
}

impl<'a> SampleEntry<'a> {
    /// Parse one entry, using `handler` to choose the body layout.
    ///
    /// Never fails: a truncated body leaves the fields at their defaults and
    /// the extension slice empty. A sample entry too short to hold its own
    /// fixed fields describes a track nobody can decode, but the track's
    /// existence, its four-character code and its timing are still worth
    /// reporting — which is what `ffprobe` does with such a file.
    #[must_use]
    pub fn parse(entry: &IsoBox<'a>, handler: FourCc) -> Self {
        let mut r = vaco_bitstream::ByteReader::new(entry.payload);
        let _reserved = r.bytes(6);
        let data_reference_index = r.be16();
        let mut me = Self {
            format: entry.kind(),
            data_reference_index,
            visual: None,
            audio: None,
            extensions: &[],
            extensions_offset: entry.payload_offset(),
        };
        let body_len = match handler {
            boxes::VIDE => {
                me.visual = Some(parse_visual(&mut r));
                VISUAL_BODY_LEN
            }
            boxes::SOUN => {
                let (audio, len) = parse_audio(&mut r);
                me.audio = Some(audio);
                len
            }
            _ => 0,
        };
        let at = SAMPLE_ENTRY_LEN.saturating_add(body_len);
        me.extensions = entry.payload.get(at..).unwrap_or(&[]);
        me.extensions_offset = entry.payload_offset().saturating_add(at as u64);
        me
    }

    /// The extension boxes as an iterator.
    #[must_use]
    pub const fn extension_boxes(&self) -> BoxIter<'a> {
        BoxIter::new(self.extensions, self.extensions_offset)
    }

    /// The original format of an encrypted entry, from `sinf ▸ frma`.
    ///
    /// `None` when the entry is not encrypted. A demuxer reports the *original*
    /// codec and marks the stream encrypted rather than reporting `encv`,
    /// because that is what the reference prints and what a user can act on.
    #[must_use]
    pub fn original_format(&self) -> Option<FourCc> {
        let sinf = self.extension_boxes().find(boxes::SINF)?;
        let frma = sinf.children().find(boxes::FRMA)?;
        let b = frma.payload.first_chunk::<4>()?;
        Some(FourCc(*b))
    }

    /// The four-character code to identify the codec by: the original format
    /// when encrypted, the stored format otherwise.
    #[must_use]
    pub fn effective_format(&self) -> FourCc {
        self.original_format().unwrap_or(self.format)
    }

    /// The configuration box for this entry, if it has a recognised one.
    ///
    /// Searches the extensions, then — for `QuickTime` audio — inside `wave`,
    /// where old writers nest the `esds`.
    #[must_use]
    pub fn config(&self) -> Option<CodecConfig<'a>> {
        if let Some(c) = find_config(self.extension_boxes()) {
            return Some(c);
        }
        let wave = self.extension_boxes().find(boxes::WAVE)?;
        find_config(wave.children())
    }

    /// The codec this entry names, refining `mp4a`/`mp4v` through `esds` where
    /// one is present.
    #[must_use]
    pub fn codec(&self) -> Option<CodecId> {
        let fourcc = self.effective_format();
        if fourcc == FourCc::new(b"mp4a") || fourcc == FourCc::new(b"mp4v") {
            if let Some(c) = self.config()
                && c.flavour == ConfigFlavour::Esds
                && let Ok(full) = crate::boxes::FullBox::parse(c.data, 0)
                && let Ok(d) = crate::esds::EsDescriptor::parse(&full)
                && let Some(id) = d.codec()
            {
                return Some(id);
            }
            // An `mp4a` with no usable `esds` is AAC by overwhelming
            // convention; `mp4v` without one is not guessable.
            return (fourcc == FourCc::new(b"mp4a")).then_some(CodecId::Aac);
        }
        sample_entry_codec(fourcc)
    }
}

fn parse_visual(r: &mut vaco_bitstream::ByteReader<'_>) -> VisualSampleEntry {
    let _pre_defined = r.be16();
    let _reserved = r.be16();
    let _pre_defined2 = r.bytes(12);
    let width = r.be16();
    let height = r.be16();
    let horiz = r.be32();
    let vert = r.be32();
    let _reserved2 = r.be32();
    let frame_count = r.be16();
    let name = r.bytes(32);
    let depth = r.be16();
    let _pre_defined3 = r.be16();
    let mut compressor_name = [0u8; 31];
    let declared = name.first().copied().unwrap_or(0);
    let n = usize::from(declared).min(31);
    if let (Some(dst), Some(src)) = (
        compressor_name.get_mut(..n),
        name.get(1..1usize.saturating_add(n)),
    ) {
        dst.copy_from_slice(src);
    }
    VisualSampleEntry {
        width,
        height,
        horiz_resolution: fp16u(horiz),
        vert_resolution: fp16u(vert),
        frame_count,
        depth,
        compressor_name,
        compressor_name_len: u8::try_from(n).unwrap_or(0),
    }
}

fn parse_audio(r: &mut vaco_bitstream::ByteReader<'_>) -> (AudioSampleEntry, usize) {
    let version = r.be16();
    let _revision = r.be16();
    let _vendor = r.be32();
    let channel_count = r.be16();
    let sample_size = r.be16();
    let _compression_id = r.be16();
    let _packet_size = r.be16();
    let sample_rate = r.be32();
    let mut me = AudioSampleEntry {
        version,
        channel_count,
        sample_size,
        sample_rate: fp16u(sample_rate),
        ..AudioSampleEntry::default()
    };
    match version {
        1 => {
            me.samples_per_packet = r.be32();
            me.bytes_per_packet = r.be32();
            me.bytes_per_frame = r.be32();
            me.bytes_per_sample = r.be32();
            (me, AUDIO_BODY_V0.saturating_add(AUDIO_EXTRA_V1))
        }
        2 => {
            let _size_of_struct_only = r.be32();
            let rate = r.f64_be();
            let channels = r.be32();
            let _always_7f = r.be32();
            let _const_bits = r.be32();
            let _format_flags = r.be32();
            let _const_bytes_packet = r.be32();
            let _const_frames_packet = r.be32();
            me.sample_rate_f64 = Some(rate);
            me.channel_count = u16::try_from(channels).unwrap_or(u16::MAX);
            (me, AUDIO_BODY_V0.saturating_add(AUDIO_EXTRA_V2))
        }
        _ => (me, AUDIO_BODY_V0),
    }
}

fn find_config(iter: BoxIter<'_>) -> Option<CodecConfig<'_>> {
    for b in iter.flatten() {
        let flavour = match b.kind() {
            boxes::AVCC => ConfigFlavour::Avcc,
            boxes::HVCC => ConfigFlavour::Hvcc,
            boxes::VVCC => ConfigFlavour::Vvcc,
            boxes::AV1C => ConfigFlavour::Av1c,
            boxes::VPCC => ConfigFlavour::Vpcc,
            boxes::ESDS => ConfigFlavour::Esds,
            boxes::DOPS => ConfigFlavour::Dops,
            boxes::DFLA => ConfigFlavour::Dfla,
            boxes::DAC3 | boxes::DEC3 => ConfigFlavour::Dac3,
            boxes::ALAC => ConfigFlavour::Alac,
            boxes::GLBL => ConfigFlavour::Glbl,
            _ => continue,
        };
        return Some(CodecConfig {
            flavour,
            kind: b.kind(),
            data: b.payload,
        });
    }
    None
}

/// Parse a whole `stsd` box into its entries.
///
/// # Errors
///
/// [`vaco_core::Error::InvalidData`] for a truncated full-box header or a
/// malformed child.
pub fn parse_stsd<'a>(stsd: &IsoBox<'a>, handler: FourCc) -> Result<Vec<SampleEntry<'a>>> {
    let full = stsd.full()?;
    let declared = full
        .body
        .first_chunk::<4>()
        .map_or(0, |b| u32::from_be_bytes(*b));
    // Every entry is at least a box header, so the payload bounds the count
    // exactly — the declared value is a hint, never an allocation size.
    #[allow(
        clippy::integer_division,
        reason = "the divisor is the constant minimum box size"
    )]
    let cap = full.body.len().saturating_sub(4) / crate::boxes::HEADER_LEN as usize;
    let n = (declared as usize).min(cap);
    let mut out = Vec::new();
    for entry in stsd.children_after(8) {
        if out.len() >= n {
            break;
        }
        out.push(SampleEntry::parse(&entry?, handler));
    }
    Ok(out)
}

/// The media type a handler implies.
#[must_use]
pub fn handler_media_type(handler: FourCc) -> Option<MediaType> {
    match handler {
        boxes::VIDE => Some(MediaType::Video),
        boxes::SOUN => Some(MediaType::Audio),
        boxes::SUBT | boxes::SBTL | boxes::TEXT | boxes::CLCP => Some(MediaType::Subtitle),
        boxes::META_HDLR | boxes::TMCD => Some(MediaType::Data),
        _ => None,
    }
}

/// Sample-entry four-character code to codec identifier.
///
/// Registered in ISO/IEC 14496-15 (`avc*`, `hvc*`, `hev*`), 14496-14 (`mp4a`,
/// `mp4v`), the AOM `AV1` ISOBMFF binding (`av01`), the `WebM` VP binding
/// (`vp08`/`vp09`), Xiph's Opus and FLAC encapsulations (`Opus`, `fLaC`) and
/// Apple's `QuickTime` specification for the PCM flavours.
///
/// Codes with no entry in this workspace's [`CodecId`] map to `None` rather
/// than to a near miss; the caller keeps the four-character code either way,
/// and `ffprobe` prints it as `codec_tag_string` regardless.
#[must_use]
pub fn sample_entry_codec(format: FourCc) -> Option<CodecId> {
    match &format.0 {
        b"avc1" | b"avc2" | b"avc3" | b"avc4" | b"dva1" | b"dvav" => Some(CodecId::H264),
        b"hvc1" | b"hev1" | b"hvc2" | b"hev2" | b"dvh1" | b"dvhe" | b"hvt1" => Some(CodecId::Hevc),
        b"av01" => Some(CodecId::Av1),
        b"vp08" => Some(CodecId::Vp8),
        b"vp09" => Some(CodecId::Vp9),
        b"png " | b"png\0" => Some(CodecId::Png),
        b"jpeg" | b"mjpa" | b"mjpb" | b"AVDJ" => Some(CodecId::Jpeg),
        b"mp4a" => Some(CodecId::Aac),
        b"Opus" => Some(CodecId::Opus),
        b"fLaC" => Some(CodecId::Flac),
        b".mp3" | b"mp3 " => Some(CodecId::Mp3),
        // QuickTime PCM flavours: signed/unsigned, both byte orders, and the
        // generic `lpcm` entry whose real layout lives in its version-2 body.
        b"sowt" | b"twos" | b"raw " | b"lpcm" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"NONE" => {
            Some(CodecId::Pcm)
        }
        // MPEG-4 timed text. Measured: the *same* SubRip content muxed into MP4
        // prints `codec_name=mov_text`, not `subrip` — the reference treats the
        // two carriages as different codecs, so this is not `CodecId::SubRip`.
        b"tx3g" => Some(CodecId::MovText),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::float_cmp,
    reason = "test code; the fixed-point conversions are exact by construction"
)]
mod tests {
    use super::*;
    use crate::testutil::{bx, first_box, fullbx};

    fn visual_entry(kind: [u8; 4], w: u16, h: u16, extensions: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 6];
        b.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        b.extend_from_slice(&[0; 2]); // pre_defined
        b.extend_from_slice(&[0; 2]); // reserved
        b.extend_from_slice(&[0; 12]); // pre_defined
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        b.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&1u16.to_be_bytes());
        let mut name = [0u8; 32];
        name[0] = 5;
        name[1..6].copy_from_slice(b"x264 ");
        b.extend_from_slice(&name);
        b.extend_from_slice(&0x0018u16.to_be_bytes());
        b.extend_from_slice(&0xFFFFu16.to_be_bytes());
        b.extend_from_slice(extensions);
        bx(&kind, &b)
    }

    fn audio_entry(kind: [u8; 4], version: u16, channels: u16, rate: u32, ext: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 6];
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&version.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&channels.to_be_bytes());
        b.extend_from_slice(&16u16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&(rate << 16).to_be_bytes());
        if version == 1 {
            for v in [1024u32, 0, 0, 2] {
                b.extend_from_slice(&v.to_be_bytes());
            }
        } else if version == 2 {
            b.extend_from_slice(&72u32.to_be_bytes());
            b.extend_from_slice(&96_000f64.to_be_bytes());
            b.extend_from_slice(&2u32.to_be_bytes());
            for _ in 0..5 {
                b.extend_from_slice(&0u32.to_be_bytes());
            }
        }
        b.extend_from_slice(ext);
        bx(&kind, &b)
    }

    #[test]
    fn a_visual_entry_reports_its_fixed_fields_and_extensions() {
        let avcc = bx(b"avcC", &[0x01, 0x4D, 0x40, 0x0B, 0xFF]);
        let raw = visual_entry(*b"avc1", 160, 120, &avcc);
        let e = SampleEntry::parse(&first_box(&raw), boxes::VIDE);
        let v = e.visual.unwrap();
        assert_eq!((v.width, v.height), (160, 120));
        assert_eq!(v.depth, 0x18);
        assert_eq!(v.horiz_resolution.to_f64(), 72.0);
        assert_eq!(v.compressor(), Some("x264 "));
        assert_eq!(e.data_reference_index, 1);
        let c = e.config().unwrap();
        assert_eq!(c.flavour, ConfigFlavour::Avcc);
        assert_eq!(c.data, &[0x01, 0x4D, 0x40, 0x0B, 0xFF]);
        assert_eq!(e.codec(), Some(CodecId::H264));
        assert!(e.audio.is_none());
    }

    #[test]
    fn an_audio_entry_version_zero() {
        let raw = audio_entry(*b"mp4a", 0, 1, 44_100, &[]);
        let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
        let a = e.audio.unwrap();
        assert_eq!(a.version, 0);
        assert_eq!(a.channel_count, 1);
        assert_eq!(a.rate_hz(), 44_100);
        assert_eq!(e.codec(), Some(CodecId::Aac));
    }

    #[test]
    fn an_audio_entry_version_one_puts_extensions_after_the_extra_fields() {
        let dops = bx(b"dOps", &[0, 2, 0x01, 0x38]);
        let raw = audio_entry(*b"Opus", 1, 2, 48_000, &dops);
        let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
        let a = e.audio.unwrap();
        assert_eq!(a.version, 1);
        assert_eq!(a.samples_per_packet, 1024);
        assert_eq!(a.bytes_per_sample, 2);
        assert_eq!(e.config().unwrap().flavour, ConfigFlavour::Dops);
        assert_eq!(e.codec(), Some(CodecId::Opus));
    }

    #[test]
    fn an_audio_entry_version_two_takes_its_rate_from_the_double() {
        let raw = audio_entry(*b"lpcm", 2, 0, 0, &[]);
        let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
        let a = e.audio.unwrap();
        assert_eq!(a.version, 2);
        assert_eq!(a.channel_count, 2);
        assert_eq!(a.rate_hz(), 96_000);
        assert_eq!(e.codec(), Some(CodecId::Pcm));
    }

    #[test]
    fn a_sixteen_sixteen_sample_rate_cannot_hold_ninety_six_kilohertz() {
        // 96000 << 16 overflows the 16 integer bits, so a version-0 entry
        // records something else entirely. The rational preserves whatever it
        // was rather than pretending.
        let raw = audio_entry(*b"mp4a", 0, 2, 96_000, &[]);
        let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
        assert_ne!(e.audio.unwrap().rate_hz(), 96_000);
    }

    #[test]
    fn the_handler_and_not_the_fourcc_chooses_the_body() {
        let raw = audio_entry(*b"mp4a", 0, 1, 44_100, &[]);
        // Same bytes, read as a video track: no audio body is invented.
        let e = SampleEntry::parse(&first_box(&raw), boxes::VIDE);
        assert!(e.audio.is_none());
        assert!(e.visual.is_some());
    }

    #[test]
    fn an_unknown_handler_leaves_the_whole_payload_as_extensions() {
        let raw = bx(b"tx3g", &[0u8; 40]);
        let e = SampleEntry::parse(&first_box(&raw), FourCc::new(b"sbtl"));
        assert!(e.visual.is_none() && e.audio.is_none());
        assert_eq!(e.extensions.len(), 32);
    }

    #[test]
    fn a_truncated_entry_yields_defaults_rather_than_failing() {
        let raw = bx(b"avc1", &[0u8; 4]);
        let e = SampleEntry::parse(&first_box(&raw), boxes::VIDE);
        assert_eq!(e.visual.unwrap().width, 0);
        assert!(e.extensions.is_empty());
        assert!(e.config().is_none());
    }

    #[test]
    fn an_encrypted_entry_reports_its_original_format() {
        let frma = bx(b"frma", b"avc1");
        let mut sinf_body = frma;
        sinf_body.extend_from_slice(&fullbx(b"schm", 0, 0, b"cenc\0\0\x01\0"));
        let sinf = bx(b"sinf", &sinf_body);
        let raw = visual_entry(*b"encv", 640, 480, &sinf);
        let e = SampleEntry::parse(&first_box(&raw), boxes::VIDE);
        assert_eq!(e.format, FourCc::new(b"encv"));
        assert_eq!(e.original_format(), Some(FourCc::new(b"avc1")));
        assert_eq!(e.effective_format(), FourCc::new(b"avc1"));
        assert_eq!(e.codec(), Some(CodecId::H264));
    }

    #[test]
    fn a_quicktime_wave_nested_esds_is_still_found() {
        let esds = fullbx(b"esds", 0, 0, &[0x03, 0x80, 0x80, 0x80, 0x00]);
        let mut wave_body = bx(b"frma", b"mp4a");
        wave_body.extend_from_slice(&esds);
        let wave = bx(b"wave", &wave_body);
        let raw = audio_entry(*b"mp4a", 1, 2, 48_000, &wave);
        let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
        assert_eq!(e.config().unwrap().flavour, ConfigFlavour::Esds);
    }

    #[test]
    fn stsd_entry_count_is_clamped_to_the_payload() {
        let entry = visual_entry(*b"avc1", 16, 16, &[]);
        let mut body = u32::MAX.to_be_bytes().to_vec();
        body.extend_from_slice(&entry);
        let raw = fullbx(b"stsd", 0, 0, &body);
        let got = parse_stsd(&first_box(&raw), boxes::VIDE).unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn multiple_stsd_entries_are_all_returned() {
        let mut body = 2u32.to_be_bytes().to_vec();
        body.extend_from_slice(&visual_entry(*b"avc1", 16, 16, &[]));
        body.extend_from_slice(&visual_entry(*b"hvc1", 32, 32, &[]));
        let raw = fullbx(b"stsd", 0, 0, &body);
        let got = parse_stsd(&first_box(&raw), boxes::VIDE).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].codec(), Some(CodecId::H264));
        assert_eq!(got[1].codec(), Some(CodecId::Hevc));
        assert_eq!(got[1].visual.unwrap().width, 32);
    }

    #[test]
    fn handler_media_types_cover_the_tracks_we_surface() {
        assert_eq!(handler_media_type(boxes::VIDE), Some(MediaType::Video));
        assert_eq!(handler_media_type(boxes::SOUN), Some(MediaType::Audio));
        assert_eq!(handler_media_type(boxes::SBTL), Some(MediaType::Subtitle));
        assert_eq!(handler_media_type(boxes::HINT), None);
    }

    #[test]
    fn the_codec_table_covers_the_registered_codes() {
        assert_eq!(
            sample_entry_codec(FourCc::new(b"avc3")),
            Some(CodecId::H264)
        );
        assert_eq!(
            sample_entry_codec(FourCc::new(b"hev1")),
            Some(CodecId::Hevc)
        );
        assert_eq!(sample_entry_codec(FourCc::new(b"av01")), Some(CodecId::Av1));
        assert_eq!(
            sample_entry_codec(FourCc::new(b"fLaC")),
            Some(CodecId::Flac)
        );
        assert_eq!(sample_entry_codec(FourCc::new(b"twos")), Some(CodecId::Pcm));
        // Not a near miss: AC-3 has no CodecId in this workspace yet.
        assert_eq!(sample_entry_codec(FourCc::new(b"ac-3")), None);
        assert_eq!(sample_entry_codec(FourCc::new(b"zzzz")), None);
    }
}
