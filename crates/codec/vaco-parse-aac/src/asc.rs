//! `AudioSpecificConfig` — ISO/IEC 14496-3 subpart 1 §1.6.2.1, Table 1.15.
//!
//! This is the structure MP4 carries in `esds` → `DecoderSpecificInfo`
//! (ISO/IEC 14496-14 §5.6) and that LATM carries inline in its
//! `StreamMuxConfig`. It is where AAC's reported sample rate and channel count
//! actually come from, and where SBR and PS make the *reported* values differ
//! from the *core* ones.
//!
//! # What this module does not do
//!
//! It reads the configuration; it does not decode. Everything past
//! `GASpecificConfig`'s first three flags is skipped rather than interpreted,
//! because nothing beyond it changes what a container reports.

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Profile};
use vaco_core::{Error, Result};

use crate::tables;

/// An MPEG-4 Audio Object Type.
///
/// A newtype rather than an enum because the field is an open space (5 bits,
/// escaping to `31 + <6 bits>`) that later amendments keep extending; a closed
/// enum would reject a stream that is merely newer than we are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct AudioObjectType(pub u8);

impl AudioObjectType {
    /// `NULL` — the "no object type" value.
    pub const NULL: Self = Self(0);
    /// AAC Main.
    pub const AAC_MAIN: Self = Self(1);
    /// AAC Low Complexity: by far the most common.
    pub const AAC_LC: Self = Self(2);
    /// AAC Scalable Sample Rate.
    pub const AAC_SSR: Self = Self(3);
    /// AAC Long Term Prediction.
    pub const AAC_LTP: Self = Self(4);
    /// Spectral Band Replication. Appears as a *wrapper* around a core type.
    pub const SBR: Self = Self(5);
    /// AAC Scalable.
    pub const AAC_SCALABLE: Self = Self(6);
    /// `TwinVQ`.
    pub const TWIN_VQ: Self = Self(7);
    /// Error-Resilient AAC LC.
    pub const ER_AAC_LC: Self = Self(17);
    /// Error-Resilient AAC LTP.
    pub const ER_AAC_LTP: Self = Self(19);
    /// Error-Resilient AAC Scalable.
    pub const ER_AAC_SCALABLE: Self = Self(20);
    /// Error-Resilient `TwinVQ`.
    pub const ER_TWIN_VQ: Self = Self(21);
    /// Error-Resilient BSAC — the one type that carries an
    /// `extensionChannelConfiguration`.
    pub const ER_BSAC: Self = Self(22);
    /// Error-Resilient AAC Low Delay.
    pub const ER_AAC_LD: Self = Self(23);
    /// Parametric Stereo. Like [`SBR`](Self::SBR), a wrapper.
    pub const PS: Self = Self(29);
    /// MPEG-1/2 Layer-1.
    pub const LAYER1: Self = Self(32);
    /// MPEG-1/2 Layer-2.
    pub const LAYER2: Self = Self(33);
    /// MPEG-1/2 Layer-3.
    pub const LAYER3: Self = Self(34);
    /// Error-Resilient AAC Enhanced Low Delay.
    pub const ER_AAC_ELD: Self = Self(39);
    /// Unified Speech and Audio Coding.
    pub const USAC: Self = Self(42);

    /// Whether this type's specific configuration is a `GASpecificConfig`.
    ///
    /// ISO/IEC 14496-3 subpart 1 §1.6.2.1, the `switch (audioObjectType)` in
    /// Table 1.15.
    #[must_use]
    pub const fn has_ga_specific_config(self) -> bool {
        matches!(self.0, 1..=4 | 6 | 7 | 17 | 19..=23)
    }

    /// The profile `ffprobe` reports, which is the object type minus one.
    ///
    /// Probed rather than assumed: `audioObjectType` 1 prints `Main`, 2 prints
    /// `LC`, and 17 prints the bare integer `16` because the name table has no
    /// entry for it. The name table is [`tables::profile_name`].
    #[must_use]
    pub fn profile(self) -> Option<Profile> {
        if self == Self::NULL {
            return None;
        }
        let value = i32::from(self.0) - 1;
        Some(Profile {
            value,
            name: tables::profile_name(value).unwrap_or(""),
        })
    }

    /// Read a `GetAudioObjectType()` — 5 bits, escaping to `32 + <6 bits>`.
    ///
    /// ISO/IEC 14496-3 subpart 1 §1.6.2.1 (`GetAudioObjectType`). Note the
    /// escape adds **32**, not 31: the escape value itself is not reachable
    /// twice, so 31 stays 31 and the extension starts at 32. The escaped value
    /// caps at `32 + 63 = 95`, so it always fits a `u8`.
    fn read(r: &mut BitReader<'_>) -> Self {
        let v = r.get(5);
        Self(if v == 31 { 32 + r.get(6) } else { v } as u8)
    }
}

/// A flag that the bitstream may leave unstated.
///
/// SBR and PS are genuinely three-valued in an `AudioSpecificConfig`: a
/// configuration can say "present", say "absent", or say nothing at all — and
/// the reference treats the third case differently from the second. A `bool`
/// would silently merge [`Unknown`](Signal::Unknown) with
/// [`Absent`](Signal::Absent) and get mono HE-AACv2 wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Signal {
    /// The configuration does not say. For SBR this means "the decoder must
    /// look at the payload"; for PS it means "assume present if SBR is".
    #[default]
    Unknown,
    /// Explicitly signalled absent.
    Absent,
    /// Explicitly signalled present.
    Present,
}

impl Signal {
    /// Whether the flag is anything other than an explicit zero.
    #[must_use]
    pub const fn is_not_absent(self) -> bool {
        !matches!(self, Self::Absent)
    }

    const fn from_flag(bit: u32) -> Self {
        if bit == 0 {
            Self::Absent
        } else {
            Self::Present
        }
    }
}

/// The 11-bit `syncExtensionType` that introduces backward-compatible SBR
/// signalling. ISO/IEC 14496-3 subpart 1 §1.6.2.1.
const SYNC_EXTENSION_SBR: u32 = 0x2b7;

/// The 11-bit `syncExtensionType` that introduces the `psPresentFlag`.
const SYNC_EXTENSION_PS: u32 = 0x548;

/// A parsed `AudioSpecificConfig`.
///
/// `object_type` is the **core** object type: for a hierarchically signalled
/// HE-AAC stream (`audioObjectType` 5 or 29) the syntax nests the real type
/// after the extension fields, and this field holds that inner value. The
/// wrapper is preserved separately in `extension_object_type`. That split is
/// what makes the reported profile `LC` for an HE-AAC stream, which is what the
/// reference prints when it has only the configuration to go on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AudioSpecificConfig {
    /// The core object type, after any SBR/PS wrapper has been unwrapped.
    pub object_type: AudioObjectType,
    /// The wrapper object type — [`AudioObjectType::SBR`] (5) for explicit
    /// backward-compatible SBR signalling, or [`AudioObjectType::NULL`] (0).
    pub extension_object_type: AudioObjectType,
    /// The core `samplingFrequencyIndex`.
    pub sampling_frequency_index: u8,
    /// The core sampling frequency in Hz.
    pub sampling_frequency: u32,
    /// The extension (SBR) sampling frequency in Hz, or `0` when absent.
    pub extension_sampling_frequency: u32,
    /// `channelConfiguration`. Zero means "defined by a program config element
    /// in the payload", which a header parser cannot resolve.
    pub channel_configuration: u8,
    /// `extensionChannelConfiguration`, present only for ER BSAC.
    pub extension_channel_configuration: Option<u8>,
    /// Whether SBR is signalled.
    pub sbr: Signal,
    /// Whether Parametric Stereo is signalled.
    pub ps: Signal,
    /// `frameLengthFlag`: selects the shorter of the type's two frame lengths.
    pub frame_length_flag: bool,
    /// `dependsOnCoreCoder`.
    pub depends_on_core_coder: bool,
    /// `coreCoderDelay`, meaningful only when `depends_on_core_coder` is set.
    pub core_coder_delay: u16,
    /// `extensionFlag`.
    pub extension_flag: bool,
    /// How many bits of the input the configuration occupied. A LATM
    /// `StreamMuxConfig` needs this to reconcile the configuration against the
    /// length its own header declares.
    pub bits_read: u32,
}

impl AudioSpecificConfig {
    /// Parse an `AudioSpecificConfig` from a byte slice — the `esds`
    /// `DecoderSpecificInfo` payload, or a stream's `extradata`.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] when the configuration is truncated, and
    /// [`Error::InvalidData`] when a field is out of range.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = BitReader::new(data);
        let cfg = Self::read(&mut r)?;
        r.check()?;
        Ok(cfg)
    }

    /// Parse from a bit reader positioned at the start of the configuration.
    ///
    /// LATM embeds the configuration mid-bitstream, so the reader — not the
    /// byte slice — is the primitive here.
    ///
    /// # Errors
    ///
    /// As [`AudioSpecificConfig::parse`].
    pub fn read(r: &mut BitReader<'_>) -> Result<Self> {
        let start = r.bit_pos();
        let mut object_type = AudioObjectType::read(r);
        let sampling_frequency_index = r.get(4) as u8;
        let sampling_frequency = read_core_frequency(sampling_frequency_index)?;
        let channel_configuration = r.get(4) as u8;

        let mut sbr = Signal::Unknown;
        let mut ps = Signal::Unknown;
        let mut extension_object_type = AudioObjectType::NULL;
        let mut extension_sampling_frequency = 0;
        let mut extension_channel_configuration = None;

        // Hierarchical signalling: `audioObjectType` 5 or 29 declares the
        // extension up front and nests the core type behind it.
        if object_type == AudioObjectType::SBR || object_type == AudioObjectType::PS {
            extension_object_type = AudioObjectType::SBR;
            sbr = Signal::Present;
            if object_type == AudioObjectType::PS {
                ps = Signal::Present;
            }
            let index = r.get(4) as u8;
            extension_sampling_frequency = read_extension_frequency(r, index)?;
            object_type = AudioObjectType::read(r);
            if object_type == AudioObjectType::ER_BSAC {
                extension_channel_configuration = Some(r.get(4) as u8);
            }
        }

        let mut frame_length_flag = false;
        let mut depends_on_core_coder = false;
        let mut core_coder_delay = 0;
        let mut extension_flag = false;
        if object_type.has_ga_specific_config() {
            frame_length_flag = r.get_bit() != 0;
            depends_on_core_coder = r.get_bit() != 0;
            if depends_on_core_coder {
                core_coder_delay = r.get(14) as u16;
            }
            extension_flag = r.get_bit() != 0;
            // Everything past this point — the program config element, the
            // error-resilience flags, the layer and extension descriptions — is
            // decoder configuration and cannot change what a container reports.
            // A header parser stops here rather than pretending to model it.
        }

        // The backward-compatible sync extension: the trailing
        // `if (extensionAudioObjectType != 5 && bits_to_decode() >= 16)` block
        // of Table 1.15.
        if extension_object_type != AudioObjectType::SBR && r.bits_left() >= 16 {
            let mark = r.mark();
            if r.get(11) == SYNC_EXTENSION_SBR {
                extension_object_type = AudioObjectType::read(r);
                if extension_object_type == AudioObjectType::SBR {
                    sbr = Signal::from_flag(r.get_bit());
                    if sbr == Signal::Present {
                        let index = r.get(4) as u8;
                        extension_sampling_frequency = read_extension_frequency(r, index)?;
                        if r.bits_left() >= 12 && r.get(11) == SYNC_EXTENSION_PS {
                            ps = Signal::from_flag(r.get_bit());
                        }
                    }
                } else if extension_object_type == AudioObjectType::ER_BSAC {
                    sbr = Signal::from_flag(r.get_bit());
                    if sbr == Signal::Present {
                        let index = r.get(4) as u8;
                        extension_sampling_frequency = read_extension_frequency(r, index)?;
                    }
                    extension_channel_configuration = Some(r.get(4) as u8);
                }
            } else {
                // Not a sync extension: the trailing bits are padding, or
                // syntax we do not model. Put them back.
                r.restore(mark);
            }
        }

        r.check()?;
        let bits_read = u32::try_from(r.bit_pos().saturating_sub(start))
            .map_err(|_| Error::InvalidData("AudioSpecificConfig is implausibly long"))?;

        Ok(Self {
            object_type,
            extension_object_type,
            sampling_frequency_index,
            sampling_frequency,
            extension_sampling_frequency,
            channel_configuration,
            extension_channel_configuration,
            sbr,
            ps,
            frame_length_flag,
            depends_on_core_coder,
            core_coder_delay,
            extension_flag,
            bits_read,
        })
    }

    /// The sample rate a container reports for this stream.
    ///
    /// **This is the user-visible contract**, and it is not simply "double the
    /// core rate": it is the *extension* rate, whatever that happens to be.
    /// Probed against `ffprobe 8.1` — a configuration with `sfi = 4` (44100)
    /// and `extensionSamplingFrequencyIndex = 3` reports 48000, not 88200.
    ///
    /// An extension rate of zero falls back to the core rate, which is what the
    /// reference does for an explicit extension rate of `0`.
    #[must_use]
    pub const fn output_sample_rate(&self) -> u32 {
        if matches!(self.sbr, Signal::Present) && self.extension_sampling_frequency != 0 {
            self.extension_sampling_frequency
        } else {
            self.sampling_frequency
        }
    }

    /// The channel count a container reports, or `None` when the configuration
    /// defers to a program config element (`channelConfiguration == 0`) or
    /// names a reserved configuration.
    ///
    /// Parametric Stereo turns a mono core into a stereo output. The condition
    /// was probed, not guessed, and the `Unknown` case is the one that matters:
    ///
    /// | `channelConfiguration` | SBR | PS | reported |
    /// |---|---|---|---|
    /// | 1 | unknown | unknown | 1 |
    /// | 1 | present | unknown | **2** |
    /// | 1 | present | absent | 1 |
    /// | 1 | present | present | 2 |
    /// | 2 | present | present | 2 |
    ///
    /// A mono core with SBR and *no* `psPresentFlag` at all therefore reports
    /// stereo: the reference assumes PS unless the configuration denies it.
    #[must_use]
    pub fn output_channels(&self) -> Option<u32> {
        let base = tables::channels_for_config(self.channel_configuration)?;
        if base == 1 && matches!(self.sbr, Signal::Present) && self.ps.is_not_absent() {
            Some(2)
        } else {
            Some(base)
        }
    }

    /// The layout a container reports, in `vaco-chlayout`'s vocabulary.
    #[must_use]
    pub fn channel_layout(&self) -> Option<ChannelLayout> {
        if self.channel_configuration == 1 && self.output_channels()? == 2 {
            // Parametric Stereo: a mono core presented as stereo.
            return Some(ChannelLayout::STEREO);
        }
        tables::layout_for_config(self.channel_configuration)
    }

    /// Samples per frame at the **core** sampling frequency.
    ///
    /// `frameLengthFlag` selects 1024 or 960 for the general audio types, and
    /// 512 or 480 for the low-delay ones (ISO/IEC 14496-3 subpart 1 §1.6.2.2
    /// and subpart 4's ER AAC LD/ELD definitions).
    #[must_use]
    pub const fn frame_length(&self) -> u32 {
        let low_delay = matches!(
            self.object_type,
            AudioObjectType::ER_AAC_LD | AudioObjectType::ER_AAC_ELD
        );
        match (low_delay, self.frame_length_flag) {
            (false, false) => 1024,
            (false, true) => 960,
            (true, false) => 512,
            (true, true) => 480,
        }
    }

    /// Whether SBR is signalled, hierarchically or through the sync extension.
    #[must_use]
    pub const fn has_sbr(&self) -> bool {
        matches!(self.sbr, Signal::Present)
    }

    /// The profile a container reports.
    ///
    /// Derived from the **core** object type, which is why an HE-AAC stream
    /// described only by its configuration reports `LC`. The reference upgrades
    /// this to `HE-AAC`/`HE-AACv2` only after its decoder has seen an SBR or PS
    /// element in the payload — see the divergence note in
    /// `docs/codec/vaco-parse-aac.md`.
    #[must_use]
    pub fn profile(&self) -> Option<Profile> {
        self.object_type.profile()
    }

    /// Fold the configuration into the parameters a container reports.
    #[must_use]
    pub fn to_codec_parameters(&self) -> CodecParameters {
        let mut params = CodecParameters::audio().with_codec(CodecId::Aac);
        params.profile = self.profile();
        params.audio = Some(AudioParameters {
            sample_rate: self.output_sample_rate(),
            // D17: the decoder's **output** format, which is what the
            // reference prints. `sample_fmt` is `AVCodecParameters::format`,
            // and for a compressed audio stream ffprobe fills it from the
            // decoder's chosen output rather than from anything in the
            // bitstream — there is no sample format in an
            // `AudioSpecificConfig` at all. Measured `fltp` for every AAC
            // stream in the corpus: MP4, MOV, M4A, Matroska and MPEG-TS.
            //
            // A parse-only crate naming a decoder's output format is a real
            // wrinkle, and the alternative is worse: `sample_fmt` is in the
            // D6 byte-identity contract, and leaving it `unknown` diverges on
            // every AAC stream there is.
            format: Some(::vaco_sampfmt::SampleFmt::F32P),
            layout: self
                .channel_layout()
                .or_else(|| self.output_channels().map(ChannelLayout::unspecified)),
            // A compressed codec states no stored depth; the container may,
            // and fills this in through `fill_from`.
            bits_per_coded_sample: None,
            bits_per_raw_sample: None,
            initial_padding: 0,
        });
        params
    }
}

/// Resolve a `samplingFrequencyIndex` in the **core** position.
///
/// # D17: the escape index is rejected here, and only here
///
/// ISO/IEC 14496-3 subpart 1 §1.6.2.4 Table 1.16 defines index 15 as an escape
/// followed by an explicit 24-bit `samplingFrequency`. `ffprobe 8.1` rejects it
/// outright in the core position — `invalid sampling rate index 15`, and the
/// stream vanishes from `-show_streams` — while **accepting** the same escape
/// in the extension (SBR) position, where an explicit 12345 Hz reads back as
/// `sample_rate=12345`.
///
/// We reproduce both halves. This is not a bug to be "fixed" by someone reading
/// the standard: `core_escape_index_is_rejected` and
/// `extension_escape_index_is_accepted` pin the asymmetry so that a change in
/// either direction shows up as a test failure.
fn read_core_frequency(index: u8) -> Result<u32> {
    if index == tables::SAMPLING_FREQUENCY_INDEX_ESCAPE {
        return Err(Error::InvalidData(
            "AAC samplingFrequencyIndex 15 in the core position",
        ));
    }
    tables::frequency_for_index(index)
        .ok_or(Error::InvalidData("reserved AAC samplingFrequencyIndex"))
}

/// Resolve a `samplingFrequencyIndex` in an extension position, where the
/// escape to an explicit 24-bit rate *is* honoured. See the D17 note above.
fn read_extension_frequency(r: &mut BitReader<'_>, index: u8) -> Result<u32> {
    if index == tables::SAMPLING_FREQUENCY_INDEX_ESCAPE {
        return Ok(r.get_long(24) as u32);
    }
    tables::frequency_for_index(index).ok_or(Error::InvalidData(
        "reserved AAC extensionSamplingFrequencyIndex",
    ))
}
