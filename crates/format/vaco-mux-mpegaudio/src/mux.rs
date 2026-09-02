//! The `mp3` muxer's `Muxer` implementation.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Rational, Result};
use vaco_format_core::Muxer;
use vaco_format_mpegaudio::{ChannelMode, Layer, MpegAudioHeader, version_for_sample_rate};

use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::{Packet, PacketSideData};

/// The reference's own empty `ID3v2.4` tag: a ten-byte header (synchsafe
/// size `10`) followed by ten zero bytes of reserved padding and no frames.
/// Confirmed byte-for-byte against `ffmpeg -c copy -fflags +bitexact -f mp3`,
/// which writes exactly this regardless of the source's own tag content.
const EMPTY_ID3V2_TAG: [u8; 20] = [
    b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

const XING_HEADER_LEN: usize = 4 + 4 + 4 + 4 + 100 + 4;
const LAME_EXT_LEN: usize = 36;

/// The reference decoder's own extra filterbank delay: see
/// `vaco-demux-mpegaudio`'s use of the same constant on the read side.
const DECODER_DELAY: u32 = 529;

#[derive(Debug, Clone, Copy)]
struct StreamInfo {
    sample_rate: u32,
    channels: u8,
    bit_rate: Option<u64>,
}

#[derive(Debug)]
pub struct MpegAudioMuxer {
    out: IoWriter,
    stream: Option<StreamInfo>,
    header_written: bool,
    /// Byte offset of the synthesized Xing/LAME frame's own start, for the
    /// trailer patch.
    xing_frame_pos: u64,
    xing_frame_len: u32,
    packets_written: u64,
    audio_bytes_written: u64,
    skip_start: u32,
    skip_end: u32,
    /// The first packet's own declared bit-rate index (`255` if its header
    /// did not parse), to notice a later one that differs.
    first_bitrate_index: Option<u8>,
    /// Whether every packet's header has declared the same bit rate so far.
    /// A truly constant bit rate is what the reference calls `"Info"`
    /// instead of `"Xing"` in the header it writes.
    constant_length: bool,
}

impl MpegAudioMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            stream: None,
            header_written: false,
            xing_frame_pos: 0,
            xing_frame_len: 0,
            packets_written: 0,
            audio_bytes_written: 0,
            skip_start: 0,
            skip_end: 0,
            first_bitrate_index: None,
            constant_length: true,
        })
    }
}

impl Muxer for MpegAudioMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("mp3: only one stream is supported"));
        }
        if params.codec_id != Some(CodecId::Mp3) {
            return Err(Error::Unsupported(
                "mp3: only MPEG audio layer 3 can be muxed",
            ));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("mp3: not an audio stream"))?;
        self.stream = Some(StreamInfo {
            sample_rate: audio.sample_rate.max(1),
            channels: audio
                .layout
                .as_ref()
                .map_or(2, |l| l.channels.clamp(1, 2) as u8),
            bit_rate: params.bit_rate,
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let info = self
            .stream
            .ok_or(Error::InvalidData("mp3: no stream added"))?;
        self.out.write(&EMPTY_ID3V2_TAG)?;
        self.xing_frame_pos = self.out.pos();
        let frame = build_xing_frame(info)?;
        self.xing_frame_len = frame.len() as u32;
        self.out.write(&frame)?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("mp3: packet written before the header"));
        }
        for sd in &packet.side_data {
            if let PacketSideData::SkipSamples { start, end, .. } = sd {
                if *start != 0 {
                    self.skip_start = *start;
                }
                if *end != 0 {
                    self.skip_end = *end;
                }
            }
        }
        self.packets_written = self.packets_written.saturating_add(1);
        let len = packet.payload().len();
        // A constant *byte* length is the wrong test: a real CBR stream still
        // alternates length by one byte through the padding bit to average out
        // a fractional bits-per-frame. What stays constant is the bit rate the
        // frame header itself declares.
        let bitrate_index =
            MpegAudioHeader::parse_bytes(packet.payload()).map_or(255, |h| h.bitrate_index);
        match self.first_bitrate_index {
            None => self.first_bitrate_index = Some(bitrate_index),
            Some(first) if first != bitrate_index => self.constant_length = false,
            Some(_) => {}
        }
        self.audio_bytes_written = self.audio_bytes_written.saturating_add(len as u64);
        self.out.write(packet.payload())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        if stream_index != 0 {
            return None;
        }
        self.stream.map(|s| Rational {
            num: 1,
            den: s.sample_rate.cast_signed(),
        })
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("mp3: trailer written before the header"));
        }
        if self.out.is_seekable() {
            patch_xing_frame(self)?;
        }
        self.out.flush()
    }
}

/// The smallest bit-rate index whose frame length can hold the full
/// Xing+LAME payload, unless the stream's own declared bit rate already
/// fits, in which case that one is used (matches the reference's choice on
/// a CBR source: the placeholder frame is written at the stream's own bit
/// rate there, not the theoretical minimum).
#[allow(
    clippy::integer_division,
    reason = "the source's declared bit rate is only meaningful in whole kbps"
)]
fn choose_bitrate_index(base: MpegAudioHeader, required: u32, bit_rate: Option<u64>) -> u8 {
    if let Some(bps) = bit_rate {
        let kbps = bps / 1000;
        for idx in 1u8..=14 {
            let candidate = MpegAudioHeader {
                bitrate_index: idx,
                ..base
            };
            if candidate
                .bitrate_kbps()
                .is_some_and(|k| u64::from(k) == kbps)
                && candidate.frame_len().is_some_and(|l| l >= required)
            {
                return idx;
            }
        }
    }
    for idx in 1u8..=14 {
        let candidate = MpegAudioHeader {
            bitrate_index: idx,
            ..base
        };
        if candidate.frame_len().is_some_and(|l| l >= required) {
            return idx;
        }
    }
    14
}

fn build_xing_frame(info: StreamInfo) -> Result<Vec<u8>> {
    let (version, sample_rate_index) = version_for_sample_rate(info.sample_rate)
        .ok_or(Error::Unsupported("mp3: unsupported sample rate"))?;
    let channel_mode = if info.channels <= 1 {
        ChannelMode::Mono
    } else {
        ChannelMode::Stereo
    };
    let base = MpegAudioHeader {
        version,
        layer: Layer::III,
        has_crc: false,
        bitrate_index: 1,
        sample_rate_index,
        padding: false,
        private_bit: false,
        channel_mode,
        mode_extension: 0,
        copyright: false,
        original: false,
        emphasis: vaco_format_mpegaudio::Emphasis::None,
    };
    let side_info_len = base
        .side_info_len()
        .ok_or(Error::Unsupported("mp3: side info length is undefined"))?;
    let required = (MpegAudioHeader::LEN + side_info_len + XING_HEADER_LEN + LAME_EXT_LEN) as u32;
    let bitrate_index = choose_bitrate_index(base, required, info.bit_rate);
    let header = MpegAudioHeader {
        bitrate_index,
        ..base
    };
    let frame_len = header.frame_len().unwrap_or(required).max(required) as usize;

    let mut frame = vec![0u8; frame_len];
    write_at(&mut frame, 0, &header.to_bytes());
    let xing_off = MpegAudioHeader::LEN + side_info_len;
    write_at(&mut frame, xing_off, b"Xing");
    write_at(&mut frame, xing_off + 4, &0x0000_000Fu32.to_be_bytes());
    let lame_off = xing_off + XING_HEADER_LEN;
    write_at(&mut frame, lame_off, b"Lavf\0\0\0\0\0");
    Ok(frame)
}

fn write_at(buf: &mut [u8], at: usize, bytes: &[u8]) {
    if let Some(dst) = buf.get_mut(at..at.saturating_add(bytes.len())) {
        dst.copy_from_slice(bytes);
    }
}

fn patch_xing_frame(muxer: &mut MpegAudioMuxer) -> Result<()> {
    let Some(info) = muxer.stream else {
        return Ok(());
    };
    let (version, sample_rate_index) = version_for_sample_rate(info.sample_rate)
        .unwrap_or((vaco_format_mpegaudio::Version::Mpeg1, 0));
    let channel_mode = if info.channels <= 1 {
        ChannelMode::Mono
    } else {
        ChannelMode::Stereo
    };
    let base = MpegAudioHeader {
        version,
        layer: Layer::III,
        has_crc: false,
        bitrate_index: 1,
        sample_rate_index,
        padding: false,
        private_bit: false,
        channel_mode,
        mode_extension: 0,
        copyright: false,
        original: false,
        emphasis: vaco_format_mpegaudio::Emphasis::None,
    };
    let Some(side_info_len) = base.side_info_len() else {
        return Ok(());
    };
    let xing_off = MpegAudioHeader::LEN + side_info_len;

    let total_bytes = u64::from(muxer.xing_frame_len).saturating_add(muxer.audio_bytes_written);
    let frames_field = u32::try_from(muxer.packets_written).unwrap_or(u32::MAX);
    let bytes_field = u32::try_from(total_bytes).unwrap_or(u32::MAX);
    let lame_delay = muxer.skip_start.saturating_sub(DECODER_DELAY).min(0x0FFF);
    let lame_padding = muxer
        .skip_end
        .saturating_add(if muxer.skip_end > 0 { DECODER_DELAY } else { 0 })
        .min(0x0FFF);

    if muxer.constant_length {
        muxer.out.seek(muxer.xing_frame_pos + xing_off as u64)?;
        muxer.out.write(b"Info")?;
    }

    let frames_pos = muxer.xing_frame_pos + (xing_off as u64) + 8;
    muxer.out.seek(frames_pos)?;
    muxer.out.write(&frames_field.to_be_bytes())?;
    muxer.out.write(&bytes_field.to_be_bytes())?;

    let lame_off = xing_off + XING_HEADER_LEN;
    let delay_padding_pos = muxer.xing_frame_pos + (lame_off as u64) + 21;
    let packed = [
        (lame_delay >> 4) as u8,
        (((lame_delay & 0x0F) << 4) | (lame_padding >> 8)) as u8,
        (lame_padding & 0xFF) as u8,
    ];
    muxer.out.seek(delay_padding_pos)?;
    muxer.out.write(&packed)?;

    let musiclength_pos = muxer.xing_frame_pos + (lame_off as u64) + 28;
    muxer.out.seek(musiclength_pos)?;
    muxer.out.write(&bytes_field.to_be_bytes())?;

    muxer
        .out
        .seek(muxer.xing_frame_pos + u64::from(muxer.xing_frame_len))?;
    Ok(())
}
