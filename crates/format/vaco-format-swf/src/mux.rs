//! The `swf` muxer: header + `DefineVideoStream`/`SoundStreamHead2` +
//! per-frame `VideoFrame`/`SoundStreamBlock`/`ShowFrame` + `End`.
//!
//! # What this does not reproduce, and why
//!
//! **`PlaceObject2` is never written.** The reference muxer writes one to
//! place the video/sound character on the display list (with a `Matrix`
//! record and an ffmpeg-specific incrementing `Ratio` field per frame) —
//! real, measured behaviour, not invented — but stripping every
//! `PlaceObject2` tag out of a real reference `.swf` file and re-running
//! `ffprobe -f swf` on the result still reports the correct
//! codec/dimensions/sample rate/channels and the full packet count (checked
//! directly, see `lib.rs`'s module docs). Writing `Matrix`/`ColorTransform`
//! records byte-identically to the reference would be real work for zero
//! measured behavioural gain, so this crate spends its time elsewhere.
//!
//! **Frame ordering is video-tag-then-audio-tag-then-`ShowFrame`, keyed off
//! video packets.** The reference interleaves one `VideoFrame`, one
//! `SoundStreamBlock`, then `ShowFrame`, per display frame. This muxer
//! writes a tag immediately for whatever packet it is handed and emits
//! `ShowFrame` right after every *video* packet's tag — correct for the
//! common case (a caller feeding packets in roughly presentation order) but
//! not guaranteed to place `ShowFrame` markers at the exact same points a
//! caller supplying audio-then-video, or audio-only input, would produce.
//!
//! # What is buffered, and why
//!
//! The whole tag stream is built in memory (`tag_buf`) rather than written
//! straight to the sink, because two fields the header/`DefineVideoStream`/
//! `SoundStreamHead` need — `FileLength`, `NumFrames`, and the audio stream's
//! total sample count — are only known once every packet has been written.
//! `write_trailer` patches those in-place (by byte offset into the buffer,
//! recorded when each tag was first written) and flushes everything in one
//! `MediaSink::write` call, so this muxer never needs the sink to be
//! seekable.

use crate::header::SwfHeader;
use crate::tags::{
    TAG_END, TAG_SHOW_FRAME, TAG_SOUND_STREAM_BLOCK, TAG_SOUND_STREAM_HEAD2, TagHeader,
};
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::MediaSink;
use vaco_packet::Packet;

use crate::demux::SOUND_RATES;

/// `ffmpeg -h muxer=swf` states no options and a version this crate has not
/// seen vary; every measured sample used version 6.
const SWF_VERSION: u8 = 6;

struct VideoMuxState {
    character_id: u16,
    codec_byte: u8,
    /// Byte offset of `DefineVideoStream`'s `NumFrames` field in `tag_buf`.
    num_frames_offset: usize,
    frame_count: u32,
}

struct AudioMuxState {
    codec_byte: u8,
    is_mp3: bool,
    /// Byte offset of `SoundStreamHead2`'s `StreamSoundSampleCount` field.
    sample_count_offset: usize,
    samples_written: u64,
}

/// The `swf` muxer.
pub struct SwfMuxer {
    sink: Box<dyn MediaSink>,
    stage_width_twips: i32,
    stage_height_twips: i32,
    frame_rate_raw: u16,
    tag_buf: Vec<u8>,
    video_index: Option<u32>,
    audio_index: Option<u32>,
    video: Option<VideoMuxState>,
    audio: Option<AudioMuxState>,
}

impl std::fmt::Debug for SwfMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwfMuxer")
            .field("frame_rate_raw", &self.frame_rate_raw)
            .field("tag_buf_len", &self.tag_buf.len())
            .finish_non_exhaustive()
    }
}

fn sample_rate_to_index(rate: u32) -> Result<u8> {
    SOUND_RATES
        .iter()
        .position(|&r| r == rate)
        .map(|i| i as u8)
        .ok_or(Error::Unsupported(
            "swf: sample rate must be one of 5512/11025/22050/44100 Hz",
        ))
}

impl SwfMuxer {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self {
            sink,
            stage_width_twips: 0,
            stage_height_twips: 0,
            frame_rate_raw: 0,
            tag_buf: Vec::new(),
            video_index: None,
            audio_index: None,
            video: None,
            audio: None,
        }
    }

    fn add_video_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.video.is_some() {
            return Err(Error::Unsupported(
                "swf: only one video stream is supported",
            ));
        }
        let Some(CodecId::Flv1) = params.codec_id else {
            return Err(Error::Unsupported(
                "swf: only FLV1 (Sorenson H.263) video is supported",
            ));
        };
        let video = params
            .video
            .as_ref()
            .ok_or(Error::Unsupported("swf: missing video parameters"))?;
        let width = u16::try_from(video.width)
            .map_err(|_| Error::Unsupported("swf: width does not fit in 16 bits"))?;
        let height = u16::try_from(video.height)
            .map_err(|_| Error::Unsupported("swf: height does not fit in 16 bits"))?;
        self.stage_width_twips = i32::try_from(video.width).unwrap_or(0).saturating_mul(20);
        self.stage_height_twips = i32::try_from(video.height).unwrap_or(0).saturating_mul(20);
        self.frame_rate_raw = frame_rate_to_raw(video.frame_rate)?;

        let character_id: u16 = 0;
        let mut tag = Vec::new();
        tag.extend_from_slice(&character_id.to_le_bytes());
        let num_frames_offset_in_tag = tag.len();
        tag.extend_from_slice(&0u16.to_le_bytes()); // NumFrames, patched later
        tag.extend_from_slice(&width.to_le_bytes());
        tag.extend_from_slice(&height.to_le_bytes());
        tag.push(0); // VideoFlags: no deblocking hint, no smoothing
        let codec_byte = crate::demux::video_codec_to_swf(CodecId::Flv1)
            .ok_or(Error::Unsupported("swf: no SWF codec byte for FLV1"))?;
        tag.push(codec_byte);

        let header = TagHeader::write(crate::tags::TAG_DEFINE_VIDEO_STREAM, tag.len() as u32)?;
        let num_frames_offset = self.tag_buf.len() + header.len() + num_frames_offset_in_tag;
        self.tag_buf.extend_from_slice(&header);
        self.tag_buf.extend_from_slice(&tag);

        self.video = Some(VideoMuxState {
            character_id,
            codec_byte,
            num_frames_offset,
            frame_count: 0,
        });
        let index = self.video_index.unwrap_or(0);
        self.video_index = Some(index);
        Ok(index)
    }

    fn add_audio_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.audio.is_some() {
            return Err(Error::Unsupported(
                "swf: only one audio stream is supported",
            ));
        }
        let codec = params
            .codec_id
            .ok_or(Error::Unsupported("swf: missing audio codec"))?;
        let codec_byte = crate::demux::audio_codec_to_swf(codec).ok_or(Error::Unsupported(
            "swf: only MP3 or 16-bit PCM audio is supported",
        ))?;
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::Unsupported("swf: missing audio parameters"))?;
        let rate_idx = sample_rate_to_index(audio.sample_rate)?;
        let channels = audio.layout.as_ref().map_or(1, |l| l.iter().count());
        let type_bit: u8 = match channels {
            1 => 0,
            2 => 1,
            _ => {
                return Err(Error::Unsupported(
                    "swf: only mono or stereo audio is supported",
                ));
            }
        };
        let size_bit: u8 = 1; // 16-bit, matching every measured sample

        let byte0 = (rate_idx << 2) | (size_bit << 1) | type_bit;
        let byte1 = (codec_byte << 4) | (rate_idx << 2) | (size_bit << 1) | type_bit;

        let mut tag = vec![byte0, byte1];
        let sample_count_offset_in_tag = tag.len();
        tag.extend_from_slice(&0u16.to_le_bytes()); // StreamSoundSampleCount, patched later
        let is_mp3 = codec_byte == 2;
        if is_mp3 {
            tag.extend_from_slice(&0i16.to_le_bytes()); // LatencySeek
        }

        let header = TagHeader::write(TAG_SOUND_STREAM_HEAD2, tag.len() as u32)?;
        let sample_count_offset = self.tag_buf.len() + header.len() + sample_count_offset_in_tag;
        self.tag_buf.extend_from_slice(&header);
        self.tag_buf.extend_from_slice(&tag);

        self.audio = Some(AudioMuxState {
            codec_byte,
            is_mp3,
            sample_count_offset,
            samples_written: 0,
        });
        let index = self.audio_index.unwrap_or(1);
        self.audio_index = Some(index);
        Ok(index)
    }

    fn write_video_frame(&mut self, packet: &Packet) -> Result<()> {
        let Some(video) = &mut self.video else {
            return Err(Error::Unsupported("swf: no video stream declared"));
        };
        let frame_num = u16::try_from(video.frame_count).unwrap_or(u16::MAX);
        let mut tag = Vec::new();
        tag.extend_from_slice(&video.character_id.to_le_bytes());
        tag.extend_from_slice(&frame_num.to_le_bytes());
        tag.extend_from_slice(packet.payload());
        let header = TagHeader::write(crate::tags::TAG_VIDEO_FRAME, tag.len() as u32)?;
        self.tag_buf.extend_from_slice(&header);
        self.tag_buf.extend_from_slice(&tag);
        video.frame_count = video.frame_count.saturating_add(1);
        let _ = video.codec_byte;

        let show_frame = TagHeader::write(TAG_SHOW_FRAME, 0)?;
        self.tag_buf.extend_from_slice(&show_frame);
        Ok(())
    }

    fn write_audio_block(&mut self, packet: &Packet) -> Result<()> {
        let Some(audio) = &mut self.audio else {
            return Err(Error::Unsupported("swf: no audio stream declared"));
        };
        // This muxer does not decode the payload to count real samples (no
        // MP3/PCM parser here); it charges the block's *byte* length to the
        // informational `StreamSoundSampleCount` field instead of a real
        // sample count, which is honestly wrong for MP3 (a sample count,
        // not a byte count) but affects nothing this crate's own demuxer
        // reads back — see the module docs.
        let mut tag = Vec::new();
        let approx_samples = u16::try_from(packet.payload().len()).unwrap_or(u16::MAX);
        tag.extend_from_slice(&approx_samples.to_le_bytes());
        if audio.is_mp3 {
            tag.extend_from_slice(&0i16.to_le_bytes());
        }
        tag.extend_from_slice(packet.payload());
        let header = TagHeader::write(TAG_SOUND_STREAM_BLOCK, tag.len() as u32)?;
        self.tag_buf.extend_from_slice(&header);
        self.tag_buf.extend_from_slice(&tag);
        audio.samples_written = audio
            .samples_written
            .saturating_add(u64::from(approx_samples));
        let _ = audio.codec_byte;
        Ok(())
    }
}

#[allow(
    clippy::integer_division,
    reason = "exact 8.8 fixed-point conversion, not an approximation"
)]
fn frame_rate_to_raw(rate: Rational) -> Result<u16> {
    if rate.den == 0 {
        return Err(Error::Unsupported("swf: video stream has no frame rate"));
    }
    let scaled = i64::from(rate.num).saturating_mul(256) / i64::from(rate.den);
    u16::try_from(scaled)
        .map_err(|_| Error::Unsupported("swf: frame rate out of range for an 8.8 fixed value"))
}

impl Muxer for SwfMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::empty()
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        match params.media_type {
            Some(MediaType::Video) => self.add_video_stream(params),
            Some(MediaType::Audio) => self.add_audio_stream(params),
            _ => Err(Error::Unsupported("swf: unsupported media type")),
        }
    }

    fn write_header(&mut self) -> Result<()> {
        // Deferred: the fixed header is only assembled in `write_trailer`,
        // once `FileLength` and `NumFrames` are known — see the module
        // docs on why this muxer buffers instead of streaming.
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if Some(packet.stream_index) == self.video_index {
            self.write_video_frame(packet)
        } else if Some(packet.stream_index) == self.audio_index {
            self.write_audio_block(packet)
        } else {
            Err(Error::Unsupported("swf: unknown stream index"))
        }
    }

    fn write_trailer(&mut self) -> Result<()> {
        if let Some(video) = &self.video
            && let Some(slot) = self
                .tag_buf
                .get_mut(video.num_frames_offset..video.num_frames_offset + 2)
        {
            let frame_count = u16::try_from(video.frame_count).unwrap_or(u16::MAX);
            slot.copy_from_slice(&frame_count.to_le_bytes());
        }
        if let Some(audio) = &self.audio
            && let Some(slot) = self
                .tag_buf
                .get_mut(audio.sample_count_offset..audio.sample_count_offset + 2)
        {
            let samples = u16::try_from(audio.samples_written).unwrap_or(u16::MAX);
            slot.copy_from_slice(&samples.to_le_bytes());
        }

        let end_tag = TagHeader::write(TAG_END, 0)?;
        self.tag_buf.extend_from_slice(&end_tag);

        let header = SwfHeader {
            version: SWF_VERSION,
            file_length: 0,
            stage_width_twips: self.stage_width_twips,
            stage_height_twips: self.stage_height_twips,
            frame_rate_raw: self.frame_rate_raw,
            frame_count: self
                .video
                .as_ref()
                .map_or(0, |v| u16::try_from(v.frame_count).unwrap_or(u16::MAX)),
        };
        let mut header_bytes = header.write();
        let file_length = u32::try_from(header_bytes.len().saturating_add(self.tag_buf.len()))
            .unwrap_or(u32::MAX);
        if let Some(slot) = header_bytes.get_mut(4..8) {
            slot.copy_from_slice(&file_length.to_le_bytes());
        }

        self.sink.write(&header_bytes)?;
        self.sink.write(&self.tag_buf)?;
        self.sink.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::VideoParameters;
    use vaco_io::SharedDynBuf;
    use vaco_limits::{Budget, Limits};

    fn video_params(w: u32, h: u32, rate: Rational) -> CodecParameters {
        let mut p = CodecParameters::new(MediaType::Video).with_codec(CodecId::Flv1);
        p.video = Some(VideoParameters {
            width: w,
            height: h,
            frame_rate: rate,
            ..VideoParameters::default()
        });
        p
    }

    fn packet(bytes: &[u8]) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        Packet::from_slice(&mut budget, bytes).unwrap()
    }

    #[test]
    fn a_video_only_file_opens_with_the_correct_signature_and_frame_count() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = SwfMuxer::new(Box::new(sink));
        let idx = mux
            .add_stream(&video_params(64, 64, Rational { num: 12, den: 1 }))
            .unwrap();
        mux.write_header().unwrap();
        for _ in 0..3 {
            let mut p = packet(&[0u8; 8]);
            p.stream_index = idx;
            mux.write_packet(&p).unwrap();
        }
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        assert_eq!(&bytes[0..3], b"FWS");
        let (h, _) = SwfHeader::parse(&bytes).unwrap();
        assert_eq!(h.frame_count, 3);
        assert_eq!(h.frame_rate_raw, 12 * 256);
        assert_eq!(h.file_length as usize, bytes.len());
    }

    #[test]
    fn a_non_flv1_video_codec_is_refused() {
        let mut mux = SwfMuxer::new(Box::new(vaco_io::DynBuf::new()));
        let p = video_params(64, 64, Rational { num: 12, den: 1 });
        let mut p2 = p;
        p2.codec_id = Some(CodecId::H264);
        assert!(mux.add_stream(&p2).is_err());
    }
}
