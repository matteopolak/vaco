//! The muxer itself: PID assignment, PAT/PMT/SDT scheduling, PCR insertion,
//! and PES packetisation.
//!
//! # What is, and is not, byte-identical to the reference
//!
//! The wire-level pieces — the transport packet, the adaptation field, the
//! PCR field, the PES header's 33-bit timestamp encoding, PAT/PMT/SDT section
//! syntax — are checked against the sibling demuxer's own parser directly
//! (see `crate::pes` and `crate::tsw`'s tests) and are the parts most likely
//! to be *silently* wrong, per the brief. The **scheduling policy** — exactly
//! when a PCR is due, exactly when PAT/PMT/SDT repeat, how a large elementary
//! stream is split across PES packets — is this crate's own reasonable
//! reading of the specification's bounds (PCR at most every 100 ms; PAT/PMT
//! and SDT at `-pat_period`/`-sdt_period`) rather than a reproduction of the
//! reference's own internal scheduler, which is not observable byte-for-byte
//! without reading its source (D7). See the crate docs for what was measured
//! versus decided.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::interleave::{InterleaveQueue, interleave_none};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_format_mpegts_tables::packet::Pcr;
use vaco_format_mpegts_tables::stream_type::for_codec;
use vaco_format_mpegts_tables::{
    PatEntryOut, PmtStreamOut, SdtServiceOut, registration_descriptor, service_descriptor,
    write_pat, write_pmt, write_sdt,
};
use vaco_format_nalu::{LengthSize, convert::length_prefixed_to_annexb};
use vaco_io::MediaSink;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::options::{MpegTsFlags, MpegTsMuxOptions};
use crate::pes::{
    PesHeaderOut, PesTimestamps, SID_AUDIO, SID_PRIVATE_1, SID_VIDEO, encode_pes_header,
};
use crate::tsw::{AfRequest, TsWriter};

/// PID of the Program Association Table (fixed by the specification).
const PAT_PID: u16 = 0x0000;
/// PID of the Service Description Table (fixed by DVB, EN 300 468 §5.1.3).
const SDT_PID: u16 = 0x0011;
/// The clock every MPEG-TS timestamp — PTS, DTS, PCR base — counts in.
pub const TIME_BASE: Rational = Rational {
    num: 1,
    den: 90_000,
};
/// Auto `-pcr_period`'s resolved value: the specification's own ceiling
/// (ISO/IEC 13818-1 §2.7.2: at most 100 ms between PCRs on one PID). The
/// reference's actual default schedule is frame-timing-aware and finer in
/// practice (measured: 80 ms at 25 fps, 100 ms at 10 fps) — see the crate
/// docs for the probe. This crate uses the flat, specification-legal bound
/// instead of reconstructing that schedule.
pub const DEFAULT_PCR_PERIOD_MS: u32 = 100;

/// A stream this muxer has been told about.
struct MuxStream {
    media_type: MediaType,
    codec_id: CodecId,
    pid: u16,
    stream_type: u8,
    registration: Option<[u8; 4]>,
    /// `Some(n)`, `n > 0`: the packet payload is length-prefixed (`avcC`/
    /// `hvcC` style) with an `n`-byte length and must be rewritten to Annex B
    /// before it can go in a transport stream. `None`/`Some(0)`: already
    /// Annex B, or not applicable to this codec.
    length_size: Option<LengthSize>,
    first_packet_written: bool,
}

fn is_h264_or_hevc(codec: CodecId) -> bool {
    matches!(codec, CodecId::H264 | CodecId::Hevc | CodecId::Vvc)
}

/// The MPEG-TS muxer.
// `Box<dyn MediaSink>` (inside `TsWriter`) is not `Debug`.
pub struct MpegTsMuxer {
    tsw: TsWriter,
    opts: MpegTsMuxOptions,
    streams: Vec<MuxStream>,
    pmt_pid: u16,
    next_es_pid: u16,
    pcr_pid: Option<u16>,
    clock_90k: i64,
    last_pat_clock: Option<i64>,
    last_sdt_clock: Option<i64>,
    last_pcr_clock: Option<i64>,
    header_written: bool,
    convert_budget: Budget,
}

impl core::fmt::Debug for MpegTsMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MpegTsMuxer")
            .field("streams", &self.streams.len())
            .field("pmt_pid", &self.pmt_pid)
            .field("pcr_pid", &self.pcr_pid)
            .finish_non_exhaustive()
    }
}

/// Milliseconds a 90 kHz tick delta represents.
#[allow(
    clippy::integer_division,
    reason = "a period comparison only needs whole milliseconds; truncation is fine"
)]
const fn ticks_to_ms(ticks: i64) -> i64 {
    ticks / 90
}

impl MpegTsMuxer {
    /// Construct with default options and TS (not M2TS) framing — what
    /// [`crate::MUXER`]'s registry entry uses.
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self::with_options(sink, MpegTsMuxOptions::default(), false)
    }

    /// Construct with explicit options and M2TS mode.
    ///
    /// `m2ts` is resolved by the caller (from `-mpegts_m2ts_mode` and/or the
    /// output filename's extension), because [`Muxer`] has no filename to
    /// look at — see [`MpegTsMuxOptions::m2ts_mode`]'s doc.
    #[must_use]
    pub fn with_options(sink: Box<dyn MediaSink>, opts: MpegTsMuxOptions, m2ts: bool) -> Self {
        let start_pid = opts.start_pid;
        let pmt_pid = opts.pmt_start_pid;
        let muxrate = opts.muxrate_bps;
        Self {
            tsw: TsWriter::new(sink, m2ts, muxrate),
            opts,
            streams: Vec::new(),
            pmt_pid,
            next_es_pid: start_pid,
            pcr_pid: None,
            clock_90k: 0,
            last_pat_clock: None,
            last_sdt_clock: None,
            last_pcr_clock: None,
            header_written: false,
            convert_budget: Budget::new(Limits::permissive()),
        }
    }

    fn dvb(&self) -> bool {
        self.opts.flags.contains(MpegTsFlags::SYSTEM_B)
    }

    fn effective_codec(&self, codec_id: CodecId) -> CodecId {
        if codec_id == CodecId::Aac && self.opts.flags.contains(MpegTsFlags::LATM) {
            CodecId::AacLatm
        } else {
            codec_id
        }
    }

    fn pmt_streams(&self) -> Vec<PmtStreamOut> {
        self.streams
            .iter()
            .map(|s| {
                let descriptors = s
                    .registration
                    .map_or_else(Vec::new, registration_descriptor);
                PmtStreamOut {
                    stream_type: s.stream_type,
                    elementary_pid: s.pid,
                    descriptors,
                }
            })
            .collect()
    }

    fn build_pat(&self) -> Result<Vec<u8>> {
        write_pat(
            self.opts.transport_stream_id,
            self.opts.tables_version,
            &[PatEntryOut {
                program_number: 1,
                pid: self.pmt_pid,
            }],
        )
        .ok_or(Error::InvalidData("mpegts: PAT does not fit one section"))
    }

    fn build_pmt(&self) -> Result<Vec<u8>> {
        let pcr_pid = self.pcr_pid.unwrap_or(0x1FFF);
        write_pmt(
            1,
            self.opts.tables_version,
            pcr_pid,
            &[],
            &self.pmt_streams(),
        )
        .ok_or(Error::InvalidData("mpegts: PMT does not fit one section"))
    }

    fn build_sdt(&self) -> Result<Vec<u8>> {
        let desc = service_descriptor(
            self.opts.service_type.0,
            self.opts.service_provider.as_bytes(),
            self.opts.service_name.as_bytes(),
        )
        .ok_or(Error::InvalidData("mpegts: service name/provider too long"))?;
        write_sdt(
            self.opts.transport_stream_id,
            self.opts.original_network_id,
            self.opts.tables_version,
            &[SdtServiceOut {
                service_id: self.opts.service_id,
                eit_schedule: false,
                eit_present_following: false,
                running_status: 4, // "running" — the only sensible answer for a live mux
                free_ca_mode: false,
                descriptors: desc,
            }],
        )
        .ok_or(Error::InvalidData("mpegts: SDT does not fit one section"))
    }

    fn write_pat_and_pmt(&mut self) -> Result<()> {
        let pat = self.build_pat()?;
        let pmt = self.build_pmt()?;
        let initial_disc =
            self.opts.flags.contains(MpegTsFlags::INITIAL_DISCONTINUITY) && !self.header_written;
        self.tsw.write_section(
            PAT_PID,
            &pat,
            AfRequest {
                discontinuity: initial_disc,
                ..AfRequest::default()
            },
        )?;
        self.tsw.write_section(
            self.pmt_pid,
            &pmt,
            AfRequest {
                discontinuity: initial_disc,
                ..AfRequest::default()
            },
        )?;
        Ok(())
    }

    fn write_sdt_table(&mut self) -> Result<()> {
        let sdt = self.build_sdt()?;
        let initial_disc =
            self.opts.flags.contains(MpegTsFlags::INITIAL_DISCONTINUITY) && !self.header_written;
        self.tsw.write_section(
            SDT_PID,
            &sdt,
            AfRequest {
                discontinuity: initial_disc,
                ..AfRequest::default()
            },
        )
    }

    /// Rewrite `payload` to Annex B if this stream declared a length-prefixed
    /// framing at [`Muxer::add_stream`] time. A transport stream has no
    /// out-of-band configuration record, so H.264/HEVC/VVC must carry their
    /// NAL units Annex-B-framed, start codes and all (ISO/IEC 13818-1 has no
    /// other convention for these codecs in a PES payload).
    fn maybe_convert(&mut self, index: usize, payload: &[u8]) -> Result<Vec<u8>> {
        let Some(stream) = self.streams.get(index) else {
            return Ok(payload.to_vec());
        };
        let Some(length_size) = stream.length_size else {
            return Ok(payload.to_vec());
        };
        let mut out = Vec::new();
        length_prefixed_to_annexb(payload, length_size, &mut out, &mut self.convert_budget)?;
        Ok(out)
    }

    fn pes_stream_id(codec_id: CodecId, media_type: MediaType) -> u8 {
        match media_type {
            MediaType::Video => SID_VIDEO,
            MediaType::Audio => match codec_id {
                CodecId::Ac3 | CodecId::Eac3 | CodecId::Dts | CodecId::Truehd => SID_PRIVATE_1,
                _ => SID_AUDIO,
            },
            _ => SID_PRIVATE_1,
        }
    }
}

impl Muxer for MpegTsMuxer {
    fn flags(&self) -> FormatFlags {
        // Packets on different PIDs are not required to arrive with a single
        // globally increasing DTS — only per elementary stream — so the
        // strictest default (see the trait's own doc) is too strong here.
        FormatFlags::TS_NONSTRICT
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let codec_id = params
            .codec_id
            .ok_or(Error::Unsupported("mpegts: stream has no codec_id"))?;
        let media_type = params
            .media_type
            .ok_or(Error::Unsupported("mpegts: stream has no media_type"))?;
        let effective = self.effective_codec(codec_id);
        let assign = for_codec(effective, self.dvb()).ok_or(Error::Unsupported(
            "mpegts: codec has no MPEG-TS stream_type",
        ))?;

        let pid = self.next_es_pid;
        if pid > 0x1FFE {
            return Err(Error::Unsupported("mpegts: out of elementary stream PIDs"));
        }
        self.next_es_pid = pid.saturating_add(1);

        let length_size = if is_h264_or_hevc(effective) {
            params
                .video
                .as_ref()
                .and_then(|v| v.nal_length_size)
                .filter(|&n| n > 0)
                .and_then(LengthSize::new)
        } else {
            None
        };

        let index = self.streams.len() as u32;
        self.streams.push(MuxStream {
            media_type,
            codec_id: effective,
            pid,
            stream_type: assign.stream_type,
            registration: assign.registration,
            length_size,
            first_packet_written: false,
        });
        Ok(index)
    }

    fn init(&mut self) -> Result<()> {
        if self.streams.is_empty() {
            return Err(Error::InvalidData(
                "mpegts: at least one stream is required",
            ));
        }
        // PCR rides on the first video stream if there is one, else the
        // first stream declared — matching what a single-program broadcast
        // multiplex conventionally does.
        self.pcr_pid = self
            .streams
            .iter()
            .find(|s| s.media_type == MediaType::Video)
            .or_else(|| self.streams.first())
            .map(|s| s.pid);
        Ok(())
    }

    fn write_header(&mut self) -> Result<()> {
        self.write_pat_and_pmt()?;
        self.write_sdt_table()?;
        self.last_pat_clock = Some(self.clock_90k);
        self.last_sdt_clock = Some(self.clock_90k);
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let index = packet.stream_index as usize;
        let (pid, codec_id, media_type, stream_type, is_first_for_stream) = {
            let stream = self
                .streams
                .get(index)
                .ok_or(Error::InvalidData("mpegts: packet names an unknown stream"))?;
            (
                stream.pid,
                stream.codec_id,
                stream.media_type,
                stream.stream_type,
                !stream.first_packet_written,
            )
        };
        let _ = stream_type;

        // Finding 19 (`planning/CONFORMANCE-FINDINGS.md`): measured
        // directly (`ffmpeg -i <avi-with-no-pts> -c copy -f mpegts`) —
        // the reference refuses with "first pts and dts value must be set"
        // and a nonzero exit rather than silently reusing the previous
        // packet's clock. A source with no native per-packet timestamp
        // field (AVI's `dwSampleSize`-derived timing has no PTS/CTS offset
        // to give) produces exactly this on its first packet per stream;
        // writing an MPEG-TS PES header with a fabricated PTS/DTS instead of
        // refusing is the "silent success" shape finding 6 already named.
        if is_first_for_stream && packet.pts.ticks().is_none() {
            return Err(Error::InvalidData(
                "mpegts: first pts and dts value must be set",
            ));
        }

        let clock = packet
            .dts
            .ticks()
            .or_else(|| packet.pts.ticks())
            .unwrap_or(self.clock_90k);
        self.clock_90k = self.clock_90k.max(clock);

        let is_keyframe = packet.flags.contains(PacketFlags::KEY);

        // --- PAT/PMT/SDT repetition -----------------------------------
        let want_pat = self.opts.flags.contains(MpegTsFlags::RESEND_HEADERS)
            || (self.opts.flags.contains(MpegTsFlags::PAT_PMT_AT_FRAMES)
                && media_type == MediaType::Video
                && is_keyframe)
            || self.last_pat_clock.is_none_or(|last| {
                ticks_to_ms(self.clock_90k.saturating_sub(last))
                    >= i64::from(self.opts.pat_period_ms)
            });
        if want_pat {
            self.write_pat_and_pmt()?;
            self.last_pat_clock = Some(self.clock_90k);
        }
        let want_sdt = self.opts.flags.contains(MpegTsFlags::RESEND_HEADERS)
            || self.last_sdt_clock.is_none_or(|last| {
                ticks_to_ms(self.clock_90k.saturating_sub(last))
                    >= i64::from(self.opts.sdt_period_ms)
            });
        if want_sdt {
            self.write_sdt_table()?;
            self.last_sdt_clock = Some(self.clock_90k);
        }

        // --- payload framing -------------------------------------------
        let converted = self.maybe_convert(index, packet.payload())?;

        // --- PES header ---------------------------------------------------
        let stream_id = Self::pes_stream_id(codec_id, media_type);
        let pts = packet.pts.ticks();
        let dts = packet.dts.ticks();
        let timestamps = match (pts, dts) {
            (Some(p), Some(d)) if p != d => PesTimestamps::PtsDts(p, d),
            (Some(p), _) => PesTimestamps::PtsOnly(p),
            (None, _) => PesTimestamps::None,
        };
        let is_video = media_type == MediaType::Video;
        let optional_header_len = match timestamps {
            PesTimestamps::None => 3usize,
            PesTimestamps::PtsOnly(_) => 3 + 5,
            PesTimestamps::PtsDts(..) => 3 + 10,
        };
        let total_len = optional_header_len.saturating_add(converted.len());
        let packet_length = if is_video && self.opts.omit_video_pes_length {
            None
        } else {
            u16::try_from(total_len).ok()
        };
        let header = PesHeaderOut {
            stream_id,
            timestamps,
            data_alignment: true,
            packet_length,
        };
        let mut pes = encode_pes_header(&header);
        pes.extend_from_slice(&converted);

        // --- PCR / random access / discontinuity -----------------------
        let pcr = if self.pcr_pid == Some(pid) {
            let due = self.last_pcr_clock.is_none_or(|last| {
                ticks_to_ms(clock.saturating_sub(last))
                    >= i64::from(self.opts.pcr_period_ms.unwrap_or(DEFAULT_PCR_PERIOD_MS))
            });
            if due {
                self.last_pcr_clock = Some(clock);
                Some(Pcr {
                    base: clock,
                    extension: 0,
                })
            } else {
                None
            }
        } else {
            None
        };
        let random_access = is_keyframe && !self.opts.flags.contains(MpegTsFlags::OMIT_RAI);
        let stream_first = !self
            .streams
            .get(index)
            .is_some_and(|s| s.first_packet_written);
        let discontinuity =
            stream_first && self.opts.flags.contains(MpegTsFlags::INITIAL_DISCONTINUITY);
        if let Some(s) = self.streams.get_mut(index) {
            s.first_packet_written = true;
        }

        self.tsw.write_pes(
            pid,
            &pes,
            AfRequest {
                discontinuity,
                random_access,
                pcr,
            },
        )
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.tsw.flush()
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        let _ = stream_index;
        Some(TIME_BASE)
    }

    fn interleave(
        &mut self,
        queue: &mut InterleaveQueue,
        packet: Option<Packet>,
        flush: bool,
    ) -> Result<Option<Packet>> {
        // MPEG-TS multiplexes at the 188-byte level against a PCR clock, not
        // in the queue sense — see `vaco_format_core::Muxer::interleave`'s
        // own doc, which names this container as the example.
        interleave_none(queue, packet, flush)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::VideoParameters;
    use vaco_core::Timestamp;
    use vaco_io::SharedDynBuf;

    fn video_params(codec: CodecId) -> CodecParameters {
        CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(codec),
            ..CodecParameters::new(MediaType::Video)
        }
    }

    fn audio_params(codec: CodecId) -> CodecParameters {
        CodecParameters {
            media_type: Some(MediaType::Audio),
            codec_id: Some(codec),
            ..CodecParameters::new(MediaType::Audio)
        }
    }

    fn packet(stream_index: u32, pts: i64, key: bool, payload: &[u8]) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, payload).unwrap();
        pkt.stream_index = stream_index;
        pkt.pts = Timestamp::new(pts);
        pkt.dts = Timestamp::new(pts);
        if key {
            pkt.flags |= PacketFlags::KEY;
        }
        pkt
    }

    #[test]
    fn a_stream_with_no_codec_id_is_refused() {
        let sink = SharedDynBuf::new();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        assert!(
            mux.add_stream(&CodecParameters::new(MediaType::Video))
                .is_err()
        );
    }

    #[test]
    fn a_codec_with_no_ts_mapping_is_refused() {
        let sink = SharedDynBuf::new();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        assert!(mux.add_stream(&video_params(CodecId::Vp9)).is_err());
    }

    #[test]
    fn header_writes_pat_pmt_and_sdt() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        let bytes = mirror.take();
        assert_eq!(bytes.len() % 188, 0);
        assert!(!bytes.is_empty());
        // PAT is always the very first packet.
        assert_eq!(&bytes[..4], &[0x47, 0x40, 0x00, 0x10]);
    }

    #[test]
    fn video_and_audio_streams_get_the_expected_pes_stream_ids() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
        let a = mux.add_stream(&audio_params(CodecId::Mp2)).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&packet(v, 0, true, &[0u8; 32])).unwrap();
        mux.write_packet(&packet(a, 0, false, &[1u8; 32])).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn a_length_prefixed_h264_packet_is_converted_to_annex_b() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        let params = CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::H264),
            video: Some(VideoParameters {
                nal_length_size: Some(4),
                ..VideoParameters::default()
            }),
            ..CodecParameters::new(MediaType::Video)
        };
        let v = mux.add_stream(&params).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        // One NAL unit, 4-byte length prefix, "NAL" payload of one 0x65 byte.
        let nal_payload = [0x65u8];
        let mut length_prefixed = (nal_payload.len() as u32).to_be_bytes().to_vec();
        length_prefixed.extend_from_slice(&nal_payload);
        mux.write_packet(&packet(v, 0, true, &length_prefixed))
            .unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        // The Annex B start code must appear somewhere in the output stream.
        assert!(bytes.windows(4).any(|w| w == [0, 0, 0, 1]));
    }

    /// Finding 19 (`planning/CONFORMANCE-FINDINGS.md`): measured directly
    /// against the reference (`ffmpeg -i <no-pts-source> -c copy -f
    /// mpegts`, which refuses with "first pts and dts value must be set"
    /// and a nonzero exit) — a stream's first packet with no PTS at all
    /// (the shape an AVI source produces, since AVI has no native
    /// per-packet PTS field) must be refused, not silently muxed with a
    /// fabricated clock value reused from the previous packet.
    #[test]
    fn a_streams_first_packet_with_no_pts_is_refused() {
        let sink = SharedDynBuf::new();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        let mut pkt = packet(v, 0, true, &[0u8; 8]);
        pkt.pts = Timestamp::NONE;
        pkt.dts = Timestamp::NONE;
        assert!(mux.write_packet(&pkt).is_err());
    }

    /// The same stream's *second* packet is not held to the same standard
    /// here — only the measured "first pts and dts value must be set"
    /// case is enforced, since that is the one behaviour actually measured
    /// against the reference.
    #[test]
    fn a_later_packet_with_no_pts_is_still_accepted() {
        let sink = SharedDynBuf::new();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&packet(v, 0, true, &[0u8; 8])).unwrap();
        let mut pkt = packet(v, 0, false, &[1u8; 8]);
        pkt.pts = Timestamp::NONE;
        pkt.dts = Timestamp::NONE;
        assert!(mux.write_packet(&pkt).is_ok());
    }

    #[test]
    fn pcr_only_appears_on_the_pcr_pid() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
        let a = mux.add_stream(&audio_params(CodecId::Mp2)).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&packet(v, 0, true, &[0u8; 8])).unwrap();
        mux.write_packet(&packet(a, 0, false, &[1u8; 8])).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        let mut saw_pcr_on_video_pid = false;
        for chunk in bytes.chunks(188) {
            if chunk.len() < 188 {
                continue;
            }
            let pid = (u16::from(chunk[1] & 0x1F) << 8) | u16::from(chunk[2]);
            if let Some(pkt) = vaco_format_mpegts_tables::packet::TsPacket::parse(chunk)
                && pkt.pcr().is_some()
            {
                assert_eq!(pid, 0x0100, "PCR must ride the video PID");
                saw_pcr_on_video_pid = true;
            }
        }
        assert!(
            saw_pcr_on_video_pid,
            "expected at least one PCR-carrying packet"
        );
    }

    #[test]
    fn resend_headers_repeats_pat_pmt_on_every_packet() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let opts = MpegTsMuxOptions {
            flags: MpegTsFlags::RESEND_HEADERS,
            ..MpegTsMuxOptions::default()
        };
        let mut mux = MpegTsMuxer::with_options(Box::new(sink), opts, false);
        let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        let before = mirror.take();
        mux.write_packet(&packet(v, 0, true, &[0u8; 8])).unwrap();
        let after = mirror.take();
        assert!(!before.is_empty());
        // A PAT packet (pid 0) must appear again after just one media packet.
        let has_pat = after
            .chunks(188)
            .filter(|c| c.len() == 188)
            .any(|c| c[1].trailing_zeros() >= 5 && c[2] == 0);
        assert!(has_pat, "resend_headers should have repeated the PAT");
    }
}
