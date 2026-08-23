//! The FLV muxer: the 9-byte file header, `onMetaData`, and per-tag codec
//! framing for both legacy and Enhanced RTMP codec signalling.
//!
//! # What this crate reuses from `vaco-demux-flv`
//!
//! [`vaco_demux_flv::AmfValue`] — one AMF0 encoder/decoder for the format,
//! not two (D19). `onMetaData` is written with it directly.
//!
//! # Timestamps, the other way round from `vaco-demux-avi`
//!
//! Unlike AVI, an FLV tag's timestamp is exactly what gets written — there is
//! no clock to derive, only `pts`/`dts` to place in the tag header and
//! `CompositionTime` field. [`Muxer::stream_time_base`] reports
//! milliseconds, so by the time a packet reaches [`FlvMuxer::write_packet`]
//! its `pts`/`dts` are already in the unit this format states directly.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_demux_flv::AmfValue;
use vaco_format_core::{FormatOptions, Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

/// FLV's one time base: milliseconds. See `vaco-demux-flv`'s module docs for
/// why there is no per-stream one to choose instead.
const MS_BASE: Rational = Rational::new(1, 1_000);

/// The registry descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "flv",
    long_name: "FLV (Flash Video)",
    extensions: &["flv"],
    // Measured: `ffmpeg -h muxer=flv` -> flv1 / mp3. Not h264/aac, which is
    // what FLV is usually *used* for and not what the muxer defaults to.
    default_video: Some(CodecId::Flv1),
    default_audio: Some(CodecId::Mp3),
    open: open_muxer,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(FlvMuxer::new(sink, &FormatOptions::default())?))
}

/// How a codec's tags are framed: the legacy 4-bit field, or an Enhanced RTMP
/// `FourCC`.
#[derive(Debug, Clone, Copy)]
enum Framing {
    LegacyVideoAvc,
    LegacyAudio(u8), // SoundFormat
    Enhanced([u8; 4]),
}

#[derive(Debug, Clone, Copy)]
struct StreamOut {
    is_video: bool,
    framing: Framing,
    /// What `write_metadata_tag` needs from the stream's own
    /// `CodecParameters`, which `add_stream` used to discard entirely after
    /// pulling out `extradata` — finding 18
    /// (`planning/CONFORMANCE-FINDINGS.md`): `onMetaData` carried `duration`
    /// and (conditionally) `videocodecid`/`audiocodecid` and nothing else,
    /// because nothing forwarded the rest this far.
    onmeta: OnMetaFields,
}

/// The subset of a stream's `CodecParameters` that survives into
/// `onMetaData`, captured at [`Muxer::add_stream`] time since nothing else
/// keeps the original value around.
#[derive(Debug, Clone, Copy, Default)]
struct OnMetaFields {
    width: u32,
    height: u32,
    /// `0.0` when the source declared no usable frame rate — omitted from
    /// `onMetaData` in that case rather than writing a fabricated `0`.
    frame_rate: f64,
    sample_rate: u32,
    stereo: bool,
    /// `onMetaData`'s `audiosamplesize`: the container's stated bit depth
    /// when it has one, else `16` — measured on AAC (whose own
    /// `bits_per_coded_sample` is `0`, not absent, per
    /// `vaco_codec_core::AudioParameters`'s own doc comment) writing `16`,
    /// which is the value every FLV reader assumes for "not literally
    /// 8-bit PCM" anyway.
    audio_sample_size: u8,
    /// `Some` kbit/s when `CodecParameters::bit_rate` states one — this is
    /// the source's own declared rate, not a byte-count estimate this crate
    /// computes itself, so it is honestly omitted rather than guessed at
    /// when the source did not state one.
    kbit_rate: Option<f64>,
}

/// The FLV muxer.
#[derive(Debug)]
pub struct FlvMuxer {
    out: IoWriter,
    streams: Vec<(StreamOut, Option<Vec<u8>>)>, // (stream state, pending extradata to write as the sequence header)
    video_index: Option<usize>,
    audio_index: Option<usize>,
    header_written: bool,
    trailer_written: bool,
    /// Absolute position of `onMetaData`'s `duration` value, if written and
    /// the sink is seekable — patched at [`FlvMuxer::write_trailer`].
    duration_field_at: Option<u64>,
    /// Absolute position of `onMetaData`'s `filesize` value, mirroring
    /// `duration_field_at` — finding 18: the reference always writes this,
    /// patched to the true final byte count once the whole file exists.
    filesize_field_at: Option<u64>,
    max_timestamp_ms: i64,
}

/// The byte offset, within an already-encoded `onMetaData` tag body, of the
/// raw 8-byte `f64` backing the `Number` value for `key` — found by
/// searching for the field's own encoded key-plus-type-marker bytes rather
/// than computed from a fixed layout, since `write_metadata_tag` emits a
/// different set of keys depending on which streams exist.
fn number_value_offset(body: &[u8], key: &str) -> Option<usize> {
    let key_len = u16::try_from(key.len()).ok()?;
    let mut needle = key_len.to_be_bytes().to_vec();
    needle.extend_from_slice(key.as_bytes());
    needle.push(0x00); // AMF0 Number type marker
    let at = body
        .windows(needle.len())
        .position(|w| w == needle.as_slice())?;
    Some(at + needle.len())
}

impl FlvMuxer {
    /// A muxer over `sink`.
    ///
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, _opts: &FormatOptions) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            streams: Vec::new(),
            video_index: None,
            audio_index: None,
            header_written: false,
            trailer_written: false,
            duration_field_at: None,
            filesize_field_at: None,
            max_timestamp_ms: 0,
        })
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.out.pos()
    }

    fn write_tag_header(&mut self, tag_type: u8, data_size: u32, timestamp_ms: i64) -> Result<()> {
        self.out.w8(tag_type)?;
        self.out.wb24(data_size)?;
        let ts = timestamp_ms.clamp(0, i64::from(u32::MAX));
        #[expect(
            clippy::cast_sign_loss,
            reason = "clamped to [0, u32::MAX] immediately above"
        )]
        let ts = ts as u32;
        self.out.wb24(ts & 0x00FF_FFFF)?;
        self.out.w8(u8::try_from((ts >> 24) & 0xFF).unwrap_or(0))?;
        self.out.wb24(0)?; // StreamID
        Ok(())
    }

    fn write_tag(&mut self, tag_type: u8, timestamp_ms: i64, body: &[u8]) -> Result<()> {
        let size =
            u32::try_from(body.len()).map_err(|_| Error::Unsupported("flv: tag too large"))?;
        self.write_tag_header(tag_type, size, timestamp_ms)?;
        self.out.write(body)?;
        let total = 11u32.saturating_add(size);
        self.out.wb32(total)?; // PreviousTagSize
        Ok(())
    }
}

fn framing_for(is_video: bool, id: CodecId) -> Option<Framing> {
    match (is_video, id) {
        (true, CodecId::H264) => Some(Framing::LegacyVideoAvc),
        (true, CodecId::Hevc) => Some(Framing::Enhanced(*b"hvc1")),
        (true, CodecId::Av1) => Some(Framing::Enhanced(*b"av01")),
        (true, CodecId::Vp9) => Some(Framing::Enhanced(*b"vp09")),
        (false, CodecId::Aac) => Some(Framing::LegacyAudio(10)),
        (false, CodecId::Mp3) => Some(Framing::LegacyAudio(2)),
        (false, CodecId::Pcm) => Some(Framing::LegacyAudio(3)), // "PCM, little-endian"
        (false, CodecId::Opus) => Some(Framing::Enhanced(*b"Opus")),
        (false, CodecId::Flac) => Some(Framing::Enhanced(*b"fLaC")),
        _ => None,
    }
}

impl Muxer for FlvMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "flv: streams must be added before the header is written",
            ));
        }
        let media = params
            .effective_media_type()
            .ok_or(Error::Unsupported("flv: stream has no media type"))?;
        let is_video = match media {
            MediaType::Video => true,
            MediaType::Audio => false,
            _ => return Err(Error::Unsupported("flv: only video and audio streams")),
        };
        if is_video && self.video_index.is_some() {
            return Err(Error::Unsupported("flv: only one video stream"));
        }
        if !is_video && self.audio_index.is_some() {
            return Err(Error::Unsupported("flv: only one audio stream"));
        }
        let codec_id = params
            .codec_id
            .ok_or(Error::Unsupported("flv: stream has no codec id"))?;
        let framing = framing_for(is_video, codec_id)
            .ok_or(Error::Unsupported("flv: codec has no FLV framing"))?;

        let mut onmeta = OnMetaFields::default();
        if is_video {
            if let Some(v) = &params.video {
                onmeta.width = v.width;
                onmeta.height = v.height;
                if v.frame_rate.is_defined() && !v.frame_rate.is_zero() && !v.frame_rate.is_infinite() {
                    onmeta.frame_rate = f64::from(v.frame_rate.num) / f64::from(v.frame_rate.den);
                }
            }
        } else if let Some(a) = &params.audio {
            onmeta.sample_rate = a.sample_rate;
            onmeta.stereo = a.layout.as_ref().is_some_and(|l| l.channels >= 2);
            onmeta.audio_sample_size = a.bits_per_coded_sample.filter(|&b| b > 0).unwrap_or(16);
        }
        onmeta.kbit_rate = params.bit_rate.map(|b| b as f64 / 1000.0);

        let index = u32::try_from(self.streams.len())
            .map_err(|_| Error::Unsupported("flv: too many streams"))?;
        if is_video {
            self.video_index = Some(self.streams.len());
        } else {
            self.audio_index = Some(self.streams.len());
        }
        self.streams.push((
            StreamOut {
                is_video,
                framing,
                onmeta,
            },
            params.extradata.clone(),
        ));
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("flv: header written twice"));
        }
        if self.streams.is_empty() {
            return Err(Error::Unsupported("flv: no streams to mux"));
        }

        self.out.write(b"FLV")?;
        self.out.w8(1)?; // version
        let has_video = self.video_index.is_some();
        let has_audio = self.audio_index.is_some();
        let flags = (u8::from(has_video)) | (u8::from(has_audio) << 2);
        self.out.w8(flags)?;
        self.out.wb32(9)?; // DataOffset
        self.out.wb32(0)?; // PreviousTagSize0

        self.write_metadata_tag()?;

        // Any codec that carries out-of-band configuration writes its
        // sequence-header tag immediately, at timestamp 0, before any real
        // frame — the order every FLV reader relies on.
        let pending: Vec<(StreamOut, Vec<u8>)> = self
            .streams
            .iter()
            .filter_map(|(s, extra)| extra.clone().map(|e| (*s, e)))
            .collect();
        for (s, extra) in pending {
            self.write_sequence_header(s, &extra)?;
        }

        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("flv: packet written before the header"));
        }
        let idx = usize::try_from(packet.stream_index).unwrap_or(usize::MAX);
        let state = self
            .streams
            .get(idx)
            .map(|(s, _)| *s)
            .ok_or(Error::InvalidData("flv: packet names an unknown stream"))?;
        // Finding 19 (`planning/CONFORMANCE-FINDINGS.md`): measured
        // directly (`ffmpeg -i <avi-with-no-pts> -c copy -f flv`, which
        // refuses with "Packet is missing PTS" and a nonzero exit) — a
        // packet with no PTS at all must be refused, not silently written
        // with a fabricated `0`. AVI is the concrete source this bites:
        // its own per-packet model has no PTS field to give.
        let pts_ms = packet
            .pts
            .ticks()
            .ok_or(Error::InvalidData("flv: packet is missing PTS"))?;
        let dts_ms = packet.dts.ticks().unwrap_or(pts_ms);
        self.max_timestamp_ms = self.max_timestamp_ms.max(pts_ms).max(dts_ms);

        if state.is_video {
            let frame_type = if packet.is_key() { 1u8 } else { 2u8 };
            let mut body = Vec::new();
            match state.framing {
                Framing::LegacyVideoAvc => {
                    body.push((frame_type << 4) | 7);
                    body.push(1); // AVCPacketType::Nalu
                    let comp = i32::try_from(pts_ms.saturating_sub(dts_ms)).unwrap_or(0);
                    body.extend_from_slice(&comp.to_be_bytes()[1..]);
                }
                Framing::Enhanced(fourcc) => {
                    // CodedFramesX: never carries a composition time, so
                    // writing it sidesteps the ambiguity
                    // `vaco-demux-flv::tag`'s module docs describe for
                    // `CodedFrames`.
                    body.push(0x80 | (frame_type << 4) | 3);
                    body.extend_from_slice(&fourcc);
                }
                Framing::LegacyAudio(_) => {
                    return Err(Error::InvalidData("flv: video stream has audio framing"));
                }
            }
            body.extend_from_slice(packet.payload());
            self.write_tag(9, dts_ms, &body)?;
        } else {
            let mut body = Vec::new();
            match state.framing {
                Framing::LegacyAudio(format) => {
                    // SoundRate/SoundSize/SoundType are not carried in
                    // `CodecParameters` in a form this crate can read back
                    // faithfully today, so they are fixed at 44 kHz/16-bit/
                    // stereo — cosmetic fields FLV readers do not use to
                    // decide how to decode compressed formats, and `ffprobe`
                    // does not report them at all. See
                    // `docs/format/vaco-mux-flv.md`.
                    body.push((format << 4) | (3 << 2) | (1 << 1) | 1);
                    if format == 10 {
                        body.push(1); // AACPacketType::RawFrame
                    }
                }
                Framing::Enhanced(fourcc) => {
                    body.push(0x90 | 1); // ExAudioHeader, PacketType::CodedFrames
                    body.extend_from_slice(&fourcc);
                }
                Framing::LegacyVideoAvc => {
                    return Err(Error::InvalidData("flv: audio stream has video framing"));
                }
            }
            body.extend_from_slice(packet.payload());
            self.write_tag(8, dts_ms, &body)?;
        }
        Ok(())
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        Some(MS_BASE)
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("flv: trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("flv: trailer written twice"));
        }
        self.trailer_written = true;

        if self.out.is_seekable() {
            let end = self.out.pos();
            if let Some(at) = self.duration_field_at {
                self.out.seek(at)?;
                let seconds = self.max_timestamp_ms as f64 / 1000.0;
                self.out.write(&seconds.to_be_bytes())?;
            }
            // `filesize`: nothing is written after this patch, so `end` —
            // the position before either seek-back — is already the file's
            // true final byte count.
            if let Some(at) = self.filesize_field_at {
                self.out.seek(at)?;
                self.out.write(&(end as f64).to_be_bytes())?;
            }
            self.out.seek(end)?;
        }

        self.out.flush()
    }
}

impl FlvMuxer {
    fn write_metadata_tag(&mut self) -> Result<()> {
        let mut pairs = Vec::new();
        // `duration` and `filesize` are both patched in place once their
        // true values are known at `write_trailer` — everything else here is
        // written once and never touched again.
        pairs.push(("duration".to_owned(), AmfValue::Number(0.0)));

        // Finding 18 (`planning/CONFORMANCE-FINDINGS.md`): `onMetaData` used
        // to carry `duration` and (conditionally) `videocodecid`/
        // `audiocodecid` alone. Measured order for the rest, `-c copy -f
        // flv` on an H.264(+AAC) MP4 source: `width height videodatarate
        // framerate videocodecid`, then (if there is an audio stream)
        // `audiodatarate audiosamplerate audiosamplesize stereo
        // audiocodecid`, then `filesize`. `major_brand`/`minor_version`/
        // `compatible_brands`/`encoder` also appear in that measurement but
        // are MP4-`ftyp`-sourced format tags and an encoder identity string
        // this crate has no channel for (see finding 22) — left out rather
        // than guessed at.
        if let Some(i) = self.video_index
            && let Some((s, _)) = self.streams.get(i)
        {
            pairs.push(("width".to_owned(), AmfValue::Number(f64::from(s.onmeta.width))));
            pairs.push((
                "height".to_owned(),
                AmfValue::Number(f64::from(s.onmeta.height)),
            ));
            if let Some(kbps) = s.onmeta.kbit_rate {
                pairs.push(("videodatarate".to_owned(), AmfValue::Number(kbps)));
            }
            if s.onmeta.frame_rate > 0.0 {
                pairs.push(("framerate".to_owned(), AmfValue::Number(s.onmeta.frame_rate)));
            }
            let id = match s.framing {
                // Enhanced RTMP's codec identity is a FourCC, not a
                // number; `7.0` (AVC) is the closest legacy-compatible
                // best-effort value a reader ignoring FourCCs would fall
                // back to.
                Framing::LegacyVideoAvc | Framing::Enhanced(_) => 7.0,
                Framing::LegacyAudio(_) => 0.0,
            };
            pairs.push(("videocodecid".to_owned(), AmfValue::Number(id)));
        }
        if let Some(i) = self.audio_index
            && let Some((s, _)) = self.streams.get(i)
        {
            if let Some(kbps) = s.onmeta.kbit_rate {
                pairs.push(("audiodatarate".to_owned(), AmfValue::Number(kbps)));
            }
            pairs.push((
                "audiosamplerate".to_owned(),
                AmfValue::Number(f64::from(s.onmeta.sample_rate)),
            ));
            pairs.push((
                "audiosamplesize".to_owned(),
                AmfValue::Number(f64::from(s.onmeta.audio_sample_size)),
            ));
            pairs.push(("stereo".to_owned(), AmfValue::Boolean(s.onmeta.stereo)));
            let id = match s.framing {
                Framing::LegacyAudio(format) => f64::from(format),
                _ => 0.0,
            };
            pairs.push(("audiocodecid".to_owned(), AmfValue::Number(id)));
        }
        pairs.push(("filesize".to_owned(), AmfValue::Number(0.0)));

        let mut body = Vec::new();
        AmfValue::String("onMetaData".to_owned()).encode(&mut body);
        AmfValue::EcmaArray(pairs).encode(&mut body);

        let tag_pos = self.out.pos();
        self.write_tag(18, 0, &body)?;
        if self.out.is_seekable() {
            // `write_tag` wrote an 11-byte tag header, then `body` verbatim,
            // so a byte offset found in `body` sits at `tag_pos + 11 +
            // offset` in the file. Located by search rather than
            // hand-computed arithmetic, since which fields precede
            // `duration`/`filesize` now varies with which streams exist.
            let base = tag_pos + 11;
            self.duration_field_at =
                number_value_offset(&body, "duration").map(|o| base + u64::try_from(o).unwrap_or(0));
            self.filesize_field_at =
                number_value_offset(&body, "filesize").map(|o| base + u64::try_from(o).unwrap_or(0));
        }
        Ok(())
    }

    fn write_sequence_header(&mut self, s: StreamOut, extra: &[u8]) -> Result<()> {
        let mut body = Vec::new();
        match s.framing {
            Framing::LegacyVideoAvc => {
                body.push((1 << 4) | 7); // key frame, AVC
                body.push(0); // AVCPacketType::SequenceHeader
                body.extend_from_slice(&[0, 0, 0]); // CompositionTime
            }
            Framing::LegacyAudio(format) => {
                body.push((format << 4) | (3 << 2) | (1 << 1) | 1);
                body.push(0); // AACPacketType::SequenceHeader (ignored for non-AAC)
            }
            Framing::Enhanced(fourcc) if s.is_video => {
                body.push(0x80 | (1 << 4)); // key frame, PacketType::SequenceStart (0)
                body.extend_from_slice(&fourcc);
            }
            Framing::Enhanced(fourcc) => {
                body.push(0x90); // ExAudioHeader, PacketType::SequenceStart (0)
                body.extend_from_slice(&fourcc);
            }
        }
        body.extend_from_slice(extra);
        let tag_type = if s.is_video { 9 } else { 8 };
        self.write_tag(tag_type, 0, &body)
    }
}
