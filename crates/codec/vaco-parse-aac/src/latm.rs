//! LATM payload multiplexing and its LOAS `AudioSyncStream` framing.
//!
//! ISO/IEC 14496-3 subpart 4 §1.7: `AudioSyncStream()` (Table 1.28),
//! `AudioMuxElement()` (Table 1.41), `StreamMuxConfig()` (Table 1.42) and
//! `LatmGetValue()` (Table 1.44).
//!
//! The interesting difference from ADTS is that the `AudioSpecificConfig` is
//! carried **in band**, inside `StreamMuxConfig`, so a LATM stream describes
//! itself completely — SBR signalling included — without a container.
//!
//! # Scope
//!
//! [`LoasParser`] frames `AudioSyncStream` elements and reads their
//! `StreamMuxConfig`. It does **not** de-multiplex `PayloadMux()` into
//! individual access units: each emitted packet is the whole
//! `AudioSyncStream` frame, sync word included, which is the packetisation the
//! reference produces (probed by comparing `ffprobe -show_packets` positions
//! and sizes against the `audioMuxLengthBytes` chain).

use vaco_bitstream::BitReader;
use vaco_codec_core::{CodecParameters, Parser};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::asc::{AudioObjectType, AudioSpecificConfig};

/// The 11-bit LOAS sync word.
pub const SYNC_WORD: u16 = 0x2b7;

/// Bytes in the `AudioSyncStream` header: 11 bits of sync plus a 13-bit length.
pub const SYNC_HEADER_LEN: usize = 3;

/// The largest `audioMuxLengthBytes` a 13-bit field can hold.
pub const MAX_MUX_LENGTH: usize = (1 << 13) - 1;

/// The most streams a `StreamMuxConfig` can describe: 16 programs of 8 layers.
const MAX_STREAMS: usize = 16 * 8;

/// How `frameLength` is signalled for one stream. Table 1.42.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameLengthType {
    /// Variable payload length, given per frame by `PayloadLengthInfo()`.
    Variable {
        /// `latmBufferFullness`.
        buffer_fullness: u8,
    },
    /// Fixed payload length, in bytes minus one, as `frameLength` declares.
    Fixed {
        /// The declared `frameLength` field, a 9-bit value.
        frame_length: u16,
    },
    /// A CELP frame-length table index.
    Celp {
        /// `CELPframeLengthTableIndex`.
        index: u8,
    },
    /// An HVXC frame-length table index.
    Hvxc {
        /// `HVXCframeLengthTableIndex`.
        index: u8,
    },
    /// A `frameLengthType` the specification reserves.
    Reserved {
        /// The raw three-bit value.
        value: u8,
    },
}

/// One layer of one program inside a `StreamMuxConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MuxStream {
    /// Program index.
    pub program: u8,
    /// Layer index within the program.
    pub layer: u8,
    /// The configuration this layer uses. `None` when `useSameConfig` said to
    /// reuse the previous layer's.
    pub config: Option<AudioSpecificConfig>,
    /// How this layer's payload length is signalled.
    pub frame_length_type: FrameLengthType,
}

/// A parsed `StreamMuxConfig`. ISO/IEC 14496-3 subpart 4 §1.7.3, Table 1.42.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StreamMuxConfig {
    /// `audioMuxVersion`.
    pub version: u8,
    /// `audioMuxVersionA`. A non-zero value means the rest of the element uses
    /// syntax this specification does not define, so nothing after it is read.
    pub version_a: u8,
    /// `taraBufferFullness`, present only when `audioMuxVersion == 1`.
    pub tara_buffer_fullness: Option<u32>,
    /// `allStreamsSameTimeFraming`.
    pub all_streams_same_time_framing: bool,
    /// `numSubFrames + 1`: how many `PayloadMux()` elements a frame carries.
    pub sub_frames: u8,
    /// `numProgram + 1`.
    pub programs: u8,
    /// Every layer of every program, in bitstream order.
    pub streams: Vec<MuxStream>,
    /// `otherDataLenBits`, or `None` when `otherDataPresent` was zero.
    pub other_data_bits: Option<u32>,
    /// `crcCheckSum`, or `None` when `crcCheckPresent` was zero.
    pub crc: Option<u8>,
}

impl StreamMuxConfig {
    /// Read a `StreamMuxConfig` from a reader positioned at its first bit.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] on truncation, [`Error::InvalidData`] on a
    /// field the syntax cannot produce, and [`Error::Unsupported`] for
    /// `audioMuxVersionA != 0`, which reserves the remaining syntax for a
    /// future amendment.
    pub fn read(r: &mut BitReader<'_>) -> Result<Self> {
        let version = r.get(1) as u8;
        let version_a = if version == 1 { r.get(1) as u8 } else { 0 };
        if version_a != 0 {
            return Err(Error::Unsupported(
                "LATM audioMuxVersionA is reserved for a future amendment",
            ));
        }
        let tara_buffer_fullness = if version == 1 {
            Some(latm_get_value(r)?)
        } else {
            None
        };
        let all_streams_same_time_framing = r.get_bit() != 0;
        let sub_frames = r.get(6) as u8 + 1;
        let programs = r.get(4) as u8 + 1;

        let mut streams: Vec<MuxStream> = Vec::new();
        let mut last_object_type = AudioObjectType::NULL;
        for program in 0..programs {
            let layers = r.get(3) as u8 + 1;
            for layer in 0..layers {
                r.check()?;
                if streams.len() >= MAX_STREAMS {
                    return Err(Error::InvalidData("LATM stream count out of range"));
                }
                let use_same_config = if program == 0 && layer == 0 {
                    false
                } else {
                    r.get_bit() != 0
                };
                let mut config = None;
                if !use_same_config {
                    if version == 0 {
                        let cfg = AudioSpecificConfig::read(r)?;
                        last_object_type = cfg.object_type;
                        config = Some(cfg);
                    } else {
                        // `audioMuxVersion == 1` prefixes the configuration with
                        // its length so a reader that does not understand it can
                        // still skip it. Honour the declared length: it is
                        // authoritative, and trusting our own bit count instead
                        // would desynchronise on any configuration carrying
                        // syntax we stop short of.
                        let asc_bits = latm_get_value(r)?;
                        let start = r.bit_pos();
                        let cfg = AudioSpecificConfig::read(r)?;
                        last_object_type = cfg.object_type;
                        config = Some(cfg);
                        let read = r.bit_pos().saturating_sub(start);
                        let declared = u64::from(asc_bits);
                        if read > declared {
                            return Err(Error::InvalidData(
                                "LATM ascLen is shorter than the configuration it introduces",
                            ));
                        }
                        r.skip_long(declared - read);
                    }
                }
                let frame_length_type =
                    read_frame_length_type(r, all_streams_same_time_framing, last_object_type);
                streams.push(MuxStream {
                    program,
                    layer,
                    config,
                    frame_length_type,
                });
            }
        }

        let other_data_bits = if r.get_bit() != 0 {
            Some(if version == 1 {
                latm_get_value(r)?
            } else {
                read_escaped_other_data_len(r)?
            })
        } else {
            None
        };
        let crc = if r.get_bit() != 0 {
            Some(r.get(8) as u8)
        } else {
            None
        };
        r.check()?;

        Ok(Self {
            version,
            version_a,
            tara_buffer_fullness,
            all_streams_same_time_framing,
            sub_frames,
            programs,
            streams,
            other_data_bits,
            crc,
        })
    }

    /// The configuration of the first layer that carries one.
    ///
    /// That is the stream a container describes: `ffprobe` reports the first
    /// program's first layer and ignores the rest.
    #[must_use]
    pub fn primary_config(&self) -> Option<&AudioSpecificConfig> {
        self.streams.iter().find_map(|s| s.config.as_ref())
    }

    /// Fold the primary configuration into codec parameters.
    #[must_use]
    pub fn to_codec_parameters(&self) -> Option<CodecParameters> {
        self.primary_config()
            .map(AudioSpecificConfig::to_codec_parameters)
    }
}

/// `LatmGetValue()` — a length-prefixed big-endian integer of one to four bytes.
///
/// Table 1.44. The two-bit `bytesForValue` caps the result at 32 bits, so this
/// cannot be made to consume an unbounded amount of input.
fn latm_get_value(r: &mut BitReader<'_>) -> Result<u32> {
    let bytes = r.get(2) + 1;
    let mut value: u32 = 0;
    for _ in 0..bytes {
        value = (value << 8) | r.get(8);
    }
    r.check()?;
    Ok(value)
}

/// The escaped `otherDataLenBits` loop of Table 1.42.
///
/// Each iteration reads nine bits and shifts the accumulator left by eight, so
/// four iterations already overflow a `u32`. The specification puts no bound on
/// the loop; we stop at the point the value stops being representable, which is
/// also the point past which no real stream can go.
fn read_escaped_other_data_len(r: &mut BitReader<'_>) -> Result<u32> {
    let mut bits: u32 = 0;
    for _ in 0..4 {
        bits = bits.wrapping_shl(8);
        let escape = r.get_bit() != 0;
        bits = bits.saturating_add(r.get(8));
        r.check()?;
        if !escape {
            return Ok(bits);
        }
    }
    Err(Error::InvalidData(
        "LATM otherDataLenBits does not terminate",
    ))
}

/// The `frameLengthType` switch of Table 1.42.
fn read_frame_length_type(
    r: &mut BitReader<'_>,
    all_streams_same_time_framing: bool,
    object_type: AudioObjectType,
) -> FrameLengthType {
    let value = r.get(3) as u8;
    match value {
        0 => {
            let buffer_fullness = r.get(8) as u8;
            if !all_streams_same_time_framing
                && matches!(
                    object_type,
                    AudioObjectType::AAC_SCALABLE | AudioObjectType::ER_AAC_SCALABLE
                )
            {
                // `coreFrameOffset`, present only for the scalable object types
                // in a stream that does not share time framing.
                r.skip(6);
            }
            FrameLengthType::Variable { buffer_fullness }
        }
        1 => FrameLengthType::Fixed {
            frame_length: r.get(9) as u16,
        },
        3..=5 => FrameLengthType::Celp {
            index: r.get(6) as u8,
        },
        6 | 7 => FrameLengthType::Hvxc {
            index: r.get(1) as u8,
        },
        _ => FrameLengthType::Reserved { value },
    }
}

/// The `AudioSyncStream` header: a sync word and a payload length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SyncStreamHeader {
    /// `audioMuxLengthBytes`: the `AudioMuxElement` that follows, in bytes.
    pub mux_length: u16,
}

impl SyncStreamHeader {
    /// Whether `data` opens with the LOAS sync word.
    #[must_use]
    pub fn looks_like_sync(data: &[u8]) -> bool {
        matches!(data, [0x56, b, ..] if b & 0xe0 == 0xe0)
    }

    /// Parse the three-byte header at the start of `data`.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] when fewer than [`SYNC_HEADER_LEN`] bytes are
    /// available, [`Error::InvalidData`] when the sync word is absent.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let Some(&[a, b, c]) = data
            .get(..SYNC_HEADER_LEN)
            .and_then(|s| s.first_chunk::<3>())
        else {
            return Err(Error::UnexpectedEof);
        };
        if a != 0x56 || (b & 0xe0) != 0xe0 {
            return Err(Error::InvalidData("LOAS sync word"));
        }
        Ok(Self {
            mux_length: (u16::from(b & 0x1f) << 8) | u16::from(c),
        })
    }

    /// Total frame bytes, header included.
    #[must_use]
    pub const fn frame_len(&self) -> usize {
        SYNC_HEADER_LEN + self.mux_length as usize
    }
}

/// Splits a LOAS byte stream into `AudioSyncStream` frames.
///
/// Each emitted [`Packet`] is a whole frame, sync word included.
#[derive(Debug)]
pub struct LoasParser {
    config: Option<StreamMuxConfig>,
    params: Option<CodecParameters>,
    budget: Budget,
    /// A candidate frame waiting for the sync word that would confirm it. See
    /// `AdtsParser`'s field of the same name: it is what lets the final frame
    /// of a stream be emitted. Bounded by [`MAX_MUX_LENGTH`] plus the header.
    deferred: Vec<u8>,
    synced: bool,
    frames: u64,
    resyncs: u64,
}

impl LoasParser {
    /// A parser that allocates packets against `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            config: None,
            params: None,
            budget: Budget::new(limits),
            deferred: Vec::new(),
            synced: false,
            frames: 0,
            resyncs: 0,
        }
    }

    /// The most recent `StreamMuxConfig`, once one has been read.
    #[must_use]
    pub const fn config(&self) -> Option<&StreamMuxConfig> {
        self.config.as_ref()
    }

    /// Frames emitted so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// How many times the parser had to scan for a new sync word.
    #[must_use]
    pub const fn resyncs(&self) -> u64 {
        self.resyncs
    }

    /// Forget the current sync position, as after a seek. The configuration
    /// survives; a `useSameStreamMux` frame after the seek still needs it.
    pub fn reset(&mut self) {
        self.synced = false;
        self.deferred.clear();
    }

    /// Read the `AudioMuxElement` prologue, updating the cached configuration.
    ///
    /// Returns `Ok(())` even when the frame reuses the previous configuration,
    /// which is what `useSameStreamMux` means.
    fn read_mux_element(&mut self, frame: &[u8]) -> Result<()> {
        let Some(body) = frame.get(SYNC_HEADER_LEN..) else {
            return Err(Error::InvalidData("LOAS frame shorter than its header"));
        };
        let mut r = BitReader::new(body);
        if r.get_bit() != 0 {
            // `useSameStreamMux`: the previous configuration still applies.
            return if self.config.is_some() {
                Ok(())
            } else {
                Err(Error::InvalidData(
                    "LATM frame reuses a StreamMuxConfig that has not been seen",
                ))
            };
        }
        let config = StreamMuxConfig::read(&mut r)?;
        self.params = config.to_codec_parameters();
        self.config = Some(config);
        Ok(())
    }
}

impl Parser for LoasParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            // End of stream: emit the frame that was waiting for confirmation.
            if self.deferred.is_empty() {
                return Ok((None, 0));
            }
            let frame = std::mem::take(&mut self.deferred);
            self.read_mux_element(&frame)?;
            let mut packet = Packet::from_slice(&mut self.budget, &frame)?;
            packet.flags = PacketFlags::KEY;
            self.frames = self.frames.saturating_add(1);
            self.synced = true;
            return Ok((Some(packet), 0));
        }
        self.deferred.clear();
        let mut i = 0usize;
        while let Some(rest) = input.get(i..) {
            if rest.len() < SYNC_HEADER_LEN {
                break;
            }
            let Ok(header) = SyncStreamHeader::parse(rest) else {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            };
            let frame_len = header.frame_len();
            if rest.len() < frame_len {
                return Ok((None, i));
            }
            // See `AdtsParser::parse`: confirmation while out of sync, and
            // the decision depends only on `synced` so that framing does not
            // change with the chunking.
            if !self.synced {
                let next = rest.get(frame_len..).unwrap_or_default();
                match next {
                    [a, b, ..] if !SyncStreamHeader::looks_like_sync(&[*a, *b]) => {
                        i = advance_to_sync(input, i + 1);
                        continue;
                    }
                    [a] if *a != 0x56 => {
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
                return Err(Error::InvalidData("LOAS frame slice out of range"));
            };
            // A frame whose mux element does not parse is not a frame: fall
            // back to scanning rather than emitting rubbish downstream.
            if let Err(e) = self.read_mux_element(frame) {
                if !e.is_recoverable() {
                    return Err(e);
                }
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            }
            let mut packet = Packet::from_slice(&mut self.budget, frame)?;
            packet.flags = PacketFlags::KEY;
            if !self.synced {
                self.resyncs = self.resyncs.saturating_add(1);
            }
            self.synced = true;
            self.frames = self.frames.saturating_add(1);
            return Ok((Some(packet), i + frame_len));
        }
        Ok((None, i.min(input.len())))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }
}

/// The next offset at or after `from` whose byte could begin a sync word.
fn advance_to_sync(input: &[u8], from: usize) -> usize {
    match input.get(from..) {
        Some(rest) => from + rest.iter().position(|&b| b == 0x56).unwrap_or(rest.len()),
        None => input.len(),
    }
}
