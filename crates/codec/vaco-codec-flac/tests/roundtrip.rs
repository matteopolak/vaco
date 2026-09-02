//! Encoder round trips: synthetic PCM through [`FlacEncoder`], back through
//! [`FlacDecoder`] (the `claxon` D11 boundary), and compared to the input
//! at zero tolerance. FLAC is lossless, so exact equality — not "close
//! enough" — is the only correct outcome; see also `ffmpeg_fixture.rs` for
//! the cross-check against a real ffmpeg-produced stream.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap
)]

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{Decoder, Encoder};
use vaco_codec_flac::{FlacDecoder, FlacEncoder};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_sampfmt::SampleFmt;

fn s16p_frame(per_channel: &[Vec<i16>], sample_rate: u32) -> Frame {
    let channels = per_channel.len() as u32;
    let layout = ChannelLayout::default_for(channels).expect("a layout for this channel count");
    let samples = per_channel.first().map_or(0, Vec::len) as u32;
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(&mut budget, SampleFmt::S16P, layout, samples, sample_rate)
        .expect("alloc audio frame");
    {
        let mut planes = frame.planes_mut();
        for (ch, plane) in planes.iter_mut().enumerate() {
            let row = plane.row_mut(0).expect("row 0");
            let Some(src) = per_channel.get(ch) else {
                continue;
            };
            for (i, &s) in src.iter().enumerate() {
                if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                    dst.copy_from_slice(&s.to_ne_bytes());
                }
            }
        }
    }
    frame
}

/// Encode `per_channel` whole, then decode every packet produced and
/// concatenate the result back into one `Vec<i16>` per channel.
fn round_trip(per_channel: &[Vec<i16>], sample_rate: u32) -> Vec<Vec<i16>> {
    let frame = s16p_frame(per_channel, sample_rate);

    let mut enc = FlacEncoder::new(Limits::permissive());
    enc.send_frame(Some(&frame)).expect("send frame");
    enc.send_frame(None).expect("start drain");
    let mut packets = Vec::new();
    while let Ok(packet) = enc.receive_packet() {
        packets.push(packet);
    }
    let extradata = enc.extradata();

    let mut dec = FlacDecoder::new(Limits::permissive());
    dec.set_extradata(&extradata).expect("set extradata");
    let mut out: Vec<Vec<i16>> = per_channel.iter().map(|_| Vec::new()).collect();
    for packet in &packets {
        dec.send_packet(Some(packet)).expect("send packet");
        while let Ok(frame) = dec.receive_frame() {
            append_samples(&frame, &mut out);
        }
    }
    out
}

fn append_samples(frame: &Frame, out: &mut [Vec<i16>]) {
    let FrameData::Audio {
        format, samples, ..
    } = &frame.data
    else {
        return;
    };
    for (ch, dst) in out.iter_mut().enumerate() {
        let Some(plane) = frame.plane(ch) else {
            continue;
        };
        let Some(row) = plane.row(0) else { continue };
        match format {
            SampleFmt::S16P => {
                for chunk in row.chunks_exact(2).take(*samples as usize) {
                    let bytes: [u8; 2] = chunk.try_into().unwrap_or([0, 0]);
                    dst.push(i16::from_ne_bytes(bytes));
                }
            }
            SampleFmt::S32P => {
                for chunk in row.chunks_exact(4).take(*samples as usize) {
                    let bytes: [u8; 4] = chunk.try_into().unwrap_or([0, 0, 0, 0]);
                    let v = i32::from_ne_bytes(bytes);
                    let clamped = v.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                    dst.push(clamped as i16);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn silence_round_trips() {
    let ch = vec![vec![0i16; 1000]];
    assert_eq!(round_trip(&ch, 44_100), ch);
}

#[test]
fn alternating_extremes_round_trip() {
    let samples: Vec<i16> = (0..500)
        .map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN })
        .collect();
    let ch = vec![samples.clone(), samples];
    let want = ch.clone();
    assert_eq!(round_trip(&ch, 48_000), want);
}

#[test]
fn sine_wave_mono_round_trips() {
    let samples: Vec<i16> = (0..8000)
        .map(|i| {
            let t = f64::from(i) / 8000.0;
            (10000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
        })
        .collect();
    let ch = vec![samples.clone()];
    assert_eq!(round_trip(&ch, 8_000), ch);
}

#[test]
fn sine_wave_stereo_with_out_of_phase_channels_round_trips() {
    let left: Vec<i16> = (0..8000)
        .map(|i| {
            let t = f64::from(i) / 8000.0;
            (8000.0 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()) as i16
        })
        .collect();
    let right: Vec<i16> = (0..8000)
        .map(|i| {
            let t = f64::from(i) / 8000.0;
            (8000.0 * (2.0 * std::f64::consts::PI * 220.0 * t + std::f64::consts::PI).sin()) as i16
        })
        .collect();
    let ch = vec![left, right];
    let want = ch.clone();
    assert_eq!(round_trip(&ch, 8_000), want);
}

#[test]
fn a_block_smaller_than_the_configured_block_size_round_trips() {
    let ch = vec![vec![1i16, -1, 2, -2, 3, -3, 100, -100]];
    let want = ch.clone();
    assert_eq!(round_trip(&ch, 22_050), want);
}

#[test]
fn exactly_one_block_round_trips() {
    let n = usize::try_from(vaco_codec_flac::encoder::BLOCK_SIZE).unwrap_or(4096);
    let samples: Vec<i16> = (0..n)
        .map(|i| (((i * 31) % 40000) as i32 - 20000) as i16)
        .collect();
    let ch = vec![samples.clone()];
    assert_eq!(round_trip(&ch, 44_100), ch);
}

#[test]
fn one_block_plus_a_short_remainder_round_trips() {
    let n = usize::try_from(vaco_codec_flac::encoder::BLOCK_SIZE).unwrap_or(4096) + 37;
    let samples: Vec<i16> = (0..n).map(|i| ((i * 7919) % 65536) as i32 as i16).collect();
    let ch = vec![samples.clone()];
    assert_eq!(round_trip(&ch, 44_100), ch);
}

#[test]
fn many_blocks_of_noise_round_trip() {
    // A fixed, deterministic pseudo-random sequence (xorshift) rather than
    // `rand`: this crate has no dependency on it, and determinism is worth
    // more here than statistical quality.
    let mut state = 0x1234_5678u32;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let n = 10_000usize;
    let left: Vec<i16> = (0..n).map(|_| next() as i16).collect();
    let right: Vec<i16> = (0..n).map(|_| next() as i16).collect();
    let ch = vec![left, right];
    let want = ch.clone();
    assert_eq!(round_trip(&ch, 48_000), want);
}

#[test]
fn a_single_sample_round_trips() {
    let ch = vec![vec![42i16]];
    assert_eq!(round_trip(&ch, 16_000), ch);
}

/// E2E-GAPS #2: without `prime_audio`, `extradata()` answers `None` until
/// the first frame -- too late for a container's `add_stream`, which is why
/// a real `-c:a flac` transcode to Matroska produced a file with no
/// `STREAMINFO` at all and nothing else (not even `ffmpeg`'s own decoder)
/// could read it back. `prime_audio` closes that: the extradata is
/// available immediately, from the pipeline's own already-known stream
/// shape, with no frame sent at all.
#[test]
fn prime_audio_makes_extradata_available_before_the_first_frame() {
    let mut enc = FlacEncoder::new(Limits::permissive());
    assert_eq!(Encoder::extradata(&enc), None, "nothing primed yet");

    enc.prime_audio(44_100, ChannelLayout::MONO, SampleFmt::S16P);
    let primed = Encoder::extradata(&enc).expect("extradata after priming");
    assert!(
        primed.starts_with(b"fLaC"),
        "must start with the fLaC magic"
    );

    // And it must be the *same* extradata `ingest` would have produced from
    // a real frame -- priming is a shortcut to the same state, not a
    // separate, possibly-diverging path.
    let frame = s16p_frame(&[vec![0i16; 4]], 44_100);
    let mut enc2 = FlacEncoder::new(Limits::permissive());
    enc2.send_frame(Some(&frame)).expect("send frame");
    let from_frame = Encoder::extradata(&enc2).expect("extradata after a real frame");
    assert_eq!(primed, from_frame);
}

/// The regression `prime_audio` almost caused: seeding `state` ahead of the
/// first frame must not swallow that first frame's own `pts`, which is the
/// only source `emit_block`'s sample-accurate arithmetic has for the
/// stream's time-zero. Priming only pre-fills the *shape* (channels/rate/
/// bit depth); the timeline still has to come from a real frame.
#[test]
fn priming_does_not_prevent_the_first_real_frames_pts_from_being_captured() {
    use vaco_core::Timestamp;

    let mut enc = FlacEncoder::new(Limits::permissive());
    enc.prime_audio(44_100, ChannelLayout::MONO, SampleFmt::S16P);

    let mut frame = s16p_frame(&[vec![0i16; 4]], 44_100);
    frame.pts = Timestamp::new(5000);
    enc.send_frame(Some(&frame)).expect("send frame");
    enc.send_frame(None).expect("start drain");
    let packet = enc.receive_packet().expect("packet");
    assert_eq!(packet.pts, Timestamp::new(5000));
}

/// E2E-GAPS #5-adjacent: `FlacEncoder` never set a packet's `pts` at all
/// before this, so a real `-c:a flac` transcode to any strict container
/// (Matroska among them) failed with "this container needs timestamps and
/// the packet has none" downstream, even though every sample encoded
/// correctly. Two full blocks make the assertion about the *second*
/// packet's `pts` a real one -- a fixed-blocksize FLAC stream's sample
/// position for frame `n` is exactly `n * BLOCK_SIZE`, and getting this
/// wrong (say, by using the *first* frame's `pts` for every packet)
/// produces a value this test would catch.
#[test]
fn each_emitted_packet_carries_the_right_sample_accurate_pts() {
    use vaco_codec_flac::encoder::BLOCK_SIZE;
    use vaco_core::Timestamp;

    let n = (BLOCK_SIZE as usize) * 2 + 10;
    let samples: Vec<i16> = (0..n).map(|i| (i % 100) as i16).collect();
    let mut frame = s16p_frame(&[samples], 44_100);
    frame.pts = Timestamp::new(1000);

    let mut enc = FlacEncoder::new(Limits::permissive());
    enc.send_frame(Some(&frame)).expect("send frame");
    enc.send_frame(None).expect("start drain");
    let mut pts: Vec<Option<i64>> = Vec::new();
    while let Ok(packet) = enc.receive_packet() {
        pts.push(packet.pts.ticks());
    }
    assert_eq!(
        pts,
        vec![
            Some(1000),
            Some(1000 + i64::from(BLOCK_SIZE)),
            Some(1000 + 2 * i64::from(BLOCK_SIZE)),
        ]
    );
}

proptest::proptest! {
    #[test]
    fn arbitrary_mono_pcm_round_trips_exactly(
        samples in proptest::collection::vec(proptest::prelude::any::<i16>(), 0..600)
    ) {
        let ch = vec![samples.clone()];
        let got = round_trip(&ch, 44_100);
        let want: Vec<Vec<i16>> = if samples.is_empty() { vec![Vec::new()] } else { ch };
        proptest::prop_assert_eq!(got, want);
    }

    #[test]
    fn arbitrary_stereo_pcm_round_trips_exactly(
        left in proptest::collection::vec(proptest::prelude::any::<i16>(), 1..400),
        right in proptest::collection::vec(proptest::prelude::any::<i16>(), 1..400)
    ) {
        let n = left.len().min(right.len());
        let left = left[..n].to_vec();
        let right = right[..n].to_vec();
        let ch = vec![left, right];
        let got = round_trip(&ch, 48_000);
        proptest::prop_assert_eq!(got, ch);
    }
}
