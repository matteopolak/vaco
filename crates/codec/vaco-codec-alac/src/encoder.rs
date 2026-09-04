//! [`vaco_codec_core::Encoder`] wrapper over [`frame_codec::encode`].

use std::collections::VecDeque;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Encoder;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

use crate::cookie::AlacSpecificConfig;
use crate::frame_codec;

#[derive(Debug)]
pub struct AlacEncoder {
    limits: Limits,
    pending: VecDeque<Packet>,
    /// The magic cookie this encoder will declare, once
    /// [`Encoder::prime_audio`] has told it the stream shape — `None` until
    /// then. See [`AlacEncoder::extradata`]'s docs for why a container needs
    /// this *before* the first [`Encoder::send_frame`], the same gap
    /// `vaco-codec-flac`'s own `prime_audio` closes for `STREAMINFO`.
    cookie: Option<AlacSpecificConfig>,
    /// `Error::Eof` once draining starts and `pending` is empty, rather than
    /// `NeedMoreInput` forever. Before this, `send_frame(None)` never
    /// updated any state to say draining had begun, so `receive_packet`
    /// kept answering `NeedMoreInput` after end of stream and the
    /// scheduler's `ProgressGuard` eventually killed the run with
    /// `NoProgress` ("progress limit exceeded") instead of a clean `Eof` —
    /// measured end to end via `vaco -i in.wav -c:a alac out.mkv`, which hit
    /// exactly this livelock (on the *encode* side, not the decode side
    /// this crate's `AlacDecoder` shares the same fix for) before this
    /// field existed. Same shape `vaco-codec-flac`'s decoder already
    /// carries, applied to `Encoder::send_frame`/`receive_packet` instead of
    /// `Decoder::send_packet`/`receive_frame`.
    draining: bool,
}

impl AlacEncoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: VecDeque::new(),
            cookie: None,
            draining: false,
        }
    }
}

impl Encoder for AlacEncoder {
    /// Build the magic cookie from the real stream shape, before any frame
    /// has been sent.
    ///
    /// # Why this exists
    ///
    /// Without it, `AlacEncoder` answered [`Encoder::extradata`] with `None`
    /// forever — `frame_codec::encode` never builds a cookie of its own, and
    /// nothing else called into `cookie::AlacSpecificConfig` from the encode
    /// side at all. A transcode's `CodecParameters::extradata` then carried
    /// whatever the *previous* codec's configuration record was (or nothing,
    /// for a source with none), and a Matroska/MP4 output — both of which
    /// require `CodecPrivate`/an `alac` sample entry to hold this cookie —
    /// had no way to tell a genuinely missing record from a stale one. That
    /// gap is closed one layer up (`CodecParameters::with_codec` clears
    /// stale extradata; `vaco-mux-matroska` refuses to finalize a track that
    /// still has none), which turned this specific gap from a silently
    /// corrupt `-c:a alac out.mkv` into a loud, correct refusal — this
    /// method is what removes the refusal by actually producing the record.
    ///
    /// Channel count is capped the same way [`Encoder::send_frame`] already
    /// caps it (mono or stereo only, `frame_codec`'s own scope); anything
    /// past that is silently clamped to the field's own domain rather than
    /// erroring here, since this method has no `Result` to report one
    /// through and the real refusal already happens at `send_frame` for a
    /// frame this encoder cannot carry.
    fn prime_audio(&mut self, sample_rate: u32, layout: ChannelLayout, format: SampleFmt) {
        let bit_depth = if matches!(format, SampleFmt::S32P) {
            32
        } else {
            16
        };
        let num_channels = u8::try_from(layout.channels.clamp(1, 2)).unwrap_or(1);
        self.cookie = Some(AlacSpecificConfig::for_encode(
            sample_rate,
            num_channels,
            bit_depth,
            crate::rice::PB0 as u8,
            crate::rice::MB0 as u8,
            crate::rice::KB0 as u8,
        ));
    }

    /// The bare 24-byte `ALACSpecificConfig`, once [`Encoder::prime_audio`]
    /// has run — `None` beforehand, matching every other encoder in this
    /// tree whose configuration record depends on stream shape it has not
    /// been told yet.
    fn extradata(&self) -> Option<Vec<u8>> {
        self.cookie.map(|c| c.write_bare().to_vec())
    }

    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        let Some(frame) = frame else {
            self.draining = true;
            return Ok(());
        };
        let mut budget = Budget::new(self.limits.clone());
        let bytes = frame_codec::encode(frame, &mut budget)?;
        let mut packet = Packet::from_slice(&mut budget, &bytes)?;
        packet.pts = frame.pts;
        // Real, not left at its `Duration::ZERO` default: `vaco-mux-mp4`'s
        // `stts` table derives every sample's duration but the *last* one
        // from the gap to the next sample's `dts`, which does not exist for
        // a track's final packet -- it falls back to whatever duration the
        // most recent packet stated, and this encoder never stated one at
        // all. Measured end to end: `vaco -i mono.wav -c:a alac out.m4a`
        // wrote a valid-looking file whose last `stts` run was `(1, 0)`
        // (should be the final frame's real sample count) and whose
        // `mdhd`/`mvhd` duration undercounted the file by exactly that many
        // samples -- `ffmpeg`'s own decode of that file silently dropped
        // the last frame, decoding 21 of 22. `Frame::duration` is not a
        // substitute: it is `Duration::ZERO` by default and nothing
        // upstream of this encoder is guaranteed to have set it, so this
        // computes the real value from what this encoder already knows
        // (`num_samples`, `sample_rate`) rather than trusting a field nothing
        // populates.
        let FrameData::Audio {
            samples,
            sample_rate,
            ..
        } = &frame.data
        else {
            return Err(Error::Unsupported("alac: encoder needs an audio frame"));
        };
        let time_base = Rational::new(1, i32::try_from(*sample_rate).unwrap_or(1).max(1));
        packet.duration = Timestamp::new(i64::from(*samples))
            .to_duration(time_base)
            .unwrap_or(Duration::ZERO);
        packet.set_duration_ts(i64::from(*samples));
        // Every packet is independently decodable: the adaptive Golomb-Rice
        // state (`rice.rs`) and the predictor's transmitted coefficients
        // both reset per packet — there is no cross-packet state — matching
        // `CodecId::Alac`'s registered `INTRA_ONLY` property.
        packet.flags = PacketFlags::KEY;
        self.pending.push_back(packet);
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
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

    /// Mirrors `frame_codec::bytes_per_sample`'s own accepted set. Same
    /// reasoning as `vaco-codec-flac`/`vaco-codec-pcm`'s overrides: without
    /// this, a caller only discovers the s16p/s32p requirement from a
    /// failed `send_frame`, too late to insert a conversion first
    /// (E2E-GAPS #3).
    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        &[vaco_sampfmt::SampleFmt::S16P, vaco_sampfmt::SampleFmt::S32P]
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_chlayout::ChannelLayout;
    use vaco_codec_core::Decoder as _;
    use vaco_core::Timestamp;
    use vaco_sampfmt::SampleFmt;

    fn mono_frame(samples: &[i32]) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_audio(
            &mut budget,
            SampleFmt::S16P,
            ChannelLayout::MONO,
            samples.len() as u32,
            44100,
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
        frame.pts = Timestamp::new(7);
        frame
    }

    #[test]
    fn send_receive_protocol_shape() {
        let samples: Vec<i32> = (0..512).map(|i| (i % 200) - 100).collect();
        let frame = mono_frame(&samples);
        let mut enc = AlacEncoder::new(Limits::permissive());
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));
        enc.send_frame(Some(&frame)).unwrap();
        let packet = enc.receive_packet().unwrap();
        assert_eq!(packet.pts, Timestamp::new(7));
        assert!(packet.flags.contains(PacketFlags::KEY));
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));

        enc.flush();
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));
    }

    /// `send_frame(None)` must switch `receive_packet` from `NeedMoreInput`
    /// to `Eof` once every buffered packet has drained — the exact contract
    /// `vaco-sched`'s drain loop polls on. Before `draining` existed,
    /// `send_frame(None)` never recorded that draining had begun, so
    /// `receive_packet` kept answering `NeedMoreInput` forever and the
    /// scheduler's `ProgressGuard` eventually reported `NoProgress`
    /// ("progress limit exceeded") instead of a clean end of stream —
    /// reproduced end to end via `vaco -i in.wav -c:a alac out.mkv`.
    #[test]
    fn draining_answers_eof_once_empty_not_need_more_input_forever() {
        let samples: Vec<i32> = (0..512).map(|i| (i % 200) - 100).collect();
        let frame = mono_frame(&samples);
        let mut enc = AlacEncoder::new(Limits::permissive());
        enc.send_frame(Some(&frame)).unwrap();
        enc.send_frame(None).unwrap();
        // The one already-buffered packet still comes out first.
        assert!(enc.receive_packet().is_ok());
        // Only now, with nothing buffered and draining under way, is it Eof.
        assert!(matches!(enc.receive_packet(), Err(Error::Eof)));
        assert!(matches!(enc.receive_packet(), Err(Error::Eof)));

        // flush() resets to the feeding state: NeedMoreInput, not Eof.
        enc.flush();
        assert!(matches!(enc.receive_packet(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn encoded_packet_decodes_back_through_the_decoder() {
        let samples: Vec<i32> = (0..1024).map(|i| ((i * 3) % 400) - 200).collect();
        let frame = mono_frame(&samples);
        let mut enc = AlacEncoder::new(Limits::permissive());
        enc.send_frame(Some(&frame)).unwrap();
        let packet = enc.receive_packet().unwrap();

        let mut dec = crate::AlacDecoder::new(Limits::permissive());
        dec.send_packet(Some(&packet)).unwrap();
        let decoded = dec.receive_frame().unwrap();
        let vaco_frame::FrameData::Audio { planes, .. } = &decoded.data else {
            panic!("audio frame");
        };
        // 16-bit input: `frame_codec::decode` matches its output `SampleFmt`
        // to the packet's actual bit depth (S16P here), not always S32P —
        // see that function's doc for why always-S32P was a real bug.
        let row = planes.first().unwrap().data.as_slice();
        let got: Vec<i32> = row
            .chunks_exact(2)
            .map(|c| i32::from(i16::from_le_bytes(c.try_into().unwrap())))
            .collect();
        assert_eq!(got, samples);
    }

    /// A real, measured bug: this encoder never set `Packet::duration`, which
    /// `vaco-mux-mp4`'s `stts` table needs for exactly the *last* sample in a
    /// track (every earlier sample's duration is inferred from the gap to
    /// the next sample's `dts` instead) -- see `send_frame`'s own doc for
    /// the full chain. `vaco -i mono.wav -c:a alac out.m4a` wrote a file
    /// whose last `stts` run was `(1, 0)` and whose declared track duration
    /// undercounted the source by exactly the last frame's sample count;
    /// `ffmpeg`'s own decode of that file silently dropped the last frame,
    /// decoding 21 of 22. A round-number sample rate (1000 Hz, 500 samples)
    /// makes the expected duration an exact microsecond count with no
    /// rounding ambiguity to paper over a wrong computation.
    #[test]
    fn send_frame_sets_a_real_nonzero_packet_duration() {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame =
            Frame::alloc_audio(&mut budget, SampleFmt::S16P, ChannelLayout::MONO, 500, 1000)
                .unwrap();
        {
            let mut plane = frame.plane_mut(0).unwrap();
            let row = plane.row_mut(0).unwrap();
            row.fill(0);
        }
        let mut enc = AlacEncoder::new(Limits::permissive());
        enc.send_frame(Some(&frame)).unwrap();
        let packet = enc.receive_packet().unwrap();
        assert_eq!(
            packet.duration,
            vaco_core::Duration::from_micros(500_000),
            "500 samples at 1000 Hz must be exactly 500ms, not the Duration::ZERO default"
        );
    }
}
