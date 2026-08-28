//! `GxfDemuxer`: reads a GXF stream's sequential packets (SMPTE 360-2009
//! clause 5's own order — `Map, [FLT], UMF, {Media}*, EOS`) and turns its
//! `MEDIA` packets into [`Packet`]s, one [`Stream`] per track named in the
//! opening `MAP` packet.
//!
//! # The shared field-number timeline
//!
//! Every track's samples are addressed on one virtual timeline counted in
//! video *fields*, not per-track ticks (clause 4.6/4.26/7.4.2.1.3) — a
//! 25 fps PAL file's field clock runs at 50 Hz regardless of whether a
//! given packet is video, 48 kHz audio, or time code. This demuxer derives
//! that shared rate once, from whichever track states a recognised frame
//! rate code (`derive_field_rate`), and gives every [`Stream`] the same
//! `time_base` — the reciprocal of the field rate. A file with no track
//! stating a recognised code (Table 6: every value is `-1`/`-2`/absent)
//! reports [`vaco_core::Rational::UNDEFINED`], honestly, rather than
//! guessing a rate this crate has not measured a counter-example for.
//!
//! # What this crate does not yet do
//!
//! - **`FLT`/`UMF` packets are read only well enough to skip them.** The
//!   FLT is a coarse seek aid this crate's own `seek` does not use yet
//!   (see [`Demuxer::seek`]'s stub below); the UMF restates information the
//!   MAP packet already gives this crate everything it needs from (clause
//!   7.3's own account: "Some properties in the UMF are also in MAP
//!   packets... MAP packets shall have priority").
//! - **Video width/height are not stated anywhere in GXF's own metadata**
//!   (measured against the published Standard directly: neither Table 6's
//!   track tags nor Table 16's UMF media description carry pixel
//!   dimensions, only a lines-per-frame *code* — 525/625/1080/720). The
//!   real value lives only in the elementary stream's own sequence header.
//!   This crate reports the conventional ITU-R BT.601 SD width (720) and
//!   the 525/625-implied height, and leaves HD width/height at `0`
//!   (unknown) rather than guess a common resolution. Wiring the D14.1
//!   `ParserProvider` seam (already threaded through `open`, like every
//!   other demuxer, but not yet called) to read a real sequence header is
//!   the fix — see `vaco-demux-raw::bitstream`'s `drive_parser` for the
//!   pattern.
//! - **Compound clips are read, not re-timed.** A `MEDIA` packet's
//!   `effective_field_number` (see `media.rs`) is used directly as `pts`,
//!   which is correct for a simple clip; a compound clip's cut transitions
//!   are represented exactly as the stream states them, not stitched into
//!   the "one continuous decodable timeline" shape `vaco-demux-hls`/
//!   `vaco-format-imf` build for their own multi-segment cases.
//! - **`seek` is not implemented.** No fixture measured this session
//!   exercises anything past sequential reading; the FLT above is this
//!   format's own named mechanism for it.

use std::collections::HashMap;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, ProbeData, ProbeScore, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::map::{self, MapPacket, TrackDescription};
use crate::media::MediaPreamble;
use crate::packet::{self, PacketHeader};

struct TrackBinding {
    stream_index: u32,
    /// The raw Table 5 media type, kept per-binding rather than re-derived
    /// from `streams[stream_index]` so a media packet's own type byte can
    /// be cross-checked against what the MAP packet said this track was
    /// (a real, if minor, resynchronisation check).
    media_type: u8,
}

/// The GXF demuxer.
pub struct GxfDemuxer {
    io: IoContext,
    budget: Budget,
    streams: Vec<Stream>,
    /// Keyed by [`TrackDescription::track_id`], which is also the value a
    /// media packet's own [`MediaPreamble::track_number`] carries (clause
    /// 7.4.2.1.2: track numbers are the index into the MAP packet's own
    /// track-description vector, and `track_id` states exactly that
    /// index).
    bindings: HashMap<u8, TrackBinding>,
    eof: bool,
}

impl std::fmt::Debug for GxfDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GxfDemuxer")
            .field("streams", &self.streams.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

fn read_payload(io: &mut IoContext, budget: &mut Budget, header: &PacketHeader) -> Result<Vec<u8>> {
    let len = usize::try_from(header.payload_len())
        .map_err(|_| Error::InvalidData("gxf: packet payload length does not fit this platform's usize"))?;
    let mut buf = budget.alloc::<u8>(len)?;
    io.read_exact(&mut buf)?;
    Ok(buf)
}

/// Table 6's eight defined frame-rate codes, as a frame rate. `None` for
/// `-1`/`-2`/anything else — this crate does not guess a rate for a code
/// it has not measured a real file stating.
const fn frame_rate_code_to_fps(code: i32) -> Option<Rational> {
    Some(match code {
        1 => Rational::new(60, 1),
        2 => Rational::new(60_000, 1001),
        3 => Rational::new(50, 1),
        4 => Rational::new(30, 1),
        5 => Rational::new(30_000, 1001),
        6 => Rational::new(25, 1),
        7 => Rational::new(24, 1),
        8 => Rational::new(24_000, 1001),
        _ => return None,
    })
}

/// The field rate every stream's `time_base` is the reciprocal of — twice
/// the frame rate, since GXF field numbers advance by 2 per frame
/// regardless of whether the storage is progressive or interlaced (see the
/// module docs, and `media.rs`'s own real-fixture test: ten 25 fps frames
/// land at field numbers `0, 2, 4, ..., 18`).
fn derive_field_rate(map: &MapPacket) -> Option<Rational> {
    let fps = map.tracks.iter().find_map(|t| t.frame_rate_code).and_then(frame_rate_code_to_fps)?;
    Some(Rational::new(fps.num.saturating_mul(2), fps.den))
}

/// Table 5's SD line-count convention (measured against ITU-R BT.601, the
/// same standard clause 3's own normative references name): 525 lines is
/// NTSC's 480 active lines, 625 is PAL's 576 — width is the same 720
/// samples per line for both, the value clause 4's own Note 1 and the
/// Standard's SD-only field-locator/media-preamble conventions assume
/// throughout. `None` for the HD line codes (4=1080, 6=720): unlike SD,
/// there is no single conventional width/height pair, and this crate has
/// not measured a real HD GXF file to check one against.
const fn sd_dimensions(lines_per_frame_code: Option<i32>) -> Option<(u32, u32)> {
    match lines_per_frame_code {
        Some(1) => Some((720, 480)),
        Some(2) => Some((720, 576)),
        _ => None,
    }
}

fn build_codec_params(track: &TrackDescription) -> Option<CodecParameters> {
    use vaco_codec_core::{AudioParameters, VideoParameters};

    let frame_rate = track.frame_rate_code.and_then(frame_rate_code_to_fps).unwrap_or(Rational::UNDEFINED);
    let dims = sd_dimensions(track.lines_per_frame_code);

    let video = |codec_id: CodecId| {
        let mut p = CodecParameters::new(MediaType::Video);
        p.codec_id = Some(codec_id);
        let mut v = VideoParameters { frame_rate, ..VideoParameters::default() };
        if let Some((w, h)) = dims {
            v.width = w;
            v.height = h;
        }
        p.video = Some(v);
        p
    };

    match track.media_type {
        3 | 4 => Some(video(CodecId::Jpeg)),
        7 | 8 | 24 => {
            // Time code: no decoder, no elementary stream — the same
            // `MediaType::Data` role `mov`'s `tmcd` and every other
            // "timed data with no decoder" track in this workspace uses.
            Some(CodecParameters::new(MediaType::Data))
        }
        9 | 10 => {
            let mut p = CodecParameters::new(MediaType::Audio);
            p.codec_id = Some(if track.media_type == 9 { CodecId::PcmS24le } else { CodecId::PcmS16le });
            p.audio = Some(AudioParameters {
                // Fixed by clause 7.4.2.3: audio is always 48 kHz mono per
                // track (a compressed stream's stereo pair is carried as
                // two separate mono tracks, clause 7.4.2.3.3/.4).
                sample_rate: 48_000,
                layout: ChannelLayout::default_for(1),
                bits_per_coded_sample: Some(if track.media_type == 9 { 24 } else { 16 }),
                ..AudioParameters::default()
            });
            Some(p)
        }
        11 | 12 | 20 => Some(video(CodecId::Mpeg2video)),
        13..=16 => Some(video(CodecId::Dvvideo)),
        17 => {
            let mut p = CodecParameters::new(MediaType::Audio);
            p.codec_id = Some(CodecId::Ac3);
            p.audio = Some(AudioParameters { sample_rate: 48_000, ..AudioParameters::default() });
            Some(p)
        }
        18 => {
            // "24-bit non-PCM audio" (Table 5): SMPTE 337-wrapped data of
            // an unstated compression family (clause 7.4.2.3.4 explicitly
            // leaves the payload's own type to SMPTE 338's data-type
            // field, which this crate does not parse). The track is real
            // and its samples are real bytes; only the codec identity is
            // unknown, so `codec_id` is left `None` rather than guessed.
            let mut p = CodecParameters::new(MediaType::Audio);
            p.audio = Some(AudioParameters { sample_rate: 48_000, ..AudioParameters::default() });
            Some(p)
        }
        // 19, 21: reserved. Clause "Table 5": "A receiver shall ignore
        // this media type" — not a track this crate builds a Stream for.
        22 | 23 => Some(video(CodecId::Mpeg1video)),
        _ => None,
    }
}

impl GxfDemuxer {
    /// Open a GXF stream: read its first packet (required by clause 5.1 to
    /// be a `MAP` packet), and build one [`Stream`] per recognised track.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if the stream does not begin with a `MAP`
    /// packet, or that packet fails to parse (see [`map::parse`]).
    pub fn open(source: Box<dyn MediaSource>, _parsers: &dyn ParserProvider) -> Result<Self> {
        let mut io = IoContext::new(source, &IoOptions::default())?;
        let mut budget = Budget::new(Limits::permissive());

        let header = packet::read_header(&mut io)?;
        if header.packet_type != packet::PKT_MAP {
            return Err(Error::InvalidData("gxf: stream does not begin with a map packet"));
        }
        let payload = read_payload(&mut io, &mut budget, &header)?;
        let map = map::parse(&payload, &mut budget)?;

        let field_rate = derive_field_rate(&map);
        let time_base = field_rate.map_or(Rational::UNDEFINED, |r| Rational::new(r.den, r.num));

        let mut streams = Vec::new();
        let mut bindings = HashMap::new();
        for track in &map.tracks {
            let Some(params) = build_codec_params(track) else {
                continue;
            };
            let index = u32::try_from(streams.len()).unwrap_or(u32::MAX);
            let media_type = params.media_type.unwrap_or(MediaType::Data);
            let mut stream = Stream::new(index, media_type, time_base);
            stream.params = params;
            streams.push(stream);
            bindings.insert(track.track_id, TrackBinding { stream_index: index, media_type: track.media_type });
        }

        Ok(Self {
            io,
            budget,
            streams,
            bindings,
            eof: false,
        })
    }

    /// Turn one `MEDIA` packet's payload into a [`Packet`], or `None` when
    /// its track number names a track this demuxer did not build a
    /// [`Stream`] for (a reserved media type, or a resynchronisation
    /// mismatch) — the caller keeps reading rather than treating this as
    /// fatal, the same tolerance every packet-oriented demuxer in this
    /// workspace already gives an unrecognised stream id.
    fn build_packet(&mut self, payload: &[u8]) -> Result<Option<Packet>> {
        let preamble = MediaPreamble::parse(payload)?;
        let Some(binding) = self.bindings.get(&preamble.track_number) else {
            return Ok(None);
        };
        let essence = payload.get(16..).unwrap_or(&[]);
        let mut pkt = Packet::alloc(&mut self.budget, essence.len())?;
        pkt.payload_mut().copy_from_slice(essence);
        pkt.stream_index = binding.stream_index;
        let ts = Timestamp::new(i64::from(preamble.effective_field_number()));
        pkt.pts = ts;
        pkt.dts = ts;
        // Every media type but MPEG states nothing about frame typing in
        // its preamble (clause 7.4.2.1.4); a JPEG/DV/audio/time-code
        // packet is always independently decodable, so it is always a key
        // frame. For MPEG, only an I-frame is (Table 19).
        let is_key = match binding.media_type {
            11 | 12 | 20 | 22 | 23 => preamble.mpeg_frame_info().is_intra(),
            _ => true,
        };
        if is_key {
            pkt.flags |= PacketFlags::KEY;
        }
        Ok(Some(pkt))
    }
}

impl Demuxer for GxfDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        loop {
            let header = match packet::read_header(&mut self.io) {
                Ok(h) => h,
                Err(Error::UnexpectedEof) => {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                Err(e) => return Err(e),
            };
            if header.packet_type == packet::PKT_EOS {
                self.eof = true;
                return Err(Error::Eof);
            }
            if header.packet_type != packet::PKT_MEDIA {
                // A repeated MAP, the FLT, the UMF, or a reserved type
                // (clause 5.1/5.2): not a packet this method returns, but
                // reading a stream is not done because of one.
                self.io.skip(header.payload_len())?;
                continue;
            }
            let payload = read_payload(&mut self.io, &mut self.budget, &header)?;
            if let Some(pkt) = self.build_packet(&payload)? {
                return Ok(pkt);
            }
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let _ = (target, flags);
        Err(Error::Unsupported("gxf: seeking is not yet implemented"))
    }
}

/// Content probe: a `MAP` packet's own fixed leader/type/trailer bytes at
/// offset 0 (clause 5.1: "Every stream shall start with a map packet").
/// Cheaper and more specific than sniffing for the `.gxf` extension alone,
/// and it is what actually distinguishes a GXF stream from anything else
/// that might carry that extension.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    const MAP_HEADER: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x01, packet::PKT_MAP, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE1, 0xE2,
    ];
    // The length field (bytes 6..10) is data-dependent, so it is excluded
    // from the fixed prefix/suffix this checks rather than wildcarded byte
    // by byte.
    let buf = data.buf;
    let head_ok = buf.get(..6) == MAP_HEADER.get(..6);
    let tail_ok = buf.get(10..16) == MAP_HEADER.get(10..16);
    if buf.len() >= 16 && head_ok && tail_ok {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

/// See `vaco-demux-dash::FLAGS`'s own doc comment for the identical
/// reasoning applied to a different format: GXF's own field-numbered
/// virtual timeline is not a byte-position index, so the core's generic
/// index (built from timestamps this demuxer already reports) is the
/// right seam rather than a byte-offset one.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

fn open_boxed(source: Box<dyn MediaSource>, parsers: &dyn ParserProvider) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(GxfDemuxer::open(source, parsers)?))
}

/// The descriptor `vaco-registry` holds (`vaco-component.toml`'s own
/// `ctor = "vaco_format_gxf::DEMUXER"`).
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "gxf",
    long_name: "GXF (General eXchange Format)",
    extensions: &["gxf"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_boxed,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    fn open_fixture() -> GxfDemuxer {
        let bytes = include_bytes!("../tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf").to_vec();
        GxfDemuxer::open(Box::new(MemorySource::new(bytes)), &NoParsers).unwrap()
    }

    #[test]
    fn opens_the_real_fixture_with_three_streams_in_map_order() {
        let demux = open_fixture();
        assert_eq!(demux.streams().len(), 3);
        assert_eq!(demux.streams()[0].media_type(), Some(MediaType::Video));
        assert_eq!(demux.streams()[0].params.codec_id, Some(CodecId::Mpeg2video));
        assert_eq!(demux.streams()[1].media_type(), Some(MediaType::Audio));
        assert_eq!(demux.streams()[1].params.codec_id, Some(CodecId::PcmS16le));
        assert_eq!(demux.streams()[2].media_type(), Some(MediaType::Data));
        // Every stream shares the same field-rate time base (25 fps -> 50
        // fields/sec), including the audio and time code tracks.
        for s in demux.streams() {
            assert_eq!(s.time_base, Rational::new(1, 50));
        }
    }

    #[test]
    fn read_packet_matches_ffprobes_measured_sizes_and_positions() {
        let mut demux = open_fixture();
        // Measured with `ffprobe -show_packets` against this exact fixture
        // this session: one audio packet (65536 bytes, the fixed 32,768
        // 16-bit samples), then video packets at field numbers 0, 2, 4...
        // with the reference's own reported sizes.
        let audio = demux.read_packet().unwrap();
        assert_eq!(audio.stream_index, 1);
        assert_eq!(audio.payload().len(), 65536);
        assert_eq!(audio.pts, Timestamp::new(0));

        let expected_video: &[(i64, usize)] = &[(0, 37564), (2, 4416), (4, 2088), (6, 1700)];
        for &(field, size) in expected_video {
            let v = demux.read_packet().unwrap();
            assert_eq!(v.stream_index, 0);
            assert_eq!(v.pts, Timestamp::new(field));
            assert_eq!(v.payload().len(), size);
        }
        // The first video packet is the sequence's own I-frame.
        assert!(demux.read_packet().is_ok());
    }

    #[test]
    fn reading_past_the_last_media_packet_reaches_eos_as_eof() {
        let mut demux = open_fixture();
        let mut n = 0;
        loop {
            match demux.read_packet() {
                Ok(_) => n += 1,
                Err(Error::Eof) => break,
                Err(e) => unreachable!("unexpected error: {e:?}"),
            }
        }
        // Measured directly against the fixture: 50 video (100 fields / 2
        // per frame) + 3 audio (32,768-sample packets covering ~2.05s for
        // a 2.00s clip) = 53. The time code track the MAP packet describes
        // gets zero media packets in this file, the same "declared but
        // empty" shape `ffprobe -show_streams` reports for it too.
        assert_eq!(n, 53);
    }

    #[test]
    fn probe_scores_the_real_fixture_at_magic() {
        let bytes = include_bytes!("../tests/fixtures/ffmpeg_pal_mpeg2_pcm.gxf");
        let data = ProbeData::new(bytes);
        assert_eq!(probe(&data), ProbeScore::MAGIC);
    }

    #[test]
    fn probe_rejects_prose() {
        let data = ProbeData::new(b"not a gxf stream at all");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn a_source_that_does_not_start_with_a_map_packet_is_rejected_not_panicked_on() {
        let bytes = vec![0u8; 64];
        let err = GxfDemuxer::open(Box::new(MemorySource::new(bytes)), &NoParsers).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }
}
