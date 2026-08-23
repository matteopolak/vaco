//! The Opus identification header.
//!
//! RFC 7845 §5.1 defines it as the `OpusHead` packet of an Ogg logical stream;
//! the Opus-in-ISOBMFF specification carries the same fields as an MP4 `dOps`
//! box, with the magic and the little-endian byte order dropped. Matroska and
//! `WebM` store the Ogg form verbatim in `CodecPrivate`.
//!
//! ```text
//!   0                   1                   2                   3
//!   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!   |      'O','p','u','s','H','e','a','d'  (64 bits)               |
//!   |  version (8)  | channels (8)  |      pre-skip (16)            |
//!   |            input sample rate (32)                             |
//!   |   output gain Q7.8 (16)       | mapping family|  (optional)   |
//! ```

use arrayvec::ArrayVec;
use vaco_bitstream::ByteReader;
use vaco_chlayout::{Channel, ChannelLayout};
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::{Error, Result};

/// The eight magic bytes an Ogg identification header opens with.
pub const MAGIC: &[u8; 8] = b"OpusHead";

/// Bytes in a mapping-family-0 header, magic included.
pub const MIN_LEN: usize = 19;

/// Opus always decodes at 48 kHz, whatever `input_sample_rate` claims.
///
/// RFC 7845 §5.1: the field is "the sample rate of the original input", for
/// informational use only. Probed to confirm the reference does the same — a
/// header declaring 8000 still reports `sample_rate=48000`.
pub const OUTPUT_SAMPLE_RATE: u32 = 48000;

/// The largest channel count the one-byte field can hold.
pub const MAX_CHANNELS: usize = 255;

/// How the channels of a multi-channel Opus stream are laid out.
///
/// RFC 7845 §5.1.1 defines families 0, 1 and 255; RFC 8486 adds 2 and 3 for
/// ambisonics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MappingFamily {
    /// Family 0: mono or stereo, no mapping table.
    Rtp,
    /// Family 1: Vorbis channel order, up to eight channels.
    Vorbis,
    /// Family 2: ambisonics with individual (ACN/SN3D) channels, RFC 8486 §3.1.
    Ambisonics,
    /// Family 3: ambisonics with a demixing matrix, RFC 8486 §3.2.
    AmbisonicsMatrix,
    /// Family 255: discrete channels with no defined positions.
    Discrete,
    /// Anything else. RFC 7845 reserves these.
    Reserved(u8),
}

impl MappingFamily {
    /// The raw byte.
    #[must_use]
    pub const fn value(self) -> u8 {
        match self {
            Self::Rtp => 0,
            Self::Vorbis => 1,
            Self::Ambisonics => 2,
            Self::AmbisonicsMatrix => 3,
            Self::Discrete => 255,
            Self::Reserved(v) => v,
        }
    }

    /// Interpret the byte.
    #[must_use]
    pub const fn from_value(value: u8) -> Self {
        match value {
            0 => Self::Rtp,
            1 => Self::Vorbis,
            2 => Self::Ambisonics,
            3 => Self::AmbisonicsMatrix,
            255 => Self::Discrete,
            v => Self::Reserved(v),
        }
    }

    /// Whether a mapping table (stream counts plus a per-channel index) follows.
    #[must_use]
    pub const fn has_mapping_table(self) -> bool {
        !matches!(self, Self::Rtp)
    }
}

/// A parsed identification header.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IdentificationHeader {
    /// The version octet. Only the upper nibble is checked; see
    /// [`IdentificationHeader::parse`].
    pub version: u8,
    /// `Output Channel Count`.
    pub channel_count: u8,
    /// Samples of encoder delay to discard, at 48 kHz.
    pub pre_skip: u16,
    /// The rate of the material *before* encoding. Informational: Opus decodes
    /// at [`OUTPUT_SAMPLE_RATE`] regardless.
    pub input_sample_rate: u32,
    /// `Output Gain`, a signed Q7.8 value in dB.
    pub output_gain_q8: i16,
    /// The channel mapping family.
    pub mapping_family: MappingFamily,
    /// How many Opus streams the packets carry. One for family 0.
    pub stream_count: u8,
    /// How many of those streams are coupled (stereo) pairs.
    pub coupled_count: u8,
    /// For each output channel, which decoded channel feeds it. `255` means
    /// silence. Empty for family 0.
    pub channel_mapping: ArrayVec<u8, MAX_CHANNELS>,
}

impl IdentificationHeader {
    /// Parse an Ogg/Matroska `OpusHead` packet, magic included.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the magic is absent or a field is out of
    /// range, [`Error::UnexpectedEof`] on truncation, and
    /// [`Error::Unsupported`] for mapping family 3, which the reference
    /// declines to implement.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let Some(rest) = data.strip_prefix(MAGIC.as_slice()) else {
            return Err(Error::InvalidData("missing OpusHead magic"));
        };
        Self::parse_body(rest, Endian::Little)
    }

    /// Parse the body of an MP4 `dOps` box — the same fields, big-endian, with
    /// no magic and with the version octet defined as `0`.
    ///
    /// # Errors
    ///
    /// As [`IdentificationHeader::parse`].
    pub fn parse_dops(data: &[u8]) -> Result<Self> {
        Self::parse_body(data, Endian::Big)
    }

    /// Serialise back to the Ogg `OpusHead` form.
    ///
    /// A demuxer that reads `dOps` still has to hand downstream consumers an
    /// `OpusHead`, because that is the shape everything else expects — the
    /// reference does the same conversion, which is why `extradata_size` is 19
    /// for an Opus track in MP4 even though `dOps` is 11 bytes.
    #[must_use]
    pub fn to_opus_head(&self) -> ArrayVec<u8, { MIN_LEN + 2 + MAX_CHANNELS }> {
        let mut out = ArrayVec::new();
        out.extend(*MAGIC);
        out.push(self.version);
        out.push(self.channel_count);
        out.extend(self.pre_skip.to_le_bytes());
        out.extend(self.input_sample_rate.to_le_bytes());
        out.extend(self.output_gain_q8.to_le_bytes());
        out.push(self.mapping_family.value());
        if self.mapping_family.has_mapping_table() {
            out.push(self.stream_count);
            out.push(self.coupled_count);
            out.extend(self.channel_mapping.iter().copied());
        }
        out
    }

    fn parse_body(data: &[u8], endian: Endian) -> Result<Self> {
        let mut r = ByteReader::new(data);
        let version = r.u8();
        let channel_count = r.u8();
        let (pre_skip, input_sample_rate, output_gain_q8) = match endian {
            Endian::Little => (r.le16(), r.le32(), r.le16().cast_signed()),
            Endian::Big => (r.be16(), r.be32(), r.be16().cast_signed()),
        };
        let family_byte = r.u8();
        r.check()?;

        // RFC 7845 §5.1: "implementations SHOULD treat streams where the upper
        // four bits are not zero as invalid". Probed: the reference accepts
        // every version 0x00..=0x0f and rejects 0x10 and above.
        if version >> 4 != 0 {
            return Err(Error::InvalidData("unsupported OpusHead major version"));
        }
        if channel_count == 0 {
            return Err(Error::InvalidData("OpusHead declares zero channels"));
        }

        let mapping_family = MappingFamily::from_value(family_byte);
        let mut stream_count = 1;
        let mut coupled_count = u8::from(channel_count == 2);
        let mut channel_mapping = ArrayVec::new();

        if mapping_family.has_mapping_table() {
            stream_count = r.u8();
            coupled_count = r.u8();
            let mapping = r.bytes(usize::from(channel_count));
            r.check()?;
            if stream_count == 0 {
                return Err(Error::InvalidData("OpusHead declares zero streams"));
            }
            let total = u16::from(stream_count) + u16::from(coupled_count);
            if u16::from(coupled_count) > u16::from(stream_count) || total > 255 {
                return Err(Error::InvalidData("OpusHead stream/coupled count"));
            }
            for &index in mapping {
                // 255 is the RFC's "this output channel is silent" escape and
                // is deliberately not range-checked.
                if index != 255 && u16::from(index) >= total {
                    return Err(Error::InvalidData("OpusHead channel mapping index"));
                }
                channel_mapping.push(index);
            }
        } else {
            r.check()?;
        }

        let header = Self {
            version,
            channel_count,
            pre_skip,
            input_sample_rate,
            output_gain_q8,
            mapping_family,
            stream_count,
            coupled_count,
            channel_mapping,
        };
        header.check_family()?;
        Ok(header)
    }

    /// The per-family constraints on `channel_count`, all probed.
    fn check_family(&self) -> Result<()> {
        match self.mapping_family {
            MappingFamily::Rtp if self.channel_count > 2 => Err(Error::InvalidData(
                "Opus mapping family 0 allows at most two channels",
            )),
            MappingFamily::Vorbis if self.channel_count > 8 => Err(Error::InvalidData(
                "Opus mapping family 1 allows at most eight channels",
            )),
            MappingFamily::Ambisonics if ambisonic_order(self.channel_count).is_none() => Err(
                Error::InvalidData("Opus mapping family 2 needs (n+1)^2 or (n+1)^2+2 channels"),
            ),
            MappingFamily::AmbisonicsMatrix => Err(Error::Unsupported(
                "Opus channel mapping family 3 (ambisonics with a demixing matrix)",
            )),
            MappingFamily::Reserved(_) => {
                Err(Error::InvalidData("reserved Opus channel mapping family"))
            }
            _ => Ok(()),
        }
    }

    /// The layout a container reports.
    ///
    /// * Families 0 and 1 map onto the standard layouts, in Vorbis channel
    ///   order — `quad` for four channels and `6.1` for seven, which is not
    ///   what an AAC stream of the same count would report.
    /// * Family 2 is ambisonic, optionally with a non-diegetic stereo pair.
    /// * Family 255 has no positions at all, so the layout is *unspecified*
    ///   with the right count — which is what makes `ffprobe` omit
    ///   `channel_layout` for such a stream rather than guessing.
    #[must_use]
    pub fn channel_layout(&self) -> Option<ChannelLayout> {
        let channels = u32::from(self.channel_count);
        match self.mapping_family {
            MappingFamily::Rtp | MappingFamily::Vorbis => VORBIS_LAYOUTS
                .iter()
                .find(|&&(n, _)| u32::from(n) == channels)
                .and_then(|&(_, mask)| ChannelLayout::from_mask(mask)),
            MappingFamily::Ambisonics => {
                let (order, extra) = ambisonic_order(self.channel_count)?;
                let extras: &[Channel] = if extra {
                    &[Channel::FrontLeft, Channel::FrontRight]
                } else {
                    &[]
                };
                ChannelLayout::ambisonic(order, extras.iter().copied())
            }
            _ => Some(ChannelLayout::unspecified(channels)),
        }
    }

    /// The output gain in dB.
    #[must_use]
    pub fn output_gain_db(&self) -> f64 {
        f64::from(self.output_gain_q8) / 256.0
    }

    /// Fold the header into the parameters a container reports.
    ///
    /// `sample_rate` is always [`OUTPUT_SAMPLE_RATE`] and `initial_padding` is
    /// `pre_skip`, which is exactly what `ffprobe` prints.
    #[must_use]
    pub fn to_codec_parameters(&self) -> CodecParameters {
        let mut params = CodecParameters::audio().with_codec(CodecId::Opus);
        params.audio = Some(AudioParameters {
            sample_rate: OUTPUT_SAMPLE_RATE,
            // D17: the decoder's **output** format. Opus decodes to float and
            // the reference prints `fltp` for every Opus stream measured
            // (Matroska, WebM). See `vaco-parse-aac`'s note for why a
            // parse-only crate states one at all.
            format: Some(::vaco_sampfmt::SampleFmt::F32P),
            layout: self
                .channel_layout()
                .or_else(|| Some(ChannelLayout::unspecified(u32::from(self.channel_count)))),
            bits_per_raw_sample: None,
            initial_padding: u32::from(self.pre_skip),
        });
        params
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

/// `(channel count, layout mask)` for mapping families 0 and 1, in Vorbis
/// channel order. RFC 7845 §5.1.1.2.
const VORBIS_LAYOUTS: [(u8, u64); 8] = [
    (1, 0x4),   // mono
    (2, 0x3),   // stereo
    (3, 0x7),   // 3.0
    (4, 0x33),  // quad
    (5, 0x37),  // 5.0
    (6, 0x3f),  // 5.1
    (7, 0x70f), // 6.1
    (8, 0x63f), // 7.1
];

/// The ambisonic order a channel count implies, and whether a non-diegetic
/// stereo pair follows the ACN components.
///
/// RFC 8486 §3: family 2 carries `(n + 1)^2` ambisonic channels, optionally
/// followed by two more. Everything else is invalid, which the reference
/// enforces — `Channel mapping 2 is only specified for channel counts which
/// are (n + 1)^2 or (n + 1)^2 + 2`.
#[must_use]
pub fn ambisonic_order(channels: u8) -> Option<(u16, bool)> {
    for extra in [false, true] {
        let acn = u32::from(channels).checked_sub(if extra { 2 } else { 0 })?;
        if acn == 0 {
            continue;
        }
        let root = acn.isqrt();
        if root * root == acn {
            return u16::try_from(root - 1).ok().map(|order| (order, extra));
        }
    }
    None
}
