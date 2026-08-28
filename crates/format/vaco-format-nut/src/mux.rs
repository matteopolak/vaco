//! The `nut` muxer: `main_header` + `stream_header`s written once at
//! `write_header`, then one `syncpoint` before every keyframe (and
//! whenever `max_distance` would otherwise be exceeded) followed by a
//! `frame` per packet, using this crate's own single generic frame code —
//! see `header.rs`'s module docs for exactly what that table looks like and
//! why it does not attempt to reproduce the reference muxer's compact one.
//!
//! # What is not attempted here
//!
//! **`back_ptr` is always `0`.** Computing the specification's exact
//! back-pointer (the closest earlier syncpoint with a qualifying keyframe
//! between it and the current one, byte-aligned to a `back_ptr_div16*16+15`
//! boundary via stuffing bytes if needed) is a real seeking optimisation
//! this crate has not implemented — every syncpoint this muxer writes
//! still satisfies the specification's *placement* rule (immediately
//! before the keyframe that justified it), only the backward-navigation
//! pointer itself is a placeholder. A demuxer reading sequentially (this
//! crate's own [`crate::demux::NutDemuxer`] included) never looks at it.
//! [`crate::demux::NutDemuxer::seek`] is unimplemented for exactly this
//! reason: there is nothing correct for it to follow yet.

use crate::codecs::{audio_fourcc_for_codec, video_fourcc_for_codec};
use crate::header::{
    FLAG_CHECKSUM, FLAG_KEY, GENERIC_FRAME_CODE, MainHeader, STREAM_CLASS_AUDIO,
    STREAM_CLASS_VIDEO, StreamClassData, StreamHeader,
};
use crate::startcode::{FILE_ID_STRING, MAIN_STARTCODE, STREAM_STARTCODE, SYNCPOINT_STARTCODE};
use crate::vlc::{write_t, write_v};
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Result};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::MediaSink;
use vaco_packet::{Packet, PacketFlags};

/// `ffmpeg -h muxer=nut` states no options that change wire layout in a way
/// this crate reproduces (`-syncpoints`/`-write_index` govern optimisations
/// this muxer does not attempt either direction of).
const NUT_VERSION: u64 = 3;

/// SHOULD be `<=32768` per spec; this crate's own choice, matched to the
/// same figure the specification itself gives as the recommended ceiling.
const MAX_DISTANCE: u64 = 32768;

struct MuxStream {
    time_base: (u64, u64),
    max_pts_distance: i64,
    last_pts: i64,
    have_pts: bool,
}

/// The `nut` muxer.
pub struct NutMuxer {
    sink: Box<dyn MediaSink>,
    streams: Vec<MuxStream>,
    stream_headers: Vec<StreamHeader>,
    header_written: bool,
    bytes_since_startcode: u64,
    wrote_any_frame: bool,
}

impl std::fmt::Debug for NutMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NutMuxer")
            .field("streams", &self.streams.len())
            .field("header_written", &self.header_written)
            .finish_non_exhaustive()
    }
}

fn write_packet_framed(sink: &mut dyn MediaSink, startcode: u64, payload: &[u8]) -> Result<()> {
    let mut header = Vec::new();
    header.extend_from_slice(&startcode.to_be_bytes());
    // forward_ptr spans the payload *and* the trailing checksum, per spec
    // (measured — see `vaco-hash`'s `crc32_nut` docs for the derivation).
    let forward_ptr = (payload.len() as u64).saturating_add(4);
    write_v(&mut header, forward_ptr);
    if forward_ptr > 4096 {
        let checksum = vaco_hash::crc32_nut(&header);
        header.extend_from_slice(&checksum.to_be_bytes());
    }
    sink.write(&header)?;
    sink.write(payload)?;
    let footer_checksum = vaco_hash::crc32_nut(payload);
    sink.write(&footer_checksum.to_be_bytes())
}

impl NutMuxer {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self {
            sink,
            streams: Vec::new(),
            stream_headers: Vec::new(),
            header_written: false,
            bytes_since_startcode: 0,
            wrote_any_frame: false,
        }
    }

    fn write_syncpoint(&mut self, time_base_id: usize, ticks: i64) -> Result<()> {
        let mut payload = Vec::new();
        let ticks_u = u64::try_from(ticks).unwrap_or(0);
        write_t(
            &mut payload,
            ticks_u,
            time_base_id as u64,
            self.streams.len().max(1) as u64,
        );
        write_v(&mut payload, 0); // back_ptr_div16 — see module docs
        write_packet_framed(&mut *self.sink, SYNCPOINT_STARTCODE, &payload)?;
        self.bytes_since_startcode = 0;
        Ok(())
    }

    fn write_frame(&mut self, stream_idx: usize, packet: &Packet) -> Result<()> {
        let payload = packet.payload();
        let pts = packet.pts.ticks().unwrap_or(0);

        let is_key = packet.flags.contains(PacketFlags::KEY);
        let needs_sync = !self.wrote_any_frame
            || is_key
            || self
                .bytes_since_startcode
                .saturating_add(payload.len() as u64)
                > MAX_DISTANCE;
        if needs_sync {
            self.write_syncpoint(self.streams.get(stream_idx).map_or(0, |_| stream_idx), pts)?;
            if let Some(s) = self.streams.get_mut(stream_idx) {
                s.last_pts = pts;
                s.have_pts = true;
            }
        }
        self.wrote_any_frame = true;

        let stream = self
            .streams
            .get_mut(stream_idx)
            .ok_or(Error::Unsupported("nut: unknown stream index"))?;
        let delta = if stream.have_pts {
            pts - stream.last_pts
        } else {
            0
        };
        let needs_checksum = (payload.len() as u64) > MAX_DISTANCE.saturating_mul(2)
            || delta.unsigned_abs() > stream.max_pts_distance.unsigned_abs();
        stream.last_pts = pts;
        stream.have_pts = true;

        let mut coded_flags: u32 = 0;
        if is_key {
            coded_flags |= FLAG_KEY;
        }
        if needs_checksum {
            coded_flags |= FLAG_CHECKSUM;
        }

        let mut header_bytes = vec![GENERIC_FRAME_CODE];
        write_v(&mut header_bytes, u64::from(coded_flags));
        write_v(&mut header_bytes, stream_idx as u64);
        write_v(&mut header_bytes, (pts.saturating_add(1)) as u64);
        write_v(&mut header_bytes, payload.len() as u64);

        if needs_checksum {
            let checksum = vaco_hash::crc32_nut(&header_bytes);
            header_bytes.extend_from_slice(&checksum.to_be_bytes());
        }

        self.sink.write(&header_bytes)?;
        self.sink.write(payload)?;
        self.bytes_since_startcode = self
            .bytes_since_startcode
            .saturating_add(header_bytes.len() as u64)
            .saturating_add(payload.len() as u64);
        Ok(())
    }
}

impl Muxer for NutMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::empty()
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::Unsupported(
                "nut: all streams must be added before write_header",
            ));
        }
        let index = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        match params.media_type {
            Some(MediaType::Video) => {
                let video = params
                    .video
                    .as_ref()
                    .ok_or(Error::Unsupported("nut: missing video parameters"))?;
                let codec = params
                    .codec_id
                    .ok_or(Error::Unsupported("nut: missing video codec"))?;
                let fourcc = video_fourcc_for_codec(codec)
                    .ok_or(Error::Unsupported("nut: unsupported video codec"))?
                    .to_vec();
                let (num, den) = if video.frame_rate.num > 0 && video.frame_rate.den > 0 {
                    (video.frame_rate.den as u64, video.frame_rate.num as u64)
                } else {
                    (1, 25)
                };
                let sh = StreamHeader {
                    stream_id: u64::from(index),
                    stream_class: STREAM_CLASS_VIDEO,
                    fourcc,
                    time_base_id: u64::from(index),
                    msb_pts_shift: 0,
                    max_pts_distance: den,
                    decode_delay: 0,
                    stream_flags: 0,
                    codec_specific_data: params.extradata.clone().unwrap_or_default(),
                    class_data: StreamClassData::Video {
                        width: u64::from(video.width),
                        height: u64::from(video.height),
                        sample_width: 0,
                        sample_height: 0,
                        colorspace_type: 0,
                    },
                };
                self.streams.push(MuxStream {
                    time_base: (num, den),
                    max_pts_distance: i64::try_from(den).unwrap_or(i64::MAX),
                    last_pts: 0,
                    have_pts: false,
                });
                self.stream_headers.push(sh);
            }
            Some(MediaType::Audio) => {
                let audio = params
                    .audio
                    .as_ref()
                    .ok_or(Error::Unsupported("nut: missing audio parameters"))?;
                let codec = params
                    .codec_id
                    .ok_or(Error::Unsupported("nut: missing audio codec"))?;
                let fourcc = audio_fourcc_for_codec(codec)
                    .ok_or(Error::Unsupported("nut: unsupported audio codec"))?
                    .to_vec();
                let sample_rate = u64::from(audio.sample_rate.max(1));
                let channels = audio.layout.as_ref().map_or(1, |l| l.iter().count()) as u64;
                let sh = StreamHeader {
                    stream_id: u64::from(index),
                    stream_class: STREAM_CLASS_AUDIO,
                    fourcc,
                    time_base_id: u64::from(index),
                    msb_pts_shift: 0,
                    max_pts_distance: sample_rate,
                    decode_delay: 0,
                    stream_flags: 0,
                    codec_specific_data: params.extradata.clone().unwrap_or_default(),
                    class_data: StreamClassData::Audio {
                        samplerate_num: sample_rate,
                        samplerate_denom: 1,
                        channel_count: channels.max(1),
                    },
                };
                self.streams.push(MuxStream {
                    time_base: (1, sample_rate),
                    max_pts_distance: i64::try_from(sample_rate).unwrap_or(i64::MAX),
                    last_pts: 0,
                    have_pts: false,
                });
                self.stream_headers.push(sh);
            }
            _ => {
                return Err(Error::Unsupported(
                    "nut: only video and audio streams are supported",
                ));
            }
        }
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.streams.is_empty() {
            return Err(Error::Unsupported("nut: at least one stream is required"));
        }
        self.sink.write(FILE_ID_STRING)?;
        let time_bases: Vec<(u64, u64)> = self.streams.iter().map(|s| s.time_base).collect();
        let main = MainHeader {
            version: NUT_VERSION,
            stream_count: self.streams.len() as u64,
            max_distance: MAX_DISTANCE,
            time_bases,
            frame_code_table: Vec::new(), // written directly by MainHeader::write
            elision_headers: vec![Vec::new()],
            main_flags: 0,
        };
        write_packet_framed(&mut *self.sink, MAIN_STARTCODE, &main.write())?;
        for sh in &self.stream_headers.clone() {
            write_packet_framed(&mut *self.sink, STREAM_STARTCODE, &sh.write())?;
        }
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let stream_idx = usize::try_from(packet.stream_index)
            .map_err(|_| Error::Unsupported("nut: bad stream index"))?;
        self.write_frame(stream_idx, packet)
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.sink.flush()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_codec_core::{CodecId, VideoParameters};
    use vaco_core::Rational;
    use vaco_io::{DynBuf, SharedDynBuf};
    use vaco_limits::{Budget, Limits};

    fn video_params() -> CodecParameters {
        let mut p = CodecParameters::new(MediaType::Video).with_codec(CodecId::Mpeg4);
        p.video = Some(VideoParameters {
            width: 64,
            height: 64,
            frame_rate: Rational { num: 25, den: 1 },
            ..VideoParameters::default()
        });
        p
    }

    fn packet(bytes: &[u8], pts: i64, key: bool) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, bytes).unwrap();
        pkt.pts = vaco_core::Timestamp::new(pts);
        if key {
            pkt.flags |= PacketFlags::KEY;
        }
        pkt
    }

    #[test]
    fn a_video_only_file_starts_with_the_file_id_string_and_main_startcode() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = NutMuxer::new(Box::new(sink));
        let idx = mux.add_stream(&video_params()).unwrap();
        mux.write_header().unwrap();
        let mut p = packet(&[1, 2, 3, 4], 0, true);
        p.stream_index = idx;
        mux.write_packet(&p).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        assert!(bytes.starts_with(crate::startcode::FILE_ID_STRING));
        let after_sig = &bytes[crate::startcode::FILE_ID_STRING.len()..];
        assert_eq!(
            u64::from_be_bytes(after_sig[0..8].try_into().unwrap()),
            crate::startcode::MAIN_STARTCODE
        );
    }

    #[test]
    fn an_unsupported_codec_is_refused() {
        let mut mux = NutMuxer::new(Box::new(DynBuf::new()));
        let mut p = video_params();
        p.codec_id = Some(CodecId::Hevc);
        assert!(mux.add_stream(&p).is_err());
    }

    fn audio_params() -> CodecParameters {
        let mut p = CodecParameters::new(MediaType::Audio).with_codec(CodecId::Mp3);
        p.audio = Some(vaco_codec_core::AudioParameters {
            sample_rate: 48_000,
            ..vaco_codec_core::AudioParameters::default()
        });
        p
    }

    /// Self-consistency round trip (this crate has no reference `nut` file
    /// to compare its own muxer's byte output against — see the module
    /// docs on why this muxer's table is not the reference's compact one —
    /// so the oracle here is this crate's own demuxer): two streams, a
    /// syncpoint forced by every keyframe, and a non-trivial pts sequence,
    /// which exercises the `pts+1` full-pts encoding
    /// (`msb_pts_shift=0` degenerate case) on the way back out.
    #[test]
    fn a_two_stream_file_this_crate_wrote_demuxes_back_to_the_same_packets() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = NutMuxer::new(Box::new(sink));
        let video_idx = mux.add_stream(&video_params()).unwrap();
        let audio_idx = mux.add_stream(&audio_params()).unwrap();
        mux.write_header().unwrap();

        let video_frames: &[(&[u8], i64, bool)] = &[
            (&[1, 1, 1, 1], 0, true),
            (&[2, 2, 2, 2], 1, false),
            (&[3, 3, 3, 3], 2, false),
        ];
        let audio_frames: &[(&[u8], i64, bool)] = &[
            (&[9, 9], 0, true),
            (&[8, 8], 100, true),
            (&[7, 7], 200, true),
        ];
        for &(bytes, pts, key) in video_frames {
            let mut p = packet(bytes, pts, key);
            p.stream_index = video_idx;
            mux.write_packet(&p).unwrap();
        }
        for &(bytes, pts, key) in audio_frames {
            let mut p = packet(bytes, pts, key);
            p.stream_index = audio_idx;
            mux.write_packet(&p).unwrap();
        }
        mux.write_trailer().unwrap();

        let bytes = mirror.take();
        let src = Box::new(vaco_io::MemorySource::new(bytes));
        let mut demux = crate::demux::NutDemuxer::open(src).unwrap();
        assert_eq!(vaco_format_core::Demuxer::streams(&demux).len(), 2);

        let mut got_video = Vec::new();
        let mut got_audio = Vec::new();
        loop {
            match vaco_format_core::Demuxer::read_packet(&mut demux) {
                Ok(p) => {
                    let payload = p.payload().to_vec();
                    let pts = p.pts.ticks().unwrap();
                    if p.stream_index == video_idx {
                        got_video.push((payload, pts));
                    } else {
                        got_audio.push((payload, pts));
                    }
                }
                Err(vaco_core::Error::Eof) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        let want_video: Vec<_> = video_frames
            .iter()
            .map(|&(b, p, _)| (b.to_vec(), p))
            .collect();
        let want_audio: Vec<_> = audio_frames
            .iter()
            .map(|&(b, p, _)| (b.to_vec(), p))
            .collect();
        assert_eq!(got_video, want_video);
        assert_eq!(got_audio, want_audio);
    }
}
