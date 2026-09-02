//! [`vaco_codec_core::Decoder`] wrapper over [`frame_codec::decode`].

use std::collections::VecDeque;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Decoder;
use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::cookie::AlacCookie;
use crate::frame_codec;

/// Falls back to this when a stream's decoder is never given extradata (no
/// `set_extradata` call at all). Arbitrary but common; every packet still
/// decodes correctly regardless, since sample rate never affects how a
/// packet's own bytes parse — see `frame_codec`'s module doc.
const DEFAULT_SAMPLE_RATE: u32 = 44100;

#[derive(Debug)]
pub struct AlacDecoder {
    limits: Limits,
    pending: VecDeque<Frame>,
    sample_rate: u32,
    bit_depth: u8,
    frame_length: u32,
    layout_hint: Option<ChannelLayout>,
    /// `Error::Eof` once draining starts and `pending` is empty, rather than
    /// `NeedMoreInput` forever. Before this, `send_packet(None)` was a
    /// no-op, so `receive_frame` kept answering `NeedMoreInput` after end of
    /// stream and the scheduler's `ProgressGuard` eventually killed the run
    /// with `NoProgress` ("progress limit exceeded") instead of a clean
    /// `Eof` — measured end to end via `vaco -i <alac-in-mkv> -f null -`,
    /// which hit exactly that livelock before this field existed. Same fix
    /// `vaco-codec-flac`'s decoder already carries (that crate's own
    /// `draining` field doc names this crate as sharing the bug).
    draining: bool,
}

impl AlacDecoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: VecDeque::new(),
            sample_rate: DEFAULT_SAMPLE_RATE,
            bit_depth: 16,
            frame_length: 4096,
            layout_hint: None,
            draining: false,
        }
    }
}

impl Decoder for AlacDecoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            self.draining = true;
            return Ok(());
        };
        let mut budget = Budget::new(self.limits.clone());
        let mut frame = frame_codec::decode(
            packet.payload(),
            self.sample_rate,
            self.bit_depth,
            self.frame_length,
            self.layout_hint.clone(),
            &mut budget,
        )?;
        frame.pts = packet.pts;
        self.pending.push_back(frame);
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
        self.draining = false;
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        let cookie = AlacCookie::parse(extradata)?;
        self.sample_rate = cookie.config.sample_rate;
        self.bit_depth = cookie.config.bit_depth;
        self.frame_length = cookie.config.frame_length;
        self.layout_hint = Some(cookie.layout());
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FrameData;
    use vaco_sampfmt::SampleFmt;

    fn encode_mono_packet(samples: &[i32], sample_rate: u32) -> Vec<u8> {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            SampleFmt::S16P,
            ChannelLayout::MONO,
            samples.len() as u32,
            sample_rate,
        )
        .unwrap();
        {
            let mut plane = frame.plane_mut(0).unwrap();
            let row = plane.row_mut(0).unwrap();
            for (i, &s) in samples.iter().enumerate() {
                if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                    dst.copy_from_slice(&(s as i16).to_le_bytes());
                }
            }
        }
        crate::frame_codec::encode(&frame, &mut budget).unwrap()
    }

    #[test]
    fn send_receive_protocol_shape() {
        let samples: Vec<i32> = (0..256).map(|i| (i % 100) - 50).collect();
        let bytes = encode_mono_packet(&samples, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();

        let mut dec = AlacDecoder::new(Limits::permissive());
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
        dec.send_packet(Some(&packet)).unwrap();
        let frame = dec.receive_frame().unwrap();
        let FrameData::Audio { samples: n, .. } = frame.data else {
            panic!("audio frame");
        };
        assert_eq!(n, samples.len() as u32);
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));

        dec.flush();
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    /// `send_packet(None)` must switch `receive_frame` from `NeedMoreInput`
    /// to `Eof` once every buffered frame has drained — the exact contract
    /// `vaco-sched`'s drain loop polls on to know a decoder is truly done.
    /// Before `draining` existed, `send_packet(None)` was a no-op and this
    /// kept answering `NeedMoreInput` forever, which the scheduler's
    /// `ProgressGuard` eventually reported as `NoProgress`
    /// ("progress limit exceeded") instead of a clean end of stream —
    /// reproduced end to end via `vaco -i <alac-in-mkv> -f null -`.
    #[test]
    fn draining_answers_eof_once_empty_not_need_more_input_forever() {
        let samples: Vec<i32> = (0..256).map(|i| (i % 100) - 50).collect();
        let bytes = encode_mono_packet(&samples, 44100);
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();

        let mut dec = AlacDecoder::new(Limits::permissive());
        dec.send_packet(Some(&packet)).unwrap();
        dec.send_packet(None).unwrap();
        // The one already-buffered frame still comes out first.
        assert!(dec.receive_frame().is_ok());
        // Only now, with nothing buffered and draining under way, is it Eof.
        assert!(matches!(dec.receive_frame(), Err(Error::Eof)));
        assert!(matches!(dec.receive_frame(), Err(Error::Eof)));

        // flush() resets to the feeding state: NeedMoreInput, not Eof.
        dec.flush();
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn set_extradata_from_a_real_cookie_drives_sample_rate_and_layout() {
        // The same 24-byte cookie pinned in `cookie.rs`'s
        // `real_ffmpeg_mono_cookie` regression test.
        const REAL_MONO_COOKIE: [u8; 24] = [
            0x00, 0x00, 0x10, 0x00, 0x00, 0x10, 0x28, 0x0a, 0x0e, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x20, 0x04, 0x00, 0x0a, 0xc4, 0x40, 0x00, 0x00, 0xac, 0x44,
        ];
        let samples: Vec<i32> = (0..64).map(|i| i - 32).collect();
        let bytes = encode_mono_packet(&samples, 999); // packet bytes carry no rate
        let mut budget = Budget::new(Limits::permissive());
        let packet = Packet::from_slice(&mut budget, &bytes).unwrap();

        let mut dec = AlacDecoder::new(Limits::permissive());
        dec.set_extradata(&REAL_MONO_COOKIE).unwrap();
        dec.send_packet(Some(&packet)).unwrap();
        let frame = dec.receive_frame().unwrap();
        let FrameData::Audio {
            sample_rate,
            layout,
            ..
        } = frame.data
        else {
            panic!("audio frame");
        };
        assert_eq!(sample_rate, 44100);
        assert_eq!(layout, ChannelLayout::MONO);
    }

    #[test]
    fn bad_extradata_is_an_error_not_a_silent_default() {
        let mut dec = AlacDecoder::new(Limits::permissive());
        assert!(dec.set_extradata(&[0u8; 3]).is_err());
    }
}
