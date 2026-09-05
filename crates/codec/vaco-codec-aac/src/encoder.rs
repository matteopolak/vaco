//! A deliberately narrow AAC-LC ADTS silence encoder.
//!
//! It accepts one packed `S16` mono or stereo 22.05, 24, 32, 44.1, or 48 kHz frame of
//! exactly 1024 zero samples per channel. It can return the raw AAC access unit
//! and its exact `AudioSpecificConfig`, or wrap that unit in self-contained
//! ADTS. General quantisation and psychoacoustics remain out of scope.

use vaco_bitstream::BitWriter;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{Accept, Caps, Machine, SendReceive};
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

const SAMPLE_RATE_48_KHZ: u32 = 48_000;
const SAMPLE_RATE_44_1_KHZ: u32 = 44_100;
const SAMPLE_RATE_32_KHZ: u32 = 32_000;
const SAMPLE_RATE_24_KHZ: u32 = 24_000;
const SAMPLE_RATE_22_05_KHZ: u32 = 22_050;
const SAMPLES_PER_FRAME: u32 = 1024;
const ADTS_HEADER_BYTES: u32 = 7;
const MONO_PACKED_S16_FRAME_BYTES: usize = 2048;
const STEREO_PACKED_S16_FRAME_BYTES: usize = 4096;
const MONO_48_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x11, 0x88];
const STEREO_48_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x11, 0x90];
const MONO_44_1_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x12, 0x08];
const STEREO_44_1_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x12, 0x10];
const MONO_32_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x12, 0x88];
const STEREO_32_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x12, 0x90];
const MONO_24_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x13, 0x08];
const STEREO_24_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x13, 0x10];
const MONO_22_05_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x13, 0x88];
const STEREO_22_05_KHZ_AUDIO_SPECIFIC_CONFIG: [u8; 2] = [0x13, 0x90];

/// A raw AAC-LC silence access unit paired with its out-of-band configuration.
///
/// The payload has no ADTS header. A container must carry
/// [`Self::audio_specific_config`] as its AAC extradata and frame the payload
/// according to its own packet rules.
#[derive(Clone, Debug)]
pub struct AacLcSilenceAccessUnit {
    payload: Vec<u8>,
    audio_specific_config: [u8; 2],
    sample_rate: u32,
    sampling_frequency_index: u32,
    channel_configuration: u32,
}

impl AacLcSilenceAccessUnit {
    /// Encode one exact-shape zero PCM frame as an unframed AAC-LC access unit.
    ///
    /// # Errors
    ///
    /// Refuses any non-`S16`, non-22.05/24/32/44.1/48 kHz, non-mono/stereo,
    /// non-1024-sample, or nonzero PCM input.
    pub fn from_frame(frame: &Frame) -> Result<Self> {
        let (
            sample_rate,
            sampling_frequency_index,
            channel_configuration,
            expected_plane_bytes,
            audio_specific_config,
        ) = silence_frame_configuration(frame)?;
        let FrameData::Audio { planes, .. } = &frame.data else {
            return Err(Error::InvalidData(
                "vaco-codec-aac: expected an audio frame",
            ));
        };
        let plane = planes
            .first()
            .ok_or(Error::InvalidData("vaco-codec-aac: missing audio plane"))?;
        let samples = plane.data.as_slice();
        if samples.len() != expected_plane_bytes || samples.iter().any(|&sample| sample != 0) {
            return Err(Error::Unsupported(
                "vaco-codec-aac: only all-zero PCM is supported by the AAC-LC encoder",
            ));
        }
        Ok(Self {
            payload: silent_raw_data_block(channel_configuration)?,
            audio_specific_config,
            sample_rate,
            sampling_frequency_index,
            channel_configuration,
        })
    }

    /// The unframed `raw_data_block()` payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The AAC-LC `AudioSpecificConfig` a container must carry out of band.
    #[must_use]
    pub const fn audio_specific_config(&self) -> [u8; 2] {
        self.audio_specific_config
    }
}

/// AAC-LC ADTS encoding for exactly one mono or stereo silent long-window frame at a time.
#[derive(Debug)]
pub struct AacLcSilenceEncoder {
    machine: Machine<Packet>,
    limits: Limits,
}

impl AacLcSilenceEncoder {
    /// Build the fixed-shape encoder bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(Caps::empty()),
            limits,
        }
    }
}

fn write_silent_individual_channel_stream(w: &mut BitWriter) {
    w.put(8, 0); // global_gain is unused by ZERO_HCB
    w.put(1, 0); // ics_reserved_bit
    w.put(2, 0); // ONLY_LONG_SEQUENCE
    w.put(1, 0); // window_shape
    w.put(6, 1); // max_sfb
    w.put(1, 0); // predictor_data_present
    w.put(4, 0); // ZERO_HCB
    w.put(5, 1); // section covers the one scalefactor band
    w.put(1, 0); // pulse_data_present
    w.put(1, 0); // tns_data_present
    w.put(1, 0); // gain_control_data_present
}

fn silent_raw_data_block(channel_configuration: u32) -> Result<Vec<u8>> {
    let mut w = BitWriter::new();
    match channel_configuration {
        1 => {
            w.put(3, 0); // ID_SCE
            w.put(4, 0); // element_instance_tag
            write_silent_individual_channel_stream(&mut w);
        }
        2 => {
            w.put(3, 1); // ID_CPE
            w.put(4, 0); // element_instance_tag
            w.put(1, 0); // common_window
            write_silent_individual_channel_stream(&mut w);
            write_silent_individual_channel_stream(&mut w);
        }
        _ => {
            return Err(Error::Unsupported(
                "vaco-codec-aac: only mono and stereo ADTS silence is supported",
            ));
        }
    }
    w.put(3, 7); // ID_END
    Ok(w.finish())
}

fn silence_frame_configuration(frame: &Frame) -> Result<(u32, u32, u32, usize, [u8; 2])> {
    let FrameData::Audio {
        format,
        sample_rate,
        samples,
        layout,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData(
            "vaco-codec-aac: expected an audio frame",
        ));
    };
    let (channel_configuration, expected_plane_bytes) = if layout == &ChannelLayout::MONO {
        (1, MONO_PACKED_S16_FRAME_BYTES)
    } else if layout == &ChannelLayout::STEREO {
        (2, STEREO_PACKED_S16_FRAME_BYTES)
    } else {
        return Err(Error::Unsupported(
            "vaco-codec-aac: only mono or stereo S16 silence can be encoded",
        ));
    };
    let (sampling_frequency_index, audio_specific_config) = match (
        *sample_rate,
        channel_configuration,
    ) {
        (SAMPLE_RATE_48_KHZ, 1) => (3, MONO_48_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_48_KHZ, 2) => (3, STEREO_48_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_44_1_KHZ, 1) => (4, MONO_44_1_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_44_1_KHZ, 2) => (4, STEREO_44_1_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_32_KHZ, 1) => (5, MONO_32_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_32_KHZ, 2) => (5, STEREO_32_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_24_KHZ, 1) => (6, MONO_24_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_24_KHZ, 2) => (6, STEREO_24_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_22_05_KHZ, 1) => (7, MONO_22_05_KHZ_AUDIO_SPECIFIC_CONFIG),
        (SAMPLE_RATE_22_05_KHZ, 2) => (7, STEREO_22_05_KHZ_AUDIO_SPECIFIC_CONFIG),
        _ => {
            return Err(Error::Unsupported(
                "vaco-codec-aac: only packed S16 mono or stereo 22.05/24/32/44.1/48 kHz silence can be encoded",
            ));
        }
    };
    if *format != SampleFmt::S16 || *samples != SAMPLES_PER_FRAME || planes.len() != 1 {
        return Err(Error::Unsupported(
            "vaco-codec-aac: only packed S16 mono or stereo 22.05/24/32/44.1/48 kHz frames of exactly 1024 samples are supported",
        ));
    }
    Ok((
        *sample_rate,
        sampling_frequency_index,
        channel_configuration,
        expected_plane_bytes,
        audio_specific_config,
    ))
}

fn silent_adts_access_unit(
    raw: &[u8],
    sampling_frequency_index: u32,
    channel_configuration: u32,
) -> Result<Vec<u8>> {
    let raw_len = u32::try_from(raw.len())
        .map_err(|_| Error::InvalidData("vaco-codec-aac: raw AAC frame is too large"))?;
    let frame_len = raw_len
        .checked_add(ADTS_HEADER_BYTES)
        .ok_or(Error::InvalidData(
            "vaco-codec-aac: ADTS frame length overflows",
        ))?;

    let mut header = BitWriter::new();
    header.put(12, 0xfff); // syncword
    header.put(1, 0); // MPEG-4 ID
    header.put(2, 0); // layer
    header.put(1, 1); // protection_absent
    header.put(2, 1); // profile: AAC-LC
    header.put(4, sampling_frequency_index); // sampling_frequency_index
    header.put(1, 0); // private_bit
    header.put(3, channel_configuration); // channel_configuration
    header.put(1, 0); // original_copy
    header.put(1, 0); // home
    header.put(1, 0); // copyright_identification_bit
    header.put(1, 0); // copyright_identification_start
    header.put(13, frame_len); // aac_frame_length
    header.put(11, 0x7ff); // adts_buffer_fullness: variable bitrate
    header.put(2, 0); // one raw_data_block
    let mut access_unit = header.finish();
    access_unit.extend_from_slice(raw);
    Ok(access_unit)
}

impl SendReceive for AacLcSilenceEncoder {
    type Input = Frame;
    type Output = Packet;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn send(&mut self, input: Option<&Frame>) -> Result<()> {
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(frame) = input else {
                    return Ok(());
                };
                let raw = AacLcSilenceAccessUnit::from_frame(frame)?;
                let access_unit = silent_adts_access_unit(
                    raw.payload(),
                    raw.sampling_frequency_index,
                    raw.channel_configuration,
                )?;
                let mut budget = Budget::new(self.limits.clone());
                let mut packet = Packet::from_slice(&mut budget, &access_unit)?;
                packet.pts = frame.pts;
                packet.duration = Timestamp::new(i64::from(SAMPLES_PER_FRAME))
                    .to_duration(Rational::new(
                        1,
                        i32::try_from(raw.sample_rate).map_err(|_| {
                            Error::InvalidData("vaco-codec-aac: sample rate exceeds time base")
                        })?,
                    ))
                    .unwrap_or(Duration::ZERO);
                packet.set_duration_ts(i64::from(SAMPLES_PER_FRAME));
                self.machine.emit(packet);
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<Packet> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
    }

    fn accepted_sample_fmts(&self) -> &'static [SampleFmt] {
        &[SampleFmt::S16]
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "oracle test code"
    )]

    use std::path::PathBuf;
    use std::process::Command;

    use super::AacLcSilenceEncoder;
    use crate::{AacLcSilenceAccessUnit, decoder::AacDecoder};
    use vaco_bitstream::BitWriter;
    use vaco_chlayout::ChannelLayout;
    use vaco_codec_core::{Decoder, SendReceive};
    use vaco_frame::{Frame, FrameData};
    use vaco_limits::{Budget, Limits};
    use vaco_packet::Packet;
    use vaco_sampfmt::SampleFmt;

    fn silence_frame(layout: ChannelLayout, sample_rate: u32) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        Frame::alloc_audio(&mut budget, SampleFmt::S16, layout, 1024, sample_rate).unwrap()
    }

    struct TemporaryAac {
        path: PathBuf,
    }

    impl Drop for TemporaryAac {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn adts_access_unit_for_raw(
        raw: &[u8],
        sampling_frequency_index: u32,
        channel_configuration: u32,
    ) -> Vec<u8> {
        let frame_len = u32::try_from(raw.len()).unwrap() + 7;
        let mut header = BitWriter::new();
        header.put(12, 0xfff);
        header.put(1, 0);
        header.put(2, 0);
        header.put(1, 1);
        header.put(2, 1);
        header.put(4, sampling_frequency_index);
        header.put(1, 0);
        header.put(3, channel_configuration);
        header.put(1, 0);
        header.put(1, 0);
        header.put(1, 0);
        header.put(1, 0);
        header.put(13, frame_len);
        header.put(11, 0x7ff);
        header.put(2, 0);
        let mut access_unit = header.finish();
        access_unit.extend_from_slice(raw);
        access_unit
    }

    fn assert_playable_adts_bytes(stream: Vec<u8>, sample_rate: u32, channels: usize) {
        let temporary = TemporaryAac {
            path: std::env::temp_dir().join(format!(
                "vaco-aac-silence-{}-{}.aac",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )),
        };
        std::fs::write(&temporary.path, stream).unwrap();

        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,sample_rate,channels",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&temporary.path)
            .output()
            .unwrap();
        assert!(
            probe.status.success(),
            "{}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let fields = String::from_utf8(probe.stdout).unwrap();
        assert!(fields.contains("codec_name=aac"), "{fields}");
        let expected_sample_rate = format!("sample_rate={sample_rate}");
        assert!(fields.contains(&expected_sample_rate), "{fields}");
        let expected_channels = format!("channels={channels}");
        assert!(fields.contains(&expected_channels), "{fields}");

        let decoded = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&temporary.path)
            .args(["-f", "f32le", "-acodec", "pcm_f32le", "-"])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert_eq!(decoded.stdout.len(), 3 * 1024 * channels * 4);
        assert!(decoded.stdout.iter().all(|&sample| sample == 0));
    }

    fn assert_playable_adts_stream(layout: ChannelLayout, sample_rate: u32, channels: usize) {
        let frame = silence_frame(layout, sample_rate);
        let mut encoder = AacLcSilenceEncoder::new(Limits::permissive());
        let mut stream = Vec::new();
        for _ in 0..3 {
            encoder.send(Some(&frame)).unwrap();
            stream.extend_from_slice(encoder.receive().unwrap().payload());
        }
        assert_playable_adts_bytes(stream, sample_rate, channels);
    }

    #[test]
    fn three_mono_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::MONO, 48_000, 1);
    }

    #[test]
    fn three_stereo_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::STEREO, 48_000, 2);
    }

    #[test]
    fn three_44100_stereo_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::STEREO, 44_100, 2);
    }

    #[test]
    fn three_44100_mono_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::MONO, 44_100, 1);
    }

    #[test]
    fn three_32000_stereo_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::STEREO, 32_000, 2);
    }

    #[test]
    fn three_32000_mono_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::MONO, 32_000, 1);
    }

    #[test]
    fn three_24000_stereo_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::STEREO, 24_000, 2);
    }

    #[test]
    fn three_24000_mono_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::MONO, 24_000, 1);
    }

    #[test]
    fn three_22050_stereo_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::STEREO, 22_050, 2);
    }

    #[test]
    fn three_22050_mono_silent_frames_form_a_playable_adts_stream_with_exact_counts() {
        assert_playable_adts_stream(ChannelLayout::MONO, 22_050, 1);
    }

    #[test]
    fn raw_stereo_access_unit_has_asc_and_decodes_with_both_paths() {
        let raw = AacLcSilenceAccessUnit::from_frame(&silence_frame(ChannelLayout::STEREO, 48_000))
            .unwrap();
        assert_eq!(raw.audio_specific_config(), [0x11, 0x90]);
        assert_playable_adts_bytes(
            (0..3)
                .flat_map(|_| adts_access_unit_for_raw(raw.payload(), 3, 2))
                .collect(),
            48_000,
            2,
        );

        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, raw.payload()).unwrap();
        let mut decoder = AacDecoder::new(Limits::permissive());
        decoder.set_extradata(&raw.audio_specific_config()).unwrap();
        decoder.send_packet(Some(&packet)).unwrap();
        let decoded = decoder.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            samples,
            layout,
            ..
        } = decoded.data
        else {
            panic!("expected audio frame");
        };
        assert_eq!((sample_rate, samples, layout.channels), (48_000, 1024, 2));
    }

    #[test]
    fn raw_44100_stereo_access_unit_has_matching_asc_and_decodes_with_both_paths() {
        let raw = AacLcSilenceAccessUnit::from_frame(&silence_frame(ChannelLayout::STEREO, 44_100))
            .unwrap();
        assert_eq!(raw.audio_specific_config(), [0x12, 0x10]);
        assert_playable_adts_bytes(
            (0..3)
                .flat_map(|_| adts_access_unit_for_raw(raw.payload(), 4, 2))
                .collect(),
            44_100,
            2,
        );

        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, raw.payload()).unwrap();
        let mut decoder = AacDecoder::new(Limits::permissive());
        decoder.set_extradata(&raw.audio_specific_config()).unwrap();
        decoder.send_packet(Some(&packet)).unwrap();
        let decoded = decoder.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            samples,
            layout,
            ..
        } = decoded.data
        else {
            panic!("expected audio frame");
        };
        assert_eq!((sample_rate, samples, layout.channels), (44_100, 1024, 2));
    }

    #[test]
    fn raw_32000_stereo_access_unit_has_matching_asc_and_decodes_with_both_paths() {
        let raw = AacLcSilenceAccessUnit::from_frame(&silence_frame(ChannelLayout::STEREO, 32_000))
            .unwrap();
        assert_eq!(raw.audio_specific_config(), [0x12, 0x90]);
        assert_playable_adts_bytes(
            (0..3)
                .flat_map(|_| adts_access_unit_for_raw(raw.payload(), 5, 2))
                .collect(),
            32_000,
            2,
        );

        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, raw.payload()).unwrap();
        let mut decoder = AacDecoder::new(Limits::permissive());
        decoder.set_extradata(&raw.audio_specific_config()).unwrap();
        decoder.send_packet(Some(&packet)).unwrap();
        let decoded = decoder.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            samples,
            layout,
            ..
        } = decoded.data
        else {
            panic!("expected audio frame");
        };
        assert_eq!((sample_rate, samples, layout.channels), (32_000, 1024, 2));
    }

    #[test]
    fn raw_24000_stereo_access_unit_has_matching_asc_and_decodes_with_both_paths() {
        let raw = AacLcSilenceAccessUnit::from_frame(&silence_frame(ChannelLayout::STEREO, 24_000))
            .unwrap();
        assert_eq!(raw.audio_specific_config(), [0x13, 0x10]);
        assert_playable_adts_bytes(
            (0..3)
                .flat_map(|_| adts_access_unit_for_raw(raw.payload(), 6, 2))
                .collect(),
            24_000,
            2,
        );

        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, raw.payload()).unwrap();
        let mut decoder = AacDecoder::new(Limits::permissive());
        decoder.set_extradata(&raw.audio_specific_config()).unwrap();
        decoder.send_packet(Some(&packet)).unwrap();
        let decoded = decoder.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            samples,
            layout,
            ..
        } = decoded.data
        else {
            panic!("expected audio frame");
        };
        assert_eq!((sample_rate, samples, layout.channels), (24_000, 1024, 2));
    }

    #[test]
    fn raw_22050_stereo_access_unit_has_matching_asc_and_decodes_with_both_paths() {
        let raw = AacLcSilenceAccessUnit::from_frame(&silence_frame(ChannelLayout::STEREO, 22_050))
            .unwrap();
        assert_eq!(raw.audio_specific_config(), [0x13, 0x90]);
        assert_playable_adts_bytes(
            (0..3)
                .flat_map(|_| adts_access_unit_for_raw(raw.payload(), 7, 2))
                .collect(),
            22_050,
            2,
        );

        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, raw.payload()).unwrap();
        let mut decoder = AacDecoder::new(Limits::permissive());
        decoder.set_extradata(&raw.audio_specific_config()).unwrap();
        decoder.send_packet(Some(&packet)).unwrap();
        let decoded = decoder.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            samples,
            layout,
            ..
        } = decoded.data
        else {
            panic!("expected audio frame");
        };
        assert_eq!((sample_rate, samples, layout.channels), (22_050, 1024, 2));
    }

    #[test]
    fn a_silent_adts_packet_decodes_to_one_mono_aac_lc_frame() {
        let frame = silence_frame(ChannelLayout::MONO, 48_000);
        let mut encoder = AacLcSilenceEncoder::new(Limits::permissive());
        encoder.send(Some(&frame)).unwrap();
        let packet = encoder.receive().unwrap();

        let mut decoder = AacDecoder::new(Limits::permissive());
        decoder.send_packet(Some(&packet)).unwrap();
        let decoded = decoder.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            samples,
            layout,
            ..
        } = decoded.data
        else {
            panic!("expected audio frame");
        };
        assert_eq!((sample_rate, samples, layout.channels), (48_000, 1024, 1));
    }

    #[test]
    fn a_stereo_silent_adts_packet_decodes_to_one_stereo_aac_lc_frame() {
        let frame = silence_frame(ChannelLayout::STEREO, 48_000);
        let mut encoder = AacLcSilenceEncoder::new(Limits::permissive());
        encoder.send(Some(&frame)).unwrap();
        let packet = encoder.receive().unwrap();

        let mut decoder = AacDecoder::new(Limits::permissive());
        decoder.send_packet(Some(&packet)).unwrap();
        let decoded = decoder.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            samples,
            layout,
            ..
        } = decoded.data
        else {
            panic!("expected audio frame");
        };
        assert_eq!((sample_rate, samples, layout.channels), (48_000, 1024, 2));
    }
}
