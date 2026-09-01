//! The ASF muxer: Header Object generation, fixed-size-packet
//! packetisation, and the Simple Index Object.
//!
//! # Packetisation, the hard part
//!
//! Every physical Data Packet this muxer writes is exactly
//! [`AsfMuxer::packet_size`] bytes and always uses the *multiple-payload*
//! framing ([\[ASF\] §5.2.3.3](vaco_format_asf)), even when it carries only
//! one payload — measured: `ffmpeg 8.1`'s own `asf`/`asf_stream` muxer does
//! the same, and it means this crate has exactly one payload-serialisation
//! path rather than two. [`PayloadEntry::serialize`] is that one path.
//!
//! A [`Muxer::write_packet`] call hands over one whole media object. Two
//! things can then happen:
//!
//! * **It fits** (with its 17-byte payload header) inside a packet with
//!   whatever is already pending: it joins [`AsfMuxer::pending`], and the
//!   packet is flushed later — when it is full, holds 63 payloads (the
//!   6-bit `Number of Payloads` field's limit), or [`Muxer::write_trailer`]
//!   runs out of packets to fill.
//! * **It does not fit even in an empty packet**: it is split into
//!   consecutive *fragments*, each one alone in its own packet, with
//!   `Offset Into Media Object` tracking how far into the object each
//!   fragment starts — see [`AsfMuxer::write_fragmented`].
//!
//! Every payload — whole object or fragment — carries the same 8-byte
//! Replicated Data (object size + presentation time in ms) on every one of
//! its parts, which is what lets `vaco-demux-asf`'s reassembly know when a
//! fragmented object is complete without needing this crate to say so
//! separately.
//!
//! # What gets patched, and what does not
//!
//! `File Properties`' `Data Packets Count`/`Play Duration`/`Send Duration`
//! and the `Data Object`'s own `Total Data Packets` are placeholders until
//! [`Muxer::write_trailer`], which seeks back and patches them if the sink
//! can seek; a non-seekable sink keeps the placeholders (`0`), the same
//! "the truth is in the index/EOF" convention `vaco-mux-avi` documents for
//! `dwTotalFrames`.

use std::collections::BTreeMap;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_asf::guid::Guid;
use vaco_format_asf::well_known;
use vaco_format_core::mux::BitstreamAction;
use vaco_format_core::{FormatOptions, Muxer, MuxerDesc};
use vaco_format_nalu::{LengthSize, convert::length_prefixed_to_annexb};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::codec;

/// Whether `codec` needs its length-prefixed (`avcC`/`hvcC`-style) samples
/// rewritten to Annex B before they can go in an ASF Data Packet.
///
/// ASF's own [\[ASF\]] spec has no length-prefixed convention for these
/// codecs' payloads — measured directly: `ffmpeg -c copy -f asf` on an
/// `avcC`-framed H.264 MP4 source writes Annex-B-framed samples, and a
/// decoder fed this crate's previous verbatim copy reported "No start code
/// is found" and failed every access unit. Same two codecs `vaco-mux-raw`
/// and `vaco-mux-mpegts` convert, for the same reason; VVC is not included
/// because [`codec::video_fourcc`] has no VVC mapping at all.
const fn needs_annexb_framing(codec: CodecId) -> bool {
    matches!(codec, CodecId::H264 | CodecId::Hevc)
}

/// Whether `payload` already opens with an Annex B start code (`00 00 01` or
/// `00 00 00 01`) — see `vaco-mux-mpegts`'s identical helper for why this
/// makes [`AsfMuxer::maybe_convert`] safe to call unconditionally even after
/// M6 has already reframed the payload.
fn starts_with_annexb_start_code(payload: &[u8]) -> bool {
    payload.starts_with(&[0, 0, 1]) || payload.starts_with(&[0, 0, 0, 1])
}

/// Default fixed Data Packet size, in bytes. Measured: `ffmpeg -h
/// muxer=asf_stream` reports `-packet_size <int> … (default 3200)`, and a
/// dump of its own `-f asf` output uses the same value for `Minimum`/
/// `Maximum Data Packet Size` — this crate adopts it as its own default for
/// the same reason `vaco-mux-avi` adopts measured defaults elsewhere:
/// interoperating with what real content looks like, not because the spec
/// requires this exact number.
pub const DEFAULT_PACKET_SIZE: u32 = 3200;

/// Bytes of fixed per-packet overhead this muxer's own framing always
/// spends: Length Type Flags(1) + Property Flags(1) + Padding Length as a
/// WORD(2) + Send Time(4) + Duration(2) + Payload Flags(1).
const PACKET_FIXED_OVERHEAD: usize = 11;

/// Bytes of per-payload overhead, excluding the payload's own data: Stream
/// Number(1) + Media Object Number as a BYTE(1) + Offset Into Media Object
/// as a DWORD(4) + Replicated Data Length(1) + Replicated Data(8) + Payload
/// Length as a WORD(2).
const PAYLOAD_HEADER_OVERHEAD: usize = 17;

/// `Property Flags`: Replicated Data Length Type=01(BYTE),
/// Offset Into Media Object Length Type=11(DWORD), Media Object Number
/// Length Type=01(BYTE), Stream Number Length Type=01(BYTE) — the
/// spec-recommended values for every one of these ([ASF] §5.2.2), and
/// measured to be the exact byte `ffmpeg 8.1`'s own muxer writes.
const PROPERTY_FLAGS: u8 = 0x5D;

/// `Length Type Flags` for a multiple-payload packet with no error
/// correction, no sequence field, no packet-length field, and a WORD-width
/// Padding Length — again the exact byte measured from `ffmpeg 8.1`.
const LENGTH_TYPE_FLAGS: u8 = 0x11;

/// The maximum payload count the 6-bit `Number of Payloads` field can hold.
const MAX_PAYLOADS_PER_PACKET: usize = 63;

/// The registry descriptor for `asf`.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "asf",
    long_name: "ASF (Advanced / Active Streaming Format)",
    extensions: &["asf", "wmv", "wma"],
    // Measured: `ffmpeg -h muxer=asf` / `=asf_stream` -> msmpeg4v3 / wmav2.
    default_video: Some(CodecId::Msmpeg4v3),
    default_audio: Some(CodecId::Wmav2),
    open: open_muxer,
};

/// The registry descriptor for `asf_stream` — measured (`ffmpeg -h
/// muxer=asf_stream`) to be the same writer with the same default codecs and
/// the same `-packet_size` option; the reference's own two-name split for
/// "file" vs "stream" output is not a format difference this crate's byte
/// layout needs to react to.
pub const MUXER_STREAM: MuxerDesc = MuxerDesc {
    name: "asf_stream",
    long_name: "ASF (Advanced / Active Streaming Format)",
    extensions: &[],
    // Measured: `ffmpeg -h muxer=asf` / `=asf_stream` -> msmpeg4v3 / wmav2.
    default_video: Some(CodecId::Msmpeg4v3),
    default_audio: Some(CodecId::Wmav2),
    open: open_muxer,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(AsfMuxer::new(sink, &FormatOptions::default())?))
}

#[derive(Debug, Clone, Copy)]
struct StreamOut {
    stream_number: u8,
    media_object_counter: u8,
    /// `Some(n)`, `n > 0`: this stream's samples are length-prefixed
    /// (`avcC`/`hvcC` style) with an `n`-byte length and must be rewritten to
    /// Annex B before they can go in a Data Packet — see
    /// [`needs_annexb_framing`]. `None`: already Annex B, or not applicable.
    length_size: Option<LengthSize>,
    /// Set the first time [`AsfMuxer::check_bitstream`] answers for this
    /// stream, mirroring `vaco-mux-mpegts::MuxStream::bsf_decided`.
    bsf_decided: bool,
}

/// One already-length-known payload, ready to be placed in a physical
/// packet.
#[derive(Debug, Clone)]
struct PayloadEntry {
    stream_number: u8,
    key_frame: bool,
    media_object_number: u8,
    /// Byte offset into the media object (`0` for a whole, unfragmented
    /// object).
    offset: u32,
    /// The *whole* media object's total size, for Replicated Data — not
    /// this payload's own (possibly fragment) length.
    object_total_len: u32,
    pts_ms: u32,
    data: Vec<u8>,
}

impl PayloadEntry {
    const fn serialized_len(&self) -> usize {
        PAYLOAD_HEADER_OVERHEAD + self.data.len()
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.stream_number | if self.key_frame { 0x80 } else { 0 });
        out.push(self.media_object_number);
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.push(8); // Replicated Data Length
        out.extend_from_slice(&self.object_total_len.to_le_bytes());
        out.extend_from_slice(&self.pts_ms.to_le_bytes());
        out.extend_from_slice(
            &u16::try_from(self.data.len())
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&self.data);
    }
}

/// One Simple Index Object's worth of collected keyframe positions for one
/// video stream, before the fixed-interval index is derived from it at
/// [`Muxer::write_trailer`].
#[derive(Debug, Clone, Default)]
struct SimpleIndexBuilder {
    /// `(presentation_time_ms, packet_number)`, in the order keyframes were
    /// written (non-decreasing in time, since packets are written in
    /// send-time order).
    keyframes: Vec<(u32, u64)>,
}

/// The ASF muxer.
#[derive(Debug)]
pub struct AsfMuxer {
    out: IoWriter,
    streams: Vec<StreamOut>,
    stream_codec_bytes: Vec<Vec<u8>>, // built at add_stream time, written at write_header
    header_written: bool,
    trailer_written: bool,
    packet_size: u32,
    /// [`vaco_format_asf::header`]'s Creation Date is ticks since
    /// 1601-01-01; this crate never calls the clock for it (`vaco-time`'s
    /// job on `wasm32`, where `SystemTime::now` panics) — `0` ("not
    /// stated") unless a caller supplies one via
    /// [`AsfMuxer::with_creation_date_100ns`].
    creation_date_100ns: u64,
    pending: Vec<PayloadEntry>,
    pending_bytes: usize,
    packets_written: u64,
    max_pts_ms: u32,
    simple_index: BTreeMap<u8, SimpleIndexBuilder>,
    // Patch positions, valid once `write_header` has run.
    file_size_at: u64,
    data_packets_count_at: u64,
    play_duration_at: u64,
    send_duration_at: u64,
    data_object_size_at: u64,
    data_object_total_packets_at: u64,
    /// Bounds [`length_prefixed_to_annexb`]'s output allocation.
    convert_budget: Budget,
}

/// `ASF_Index_Entry_Time_Interval` this crate writes for every Simple Index
/// Object: 1 second, in 100-nanosecond units — the value [\[ASF\]
/// §6.1](vaco_format_asf) itself calls "the most common".
const SIMPLE_INDEX_INTERVAL_100NS: u64 = 10_000_000;

impl AsfMuxer {
    /// A muxer over `sink`, using [`DEFAULT_PACKET_SIZE`].
    ///
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, _opts: &FormatOptions) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            streams: Vec::new(),
            stream_codec_bytes: Vec::new(),
            header_written: false,
            trailer_written: false,
            packet_size: DEFAULT_PACKET_SIZE,
            creation_date_100ns: 0,
            pending: Vec::new(),
            pending_bytes: 0,
            packets_written: 0,
            max_pts_ms: 0,
            simple_index: BTreeMap::new(),
            file_size_at: 0,
            data_packets_count_at: 0,
            play_duration_at: 0,
            send_duration_at: 0,
            data_object_size_at: 0,
            data_object_total_packets_at: 0,
            convert_budget: Budget::new(Limits::permissive()),
        })
    }

    /// Override the fixed Data Packet size (`100..=65536`, per [\[ASF\]
    /// §8.2.14](vaco_format_asf)'s "packet size must be under 64KB").
    ///
    /// # Errors
    /// [`Error::Unsupported`] outside that range.
    pub fn with_packet_size(mut self, size: u32) -> Result<Self> {
        if !(100..=65536).contains(&size) {
            return Err(Error::Unsupported(
                "asf: packet size out of range 100..=65536",
            ));
        }
        self.packet_size = size;
        Ok(self)
    }

    /// Set the File Properties Object's Creation Date, in 100-nanosecond
    /// units since 1601-01-01 00:00:00 UTC. The caller supplies this
    /// (typically converted from a Unix timestamp via `vaco-time`) rather
    /// than this crate reading the clock itself — see the struct docs.
    #[must_use]
    pub const fn with_creation_date_100ns(mut self, ticks: u64) -> Self {
        self.creation_date_100ns = ticks;
        self
    }

    fn stream_out(&self, index: usize) -> Result<&StreamOut> {
        self.streams
            .get(index)
            .ok_or(Error::InvalidData("asf: packet names an unknown stream"))
    }

    fn stream_out_mut(&mut self, index: usize) -> Result<&mut StreamOut> {
        self.streams
            .get_mut(index)
            .ok_or(Error::InvalidData("asf: packet names an unknown stream"))
    }

    /// Add `entry` to the pending packet, flushing first if it does not fit
    /// or the payload cap is reached.
    fn push_entry(&mut self, entry: PayloadEntry) -> Result<()> {
        let would_be = self.pending_bytes + entry.serialized_len();
        if self.pending.len() >= MAX_PAYLOADS_PER_PACKET
            || (!self.pending.is_empty()
                && PACKET_FIXED_OVERHEAD + would_be > self.packet_size as usize)
        {
            self.flush_packet()?;
        }
        self.pending_bytes += entry.serialized_len();
        self.pending.push(entry);
        Ok(())
    }

    /// Write out everything in [`AsfMuxer::pending`] as one physical Data
    /// Packet, padded to [`AsfMuxer::packet_size`].
    fn flush_packet(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let entries = core::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        let send_time_ms = entries.first().map_or(0, |e| e.pts_ms);

        let mut body = Vec::new();
        body.push(LENGTH_TYPE_FLAGS);
        body.push(PROPERTY_FLAGS);
        let padding_at = body.len();
        body.extend_from_slice(&0u16.to_le_bytes()); // padding length, patched below
        body.extend_from_slice(&send_time_ms.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // duration: not tracked, see module docs
        let count = u8::try_from(entries.len()).unwrap_or(u8::MAX);
        body.push(count | 0x80); // payload flags: count | payload-length-type=WORD(10<<6)
        for e in &entries {
            e.serialize(&mut body);
        }
        if body.len() > self.packet_size as usize {
            return Err(Error::Unsupported(
                "asf: packet size too small for the payloads placed in it",
            ));
        }
        let padding = self.packet_size as usize - body.len();
        let Some(slot) = body.get_mut(padding_at..padding_at + 2) else {
            return Err(Error::Unsupported(
                "asf: packet header shorter than its own padding field",
            ));
        };
        slot.copy_from_slice(&u16::try_from(padding).unwrap_or(u16::MAX).to_le_bytes());
        body.resize(self.packet_size as usize, 0);

        let packet_number = self.packets_written;
        for e in &entries {
            if e.key_frame
                && let Some(builder) = self.simple_index.get_mut(&e.stream_number)
            {
                builder.keyframes.push((e.pts_ms, packet_number));
            }
        }

        self.out.write(&body)?;
        self.packets_written += 1;
        Ok(())
    }

    /// Split `data` into consecutive fragments, each alone in its own
    /// physical packet — used when even an empty packet cannot hold the
    /// whole object plus its payload header.
    fn write_fragmented(
        &mut self,
        stream_number: u8,
        key_frame: bool,
        mo_number: u8,
        pts_ms: u32,
        data: &[u8],
    ) -> Result<()> {
        // Any small object still pending must not share a packet with a
        // fragment — flush it first so every packet from here stays
        // single-payload.
        self.flush_packet()?;
        let capacity = (self.packet_size as usize)
            .checked_sub(PACKET_FIXED_OVERHEAD + PAYLOAD_HEADER_OVERHEAD)
            .filter(|&c| c > 0)
            .ok_or(Error::Unsupported(
                "asf: packet size too small to carry any payload at all",
            ))?;
        let total_len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let mut offset = 0usize;
        while offset < data.len() {
            let take = capacity.min(data.len() - offset);
            let chunk = data.get(offset..offset + take).unwrap_or(&[]);
            let entry = PayloadEntry {
                stream_number,
                key_frame,
                media_object_number: mo_number,
                offset: u32::try_from(offset).unwrap_or(u32::MAX),
                object_total_len: total_len,
                pts_ms,
                data: chunk.to_vec(),
            };
            self.push_entry(entry)?;
            self.flush_packet()?;
            offset += take;
        }
        Ok(())
    }

    /// Rewrite `payload` to Annex B if `index`'s stream declared a
    /// length-prefixed framing at [`Muxer::add_stream`] time — the fallback
    /// a caller with no `BsfProvider` still needs, and a no-op once a real
    /// BSF (requested through [`AsfMuxer::check_bitstream`]) has already run,
    /// guarded by [`starts_with_annexb_start_code`]. Mirrors
    /// `vaco-mux-mpegts::MpegTsMuxer::maybe_convert`.
    fn maybe_convert(&mut self, index: usize, payload: &[u8]) -> Result<Vec<u8>> {
        let Some(stream) = self.streams.get(index) else {
            return Ok(payload.to_vec());
        };
        let Some(length_size) = stream.length_size else {
            return Ok(payload.to_vec());
        };
        if starts_with_annexb_start_code(payload) {
            return Ok(payload.to_vec());
        }
        let mut out = Vec::new();
        length_prefixed_to_annexb(payload, length_size, &mut out, &mut self.convert_budget)?;
        Ok(out)
    }
}

impl Muxer for AsfMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "asf: streams must be added before the header is written",
            ));
        }
        let media = params
            .effective_media_type()
            .ok_or(Error::Unsupported("asf: stream has no media type"))?;
        if !matches!(media, MediaType::Video | MediaType::Audio) {
            return Err(Error::Unsupported("asf: only video and audio streams"));
        }
        let codec_id = params
            .codec_id
            .ok_or(Error::Unsupported("asf: stream has no codec id"))?;
        let stream_number = u8::try_from(self.streams.len() + 1)
            .ok()
            .filter(|&n| n <= 127)
            .ok_or(Error::Unsupported("asf: more than 127 streams"))?;

        let type_specific = if media == MediaType::Video {
            let v = params.video.as_ref().ok_or(Error::Unsupported(
                "asf: video stream has no VideoParameters",
            ))?;
            let fourcc = codec::video_fourcc(codec_id)
                .ok_or(Error::Unsupported("asf: codec has no ASF video FourCC"))?;
            build_video_type_specific(v.width, v.height, fourcc)
        } else {
            let a = params.audio.as_ref().ok_or(Error::Unsupported(
                "asf: audio stream has no AudioParameters",
            ))?;
            if a.sample_rate == 0 {
                return Err(Error::Unsupported("asf: audio stream has no sample rate"));
            }
            if codec_id == CodecId::Aac && params.extradata.as_ref().is_none_or(Vec::is_empty) {
                // ASF's AAC carries a raw `AudioSpecificConfig` in the
                // stream properties object, the same convention MP4/`esds`
                // uses — an ADTS-framed source (MPEG-TS's own convention)
                // has no such record to copy, since ADTS repeats the
                // equivalent fields in every frame header instead. Measured
                // against `ffmpeg 9.0.1`: `-c copy -f asf` from an ADTS
                // source fails outright ("ADTS is only supported with codec
                // tag 0x1610"), it does not auto-run `aac_adtstoasc`.
                // Mirrors `vaco-mux-avi`'s identical check for the identical
                // reason.
                return Err(Error::Unsupported(
                    "asf: AAC needs a raw AudioSpecificConfig in extradata; ADTS-framed AAC is not accepted",
                ));
            }
            let tag = codec::audio_format_tag(codec_id)
                .ok_or(Error::Unsupported("asf: codec has no ASF wFormatTag"))?;
            let channels = u16::try_from(a.layout.as_ref().map_or(1, |l| l.channels)).unwrap_or(1);
            let bits = codec::pcm_bits_per_sample(codec_id)
                .unwrap_or_else(|| u16::from(a.bits_per_coded_sample.unwrap_or(16)).max(8));
            let block_align = if codec::is_uncompressed_pcm(codec_id) {
                #[allow(
                    clippy::integer_division,
                    reason = "bytes-per-sample from bits-per-sample is an exact conversion, not a ratio"
                )]
                let bytes_per_sample = (u32::from(bits) / 8).max(1);
                u16::try_from(bytes_per_sample * u32::from(channels)).unwrap_or(u16::MAX)
            } else {
                0
            };
            build_audio_type_specific(tag, channels, a.sample_rate, bits, block_align)
        };

        let length_size = if media == MediaType::Video && needs_annexb_framing(codec_id) {
            params
                .video
                .as_ref()
                .and_then(|v| v.nal_length_size)
                .filter(|&n| n > 0)
                .and_then(LengthSize::new)
        } else {
            None
        };

        let sp = build_stream_properties(stream_number, media, &type_specific);
        self.stream_codec_bytes.push(sp);
        self.streams.push(StreamOut {
            stream_number,
            media_object_counter: 0,
            length_size,
            bsf_decided: false,
        });
        if media == MediaType::Video {
            self.simple_index
                .insert(stream_number, SimpleIndexBuilder::default());
        }
        Ok(u32::try_from(self.streams.len() - 1).unwrap_or(0))
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("asf: header written twice"));
        }
        if self.streams.is_empty() {
            return Err(Error::Unsupported("asf: no streams to mux"));
        }

        let file_properties = build_file_properties(self.packet_size, self.creation_date_100ns);
        let header_extension = build_header_extension();

        let mut header_payload = Vec::new();
        header_payload.extend_from_slice(&object(
            well_known::FILE_PROPERTIES_OBJECT,
            &file_properties,
        ));
        for sp in &self.stream_codec_bytes {
            header_payload.extend_from_slice(&object(well_known::STREAM_PROPERTIES_OBJECT, sp));
        }
        header_payload.extend_from_slice(&object(
            well_known::HEADER_EXTENSION_OBJECT,
            &header_extension,
        ));
        // Header objects are counted as: File Properties + one per stream +
        // Header Extension.
        let num_header_objects = 2 + self.streams.len() as u32;

        self.out.write(&well_known::HEADER_OBJECT.as_bytes())?;
        self.out.wl64(30 + header_payload.len() as u64)?;
        self.out.wl32(num_header_objects)?;
        self.out.w8(1)?; // reserved1
        self.out.w8(2)?; // reserved2
        self.out.write(&header_payload)?;

        // File Properties' patch positions, now that the header's absolute
        // layout is known: `object()` builds `Object ID(16) + Object
        // Size(8)` before the payload, so File Properties' payload starts
        // 30 (header prefix) + 24 (its own object header) bytes in.
        let fp_payload_start = 30 + 24u64;
        self.file_size_at = fp_payload_start + 16; // past File ID(16)
        self.data_packets_count_at = fp_payload_start + 16 + 8 + 8; // + File Size(8) + Creation Date(8)
        self.play_duration_at = self.data_packets_count_at + 8;
        self.send_duration_at = self.play_duration_at + 8;

        self.out.write(&well_known::DATA_OBJECT.as_bytes())?;
        self.data_object_size_at = self.out.pos();
        self.out.wl64(0)?; // patched at write_trailer
        self.out.write(&[0u8; 16])?; // file id
        self.data_object_total_packets_at = self.out.pos();
        self.out.wl64(0)?; // total data packets, patched
        self.out.write(&[1u8, 1])?; // reserved: 0x0101

        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("asf: packet written before the header"));
        }
        let idx = usize::try_from(packet.stream_index).unwrap_or(usize::MAX);
        let (stream_number, key_frame) = {
            let s = self.stream_out(idx)?;
            (s.stream_number, packet.is_key())
        };
        // Every payload's Replicated Data carries one "Presentation Time" in
        // ms ([\[ASF\] §5.2.2](vaco_format_asf)); despite the name, it wants
        // `packet.dts`, not `packet.pts` — measured on a B-frame H.264
        // source, where PTS is not monotonic across `write_packet` calls but
        // a real ASF reader requires it to be, and silently decodes the
        // wrong picture into each slot otherwise.
        let pts_ms = u32::try_from(packet.dts.ticks().unwrap_or(0).max(0)).unwrap_or(u32::MAX);
        self.max_pts_ms = self.max_pts_ms.max(pts_ms);

        let mo_number = {
            let s = self.stream_out_mut(idx)?;
            let n = s.media_object_counter;
            s.media_object_counter = s.media_object_counter.wrapping_add(1);
            n
        };

        let converted = self.maybe_convert(idx, packet.payload())?;
        let data = converted.as_slice();
        let total_len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let whole_entry_len = PAYLOAD_HEADER_OVERHEAD + data.len();
        if PACKET_FIXED_OVERHEAD + whole_entry_len <= self.packet_size as usize {
            let entry = PayloadEntry {
                stream_number,
                key_frame,
                media_object_number: mo_number,
                offset: 0,
                object_total_len: total_len,
                pts_ms,
                data: data.to_vec(),
            };
            self.push_entry(entry)
        } else {
            self.write_fragmented(stream_number, key_frame, mo_number, pts_ms, data)
        }
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map(|_| Rational::new(1, 1000))
    }

    /// Ask M6 for `h264_mp4toannexb`/`hevc_mp4toannexb` when the stream
    /// declared length-prefixed framing — the same condition
    /// [`AsfMuxer::maybe_convert`] uses, and the same shape as
    /// `vaco-mux-mpegts::MpegTsMuxer::check_bitstream`.
    fn check_bitstream(&mut self, params: &CodecParameters, pkt: &Packet) -> Result<BitstreamAction> {
        let idx = usize::try_from(pkt.stream_index).ok();
        if idx.and_then(|i| self.streams.get(i)).is_some_and(|s| s.bsf_decided) {
            return Ok(BitstreamAction::Keep);
        }
        if let Some(s) = idx.and_then(|i| self.streams.get_mut(i)) {
            s.bsf_decided = true;
        }
        let asks_for_splice = matches!(params.codec_id, Some(CodecId::H264 | CodecId::Hevc))
            && params
                .video
                .as_ref()
                .and_then(|v| v.nal_length_size)
                .is_some_and(|n| n > 0);
        if !asks_for_splice {
            return Ok(BitstreamAction::Keep);
        }
        Ok(BitstreamAction::Insert {
            name: match params.codec_id {
                Some(CodecId::Hevc) => "hevc_mp4toannexb",
                _ => "h264_mp4toannexb",
            },
        })
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("asf: trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("asf: trailer written twice"));
        }
        self.trailer_written = true;

        self.flush_packet()?;
        let total_packets = self.packets_written;

        if self.out.is_seekable() {
            let end = self.out.pos();
            // `data_object_size_at` is the position of the Object Size
            // field itself, i.e. 16 bytes (the GUID) past the Data Object's
            // own start — Object Size must include that 24-byte header.
            let data_object_size = end - (self.data_object_size_at - 16);

            self.out.seek(self.data_object_size_at)?;
            self.out.wl64(data_object_size)?;
            self.out.seek(self.data_object_total_packets_at)?;
            self.out.wl64(total_packets)?;

            self.out.seek(self.file_size_at)?;
            self.out.wl64(end)?;
            self.out.seek(self.data_packets_count_at)?;
            self.out.wl64(total_packets)?;
            let duration_100ns = u64::from(self.max_pts_ms) * 10_000;
            self.out.seek(self.play_duration_at)?;
            self.out.wl64(duration_100ns)?;
            self.out.seek(self.send_duration_at)?;
            self.out.wl64(duration_100ns)?;

            self.out.seek(end)?;

            // Simple Index Objects, one per video stream, in ascending
            // stream-number order — the order the spec requires, and the
            // order `vaco-demux-asf` assumes when it cannot otherwise tell
            // which video stream an index belongs to.
            for (&stream_number, builder) in &self.simple_index {
                let bytes = build_simple_index(stream_number, builder, self.max_pts_ms);
                self.out
                    .write(&object(well_known::SIMPLE_INDEX_OBJECT, &bytes))?;
            }
        }

        self.out.flush()
    }
}

fn object(guid: Guid, payload: &[u8]) -> Vec<u8> {
    let mut out = guid.as_bytes().to_vec();
    out.extend_from_slice(&(24 + payload.len() as u64).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn build_file_properties(packet_size: u32, creation_date_100ns: u64) -> Vec<u8> {
    let mut p = vec![0u8; 16]; // file id: left zero, "may be set to 0" per spec
    p.extend_from_slice(&0u64.to_le_bytes()); // file size, patched
    p.extend_from_slice(&creation_date_100ns.to_le_bytes());
    p.extend_from_slice(&0u64.to_le_bytes()); // data packets count, patched
    p.extend_from_slice(&0u64.to_le_bytes()); // play duration, patched
    p.extend_from_slice(&0u64.to_le_bytes()); // send duration, patched
    p.extend_from_slice(&0u64.to_le_bytes()); // preroll: none
    p.extend_from_slice(&0x02u32.to_le_bytes()); // flags: seekable
    p.extend_from_slice(&packet_size.to_le_bytes());
    p.extend_from_slice(&packet_size.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes()); // max bitrate: not tracked
    p
}

fn build_header_extension() -> Vec<u8> {
    let mut p = well_known::RESERVED_1.as_bytes().to_vec();
    p.extend_from_slice(&6u16.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes()); // header extension data size: none
    p
}

fn build_stream_properties(stream_number: u8, media: MediaType, type_specific: &[u8]) -> Vec<u8> {
    let stream_type = if media == MediaType::Video {
        well_known::VIDEO_MEDIA
    } else {
        well_known::AUDIO_MEDIA
    };
    let mut p = stream_type.as_bytes().to_vec();
    p.extend_from_slice(&well_known::NO_ERROR_CORRECTION.as_bytes());
    p.extend_from_slice(&0u64.to_le_bytes()); // time offset
    p.extend_from_slice(&(type_specific.len() as u32).to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes()); // error correction data length
    let flags: u16 = u16::from(stream_number); // stream number, bit 15 (encrypted) clear
    p.extend_from_slice(&flags.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes()); // reserved
    p.extend_from_slice(type_specific);
    p
}

/// [\[ASF\] §9.2](vaco_format_asf): `EncodedImageWidth/Height` +
/// `ReservedFlags(=2)` + `FormatDataSize` + a 40-byte `BITMAPINFOHEADER`.
fn build_video_type_specific(width: u32, height: u32, fourcc: [u8; 4]) -> Vec<u8> {
    let mut bih = Vec::new();
    bih.extend_from_slice(&40u32.to_le_bytes()); // biSize
    bih.extend_from_slice(&width.to_le_bytes());
    bih.extend_from_slice(&height.to_le_bytes());
    bih.extend_from_slice(&1u16.to_le_bytes()); // biPlanes: shall be 1
    bih.extend_from_slice(&24u16.to_le_bytes()); // biBitCount: not otherwise tracked
    bih.extend_from_slice(&codec::fourcc_to_u32(fourcc).to_le_bytes());
    bih.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    bih.extend_from_slice(&0i32.to_le_bytes());
    bih.extend_from_slice(&0i32.to_le_bytes());
    bih.extend_from_slice(&0u32.to_le_bytes());
    bih.extend_from_slice(&0u32.to_le_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.push(2); // reserved flags: shall be 2
    out.extend_from_slice(&u16::try_from(bih.len()).unwrap_or(u16::MAX).to_le_bytes());
    out.extend_from_slice(&bih);
    out
}

/// [\[ASF\] §9.1](vaco_format_asf): `WAVEFORMATEX`, the 18-byte form with
/// `cbSize = 0` — this crate writes no codec-private data.
fn build_audio_type_specific(
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    bits: u16,
    block_align: u16,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&format_tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    let avg_bytes = sample_rate.saturating_mul(u32::from(block_align.max(1)));
    out.extend_from_slice(&avg_bytes.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    out
}

/// Build one Simple Index Object's payload from collected keyframe
/// positions: entry `k` names the closest past keyframe's packet number for
/// time `k * SIMPLE_INDEX_INTERVAL_100NS`.
fn build_simple_index(
    _stream_number: u8,
    builder: &SimpleIndexBuilder,
    max_pts_ms: u32,
) -> Vec<u8> {
    #[allow(
        clippy::integer_division,
        reason = "SIMPLE_INDEX_INTERVAL_100NS is a fixed non-zero constant; this converts it to milliseconds"
    )]
    let interval_ms = SIMPLE_INDEX_INTERVAL_100NS / 10_000;
    let mut entries: Vec<(u32, u16)> = Vec::new();
    if interval_ms > 0 && !builder.keyframes.is_empty() {
        #[allow(
            clippy::integer_division,
            reason = "interval_ms is checked non-zero just above; this counts whole intervals elapsed"
        )]
        let last_tick = u64::from(max_pts_ms) / interval_ms;
        let mut ki = 0usize;
        let mut current_packet = builder.keyframes.first().map_or(0, |&(_, p)| p);
        for k in 0..=last_tick {
            let boundary_ms = k * interval_ms;
            while let Some(&(next_ms, next_packet)) = builder.keyframes.get(ki + 1)
                && u64::from(next_ms) <= boundary_ms
            {
                ki += 1;
                current_packet = next_packet;
            }
            entries.push((u32::try_from(current_packet).unwrap_or(u32::MAX), 1));
        }
    }

    let mut p = vec![0u8; 16]; // file id: ASF parsers may ignore
    p.extend_from_slice(&SIMPLE_INDEX_INTERVAL_100NS.to_le_bytes());
    let max_packet_count = entries.iter().map(|&(_, c)| c).max().unwrap_or(0);
    p.extend_from_slice(&u32::from(max_packet_count).to_le_bytes());
    p.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (packet_number, packet_count) in entries {
        p.extend_from_slice(&packet_number.to_le_bytes());
        p.extend_from_slice(&packet_count.to_le_bytes());
    }
    p
}
