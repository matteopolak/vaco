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
/// Bytes of a `tmcd` (timecode) sample entry body: `Reserved`(4) +
/// `Flags`(4) + `TimeScale`(4) + `FrameDuration`(4) + `NumberOfFrames`(1) +
/// `Reserved`(1), per the `QuickTime` File Format specification.
const TMCD_BODY_LEN: usize = 18;

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
    /// Version 2's `constBitsPerChannel`.
    ///
    /// The real per-channel bit width for an `lpcm` entry. `sample_size`
    /// above is a fixed placeholder (`16`) in every version-2 body this crate
    /// has measured, regardless of the actual sample width — this field is
    /// not.
    pub const_bits_per_channel: Option<u32>,
    /// Version 2's `formatFlags`: a `CoreAudio` `AudioFormatFlags` bitfield.
    /// Bit 0 is float, bit 1 is big-endian, bit 2 is signed integer, bit 3 is
    /// packed. This, not the fourcc, is what actually decides an `lpcm`
    /// entry's PCM flavour — see [`SampleEntry::codec`].
    pub format_flags: Option<u32>,
}

/// The fixed fields of a `tmcd` (timecode) sample entry.
///
/// One sample of a `tmcd` track is a big-endian `u32` frame count from
/// midnight (or from a counter's start, when [`Self::is_counter`]); these
/// fields are what turns that count into `HH:MM:SS:FF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimecodeSampleEntry {
    /// Bit 0 drop-frame, bit 1 24-hour-max, bit 2 negative times allowed,
    /// bit 3 the sample is a plain counter rather than a timecode.
    pub flags: u32,
    /// The timecode track's own time scale — not necessarily the media
    /// timescale of the track this one annotates.
    pub time_scale: u32,
    /// Duration of one frame, in `time_scale` units.
    pub frame_duration: u32,
    /// Frames per second, rounded — the modulus `HH:MM:SS:FF` counts against.
    pub number_of_frames: u8,
}

impl TimecodeSampleEntry {
    const DROP_FRAME: u32 = 1 << 0;
    const COUNTER: u32 = 1 << 3;

    /// Whether the drop-frame flag (bit 0) is set.
    #[must_use]
    pub const fn is_drop_frame(&self) -> bool {
        self.flags & Self::DROP_FRAME != 0
    }

    /// Whether this track is a plain frame counter (bit 3) rather than a
    /// wall-clock timecode.
    #[must_use]
    pub const fn is_counter(&self) -> bool {
        self.flags & Self::COUNTER != 0
    }

    /// Render one sample's frame count as `HH:MM:SS:FF` (`;` before `FF` for
    /// drop-frame, matching the reference's own formatting), or `None` when
    /// [`Self::number_of_frames`] is zero and the modulus is undefined.
    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "a timecode's HH/MM/SS/FF fields are exact integer moduli of a frame count, not an approximation of a real-valued quotient"
    )]
    pub fn format(&self, frame_count: u32) -> Option<String> {
        let fps = u32::from(self.number_of_frames);
        if fps == 0 {
            return None;
        }
        let total_seconds = frame_count / fps;
        let ff = frame_count % fps;
        let hh = total_seconds / 3600;
        let mm = (total_seconds / 60) % 60;
        let ss = total_seconds % 60;
        let sep = if self.is_drop_frame() { ';' } else { ':' };
        Some(format!("{hh:02}:{mm:02}:{ss:02}{sep}{ff:02}"))
    }
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
    /// The timecode body, when the handler is `tmcd`.
    pub tmcd: Option<TimecodeSampleEntry>,
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
            tmcd: None,
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
            boxes::TMCD => {
                me.tmcd = Some(parse_tmcd(&mut r));
                TMCD_BODY_LEN
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

    /// The Common Encryption scheme and default track parameters, from
    /// `sinf ▸ schm` / `sinf ▸ schi ▸ tenc`.
    ///
    /// `None` when the entry has no `sinf` at all; a `sinf` with neither `schm`
    /// nor `tenc` — malformed, but seen — reports
    /// [`crate::cenc::CencInfo::is_empty`].
    #[must_use]
    pub fn cenc(&self) -> Option<crate::cenc::CencInfo> {
        let sinf = self.extension_boxes().find(boxes::SINF)?;
        Some(crate::cenc::CencInfo::from_sinf(&sinf))
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

    /// The `QuickTime` endian atom (`enda`), for the sample entries whose
    /// fourcc does not fix a byte order on its own: `in24`, `in32`, `fl32`,
    /// `fl64`. `Some(true)` is little-endian, `Some(false)` is big-endian,
    /// `None` is "no `enda` box present" — which measured `sowt`/`twos`
    /// files rely on, since those two encode the byte order in the fourcc
    /// itself and never carry an `enda`.
    ///
    /// Searches the extensions, then — like [`SampleEntry::config`] —
    /// inside `wave`, where the box is measured to live for `in24`/`in32`/
    /// `fl32`/`fl64` (2026-08-23, `ffmpeg`'s `mov` muxer).
    #[must_use]
    pub fn endian(&self) -> Option<bool> {
        if let Some(v) = find_enda(self.extension_boxes()) {
            return Some(v);
        }
        let wave = self.extension_boxes().find(boxes::WAVE)?;
        find_enda(wave.children())
    }

    /// [`SampleEntry::endian`], defaulted to big-endian.
    ///
    /// Apple's `QuickTime` File Format specification gives big-endian as the
    /// byte order in force when no `enda` atom is present at all — the
    /// default a demuxer must assume for the (unmeasured; `ffmpeg` never
    /// omits `enda` where it matters) case of an `in24`/`in32`/`fl32`/`fl64`
    /// entry with none.
    fn little_endian(&self) -> bool {
        self.endian().unwrap_or(false)
    }

    /// The codec an ambiguous fourcc names, using this entry's context —
    /// media type, `bits_per_sample`, and `enda` — to resolve it.
    ///
    /// This is the reason [`SampleEntry::codec`] cannot be a plain
    /// `FourCc -> CodecId` lookup for PCM: `sowt` alone means "little-endian
    /// signed PCM" without saying 8- or 16-bit, `in24`/`in32`/`fl32`/`fl64`
    /// alone fix a width but not a byte order, `lpcm`'s real layout is in its
    /// version-2 body and not in the fourcc at all, and `raw ` is `pcm_u8` in
    /// an audio entry but `rawvideo` in a video one. Measured 2026-08-23 by
    /// encoding one `.mov` per PCM variant with `ffmpeg` and reading back
    /// `codec_tag_string`/`codec_name`/`bits_per_raw_sample`, plus reading the
    /// raw sample-entry bytes for `enda` and the version-2 body directly —
    /// see `docs/format/vaco-format-isom.md` for the full table.
    fn resolve_ambiguous(&self, fourcc: FourCc) -> Option<CodecId> {
        if fourcc == FourCc::new(b"raw ") {
            if self.visual.is_some() {
                return Some(CodecId::Rawvideo);
            }
            let audio = self.audio.as_ref()?;
            return (audio.sample_size == 8).then_some(CodecId::PcmU8);
        }
        let audio = self.audio.as_ref()?;
        match &fourcc.0 {
            // Measured: little-endian always, both 8- and 16-bit observed
            // (`pcm_s8`/`pcm_s16le`), and neither carries an `enda` box.
            b"sowt" => signed_pcm(audio.sample_size, true),
            // Not directly measured (no `ffmpeg` encoder writes big-endian
            // 8-bit `twos`), but symmetric with `sowt` per the QTFF spec.
            b"twos" => signed_pcm(audio.sample_size, false),
            // Measured: `sample_size` is a fixed `16` placeholder for all
            // four of these regardless of the true width, so unlike `sowt`/
            // `twos` it is not consulted here — the fourcc already fixes the
            // width, and only the byte order (`enda`) is still open.
            b"in24" => signed_pcm(24, self.little_endian()),
            b"in32" => signed_pcm(32, self.little_endian()),
            b"fl32" => float_pcm(32, self.little_endian()),
            b"fl64" => float_pcm(64, self.little_endian()),
            // The generic ISO sample entry: layout lives in the version-2
            // body (`formatFlags` + `constBitsPerChannel`), not the fourcc.
            // Measured with an 8-channel 192 kHz `pcm_s32le` track, the
            // smallest input this `ffmpeg` build will promote to a version-2
            // `lpcm` entry rather than `sowt`/`in32`.
            b"lpcm" => lpcm_pcm(audio),
            // Unmeasured — `ffmpeg`'s `mov` muxer has no path that emits
            // `NONE` — but per the QTFF spec it is the same shape as `twos`:
            // width from `sample_size`, byte order from `enda` (defaulting to
            // big-endian, native, absent one).
            b"NONE" => signed_pcm(audio.sample_size, self.little_endian()),
            _ => None,
        }
    }

    /// The codec this entry names, refining `mp4a`/`mp4v` through `esds`
    /// where one is present and disambiguating the PCM family through
    /// [`SampleEntry::resolve_ambiguous`] otherwise.
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
        if let Some(id) = self.resolve_ambiguous(fourcc) {
            return Some(id);
        }
        sample_entry_codec(fourcc)
    }
}

/// One `enda` box's value, from an extension-box iterator: `Some(true)` for
/// little-endian (`1`), `Some(false)` for big-endian (`0`), `None` if no
/// `enda` is present. Malformed (too short) is treated the same as absent.
fn find_enda(iter: BoxIter<'_>) -> Option<bool> {
    let enda = iter.flatten().find(|b| b.kind() == boxes::ENDA)?;
    let v = enda.payload.first_chunk::<2>()?;
    Some(u16::from_be_bytes(*v) != 0)
}

/// Signed integer PCM for a measured bit width, `None` for anything this
/// workspace has no `CodecId` for.
fn signed_pcm(bits: u16, little: bool) -> Option<CodecId> {
    match bits {
        8 => Some(CodecId::PcmS8),
        16 => Some(if little {
            CodecId::PcmS16le
        } else {
            CodecId::PcmS16be
        }),
        24 => Some(if little {
            CodecId::PcmS24le
        } else {
            CodecId::PcmS24be
        }),
        32 => Some(if little {
            CodecId::PcmS32le
        } else {
            CodecId::PcmS32be
        }),
        _ => None,
    }
}

/// Floating-point PCM for a measured bit width, `None` for anything this
/// workspace has no `CodecId` for.
fn float_pcm(bits: u16, little: bool) -> Option<CodecId> {
    match bits {
        32 => Some(if little {
            CodecId::PcmF32le
        } else {
            CodecId::PcmF32be
        }),
        64 => Some(if little {
            CodecId::PcmF64le
        } else {
            CodecId::PcmF64be
        }),
        _ => None,
    }
}

/// An `lpcm` entry's flavour from its version-2 body: `formatFlags` (a
/// `CoreAudio` `AudioFormatFlags` bitfield — bit 0 float, bit 1 big-endian,
/// bit 2 signed integer) and `constBitsPerChannel`. `None` when the entry
/// lacks a version-2 body (not measured against a real file — every `lpcm`
/// entry this crate has seen from `ffmpeg` is version 2) or names a width
/// this workspace has no PCM `CodecId` for.
fn lpcm_pcm(audio: &AudioSampleEntry) -> Option<CodecId> {
    let flags = audio.format_flags?;
    let bits = u16::try_from(audio.const_bits_per_channel?).ok()?;
    let little = flags & 0x2 == 0;
    if flags & 0x1 != 0 {
        return float_pcm(bits, little);
    }
    if flags & 0x4 != 0 {
        return signed_pcm(bits, little);
    }
    (bits == 8).then_some(CodecId::PcmU8)
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
            let const_bits_per_channel = r.be32();
            let format_flags = r.be32();
            let _const_bytes_packet = r.be32();
            let _const_frames_packet = r.be32();
            me.sample_rate_f64 = Some(rate);
            me.channel_count = u16::try_from(channels).unwrap_or(u16::MAX);
            me.const_bits_per_channel = Some(const_bits_per_channel);
            me.format_flags = Some(format_flags);
            (me, AUDIO_BODY_V0.saturating_add(AUDIO_EXTRA_V2))
        }
        _ => (me, AUDIO_BODY_V0),
    }
}

fn parse_tmcd(r: &mut vaco_bitstream::ByteReader<'_>) -> TimecodeSampleEntry {
    let _reserved = r.be32();
    let flags = r.be32();
    let time_scale = r.be32();
    let frame_duration = r.be32();
    let number_of_frames = r.u8();
    let _reserved2 = r.u8();
    TimecodeSampleEntry {
        flags,
        time_scale,
        frame_duration,
        number_of_frames,
    }
}

/// `colr` — colour information (ISO/IEC 14496-12 §12.1.5; `nclc` is the
/// pre-standard `QuickTime` spelling of the same three CICP-shaped codes,
/// without the trailing `full_range` byte).
///
/// The three codes are CICP indices (ISO/IEC 23091-2 / ITU-T H.273) —
/// the same numeric space `vaco-color`'s enums already parse for H.264/HEVC
/// VUI and Matroska's `Colour` element, so this box layer reports the raw
/// `u16`s and leaves the lookup to a caller that already links that crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColourInfo {
    /// `nclx`, `nclc`, `rICC` or `prof`.
    pub colour_type: FourCc,
    /// `None` for an ICC-profile colour type (`rICC`/`prof`), which carries
    /// no CICP codes at all.
    pub primaries: Option<u16>,
    pub transfer: Option<u16>,
    pub matrix: Option<u16>,
    /// `nclx` only; `nclc` and the ICC types leave this `false`.
    pub full_range: bool,
}

impl ColourInfo {
    /// Parse a `colr` box's payload (not a full box).
    #[must_use]
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let mut r = vaco_bitstream::ByteReader::new(payload);
        let colour_type = FourCc(r.bytes(4).try_into().ok()?);
        if colour_type != FourCc::new(b"nclx") && colour_type != FourCc::new(b"nclc") {
            return Some(Self {
                colour_type,
                primaries: None,
                transfer: None,
                matrix: None,
                full_range: false,
            });
        }
        let primaries = r.be16();
        let transfer = r.be16();
        let matrix = r.be16();
        let full_range = colour_type == FourCc::new(b"nclx") && (r.u8() & 0x80) != 0;
        Some(Self {
            colour_type,
            primaries: Some(primaries),
            transfer: Some(transfer),
            matrix: Some(matrix),
            full_range,
        })
    }
}

impl SampleEntry<'_> {
    /// The `colr` box's contents, if this entry's extensions carry one.
    ///
    /// `nclx`'s explicit `full_range` bit is unambiguous, but `nclc` (the
    /// pre-`nclx` `QuickTime` spelling, which has no such bit at all) is
    /// not: measured against three real `ffmpeg`-produced `nclc` fixtures
    /// with the *same* primaries/transfer/matrix codes, the reference
    /// reports `color_range` as `tv` for `prores_ks`, `pc` for `mjpeg`
    /// (`yuvj420p`'s own full-range convention), and `unknown` for `v210`
    /// -- i.e. an `nclc` box's implied range is a **per-codec decoder
    /// policy**, not a fact this container-level accessor can state on its
    /// own. `vaco-demux-mp4::track::codec_parameters` only ever sets
    /// `VideoParameters::color.range` from an explicit `full_range = true`
    /// for exactly this reason; mapping the bit's absence to `Limited`
    /// unconditionally was tried and reverted after it broke the `mjpeg`
    /// and `v210` cases while still not fixing `prores_ks` (whose own
    /// `pix_fmt` this tree does not yet derive correctly either, a
    /// separate `vaco-codec-prores` gap this accessor cannot see from
    /// here).
    #[must_use]
    pub fn colour(&self) -> Option<ColourInfo> {
        let colr = self.extension_boxes().find(boxes::COLR)?;
        ColourInfo::parse(colr.payload)
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
/// (`vp08`/`vp09`), Xiph's Opus and FLAC encapsulations (`Opus`, `fLaC`),
/// Apple's `QuickTime` specification for the PCM flavours, for `ProRes` and for
/// `h263`, and Apple Lossless's own registration (`alac`).
///
/// This is a **fallback**, used directly for the fourccs that name exactly
/// one codec and — via [`SampleEntry::resolve_ambiguous`] — as the safety net
/// for the PCM fourccs when an entry's context does not resolve them to a
/// specific width and byte order (malformed input, or a bit depth this
/// workspace has no exact `CodecId` for). `raw ` is deliberately absent:
/// alone it is genuinely ambiguous between `pcm_u8` (audio) and `rawvideo`
/// (video), so a fourcc-only guess would be wrong half the time rather than
/// imprecise, and `SampleEntry::codec` resolves it using the media type
/// instead.
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
        // Measured: `ffmpeg -c:v h263 -f mov` writes `h263` directly, no
        // `mp4v`/`esds` wrapper — unlike `mpeg4`, this one never goes through
        // the object-type-indication table.
        b"h263" => Some(CodecId::H263),
        // ProRes: measured across every quality tier `prores_ks` has (proxy,
        // LT, standard, HQ, 4444, 4444 XQ) — all six report
        // `codec_name=prores`, distinguished only by `codec_tag_string`.
        b"apco" | b"apcs" | b"apcn" | b"apch" | b"ap4h" | b"ap4x" => Some(CodecId::Prores),
        b"alac" => Some(CodecId::Alac),
        // Measured: `ffmpeg -c:a pcm_mulaw`/`pcm_alaw -f mov` write these
        // directly with no ambiguity — one fourcc, one fixed encoding.
        b"ulaw" => Some(CodecId::PcmMulaw),
        b"alaw" => Some(CodecId::PcmAlaw),
        // Uncompressed video: measured per input pixel format. `raw ` covers
        // packed RGB and greyscale and is handled in
        // [`SampleEntry::resolve_ambiguous`] instead, since the same fourcc
        // means `pcm_u8` in an audio entry; `2vuy` is UYVY422, `yuvs` is
        // YUYV422, `24BG` is BGR24 — `ffprobe` calls all of them
        // `codec_name=rawvideo`.
        b"2vuy" | b"yuvs" | b"24BG" => Some(CodecId::Rawvideo),
        // QuickTime PCM flavours reached without entry context (see the
        // function doc): a width- and byte-order-blind "it is some flavour
        // of PCM" guess, safe because [`CodecId::Pcm`] exists for exactly
        // this case.
        b"sowt" | b"twos" | b"lpcm" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"NONE" => {
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

    /// A version-0 audio sample entry with an explicit `sample_size`, unlike
    /// [`audio_entry`] which fixes it at 16 — needed to test the PCM
    /// disambiguation, where `sample_size` is exactly the field in question.
    fn pcm_audio_entry(kind: [u8; 4], sample_size: u16, ext: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 6];
        b.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        b.extend_from_slice(&0u16.to_be_bytes()); // version
        b.extend_from_slice(&0u16.to_be_bytes()); // revision
        b.extend_from_slice(&0u32.to_be_bytes()); // vendor
        b.extend_from_slice(&1u16.to_be_bytes()); // channels
        b.extend_from_slice(&sample_size.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes()); // compression_id
        b.extend_from_slice(&0u16.to_be_bytes()); // packet_size
        b.extend_from_slice(&(44_100u32 << 16).to_be_bytes());
        b.extend_from_slice(ext);
        bx(&kind, &b)
    }

    /// A `wave ▸ frma ▸ enda` extension, the shape `ffmpeg`'s `mov` muxer
    /// writes `enda` in for `in24`/`in32`/`fl32`/`fl64`.
    fn wave_with_enda(frma: [u8; 4], little: bool) -> Vec<u8> {
        let mut body = bx(b"frma", &frma);
        let value: u16 = little.into();
        body.extend_from_slice(&bx(b"enda", &value.to_be_bytes()));
        bx(b"wave", &body)
    }

    /// A version-2 (`lpcm`-style) audio sample entry with an explicit
    /// `constBitsPerChannel` / `formatFlags`, the two fields that actually
    /// decide an `lpcm` entry's flavour.
    fn lpcm_v2_entry(channels: u32, rate: f64, const_bits: u32, format_flags: u32) -> Vec<u8> {
        let mut b = vec![0u8; 6];
        b.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        b.extend_from_slice(&2u16.to_be_bytes()); // version
        b.extend_from_slice(&0u16.to_be_bytes()); // revision
        b.extend_from_slice(&0u32.to_be_bytes()); // vendor
        b.extend_from_slice(&0xFFFEu16.to_be_bytes()); // numChannels (compat)
        b.extend_from_slice(&16u16.to_be_bytes()); // sampleSize (compat placeholder)
        b.extend_from_slice(&0xFFFEu16.to_be_bytes()); // compressionID (compat)
        b.extend_from_slice(&0u16.to_be_bytes()); // packetSize (compat)
        b.extend_from_slice(&1u32.to_be_bytes()); // sampleRate (compat placeholder)
        b.extend_from_slice(&72u32.to_be_bytes()); // sizeOfStructOnly
        b.extend_from_slice(&rate.to_be_bytes());
        b.extend_from_slice(&channels.to_be_bytes());
        b.extend_from_slice(&0x7F00_0000u32.to_be_bytes()); // "always 7f"
        b.extend_from_slice(&const_bits.to_be_bytes());
        b.extend_from_slice(&format_flags.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes()); // constBytesPerPacket
        b.extend_from_slice(&1u32.to_be_bytes()); // constFramesPerPacket
        bx(b"lpcm", &b)
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
        assert_eq!(
            sample_entry_codec(FourCc::new(b"h263")),
            Some(CodecId::H263)
        );
        assert_eq!(
            sample_entry_codec(FourCc::new(b"ap4h")),
            Some(CodecId::Prores)
        );
        assert_eq!(
            sample_entry_codec(FourCc::new(b"alac")),
            Some(CodecId::Alac)
        );
        assert_eq!(
            sample_entry_codec(FourCc::new(b"ulaw")),
            Some(CodecId::PcmMulaw)
        );
        assert_eq!(
            sample_entry_codec(FourCc::new(b"alaw")),
            Some(CodecId::PcmAlaw)
        );
        // `raw ` is deliberately absent: context-free it is ambiguous between
        // `pcm_u8` and `rawvideo`, so a fourcc-only guess is not made.
        assert_eq!(sample_entry_codec(FourCc::new(b"raw ")), None);
    }

    /// The whole point of this task: a `FourCc` alone cannot name the PCM
    /// codec. `sowt` covers 8- and 16-bit; `in24`/`in32`/`fl32`/`fl64` each
    /// cover both byte orders. Every row here is the
    /// (`FourCc`, `bits_per_sample`, `enda`) triple that
    /// [`SampleEntry::codec`] must resolve, measured 2026-08-23 by encoding
    /// one `.mov` per `ffmpeg` PCM encoder and reading the sample entry back
    /// byte for byte — see `docs/format/vaco-format-isom.md`.
    #[test]
    fn the_pcm_family_resolves_from_fourcc_bits_per_sample_and_enda() {
        struct Case {
            fourcc: [u8; 4],
            sample_size: u16,
            enda: Option<bool>,
            want: CodecId,
        }
        let cases = [
            // `sowt`/`twos`: byte order is fixed by the fourcc itself, never
            // an `enda` box; width comes from `sample_size`, which is
            // measured accurate for these two (unlike the `inNN`/`flNN`
            // group below).
            Case {
                fourcc: *b"sowt",
                sample_size: 16,
                enda: None,
                want: CodecId::PcmS16le,
            },
            Case {
                fourcc: *b"sowt",
                sample_size: 8,
                enda: None,
                want: CodecId::PcmS8,
            },
            Case {
                fourcc: *b"twos",
                sample_size: 16,
                enda: None,
                want: CodecId::PcmS16be,
            },
            // `in24`/`in32`/`fl32`/`fl64`: width is fixed by the fourcc;
            // `sample_size` is a measured-constant `16` placeholder here and
            // is not consulted. Byte order comes from `enda` alone.
            Case {
                fourcc: *b"in24",
                sample_size: 16,
                enda: Some(true),
                want: CodecId::PcmS24le,
            },
            Case {
                fourcc: *b"in24",
                sample_size: 16,
                enda: Some(false),
                want: CodecId::PcmS24be,
            },
            Case {
                fourcc: *b"in32",
                sample_size: 16,
                enda: Some(true),
                want: CodecId::PcmS32le,
            },
            Case {
                fourcc: *b"in32",
                sample_size: 16,
                enda: Some(false),
                want: CodecId::PcmS32be,
            },
            Case {
                fourcc: *b"fl32",
                sample_size: 16,
                enda: Some(true),
                want: CodecId::PcmF32le,
            },
            Case {
                fourcc: *b"fl32",
                sample_size: 16,
                enda: Some(false),
                want: CodecId::PcmF32be,
            },
            Case {
                fourcc: *b"fl64",
                sample_size: 16,
                enda: Some(true),
                want: CodecId::PcmF64le,
            },
            Case {
                fourcc: *b"fl64",
                sample_size: 16,
                enda: Some(false),
                want: CodecId::PcmF64be,
            },
        ];
        for c in cases {
            let ext = match c.enda {
                Some(little) => wave_with_enda(c.fourcc, little),
                None => Vec::new(),
            };
            let raw = pcm_audio_entry(c.fourcc, c.sample_size, &ext);
            let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
            assert_eq!(
                e.codec(),
                Some(c.want),
                "fourcc {:?} bits {} enda {:?}",
                FourCc::new(&c.fourcc),
                c.sample_size,
                c.enda
            );
        }
    }

    #[test]
    fn raw_is_pcm_u8_in_an_audio_entry_and_rawvideo_in_a_visual_one() {
        let audio = pcm_audio_entry(*b"raw ", 8, &[]);
        let e = SampleEntry::parse(&first_box(&audio), boxes::SOUN);
        assert_eq!(e.codec(), Some(CodecId::PcmU8));

        let visual = visual_entry(*b"raw ", 16, 16, &[]);
        let e = SampleEntry::parse(&first_box(&visual), boxes::VIDE);
        assert_eq!(e.codec(), Some(CodecId::Rawvideo));
    }

    #[test]
    fn an_lpcm_entry_resolves_from_its_version_two_body_not_the_fourcc() {
        // Measured: `ffmpeg -c:a pcm_s32le` on an 8-channel 192 kHz input —
        // the smallest case this `ffmpeg` build promotes past `sowt`/`in32`
        // to a version-2 `lpcm` entry — writes `formatFlags = 0x0C`
        // (signed | packed) and `constBitsPerChannel = 32`.
        let signed_packed_little_32 = lpcm_v2_entry(8, 192_000.0, 32, 0x0C);
        let e = SampleEntry::parse(&first_box(&signed_packed_little_32), boxes::SOUN);
        assert_eq!(e.codec(), Some(CodecId::PcmS32le));

        // The rest are unmeasured (this `ffmpeg` build never emits them) but
        // exercise every bit `lpcm_pcm` reads: big-endian, float and
        // unsigned-8-bit.
        let signed_packed_big_16 = lpcm_v2_entry(2, 48_000.0, 16, 0x0E);
        let e = SampleEntry::parse(&first_box(&signed_packed_big_16), boxes::SOUN);
        assert_eq!(e.codec(), Some(CodecId::PcmS16be));

        let float_packed_little_32 = lpcm_v2_entry(2, 48_000.0, 32, 0x09);
        let e = SampleEntry::parse(&first_box(&float_packed_little_32), boxes::SOUN);
        assert_eq!(e.codec(), Some(CodecId::PcmF32le));

        let float_packed_big_64 = lpcm_v2_entry(2, 48_000.0, 64, 0x0B);
        let e = SampleEntry::parse(&first_box(&float_packed_big_64), boxes::SOUN);
        assert_eq!(e.codec(), Some(CodecId::PcmF64be));

        let unsigned_packed_8 = lpcm_v2_entry(1, 8_000.0, 8, 0x08);
        let e = SampleEntry::parse(&first_box(&unsigned_packed_8), boxes::SOUN);
        assert_eq!(e.codec(), Some(CodecId::PcmU8));
    }

    #[test]
    fn enda_is_found_inside_wave_like_esds_is() {
        let raw = pcm_audio_entry(*b"in24", 16, &wave_with_enda(*b"in24", true));
        let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
        assert_eq!(e.endian(), Some(true));
    }

    #[test]
    fn no_enda_box_reports_none_rather_than_a_default() {
        let raw = pcm_audio_entry(*b"in24", 16, &[]);
        let e = SampleEntry::parse(&first_box(&raw), boxes::SOUN);
        assert_eq!(e.endian(), None);
        // But the codec still resolves, defaulting to big-endian.
        assert_eq!(e.codec(), Some(CodecId::PcmS24be));
    }

    /// Bytes read back from a real `ffmpeg 8.1` `-movflags write_colr` file
    /// (`libx264`, `-colorspace bt709 -color_range tv`): `nclx`, primaries and
    /// transfer left unspecified (`2`), matrix `bt709` (`1`), limited range.
    #[test]
    fn colr_matches_a_real_ffmpeg_nclx_atom() {
        let payload = [
            b'n', b'c', b'l', b'x', 0, 2, 0, 2, 0, 1, 0x00,
        ];
        let c = ColourInfo::parse(&payload).unwrap();
        assert_eq!(c.colour_type, FourCc::new(b"nclx"));
        assert_eq!(c.primaries, Some(2));
        assert_eq!(c.transfer, Some(2));
        assert_eq!(c.matrix, Some(1));
        assert!(!c.full_range);
    }

    #[test]
    fn colr_full_range_bit_is_the_top_bit_of_the_last_byte() {
        let payload = [b'n', b'c', b'l', b'x', 0, 1, 0, 1, 0, 1, 0x80];
        let c = ColourInfo::parse(&payload).unwrap();
        assert!(c.full_range);
    }

    #[test]
    fn colr_nclc_has_no_full_range_byte_at_all() {
        let payload = [b'n', b'c', b'l', b'c', 0, 1, 0, 1, 0, 6];
        let c = ColourInfo::parse(&payload).unwrap();
        assert_eq!(c.colour_type, FourCc::new(b"nclc"));
        assert_eq!(c.matrix, Some(6));
        assert!(!c.full_range);
    }

    #[test]
    fn colr_icc_profile_reports_only_its_type() {
        let payload = [b'r', b'I', b'C', b'C', 1, 2, 3, 4];
        let c = ColourInfo::parse(&payload).unwrap();
        assert_eq!(c.colour_type, FourCc::new(b"rICC"));
        assert!(c.primaries.is_none());
    }

    #[test]
    fn a_visual_entry_reports_its_colr_box() {
        let colr = bx(
            b"colr",
            &[b'n', b'c', b'l', b'x', 0, 1, 0, 1, 0, 1, 0x80],
        );
        let raw = visual_entry(*b"avc1", 640, 480, &colr);
        let e = SampleEntry::parse(&first_box(&raw), boxes::VIDE);
        let c = e.colour().unwrap();
        assert_eq!(c.matrix, Some(1));
        assert!(c.full_range);
    }

    #[test]
    fn tmcd_entry_reports_its_fixed_fields() {
        let mut body = vec![0u8; 6]; // reserved
        body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        body.extend_from_slice(&0u32.to_be_bytes()); // reserved
        body.extend_from_slice(&1u32.to_be_bytes()); // flags: drop-frame
        body.extend_from_slice(&30_000u32.to_be_bytes()); // time_scale
        body.extend_from_slice(&1001u32.to_be_bytes()); // frame_duration
        body.push(30); // number_of_frames
        body.push(0); // reserved
        let raw = bx(b"tmcd", &body);
        let e = SampleEntry::parse(&first_box(&raw), boxes::TMCD);
        let t = e.tmcd.unwrap();
        assert!(t.is_drop_frame());
        assert_eq!(t.time_scale, 30_000);
        assert_eq!(t.frame_duration, 1001);
        assert_eq!(t.number_of_frames, 30);
    }

    #[test]
    fn tmcd_format_renders_hh_mm_ss_ff() {
        let entry = TimecodeSampleEntry {
            flags: 0,
            time_scale: 25,
            frame_duration: 1,
            number_of_frames: 25,
        };
        // One hour of 25 fps frames, non-drop-frame separator.
        assert_eq!(entry.format(90_000).as_deref(), Some("01:00:00:00"));
        assert_eq!(entry.format(90_001).as_deref(), Some("01:00:00:01"));
    }

    #[test]
    fn tmcd_drop_frame_uses_a_semicolon_before_the_frame_count() {
        let entry = TimecodeSampleEntry {
            flags: TimecodeSampleEntry::DROP_FRAME,
            time_scale: 30_000,
            frame_duration: 1001,
            number_of_frames: 30,
        };
        assert_eq!(entry.format(30).as_deref(), Some("00:00:01;00"));
    }

    #[test]
    fn tmcd_format_is_none_when_frame_rate_is_unknown() {
        let entry = TimecodeSampleEntry::default();
        assert_eq!(entry.format(100), None);
    }
}
