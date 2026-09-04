//! `Decoder` implementation: per-packet frame-header dispatch to Layer I, II
//! or III, and the shared per-channel synthesis filterbank history.

use std::collections::VecDeque;

use vaco_codec_core::Decoder;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_format_mpegaudio::{Layer, MpegAudioHeader};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::layer3::Layer3State;
use crate::synthesis::Synthesis;

#[derive(Debug)]
pub struct MpegAudioDecoder {
    limits: Limits,
    pending: VecDeque<Frame>,
    /// One filterbank per channel, shared by all three layers. Re-created if
    /// a later packet's channel count differs from what it was built for —
    /// a channel-count change mid-stream is not something any layer's
    /// filterbank history can straddle anyway.
    synth: Vec<Synthesis>,
    /// Layer III's bit reservoir and overlap-add history. Lazily created the
    /// first time a Layer III packet arrives.
    layer3: Option<Layer3State>,
    /// `send_packet(None)` has been seen and nothing further will arrive.
    ///
    /// Every packet decodes to at most one frame with no cross-packet reorder
    /// delay (Layer III's bit reservoir lives inside `layer3::decode`, not as
    /// buffered whole frames here), so there is nothing to hold back and
    /// flush at end of stream. But `receive_frame` still has to answer
    /// `Error::Eof` once draining starts and `pending` is empty, rather than
    /// `NeedMoreInput` forever — the `Decoder`/`SendReceive` protocol has no
    /// other way to learn a component is actually finished. Measured against
    /// a real end-to-end CLI run: without this, `vaco -i x.mp3 -c:a
    /// pcm_s16le out.wav` decoded every real frame correctly and then hung
    /// indefinitely waiting for a `Eof` that never came.
    draining: bool,
}

impl MpegAudioDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: VecDeque::new(),
            synth: Vec::new(),
            layer3: None,
            draining: false,
        }
    }

    fn synth_for(&mut self, channels: usize) -> &mut [Synthesis] {
        if self.synth.len() != channels {
            self.synth = (0..channels).map(|_| Synthesis::new()).collect();
        }
        &mut self.synth
    }
}

impl Decoder for MpegAudioDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            self.draining = true;
            return Ok(());
        };
        let payload = packet.payload();
        let header = MpegAudioHeader::parse_bytes(payload).ok_or(Error::InvalidData(
            "mpegaudio: packet does not start with a valid frame header",
        ))?;
        let crc_len = header.crc_len();
        let body_start = MpegAudioHeader::LEN + crc_len;
        let body = payload.get(body_start..).ok_or(Error::InvalidData(
            "mpegaudio: packet shorter than its own header",
        ))?;

        let mut budget = Budget::new(self.limits.clone());
        let channels = usize::from(header.channels());
        let frame = match header.layer {
            Layer::I => {
                let synth = self.synth_for(channels);
                crate::layer1::decode(header, body, synth, &mut budget)?
            }
            Layer::II => {
                let synth = self.synth_for(channels);
                crate::layer2::decode(header, body, synth, &mut budget)?
            }
            Layer::III => {
                self.synth_for(channels);
                if self.layer3.is_none() {
                    self.layer3 = Some(Layer3State::new(channels)?);
                }
                let state = self
                    .layer3
                    .as_mut()
                    .ok_or(Error::Unsupported("mpegaudio: missing layer3 state"))?;
                crate::layer3::decode(header, body, state, &mut self.synth, &mut budget)?
            }
        };
        // The LAME/Xing gapless trim this packet may carry is applied once,
        // for every audio codec, in `vaco_codec_core::gapless` — a trim
        // larger than one frame has to carry into the next, which a
        // per-packet trim here structurally could not do (an MPEG-2 LSF
        // frame is 576 samples and the LAME trim is 1105).
        let mut frame = frame;
        let count = frame_sample_count(&frame);
        if count > 0 {
            frame.pts = packet.pts;
            // The decode-side mirror of this session's audio-decoder
            // duration audit (`vaco-codec-pcm`/`-adpcm`/`-simple-audio`/
            // `-vorbis`/`-ac3`/`-aac`): `count`/`header.sample_rate_hz()`
            // were already in scope, but `frame.duration` was never set.
            let time_base = Rational::new(
                1,
                i32::try_from(header.sample_rate_hz()).unwrap_or(1).max(1),
            );
            frame.duration = Timestamp::new(i64::from(count))
                .to_duration(time_base)
                .unwrap_or(Duration::ZERO);
            self.pending.push_back(frame);
        }
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(if self.draining {
            Error::Eof
        } else {
            Error::NeedMoreInput
        })
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.synth.clear();
        self.layer3 = None;
        self.draining = false;
    }
}

/// Total sample count of an audio frame; `0` for anything else (this
/// decoder never produces anything else).
fn frame_sample_count(frame: &Frame) -> u32 {
    match &frame.data {
        FrameData::Audio { samples, .. } => *samples,
        _ => 0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod duration_tests {
    use super::*;

    fn append_bits(bits: &mut Vec<bool>, value: u32, width: u32) {
        for bit in (0..width).rev() {
            bits.push((value & (1 << bit)) != 0);
        }
    }

    fn append_zero_bits(bits: &mut Vec<bool>, count: usize) {
        bits.extend(std::iter::repeat_n(false, count));
    }

    fn pack_bits(bits: &[bool]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                chunk.iter().enumerate().fold(0u8, |byte, (offset, bit)| {
                    byte | (u8::from(*bit) << (7 - offset))
                })
            })
            .collect()
    }

    /// Same bit layout `layer1`'s own tests use to build a synthetic
    /// MPEG-1 Layer I header: an all-zero-allocation body decodes to
    /// silence, but it is still a real, structurally valid frame this
    /// crate's own `send_packet` header-detection logic accepts.
    fn header_word(version: u32, layer: u32, bitrate: u32, rate: u32, mode: u32) -> u32 {
        (0x7FFu32 << 21)
            | (version << 19)
            | (layer << 17)
            | (1 << 16)
            | (bitrate << 12)
            | (rate << 10)
            | (1 << 9) // padding bit, matches the 200-byte body length below
            | (mode << 6)
    }

    /// The decode-side mirror of this session's audio-decoder duration
    /// audit (`vaco-codec-pcm`/`-adpcm`/`-simple-audio`/`-vorbis`/`-ac3`/
    /// `-aac`): `count`/`header.sample_rate_hz()` were already in scope
    /// where `frame.pts` gets set, but `frame.duration` was never set.
    #[test]
    fn send_packet_sets_a_real_nonzero_frame_duration() {
        let header_word = header_word(0b11, 0b11, 5, 0, 0b11); // MPEG-1 Layer I, mono, 44.1kHz
        let mut bytes = header_word.to_be_bytes().to_vec();
        bytes.extend(std::iter::repeat_n(0u8, 200));

        let mut dec = MpegAudioDecoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).expect("packet");
        dec.send_packet(Some(&packet)).expect("send_packet");
        let frame = dec.receive_frame().expect("receive_frame");
        assert_ne!(frame.duration, Duration::ZERO);
    }

    #[test]
    fn send_packet_rejects_a_reserved_layer2_grouped_codeword() {
        let mut body_bits = Vec::new();
        append_bits(&mut body_bits, 1, 4); // subband 0: 3-level grouped class
        append_zero_bits(&mut body_bits, 84); // remaining Table B.2a allocations
        append_bits(&mut body_bits, 2, 2); // one scalefactor for subband 0
        append_bits(&mut body_bits, 0, 6);
        append_bits(&mut body_bits, 27, 5); // 3^3 is the first reserved grouped value
        append_zero_bits(&mut body_bits, 55);

        let header = header_word(0b11, 0b10, 4, 0, 0b11); // MPEG-1 Layer II, 64 kb/s mono
        let mut bytes = header.to_be_bytes().to_vec();
        bytes.extend(pack_bits(&body_bits));
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).expect("packet");
        let mut dec = MpegAudioDecoder::new(Limits::permissive());

        assert!(matches!(
            dec.send_packet(Some(&packet)),
            Err(Error::InvalidData(_))
        ));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod eof_tests {
    use super::*;

    /// `receive_frame` used to answer `NeedMoreInput` forever once draining
    /// began and nothing was buffered — indistinguishable, to a caller
    /// pumping the `Decoder`/`SendReceive` protocol, from a component that
    /// will eventually produce something. Measured against a real `.mp3`
    /// decoded end to end through the CLI: this hung the whole pipeline
    /// (converted to a bounded, diagnosed `LimitExceeded` by
    /// `vaco-sched`'s own no-progress guard fix, but the real fix is here —
    /// the component should say `Eof`, not rely on a scheduler-level
    /// safety net to notice it never will).
    #[test]
    fn receive_frame_reports_eof_once_draining_and_empty_not_forever_need_more_input() {
        let mut dec = MpegAudioDecoder::new(Limits::permissive());
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
        dec.send_packet(None).expect("drain signal");
        assert!(
            matches!(dec.receive_frame(), Err(Error::Eof)),
            "must answer Eof once draining with nothing pending, not NeedMoreInput again"
        );
    }

    #[test]
    fn flush_resets_the_draining_flag() {
        let mut dec = MpegAudioDecoder::new(Limits::permissive());
        dec.send_packet(None).expect("drain signal");
        dec.flush();
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }
}
