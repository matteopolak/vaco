//! `GxfMuxer`: a simple-clip GXF writer (SMPTE 360-2009 clause 4.23 — every
//! track a single, contiguous segment) for one MPEG-2 video track and one
//! 16-bit PCM audio track.
//!
//! # Buffer-then-finalize, not streaming
//!
//! This muxer buffers every packet in memory and writes the whole stream
//! (`MAP`, a minimal `UMF`, every `MEDIA` packet, `EOS`) inside
//! [`Muxer::write_trailer`] — the same trade-off
//! `vaco-mux-mxf::MUXER_OPATOM` makes for clip-wrapped essence, for the
//! same reason: a `MAP` packet's own `EstimatedSizeOfStream`/
//! `LastFieldOfMaterial` values (Table 4) need the whole clip's size known
//! up front, and this crate has not built the "write one placeholder MAP,
//! rewrite it at the end via `MediaSink::seek`" streaming version yet (see
//! "How to change it" in the crate's top-level docs).
//!
//! # Scope
//!
//! Exactly one video track (`CodecId::Mpeg2video`, one of the eight Table 6
//! frame rates) and/or exactly one audio track (`CodecId::PcmS16le`) —
//! [`Muxer::add_stream`] returns [`Error::Unsupported`] for anything else,
//! including a second stream of either kind, DV/JPEG/AC-3/time-code
//! tracks this crate's own demuxer already *reads*, and an MPEG frame rate
//! that is not exactly one of Table 6's eight defined values. Widening
//! this is additive (a new arm in `add_stream` and `write_trailer`'s track
//! loop), not a redesign.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::{Packet, PacketFlags};

use crate::map::{self, MapPacket, MaterialData, TrackDescription};
use crate::packet;

/// Fixed by clause 7.4.2.3: every audio packet carries exactly this many
/// sample words, however the caller's own [`Packet`]s happen to be sized.
const AUDIO_SAMPLES_PER_PACKET: u64 = 32_768;
const AUDIO_SAMPLE_RATE: u64 = 48_000;

fn fps_to_frame_rate_code(fps: Rational) -> Option<i32> {
    const TABLE: &[(i32, Rational)] = &[
        (1, Rational::new(60, 1)),
        (2, Rational::new(60_000, 1001)),
        (3, Rational::new(50, 1)),
        (4, Rational::new(30, 1)),
        (5, Rational::new(30_000, 1001)),
        (6, Rational::new(25, 1)),
        (7, Rational::new(24, 1)),
        (8, Rational::new(24_000, 1001)),
    ];
    TABLE
        .iter()
        .find(|(_, r)| i64::from(r.num) * i64::from(fps.den) == i64::from(fps.num) * i64::from(r.den))
        .map(|(code, _)| *code)
}

/// Write `v` (little-endian, the UMF's own default byte order per clause
/// 4.3 — unlike the MAP packet's tag/length/value items, nothing in
/// clause 7.3 restates the big-endian exception for these fields) into
/// `buf` at `at`, bounds-checked rather than indexed directly.
fn put_u32_le(buf: &mut [u8], at: usize, v: u32) -> Result<()> {
    buf.get_mut(at..at + 4)
        .ok_or(Error::InvalidData("gxf: UMF field offset does not fit its own buffer"))?
        .copy_from_slice(&v.to_le_bytes());
    Ok(())
}

fn lines_code_for(height: u32) -> Option<i32> {
    match height {
        480 => Some(1),
        576 => Some(2),
        _ => None,
    }
}

struct VideoTrack {
    track_id: u8,
    frame_rate_code: i32,
    lines_code: Option<i32>,
    frames: Vec<(Vec<u8>, bool)>,
}

struct AudioTrack {
    track_id: u8,
    bytes: Vec<u8>,
}

/// The GXF muxer. See the module docs for scope and the buffer-then-write
/// strategy.
pub struct GxfMuxer {
    sink: Box<dyn MediaSink>,
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
    next_track_id: u8,
}

impl std::fmt::Debug for GxfMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GxfMuxer")
            .field("has_video", &self.video.is_some())
            .field("has_audio", &self.audio.is_some())
            .finish_non_exhaustive()
    }
}

impl GxfMuxer {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self {
            sink,
            video: None,
            audio: None,
            next_track_id: 0,
        }
    }

    /// Smallest field number `F` such that the accumulated audio sample
    /// count through field `F` (`F * AUDIO_SAMPLE_RATE / field_rate`,
    /// clause Annex B's own relationship) reaches this packet's own start
    /// sample (`packet_index * AUDIO_SAMPLES_PER_PACKET`) — i.e. the
    /// inverse of Annex B's `AudioPacketNumber` formula. Checked against
    /// the real fixture's own three audio packets (field numbers 0, 35,
    /// 69 for a 50 fields/sec file) in `mux.rs`'s own tests below.
    fn audio_packet_field_number(packet_index: u64, field_rate: Rational) -> u32 {
        let num = packet_index
            .saturating_mul(AUDIO_SAMPLES_PER_PACKET)
            .saturating_mul(u64::from(field_rate.num.unsigned_abs()));
        let den = AUDIO_SAMPLE_RATE.saturating_mul(u64::from(field_rate.den.unsigned_abs())).max(1);
        u32::try_from(num.div_ceil(den)).unwrap_or(u32::MAX)
    }

    fn field_rate(&self) -> Rational {
        self.video
            .as_ref()
            .and_then(|v| {
                const TABLE: &[(i32, Rational)] = &[
                    (1, Rational::new(60, 1)),
                    (2, Rational::new(60_000, 1001)),
                    (3, Rational::new(50, 1)),
                    (4, Rational::new(30, 1)),
                    (5, Rational::new(30_000, 1001)),
                    (6, Rational::new(25, 1)),
                    (7, Rational::new(24, 1)),
                    (8, Rational::new(24_000, 1001)),
                ];
                TABLE.iter().find(|(c, _)| *c == v.frame_rate_code).map(|(_, r)| *r)
            })
            .map_or(Rational::new(50, 1), |fps| Rational::new(fps.num.saturating_mul(2), fps.den))
    }

    fn write_packet_bytes(&mut self, packet_type: u8, payload: &[u8]) -> Result<()> {
        let total_len = 16u64
            .checked_add(u64::try_from(payload.len()).unwrap_or(u64::MAX))
            .ok_or(Error::InvalidData("gxf: packet length overflow"))?;
        let length = u32::try_from(total_len).map_err(|_| Error::LimitExceeded {
            limit: "gxf_packet_bytes",
            requested: total_len,
            cap: u64::from(u32::MAX),
        })?;
        let mut header = vec![0x00, 0x00, 0x00, 0x00, 0x01, packet_type];
        header.extend_from_slice(&length.to_be_bytes());
        header.extend_from_slice(&[0, 0, 0, 0]);
        header.extend_from_slice(&[0xE1, 0xE2]);
        self.sink.write(&header)?;
        self.sink.write(payload)
    }

    fn write_media_packet(&mut self, media_type: u8, track_number: u8, field_number: u32, field_info: [u8; 4], essence: &[u8]) -> Result<()> {
        let mut payload = Vec::new();
        payload.push(media_type);
        payload.push(track_number);
        payload.extend_from_slice(&field_number.to_be_bytes());
        payload.extend_from_slice(&field_info);
        payload.extend_from_slice(&field_number.to_be_bytes()); // timeline field number: same as media field number for a simple clip.
        payload.push(0x01); // flags: timeline field number valid.
        payload.push(0x00); // reserved.
        payload.extend_from_slice(essence);
        self.write_packet_bytes(packet::PKT_MEDIA, &payload)
    }
}

impl Muxer for GxfMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::GENERIC_INDEX
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        match (params.media_type, params.codec_id) {
            (Some(MediaType::Video), Some(CodecId::Mpeg2video)) if self.video.is_none() => {
                let video = params.video.as_ref().ok_or(Error::InvalidData("gxf: video stream has no VideoParameters"))?;
                let frame_rate_code = fps_to_frame_rate_code(video.frame_rate).ok_or(Error::Unsupported(
                    "gxf: this muxer writes only Table 6's eight defined MPEG frame rates",
                ))?;
                let track_id = self.next_track_id;
                self.next_track_id += 1;
                self.video = Some(VideoTrack {
                    track_id,
                    frame_rate_code,
                    lines_code: lines_code_for(video.height),
                    frames: Vec::new(),
                });
                Ok(u32::from(track_id))
            }
            (Some(MediaType::Audio), Some(CodecId::PcmS16le)) if self.audio.is_none() => {
                let track_id = self.next_track_id;
                self.next_track_id += 1;
                self.audio = Some(AudioTrack { track_id, bytes: Vec::new() });
                Ok(u32::from(track_id))
            }
            _ => Err(Error::Unsupported(
                "gxf: this muxer writes only one Mpeg2video track and/or one PcmS16le track (see the crate's own module docs)",
            )),
        }
    }

    fn write_header(&mut self) -> Result<()> {
        // Deferred to `write_trailer` — see the module docs.
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if let Some(video) = &mut self.video
            && packet.stream_index == u32::from(video.track_id)
        {
            video.frames.push((packet.payload().to_vec(), packet.flags.contains(PacketFlags::KEY)));
            return Ok(());
        }
        if let Some(audio) = &mut self.audio
            && packet.stream_index == u32::from(audio.track_id)
        {
            audio.bytes.extend_from_slice(packet.payload());
            return Ok(());
        }
        Err(Error::InvalidData("gxf: packet names a stream index this muxer never added"))
    }

    fn write_trailer(&mut self) -> Result<()> {
        let field_rate = self.field_rate();
        let video_fields = self.video.as_ref().map_or(0u32, |v| u32::try_from(v.frames.len()).unwrap_or(u32::MAX).saturating_mul(2));

        let mut tracks = Vec::new();
        if let Some(v) = &self.video {
            tracks.push(TrackDescription {
                media_type: 12,
                track_id: v.track_id,
                media_file_name: Some("EXT:/PDR/default/ES.M0".to_owned()),
                mpeg_video_aux: Some("Ver 1\nBr 0.000000\nIpg 1\nPpi 0\nBpiop 0\nPix 0\nCf 1\nCg 1\nSl 23\nnl16 36\nVi 1\nf1 1\n".to_owned()),
                frame_rate_code: Some(v.frame_rate_code),
                lines_per_frame_code: v.lines_code,
                fields_per_frame_code: Some(2),
                ..TrackDescription::default()
            });
        }
        if let Some(a) = &self.audio {
            tracks.push(TrackDescription {
                media_type: 10,
                track_id: a.track_id,
                media_file_name: Some("EXT:/PDR/default/ES.A0".to_owned()),
                aux_binary: Some([0u8; 8]),
                frame_rate_code: Some(-2),
                lines_per_frame_code: Some(-2),
                fields_per_frame_code: Some(-2),
                ..TrackDescription::default()
            });
        }

        let map_packet = MapPacket {
            material: MaterialData {
                media_file_name: Some("EXT:/PDR/default/out.gxf".to_owned()),
                first_field: 0,
                last_field: video_fields,
                mark_in: 0,
                mark_out: video_fields,
                estimated_size_1024_bytes: 0, // computed exactly is not worth a two-pass write; 0 matches clause 7.1.2.3's own "while under construction" convention.
            },
            tracks,
        };
        let map_bytes = map::encode(&map_packet);
        self.write_packet_bytes(packet::PKT_MAP, &map_bytes)?;

        // Minimal-but-valid UMF (clause 7.3, Table 13/14): zero tracks and
        // zero segments declared, so the track/media description sections
        // are legitimately empty rather than wrong. Clause 7.3's own text
        // is the reason this is acceptable rather than a shortcut: "MAP
        // packets shall have priority" over the UMF for any value that
        // could differ, and this crate's own MAP packet above already
        // states everything a simple clip needs.
        let mut umf_material = vec![0u8; 56];
        put_u32_le(&mut umf_material, 4, video_fields)?; // max length in fields
        put_u32_le(&mut umf_material, 8, video_fields)?; // min length in fields
        put_u32_le(&mut umf_material, 16, video_fields)?; // mark out
        let mut umf_payload_desc = vec![0u8; 48];
        let umf_total_len = u32::try_from(56 + umf_payload_desc.len()).unwrap_or(u32::MAX);
        put_u32_le(&mut umf_payload_desc, 0, umf_total_len)?;
        put_u32_le(&mut umf_payload_desc, 4, 3)?; // version
        // num_tracks, track/media section offsets and sizes, num_segments,
        // user data offset/size: all left at 0.
        let mut umf_payload = umf_payload_desc;
        umf_payload.extend_from_slice(&umf_material);
        let umf_len = u32::try_from(umf_payload.len()).unwrap_or(u32::MAX);
        let mut umf_full = Vec::new();
        umf_full.push(3); // first and last packet (only packet)
        umf_full.extend_from_slice(&umf_len.to_be_bytes());
        umf_full.extend_from_slice(&umf_payload);
        self.write_packet_bytes(packet::PKT_UMF, &umf_full)?;

        if let Some(video) = self.video.take() {
            for (i, (essence, is_key)) in video.frames.into_iter().enumerate() {
                let field_number = u32::try_from(i).unwrap_or(u32::MAX).saturating_mul(2);
                let picture_coding: u8 = if is_key { 0b01 } else { 0b10 };
                let field_info = [(picture_coding | (0b11 << 2)), 0, 0, 0];
                self.write_media_packet(12, video.track_id, field_number, field_info, &essence)?;
            }
        }
        if let Some(audio) = self.audio.take() {
            const BYTES_PER_SAMPLE: usize = 2;
            const PACKET_BYTES: usize = (AUDIO_SAMPLES_PER_PACKET as usize) * BYTES_PER_SAMPLE;
            let mut offset = 0usize;
            let mut packet_index = 0u64;
            while offset < audio.bytes.len() {
                let end = (offset + PACKET_BYTES).min(audio.bytes.len());
                let mut chunk = audio
                    .bytes
                    .get(offset..end)
                    .ok_or(Error::InvalidData("gxf: audio chunk range overran the buffered bytes"))?
                    .to_vec();
                chunk.resize(PACKET_BYTES, 0);
                let field_number = Self::audio_packet_field_number(packet_index, field_rate);
                // Clause 7.4.2.1.4 lets a short trailing packet declare a
                // smaller valid-sample range, but the real fixture this
                // crate measured does not use that: `ffmpeg -f gxf`
                // declares every audio packet fully valid
                // (`field_info = 00 00 80 00`, i.e. 32,768) even for its
                // own genuinely-partial last packet of a clip whose sample
                // count is not an exact multiple of 32,768. Measured the
                // other way too: stating the honest partial count here
                // made real `ffmpeg`'s own gxf demuxer *truncate* the
                // packet it reported to exactly that many bytes — visibly
                // different container-level behaviour from the reference
                // file's own shape. Matching the measured convention
                // (always claim full validity) is what real interop with
                // `ffmpeg` needs, at the cost of a handful of zero-padded
                // samples silently claimed valid in a genuinely short
                // final chunk.
                let field_info = [0, 0, 0x80, 0x00];
                self.write_media_packet(10, audio.track_id, field_number, field_info, &chunk)?;
                offset = end;
                packet_index += 1;
            }
        }

        self.write_packet_bytes(packet::PKT_EOS, &[])?;
        self.sink.flush()
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "MuxerDesc::open's signature is a fixed fn pointer type roughly 90 registered muxers already implement; GxfMuxer::new happens to be infallible, but the descriptor's own type is not"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GxfMuxer::new(sink)))
}

/// The descriptor `vaco-registry` holds (`vaco-component.toml`'s own
/// `ctor = "vaco_format_gxf::MUXER"`).
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "gxf",
    long_name: "GXF (General eXchange Format)",
    extensions: &["gxf"],
    default_video: Some(CodecId::Mpeg2video),
    default_audio: Some(CodecId::PcmS16le),
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn audio_field_numbers_match_the_real_fixtures_three_packets() {
        let field_rate = Rational::new(50, 1); // PAL, 25 fps video.
        assert_eq!(GxfMuxer::audio_packet_field_number(0, field_rate), 0);
        assert_eq!(GxfMuxer::audio_packet_field_number(1, field_rate), 35);
        assert_eq!(GxfMuxer::audio_packet_field_number(2, field_rate), 69);
    }
}
