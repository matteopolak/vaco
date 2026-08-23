//! ADTS — the self-framing transport for raw AAC.
//!
//! ISO/IEC 14496-3 subpart 4 §4.4.1.1: `adts_frame()`, `adts_fixed_header()`,
//! `adts_variable_header()` and `adts_error_check()`.
//!
//! ```text
//!  bit  0        12 13  15 16              28 30                   58
//!      | syncword | ID | L | P | profile | sfi | … | aac_frame_length | …
//!      \_______________ adts_fixed_header ______/ \__ variable ______/
//! ```
//!
//! # Resynchronisation
//!
//! A twelve-bit sync word occurs by chance roughly once every 4 KiB of random
//! data, so accepting the first `0xFFF` is how a parser gets a false-positive
//! storm on corrupt input. [`AdtsParser`] therefore validates the whole
//! candidate header — layer, sampling-frequency index and declared frame length
//! — and, while it is not yet in sync, additionally requires a second sync word
//! exactly `aac_frame_length` bytes later.

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Parser, Profile};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::asc::{AudioObjectType, AudioSpecificConfig};
use crate::tables;

/// Bytes in `adts_fixed_header()` plus `adts_variable_header()`.
pub const HEADER_LEN: usize = 7;

/// Bytes the CRC adds when `protection_absent` is zero.
pub const CRC_LEN: usize = 2;

/// The largest value `aac_frame_length` can hold — it is a 13-bit field, and it
/// counts the header as well as the payload.
pub const MAX_FRAME_LEN: usize = (1 << 13) - 1;

/// Which MPEG generation the `ID` bit names.
///
/// The reference ignores this for everything it reports: an ADTS stream marked
/// MPEG-2 prints the same profile, rate and channel count as the same stream
/// marked MPEG-4. It is preserved because it is in the syntax, not because it
/// changes an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVersion {
    /// `ID == 0`.
    Mpeg4,
    /// `ID == 1`.
    Mpeg2,
}

/// A parsed ADTS header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the syntax has six one-bit flags and a header struct that renamed \
              or bundled them would stop matching ISO/IEC 14496-3 Table 4.55"
)]
pub struct AdtsHeader {
    /// The `ID` bit.
    pub version: MpegVersion,
    /// `protection_absent`: when false a 16-bit CRC follows the header.
    pub protection_absent: bool,
    /// The object type, which is `profile_ObjectType + 1`.
    pub object_type: AudioObjectType,
    /// `sampling_frequency_index`, always 0..=12 in an accepted header.
    pub sampling_frequency_index: u8,
    /// The rate that index names.
    pub sampling_frequency: u32,
    /// `private_bit`.
    pub private_bit: bool,
    /// `channel_configuration`, a three-bit field, so 0..=7. ADTS cannot
    /// express the 11..=14 configurations an `AudioSpecificConfig` can.
    pub channel_configuration: u8,
    /// `original_copy`.
    pub original_copy: bool,
    /// `home`.
    pub home: bool,
    /// `copyright_identification_bit`.
    pub copyright_id_bit: bool,
    /// `copyright_identification_start`.
    pub copyright_id_start: bool,
    /// `aac_frame_length` — the whole frame, header included.
    pub frame_length: u16,
    /// `adts_buffer_fullness`. `0x7FF` means variable bit rate.
    pub buffer_fullness: u16,
    /// `number_of_raw_data_blocks_in_frame + 1`, so 1..=4.
    ///
    /// The reference frames on `aac_frame_length` alone and ignores this, so it
    /// does not enter [`AdtsHeader::header_len`].
    pub raw_data_blocks: u8,
}

impl AdtsHeader {
    /// Whether `data` opens with something that could be an ADTS header.
    ///
    /// Two bytes are enough: the twelve-bit sync word and the two-bit `layer`
    /// field, which must be zero. Used by the resynchroniser to check the
    /// *next* frame's start without parsing it.
    #[must_use]
    pub fn looks_like_sync(data: &[u8]) -> bool {
        match data {
            [a, b, ..] => *a == 0xff && (*b & 0xf0) == 0xf0 && (*b & 0x06) == 0,
            _ => false,
        }
    }

    /// Parse the header at the start of `data`.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] when fewer than [`HEADER_LEN`] bytes are
    /// available, and [`Error::InvalidData`] when the sync word, the `layer`
    /// field, the sampling-frequency index or the declared frame length is not
    /// one an ADTS frame can carry.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < HEADER_LEN {
            return Err(Error::UnexpectedEof);
        }
        let mut r = BitReader::new(data);
        if r.get(12) != 0xfff {
            return Err(Error::InvalidData("ADTS sync word"));
        }
        let version = if r.get_bit() == 0 {
            MpegVersion::Mpeg4
        } else {
            MpegVersion::Mpeg2
        };
        if r.get(2) != 0 {
            // Any non-zero `layer` means this is not ADTS. The reference does
            // not merely mis-parse such a file, it fails to recognise it as AAC
            // at all — probed with `ffprobe` on a stream whose layer bits were
            // flipped.
            return Err(Error::InvalidData("ADTS layer must be zero"));
        }
        let protection_absent = r.get_bit() != 0;
        let object_type = AudioObjectType(r.get(2) as u8 + 1);
        let sampling_frequency_index = r.get(4) as u8;
        let Some(sampling_frequency) = tables::frequency_for_index(sampling_frequency_index) else {
            return Err(Error::InvalidData("reserved ADTS sampling_frequency_index"));
        };
        let private_bit = r.get_bit() != 0;
        let channel_configuration = r.get(3) as u8;
        let original_copy = r.get_bit() != 0;
        let home = r.get_bit() != 0;
        let copyright_id_bit = r.get_bit() != 0;
        let copyright_id_start = r.get_bit() != 0;
        let frame_length = r.get(13) as u16;
        let buffer_fullness = r.get(11) as u16;
        let raw_data_blocks = r.get(2) as u8 + 1;
        r.check()?;

        let header = Self {
            version,
            protection_absent,
            object_type,
            sampling_frequency_index,
            sampling_frequency,
            private_bit,
            channel_configuration,
            original_copy,
            home,
            copyright_id_bit,
            copyright_id_start,
            frame_length,
            buffer_fullness,
            raw_data_blocks,
        };
        if usize::from(header.frame_length) < header.header_len() {
            return Err(Error::InvalidData(
                "ADTS aac_frame_length is shorter than its own header",
            ));
        }
        Ok(header)
    }

    /// Header bytes, including the CRC when one is present.
    ///
    /// `adts_error_check()` also defines `raw_data_block_position[]` entries for
    /// multi-block frames, which would make the header longer still. The
    /// reference does not account for them and neither do we — see the
    /// divergence note in `docs/codec/vaco-parse-aac.md`.
    #[must_use]
    pub const fn header_len(&self) -> usize {
        if self.protection_absent {
            HEADER_LEN
        } else {
            HEADER_LEN + CRC_LEN
        }
    }

    /// Payload bytes: `aac_frame_length` minus the header.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        (self.frame_length as usize).saturating_sub(self.header_len())
    }

    /// Whether the buffer fullness field means "variable bit rate".
    #[must_use]
    pub const fn is_vbr(&self) -> bool {
        self.buffer_fullness == 0x7ff
    }

    /// The channel count the header implies, or `None` for
    /// `channel_configuration == 0`, where the count lives in a program config
    /// element inside the payload.
    #[must_use]
    pub fn channels(&self) -> Option<u32> {
        tables::channels_for_config(self.channel_configuration)
    }

    /// The profile a container reports for this stream.
    #[must_use]
    pub fn profile(&self) -> Option<Profile> {
        self.object_type.profile()
    }

    /// The equivalent `AudioSpecificConfig` bytes.
    ///
    /// This is the conversion MP4 muxing needs: an `esds`
    /// `DecoderSpecificInfo` built from an ADTS header carries exactly the
    /// object type, sampling-frequency index and channel configuration the
    /// header declared, and nothing about SBR — which is why remuxing HE-AAC
    /// out of ADTS and into MP4 loses the extension signalling.
    #[must_use]
    pub const fn to_audio_specific_config(&self) -> [u8; 2] {
        let ot = self.object_type.0 & 0x1f;
        let sfi = self.sampling_frequency_index & 0x0f;
        let cc = self.channel_configuration & 0x0f;
        [(ot << 3) | (sfi >> 1), ((sfi & 1) << 7) | (cc << 3)]
    }

    /// Fold the header into the parameters a container reports.
    #[must_use]
    pub fn to_codec_parameters(&self) -> CodecParameters {
        let mut params = CodecParameters::audio().with_codec(CodecId::Aac);
        params.profile = self.profile();
        params.audio = Some(AudioParameters {
            sample_rate: self.sampling_frequency,
            // The decoder's output format, as in `asc.rs` — see the note
            // there. Same answer from an ADTS header as from an
            // `AudioSpecificConfig`, measured on the same content in MPEG-TS
            // and MP4.
            format: Some(vaco_sampfmt::SampleFmt::F32P),
            layout: tables::layout_for_config(self.channel_configuration)
                .or_else(|| self.channels().map(ChannelLayout::unspecified)),
            // A compressed codec states no stored depth; the container may,
            // and fills this in through `fill_from`.
            bits_per_coded_sample: None,
            bits_per_raw_sample: None,
            initial_padding: 0,
        });
        params
    }
}

/// Splits an ADTS byte stream into frames.
///
/// Each emitted [`Packet`] is a **whole ADTS frame, header included** — the
/// packetisation the reference produces, confirmed by comparing
/// `ffprobe -show_packets` positions and sizes against the frame lengths in the
/// headers themselves.
#[derive(Debug)]
pub struct AdtsParser {
    header: Option<AdtsHeader>,
    params: Option<CodecParameters>,
    budget: Budget,
    /// A candidate frame that is waiting for the sync word that would confirm
    /// it. Held so that the **last frame of a stream** can still be emitted:
    /// there is nothing after it to confirm it with, and the reference does
    /// accept a file containing a single ADTS frame (probed with `-f aac`).
    ///
    /// Bounded by [`MAX_FRAME_LEN`], which is the largest value the 13-bit
    /// `aac_frame_length` field can hold, so this cannot grow with the input.
    deferred: Vec<u8>,
    /// Whether the last frame was accepted, which is what lets the final frame
    /// of a file be emitted without a following sync word to confirm it.
    synced: bool,
    /// The `AudioSpecificConfig` [`Parser::set_extradata`] supplied, if any.
    ///
    /// When one arrived, that description wins over anything an ADTS header
    /// claims. The reason is not preference but *correctness*: in MP4 and
    /// Matroska the samples are raw AAC with no ADTS header at all, so any sync
    /// word the scanner finds in them is a coincidence, and a coincidence must
    /// not be allowed to overwrite a configuration record the container stated.
    ///
    /// Kept whole rather than reduced to a `configured` flag because
    /// [`Parser::packet_duration`] needs two fields off it — `frame_length()`
    /// and the *core* `sampling_frequency` — that `CodecParameters` does not
    /// carry.
    config: Option<AudioSpecificConfig>,
    frames: u64,
    resyncs: u64,
}

impl AdtsParser {
    /// A parser that allocates packets against `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            header: None,
            params: None,
            budget: Budget::new(limits),
            deferred: Vec::new(),
            synced: false,
            config: None,
            frames: 0,
            resyncs: 0,
        }
    }

    /// The most recent header, once one has been accepted.
    #[must_use]
    pub const fn header(&self) -> Option<&AdtsHeader> {
        self.header.as_ref()
    }

    /// Frames emitted so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// How many times the parser lost sync and had to scan for a new header.
    ///
    /// A conformance signal rather than a curiosity: a clean stream resyncs
    /// once, at the start.
    #[must_use]
    pub const fn resyncs(&self) -> u64 {
        self.resyncs
    }

    /// Forget the current sync position, as after a seek.
    ///
    /// The parameters survive, because a sampling rate does not change when the
    /// reader jumps; the sync does not, because the new position is mid-frame
    /// until proven otherwise.
    pub fn reset(&mut self) {
        self.synced = false;
        self.header = None;
        self.deferred.clear();
    }

    fn accept(&mut self, header: AdtsHeader) {
        if !self.synced {
            self.resyncs = self.resyncs.saturating_add(1);
        }
        if self.header != Some(header) && self.config.is_none() {
            self.params = Some(header.to_codec_parameters());
        }
        self.header = Some(header);
        self.synced = true;
        self.frames = self.frames.saturating_add(1);
    }
}

impl Parser for AdtsParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            // End of stream: emit the frame that was waiting for a sync word
            // that will now never arrive. `used` stays zero, which is what the
            // driver's end-of-stream convention requires.
            if self.deferred.is_empty() {
                return Ok((None, 0));
            }
            let header = AdtsHeader::parse(&self.deferred)?;
            let mut packet = Packet::from_slice(&mut self.budget, &self.deferred)?;
            packet.flags = PacketFlags::KEY;
            self.deferred.clear();
            self.accept(header);
            return Ok((Some(packet), 0));
        }
        // Any call with real input supersedes whatever was deferred: either the
        // candidate is confirmed below, or it is rejected, or it is deferred
        // again.
        self.deferred.clear();

        let mut i = 0usize;
        while let Some(rest) = input.get(i..) {
            if rest.len() < HEADER_LEN {
                break;
            }
            let Ok(header) = AdtsHeader::parse(rest) else {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            };
            let frame_len = usize::from(header.frame_length);
            if rest.len() < frame_len {
                // The header is plausible but the frame is not all here yet.
                // Consume whatever preceded it so the driver makes progress.
                return Ok((None, i));
            }
            // While out of sync, demand a second sync word where this frame
            // says the next one starts, and wait rather than guess when those
            // bytes have not arrived. Once in sync, take frames as they come —
            // otherwise the last frame of a stream could never be emitted.
            //
            // The condition is *only* `synced`, deliberately: every path that
            // advances `i` clears it first, so this decision depends on the
            // stream position and not on how the bytes were chunked. That is
            // what makes `parse_aac_adts`'s chunk-invariance property hold.
            if !self.synced {
                let next = rest.get(frame_len..).unwrap_or_default();
                match next {
                    [a, b, ..] if !AdtsHeader::looks_like_sync(&[*a, *b]) => {
                        i = advance_to_sync(input, i + 1);
                        continue;
                    }
                    [a] if *a != 0xff => {
                        i = advance_to_sync(input, i + 1);
                        continue;
                    }
                    [] | [_] => {
                        if let Some(frame) = rest.get(..frame_len) {
                            self.deferred.extend_from_slice(frame);
                        }
                        return Ok((None, i));
                    }
                    _ => {}
                }
            }

            let Some(frame) = rest.get(..frame_len) else {
                return Err(Error::InvalidData("ADTS frame slice out of range"));
            };
            let mut packet = Packet::from_slice(&mut self.budget, frame)?;
            packet.flags = PacketFlags::KEY;
            self.accept(header);
            return Ok((Some(packet), i + frame_len));
        }

        // No usable header. `i` stops `HEADER_LEN - 1` bytes short of the end,
        // so a header straddling the chunk boundary survives into the next call.
        Ok((None, i.min(input.len())))
    }

    /// Read an `AudioSpecificConfig` — the `esds` `DecoderSpecificInfo` in MP4,
    /// `CodecPrivate` in Matroska.
    ///
    /// AAC is the codec where the two paths genuinely differ. In MPEG-TS every
    /// frame carries an ADTS header and [`Parser::parse`] finds everything; in
    /// MP4 the samples are *raw* AAC and the whole description — object type,
    /// sampling frequency, channel configuration — is in the configuration
    /// record. Measured on `a.m4a`: `profile`, `sample_fmt`, `channels` and
    /// `channel_layout` all arrive here and none of them arrives from a packet.
    ///
    /// # Errors
    ///
    /// Whatever [`AudioSpecificConfig::parse`] returns.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        let config = AudioSpecificConfig::parse(extradata)?;
        self.params = Some(config.to_codec_parameters());
        self.config = Some(config);
        Ok(())
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// One frame's worth of samples over the **core** sampling frequency.
    ///
    /// AAC states its packet duration in two incompatible places depending on
    /// how the container framed it, and the split is the same one
    /// [`Parser::set_extradata`]'s note describes:
    ///
    /// * **Configured** (MP4 `esds`, Matroska `CodecPrivate`) — the samples are
    ///   raw AAC with no header of their own, so the answer is a stream
    ///   constant read off the `AudioSpecificConfig`: `frame_length()` over
    ///   `sampling_frequency`. The bytes are not consulted at all, and must not
    ///   be: a sync word found inside raw AAC is a coincidence.
    /// * **In-band** (MPEG-TS, a raw `.aac` file) — every frame carries an ADTS
    ///   header, so the payload is walked and each frame's
    ///   `raw_data_blocks × 1024` samples are summed at that header's rate. A
    ///   PES payload holding two frames therefore reports twice one frame,
    ///   which is what makes this right for a container that does not
    ///   re-frame.
    ///
    /// # SBR is not a special case here, and that is the point
    ///
    /// The brief for this work said "1024 samples per frame, or 2048 for SBR".
    /// Both are true and they are the *same duration*: SBR doubles the output
    /// rate along with the sample count. Answering in seconds off the core rate
    /// makes 1024/22050 and 2048/44100 the same value, so nothing downstream
    /// has to know whether SBR is signalled — which matters, because a caller
    /// reaching for `sample_rate` gets the reported *extension* rate and would
    /// halve every duration. `AudioSpecificConfig::frame_length` counts core
    /// samples and `sampling_frequency` is the core rate, so the pair agrees by
    /// construction.
    ///
    /// # Measured
    ///
    /// `ffprobe 8.1`, one 1 s sine per row, no `DefaultDuration` in either
    /// Matroska file:
    ///
    /// | file | stream base | reference | exact value |
    /// |---|---|---:|---|
    /// | AAC 44100 in Matroska | 1/1000 | 23 | 1024/44100 s |
    /// | AAC 48000 in Matroska | 1/1000 | 21 | 1024/48000 s |
    /// | AAC 44100 in MPEG-TS | 1/90000 | **2089** | 1024/44100 s |
    /// | AAC 44100 in MP4 | 1/44100 | 1024 | from `stts`, not from here |
    /// | AAC 22050 in MP4 | 1/22050 | 1024 | from `stts`, not from here |
    ///
    /// The MPEG-TS row is a rounding witness as well as a value: 1024 × 90000 ÷
    /// 44100 is 2089.79, and the reference prints the truncation.
    ///
    /// **One divergence is left open deliberately.** The reference prints
    /// `duration=N/A` on the *first* packet of an AAC track in Matroska and the
    /// codec-derived value on every packet after it — reproduced on four files,
    /// including one with `CodecDelay` patched to zero so no priming is
    /// involved, and it is per-track rather than per-file. Opus in the same
    /// container has a duration on its first packet, and so do FLAC in Matroska
    /// and AAC in MPEG-TS, so the pattern is "the answer comes from the
    /// configuration rather than from the packet". Reproducing it would mean
    /// teaching this trait to report *where* its answer came from, to serve one
    /// field per track. Recorded in `docs/codec/vaco-parse-aac.md` instead.
    ///
    /// # `number_of_raw_data_blocks_in_frame`
    ///
    /// Counted, at one frame of `frame_length` samples each — ISO/IEC 14496-3
    /// makes every raw data block a full frame. Unmeasured against the
    /// reference: no encoder reachable here emits more than one block per
    /// frame, and [`AdtsHeader::raw_data_blocks`]'s own note records that the
    /// reference ignores the field for *framing*. Following the specification
    /// is what D17 asks for where the behaviour is not observable.
    fn packet_duration(&self, packet: &[u8]) -> Option<vaco_core::Rational> {
        if let Some(config) = self.config.as_ref() {
            return duration(config.frame_length(), config.sampling_frequency);
        }
        // In-band: walk the frames this payload holds. Each step advances by
        // `frame_length`, which `AdtsHeader::parse` has already checked is at
        // least the header length, so the loop is bounded by the payload size.
        let mut samples = 0u32;
        let mut rate = 0u32;
        let mut rest = packet;
        while let Ok(header) = AdtsHeader::parse(rest) {
            let step = usize::from(header.frame_length);
            let Some(next) = rest.get(step..) else { break };
            samples = samples.saturating_add(
                u32::from(header.raw_data_blocks).saturating_mul(ADTS_FRAME_SAMPLES),
            );
            rate = header.sampling_frequency;
            rest = next;
        }
        duration(samples, rate)
    }
}

/// Samples per raw data block in an ADTS frame.
///
/// Always 1024: ADTS carries no `frameLengthFlag`, so the 960-sample variant
/// cannot be signalled in-band at all. The configured path reads
/// [`AudioSpecificConfig::frame_length`] instead, which does honour it.
const ADTS_FRAME_SAMPLES: u32 = 1024;

/// `samples / rate` seconds, or `None` for anything that is not a duration.
pub(crate) fn duration(samples: u32, rate: u32) -> Option<vaco_core::Rational> {
    let num = i32::try_from(samples).ok()?;
    let den = i32::try_from(rate).ok()?;
    if num <= 0 || den <= 0 {
        return None;
    }
    Some(vaco_core::Rational::new(num, den))
}

/// The next offset at or after `from` that could begin a sync word.
///
/// Scanning for the first byte before attempting a full header parse keeps a
/// buffer of random data linear rather than quadratic-with-a-big-constant.
fn advance_to_sync(input: &[u8], from: usize) -> usize {
    match input.get(from..) {
        Some(rest) => from + rest.iter().position(|&b| b == 0xff).unwrap_or(rest.len()),
        None => input.len(),
    }
}
