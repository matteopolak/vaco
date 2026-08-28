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
use vaco_format_core::mux::BitstreamAction;
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

/// The reference's resolved `-max_delay`/`-muxdelay` default (0.7 s), applied
/// to every PCR, PTS and DTS this muxer writes.
///
/// Measured directly (`ffmpeg -bitexact -c copy -f mpegts`, both plain and
/// with `-muxdelay 0.3`/`-muxdelay 1.0`/`-max_delay 700000`, against fixtures
/// with and without B-frames): the on-wire PCR is always `raw_dts +
/// MUX_DELAY_TICKS`, and the on-wire PTS/DTS is always `raw_pts_or_dts + 2 *
/// MUX_DELAY_TICKS` — a pure additive shift, constant across the whole file
/// and independent of any B-frame reorder delay (which still shows up as the
/// usual PTS-DTS gap on top of the shift). `-muxdelay 0.7`/an unset
/// `-max_delay` both produced the same 63 000-tick default, so this is the
/// reference's fallback when neither is given.
///
/// `MpegTsMuxOptions` has no live path from the generic `max_delay` format
/// option yet — nothing constructs this muxer with anything but
/// [`MpegTsMuxOptions::default`] today (see [`crate::MUXER`]'s doc) — so this
/// bakes in the reference's *default* rather than resolving a real option.
/// Wiring an actual `-max_delay`/`-muxdelay` override through is separate
/// follow-up work.
const MUX_DELAY_TICKS: i64 = 63_000;

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
    /// Set the first time [`MpegTsMuxer::check_bitstream`] answers `Insert`
    /// for this stream — mirrors `vaco-mux-avi::StreamOut::bsf_decided`; see
    /// that field's doc comment for why a muxer needs this at all.
    bsf_decided: bool,
}

fn is_h264_or_hevc(codec: CodecId) -> bool {
    matches!(codec, CodecId::H264 | CodecId::Hevc | CodecId::Vvc)
}

/// The Access Unit Delimiter (H.264 NAL type 9) the reference prepends to
/// **every** H.264 access unit it writes into MPEG-TS.
///
/// Measured directly: `ffmpeg -bitexact -c copy -f mpegts` on a source whose
/// samples carry no AUD at all (`ffprobe -show_data` on the MP4 confirms the
/// first NAL of every sample is the SEI, `06 05 ff ff…`) still shows `00 00
/// 00 01 09 f0` before that SEI on every single video PES packet — I-frame,
/// P-frame and B-frame alike, `primary_pic_type` always `7` ("any"), never
/// varying with slice type. This is specific to the MPEG-TS muxer, not the
/// `h264_mp4toannexb` conversion: the same BSF applied standalone
/// (`-bsf:v h264_mp4toannexb -f h264`) produces no AUD at all. Round-tripping
/// an MPEG-TS source that already carries one AUD per access unit does not
/// double it, hence [`starts_with_h264_aud`] guarding the insertion below.
const H264_AUD_NAL: [u8; 6] = [0x00, 0x00, 0x00, 0x01, 0x09, 0xf0];

/// Whether `payload` already opens with an H.264 Access Unit Delimiter (NAL
/// type 9) after a 3- or 4-byte Annex B start code — see [`H264_AUD_NAL`]'s
/// doc for why this muxer must not insert a second one.
fn starts_with_h264_aud(payload: &[u8]) -> bool {
    let rest = payload
        .strip_prefix([0, 0, 0, 1].as_slice())
        .or_else(|| payload.strip_prefix([0, 0, 1].as_slice()));
    rest.is_some_and(|r| r.first().is_some_and(|&b| b & 0x1F == 9))
}

/// Whether `payload` already opens with an Annex B start code (`00 00 01` or
/// `00 00 00 01`) — see `vaco-mux-avi`'s identical helper for why this makes
/// [`MpegTsMuxer::maybe_convert`] safe to call unconditionally even after M6
/// has already reframed the payload.
fn starts_with_annexb_start_code(payload: &[u8]) -> bool {
    payload.starts_with(&[0, 0, 1]) || payload.starts_with(&[0, 0, 0, 1])
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
    ///
    /// Framing only — no parameter-set splicing, and never will be for VVC
    /// (this crate offers no `vvc_mp4toannexb` through
    /// [`MpegTsMuxer::check_bitstream`], so VVC keeps this method as its only
    /// conversion). For H.264/HEVC, a caller driven through
    /// [`vaco_format_core::mux::MuxWriter`] with a real `BsfProvider` never
    /// reaches this with length-prefixed bytes at all — [`starts_with_annexb_start_code`]
    /// is what makes that safe to call anyway rather than assumed.
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
            bsf_decided: false,
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
        // SDT, PAT, PMT — the order the reference writes.
        self.write_sdt_table()?;
        self.write_pat_and_pmt()?;
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
        let converted = if codec_id == CodecId::H264
            && media_type == MediaType::Video
            && !starts_with_h264_aud(&converted)
        {
            let mut with_aud = Vec::new();
            with_aud.extend_from_slice(&H264_AUD_NAL);
            with_aud.extend_from_slice(&converted);
            with_aud
        } else {
            converted
        };

        // --- PES header ---------------------------------------------------
        let stream_id = Self::pes_stream_id(codec_id, media_type);
        // On-wire PTS/DTS carry the reference's default mux-delay shift; the
        // scheduling `clock` above (PAT/PMT/SDT periods, PCR due-check) stays
        // on the raw, unshifted ticks throughout this function, since only
        // the *differences* between those matter and a constant shift would
        // cancel out anyway — see `MUX_DELAY_TICKS`'s doc for the measurement.
        let pts = packet
            .pts
            .ticks()
            .map(|p| p.saturating_add(MUX_DELAY_TICKS * 2));
        let dts = packet
            .dts
            .ticks()
            .map(|d| d.saturating_add(MUX_DELAY_TICKS * 2));
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
            // Measured (`ffmpeg -bitexact -c copy -f mpegts`, video and
            // audio PES headers both): the reference always clears
            // `data_alignment_indicator` here, even though every packet this
            // muxer writes does start on an access-unit boundary — see
            // `PesHeaderOut::data_alignment`'s doc.
            data_alignment: false,
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
                    base: clock.saturating_add(MUX_DELAY_TICKS),
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

    /// Ask M6 for `h264_mp4toannexb`/`hevc_mp4toannexb` when the stream
    /// declared length-prefixed framing — the same condition
    /// [`MpegTsMuxer::maybe_convert`] uses. VVC is deliberately excluded:
    /// this crate has no `vvc_mp4toannexb` to ask for, so it keeps
    /// `maybe_convert`'s framing-only behaviour as its only conversion.
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
        // Measured order: SDT (0x0011), then PAT (0x0000), then PMT (0x1000).
        // This used to assert PAT came first, which is what we emitted and not
        // what the reference does.
        let pid = |n: usize| {
            let h = &bytes[n * 188..];
            (u16::from(h[1] & 0x1f) << 8) | u16::from(h[2])
        };
        assert_eq!((pid(0), pid(1), pid(2)), (0x0011, 0x0000, 0x1000));
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

    /// Issue #636, the other two named causes: the reference's default
    /// mux-delay shift on PCR/PTS/DTS, and `data_alignment_indicator` always
    /// cleared. Measured against `ffmpeg -bitexact -c copy -f mpegts`; see
    /// [`MUX_DELAY_TICKS`] and [`PesHeaderOut::data_alignment`]'s docs.
    #[test]
    fn pcr_and_pts_dts_carry_the_reference_mux_delay_and_no_data_alignment_bit() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&packet(v, 0, true, &[0u8; 8])).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();

        let mut saw_pcr = false;
        for chunk in bytes.chunks(188) {
            if chunk.len() < 188 {
                continue;
            }
            if let Some(pkt) = vaco_format_mpegts_tables::packet::TsPacket::parse(chunk)
                && let Some(pcr) = pkt.pcr()
            {
                assert_eq!(pcr.base, MUX_DELAY_TICKS, "PCR should be raw_dts + delay");
                saw_pcr = true;
            }
        }
        assert!(saw_pcr, "expected at least one PCR-carrying packet");

        // Find the PES header (payload_unit_start on the video PID) and check
        // both the shifted PTS and the cleared alignment bit through the
        // sibling demuxer's own independent parser.
        let mut found_pes = false;
        for chunk in bytes.chunks(188) {
            if chunk.len() < 188 {
                continue;
            }
            let pid = (u16::from(chunk[1] & 0x1F) << 8) | u16::from(chunk[2]);
            let payload_unit_start = chunk[1] & 0x40 != 0;
            if pid != 0x0100 || !payload_unit_start {
                continue;
            }
            if let Some(pkt) = vaco_format_mpegts_tables::packet::TsPacket::parse(chunk)
                && let Some(pes) = vaco_demux_mpegts::pes::PesHeader::parse(pkt.payload)
            {
                assert_eq!(pes.pts.ticks(), Some(MUX_DELAY_TICKS * 2));
                assert!(
                    !pes.data_alignment,
                    "the reference clears data_alignment_indicator"
                );
                found_pes = true;
            }
        }
        assert!(found_pes, "expected to find the video PES header");
    }

    /// Issue #636: the reference prepends a fixed Access Unit Delimiter to
    /// every H.264 access unit written into MPEG-TS, even when the source
    /// sample carries no AUD at all (see [`H264_AUD_NAL`]'s doc for the
    /// measurement). `maybe_convert` alone (no BSF, no `avcC`) is enough to
    /// exercise this — the AUD insertion does not depend on the SPS/PPS
    /// splice path.
    #[test]
    fn an_h264_access_unit_gets_the_reference_aud_prepended() {
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
        let idr = [0x65u8, 0x88, 0x84];
        let mut length_prefixed = (u32::try_from(idr.len()).unwrap()).to_be_bytes().to_vec();
        length_prefixed.extend_from_slice(&idr);
        mux.write_packet(&packet(v, 0, true, &length_prefixed))
            .unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        let mut expected = H264_AUD_NAL.to_vec();
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&idr);
        assert!(
            bytes.windows(expected.len()).any(|w| w == expected.as_slice()),
            "expected the AUD immediately before the converted access unit"
        );
    }

    /// The other half of #636's AUD fix: a source that already carries its
    /// own AUD (e.g. re-muxing an MPEG-TS whose elementary stream already has
    /// one per access unit — measured directly, see [`H264_AUD_NAL`]'s doc)
    /// must not get a second one spliced in front of it.
    #[test]
    fn an_h264_access_unit_that_already_has_an_aud_is_not_given_a_second_one() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpegTsMuxer::new(Box::new(sink));
        let params = CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::H264),
            // `nal_length_size: None`: already Annex B, so `maybe_convert`
            // passes the payload through unchanged and `write_packet` sees
            // exactly what `starts_with_h264_aud` must recognise.
            ..CodecParameters::new(MediaType::Video)
        };
        let v = mux.add_stream(&params).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        let mut payload = H264_AUD_NAL.to_vec();
        payload.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84]);
        mux.write_packet(&packet(v, 0, true, &payload)).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        let aud_count = bytes
            .windows(H264_AUD_NAL.len())
            .filter(|w| *w == H264_AUD_NAL.as_slice())
            .count();
        assert_eq!(aud_count, 1, "the existing AUD must not be duplicated");
    }

    /// Wraps the real `vaco-bsf-h2645` filter, not a hand test-double — see
    /// `vaco-mux-avi`'s identical provider for the reasoning.
    struct OnlyH2645ToAnnexb;

    impl vaco_format_core::mux::BsfProvider for OnlyH2645ToAnnexb {
        fn open(
            &self,
            name: &str,
            params: &CodecParameters,
        ) -> Result<Box<dyn vaco_codec_core::BitstreamFilter>> {
            match name {
                "h264_mp4toannexb" => (vaco_bsf_h2645::h264_mp4toannexb::DESC.build)(params),
                "hevc_mp4toannexb" => (vaco_bsf_h2645::hevc_mp4toannexb::DESC.build)(params),
                _ => Err(Error::Unsupported("test provider knows only the mp4toannexb pair")),
            }
        }
    }

    fn avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
        let mut r = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
        r.extend_from_slice(&(u16::try_from(sps.len()).unwrap()).to_be_bytes());
        r.extend_from_slice(sps);
        r.push(1);
        r.extend_from_slice(&(u16::try_from(pps.len()).unwrap()).to_be_bytes());
        r.extend_from_slice(pps);
        r
    }

    /// The comparison this crate's brief asked for before touching
    /// `maybe_convert`: driven through `MuxBuilder`/`MuxWriter` (M6) with a
    /// real `BsfProvider`, the output carries the SPS/PPS-spliced Annex B
    /// [`MpegTsMuxer::maybe_convert`] alone can never produce — it has no
    /// configuration record to read parameter sets out of. The two paths
    /// disagree, and per the brief, the reference (which
    /// `vaco-bsf-h2645::h264_mp4toannexb` was checked against directly)
    /// decides: this is the more correct output, not merely a different one.
    #[test]
    fn check_bitstream_through_mux_writer_gets_the_splice_maybe_convert_alone_cannot() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mux = MpegTsMuxer::new(Box::new(sink));

        let sps = [0x67, 0x64, 0x00, 0x0a, 0xAA];
        let pps = [0x68, 0xEB];
        let params = CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::H264),
            extradata: Some(avcc(&sps, &pps)),
            video: Some(VideoParameters {
                nal_length_size: Some(4),
                ..VideoParameters::default()
            }),
            ..CodecParameters::new(MediaType::Video)
        };

        let mut builder = vaco_format_core::mux::MuxBuilder::new(
            Box::new(mux),
            &vaco_format_core::FormatOptions::default(),
        )
        .with_bsfs(std::sync::Arc::new(OnlyH2645ToAnnexb));
        let v = builder
            .add_stream(&params, vaco_core::TimeBase::new(1, 90_000))
            .unwrap();
        let mut writer = builder.open().unwrap();

        let idr = [0x65, 0x88, 0x84];
        let mut lp = Vec::new();
        lp.extend_from_slice(&(u32::try_from(idr.len()).unwrap()).to_be_bytes());
        lp.extend_from_slice(&idr);
        writer.write_packet(packet(v, 0, true, &lp)).unwrap();
        writer.finish().unwrap();

        let bytes = mirror.take();
        let mut expected = Vec::new();
        for u in [&sps[..], &pps[..], &idr[..]] {
            expected.extend_from_slice(&[0, 0, 0, 1]);
            expected.extend_from_slice(u);
        }
        // Small enough to land in one PES packet's payload with no 188-byte
        // transport-packet split in the middle, so a raw byte window is a
        // valid check here — the same assumption the existing
        // `a_length_prefixed_h264_packet_is_converted_to_annex_b` test above
        // already makes.
        assert!(
            bytes.windows(expected.len()).any(|w| w == expected.as_slice()),
            "expected the SPS/PPS-spliced sample verbatim in the muxed bytes"
        );
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
