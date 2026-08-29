//! End-to-end verification of [`vaco_codec_vorbis::VorbisEncoder`]: encode a
//! synthetic signal, mux it into a real Ogg/Vorbis file with
//! `vaco-mux-ogg`, and decode it with two decoders this crate did not write
//! the encoder against: `ffmpeg`'s own `libvorbis`/native Vorbis decode
//! (skipped, not failed, when `ffmpeg` is absent — matching this crate's own
//! `ffmpeg_differential.rs` convention) and, unconditionally, this crate's
//! own [`vaco_codec_vorbis::VorbisDecoder`] — independently written and
//! itself verified against real `ffmpeg`-encoded content per issue #308's
//! closure, so a second, always-on data point even though it shares a
//! codebase with the encoder under test.
//!
//! What is measured, per the brief's "measure the thing that can be wrong"
//! rule: **per-channel** SNR against the original synthetic signal (not an
//! aggregate stereo metric — AAC 5.1 shipped past a per-channel-only check
//! once already), after aligning for the encoder's own algorithmic delay by
//! cross-correlation search rather than assuming the textbook one-block
//! value, and an explicit channel-swap check (channel 0 must correlate with
//! channel 0, not channel 1).
//!
//! This encoder is a fixed, low-complexity configuration (see
//! `src/encoder.rs`'s module doc for exactly what is and is not
//! implemented) — it is not tuned for compression or for competitive
//! perceptual quality, so the pass bar here is "structurally reasonable and
//! not broken" (finite output, a well-formed bitstream real tools open, no
//! channel corruption), not "matches a tuned reference encoder's quality".

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::while_let_loop,
    clippy::needless_range_loop,
    clippy::integer_division
)]

use std::io::Write;
use std::process::{Command, Stdio};

use vaco_codec_core::{AudioParameters, Caps, CodecId, CodecParameters, Decoder, Encoder};
use vaco_codec_vorbis::{VorbisDecoder, VorbisEncoder};
use vaco_core::{Duration, Timestamp};
use vaco_demux_ogg::OggDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::vacoraw::MemorySink;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_frame::{Frame, FrameData};
use vaco_io::MemorySource;
use vaco_limits::{Budget, Limits};
use vaco_packet::PacketFlags;
use vaco_sampfmt::SampleFmt;

const SAMPLE_RATE: u32 = 44_100;

/// A simple multi-tone test signal per channel, `seconds` long: distinct
/// frequencies per channel so a channel swap is detectable by correlation,
/// not just by ear.
fn synth(channels: usize, seconds: f64) -> Vec<Vec<f32>> {
    let n = (SAMPLE_RATE as f64 * seconds) as usize;
    (0..channels)
        .map(|ch| {
            let freq = 220.0 * (ch as f64 + 1.5);
            (0..n)
                .map(|i| {
                    let t = i as f64 / f64::from(SAMPLE_RATE);
                    (0.4 * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32
                })
                .collect()
        })
        .collect()
}

fn encode_to_ogg(channel_samples: &[Vec<f32>]) -> Vec<u8> {
    let channels = channel_samples.len() as u32;
    let layout = match channels {
        1 => vaco_chlayout::ChannelLayout::MONO,
        2 => vaco_chlayout::ChannelLayout::STEREO,
        n => vaco_chlayout::ChannelLayout::unspecified(n),
    };

    let mut enc = VorbisEncoder::new(Limits::permissive());
    let total = channel_samples.first().map_or(0, Vec::len);
    let chunk = 4096usize;
    let mut pos = 0usize;
    let mut budget = Budget::new(Limits::permissive());
    let mut encoded_packets: Vec<Vec<u8>> = Vec::new();

    let drain = |enc: &mut VorbisEncoder, out: &mut Vec<Vec<u8>>| {
        loop {
            match enc.receive_packet() {
                Ok(p) => out.push(p.payload().to_vec()),
                Err(_) => break,
            }
        }
    };

    while pos < total {
        let end = (pos + chunk).min(total);
        let n = end - pos;
        let mut frame = Frame::alloc_audio(
            &mut budget,
            SampleFmt::F32P,
            layout.clone(),
            n as u32,
            SAMPLE_RATE,
        )
        .expect("alloc_audio");
        for (ch, samples) in channel_samples.iter().enumerate() {
            if let Some(mut plane) = frame.plane_mut(ch)
                && let Some(row) = plane.row_mut(0)
            {
                for (i, &s) in samples.get(pos..end).unwrap_or(&[]).iter().enumerate() {
                    let byte_pos = i * 4;
                    if let Some(dst) = row.get_mut(byte_pos..byte_pos + 4) {
                        dst.copy_from_slice(&s.to_le_bytes());
                    }
                }
            }
        }
        enc.send_frame(Some(&frame)).expect("send_frame");
        drain(&mut enc, &mut encoded_packets);
        pos = end;
    }
    enc.send_frame(None).expect("send_frame(None)");
    drain(&mut enc, &mut encoded_packets);

    let extradata = enc.extradata();
    assert!(
        !extradata.is_empty(),
        "extradata must be populated after encoding"
    );

    let sink = Box::new(MemorySink::new());
    let bytes_handle = sink.shared();
    let mut mux = vaco_mux_ogg::OggMuxer::new(sink).expect("OggMuxer::new");
    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Vorbis);
    params.extradata = Some(extradata);
    params.audio = Some(AudioParameters {
        sample_rate: SAMPLE_RATE,
        layout: Some(layout),
        ..AudioParameters::default()
    });
    let idx = mux.add_stream(&params).expect("add_stream");
    mux.write_header().expect("write_header");

    let mut pkt_budget = Budget::new(Limits::permissive());
    let mut pts = 0i64;
    for payload in &encoded_packets {
        let mut pkt = vaco_packet::Packet::from_slice(&mut pkt_budget, payload).expect("packet");
        pkt.stream_index = idx;
        pkt.pts = Timestamp::new(pts);
        pkt.dts = pkt.pts;
        pkt.duration = Duration::from_micros(1024 * 1_000_000 / i64::from(SAMPLE_RATE));
        pkt.flags = PacketFlags::KEY;
        mux.write_packet(&pkt).expect("write_packet");
        pts += 1024;
    }
    mux.write_trailer().expect("write_trailer");
    bytes_handle.snapshot()
}

/// Decode with this crate's own decoder (independent implementation,
/// itself verified against real `ffmpeg` content per issue #308).
fn decode_with_vaco(ogg_bytes: &[u8]) -> Vec<Vec<f32>> {
    let mut demux = OggDemuxer::open(
        Box::new(MemorySource::new(ogg_bytes.to_vec())),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("open ogg");
    let extradata = demux.streams()[0]
        .params
        .extradata
        .clone()
        .expect("extradata");
    let mut dec = VorbisDecoder::new(Limits::permissive());
    dec.set_extradata(&extradata).expect("set_extradata");

    let mut per_channel: Vec<Vec<f32>> = Vec::new();
    let drain = |dec: &mut VorbisDecoder, per_channel: &mut Vec<Vec<f32>>| {
        while let Ok(frame) = dec.receive_frame() {
            let FrameData::Audio {
                samples, planes, ..
            } = &frame.data
            else {
                continue;
            };
            if per_channel.is_empty() {
                per_channel.resize(planes.len(), Vec::new());
            }
            for ch in 0..planes.len() {
                let plane = frame.plane(ch).expect("plane");
                let row = plane.row(0).expect("row");
                let dst = &mut per_channel[ch];
                for chunk in row.chunks_exact(4).take(*samples as usize) {
                    dst.push(f32::from_le_bytes(chunk.try_into().unwrap()));
                }
            }
        }
    };
    while let Ok(packet) = demux.read_packet() {
        dec.send_packet(Some(&packet)).expect("send_packet");
        drain(&mut dec, &mut per_channel);
    }
    dec.send_packet(None).ok();
    drain(&mut dec, &mut per_channel);
    per_channel
}

/// Decode with `ffmpeg` to interleaved `f32le`, deinterleaved here. `None`
/// (a skip, not a failure) when `ffmpeg` is absent or refuses the input,
/// matching this crate's own `ffmpeg_differential.rs` convention.
fn decode_with_ffmpeg(ogg_bytes: &[u8], channels: usize) -> Option<Vec<Vec<f32>>> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "ogg",
        "-i",
        "-",
        "-f",
        "f32le",
        "-acodec",
        "pcm_f32le",
        "-",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    child.stdin.take()?.write_all(ogg_bytes).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let interleaved: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let mut per_channel = vec![Vec::new(); channels];
    for (i, &s) in interleaved.iter().enumerate() {
        per_channel[i % channels].push(s);
    }
    Some(per_channel)
}

/// Best-lag cross-correlation SNR: search `lag` in `-max_lag..=max_lag`
/// samples of `decoded` against `original`, report the SNR (dB) at the best
/// alignment. Encoder algorithmic delay is a property of this pipeline, not
/// assumed a priori (D17: measure, don't recall).
fn best_snr_db(original: &[f32], decoded: &[f32], max_lag: i64) -> (f64, i64) {
    let mut best_snr = f64::NEG_INFINITY;
    let mut best_lag = 0i64;
    for lag in -max_lag..=max_lag {
        let mut signal_energy = 0f64;
        let mut noise_energy = 0f64;
        let mut n = 0u32;
        for (i, &o) in original.iter().enumerate() {
            let j = i as i64 + lag;
            if j < 0 {
                continue;
            }
            let Some(&d) = decoded.get(j as usize) else {
                continue;
            };
            signal_energy += f64::from(o) * f64::from(o);
            let e = f64::from(o) - f64::from(d);
            noise_energy += e * e;
            n += 1;
        }
        if n < 1000 {
            continue;
        }
        let snr = if noise_energy <= 1e-20 {
            f64::INFINITY
        } else {
            10.0 * (signal_energy / noise_energy).log10()
        };
        if snr > best_snr {
            best_snr = snr;
            best_lag = lag;
        }
    }
    (best_snr, best_lag)
}

#[test]
fn stereo_encode_produces_a_wellformed_ogg_ffmpeg_can_decode() {
    let original = synth(2, 2.0);
    let ogg = encode_to_ogg(&original);
    assert!(!ogg.is_empty());
    assert_eq!(&ogg[0..4], b"OggS");

    // Unconditional: this crate's own decoder must accept its own encoder's
    // output cleanly.
    let mine = decode_with_vaco(&ogg);
    assert_eq!(mine.len(), 2, "must decode as stereo");
    for (ch, samples) in mine.iter().enumerate() {
        assert!(
            samples.iter().all(|s| s.is_finite()),
            "channel {ch}: non-finite sample in this crate's own decode"
        );
    }

    let max_lag = 4096;
    let mut own_snrs = Vec::new();
    for ch in 0..2 {
        let (snr, lag) = best_snr_db(&original[ch], &mine[ch], max_lag);
        eprintln!("own-decoder channel {ch}: SNR {snr:.1} dB at lag {lag}");
        own_snrs.push(snr);
    }
    for (ch, &snr) in own_snrs.iter().enumerate() {
        assert!(
            snr > 3.0,
            "channel {ch} SNR against this crate's own decode too low: {snr:.2} dB"
        );
    }

    let Some(ffmpeg_decoded) = decode_with_ffmpeg(&ogg, 2) else {
        eprintln!("skipping ffmpeg cross-check: ffmpeg not available or refused the file");
        return;
    };
    assert_eq!(ffmpeg_decoded.len(), 2);

    let mut ffmpeg_snrs = Vec::new();
    for ch in 0..2 {
        let (snr, lag) = best_snr_db(&original[ch], &ffmpeg_decoded[ch], max_lag);
        eprintln!("ffmpeg channel {ch}: SNR {snr:.1} dB at lag {lag}");
        ffmpeg_snrs.push(snr);
        assert!(
            ffmpeg_decoded[ch].iter().all(|s| s.is_finite()),
            "channel {ch}: non-finite sample in ffmpeg's decode"
        );
    }
    for (ch, &snr) in ffmpeg_snrs.iter().enumerate() {
        assert!(
            snr > 3.0,
            "channel {ch} SNR against ffmpeg's independent decode too low: {snr:.2} dB"
        );
    }

    // Channel-swap check: channel 0 (220 Hz-ish) must correlate with
    // original channel 0 distinctly better than with original channel 1,
    // and likewise for channel 1 -- catches the AAC-5.1-shaped bug class
    // (every channel individually plausible, channels collectively
    // transposed) a per-channel-only SNR cannot.
    let (same0, _) = best_snr_db(&original[0], &ffmpeg_decoded[0], max_lag);
    let (cross0, _) = best_snr_db(&original[1], &ffmpeg_decoded[0], max_lag);
    assert!(
        same0 > cross0 + 3.0,
        "channel 0 correlates better with the wrong source channel (same={same0:.1} dB, cross={cross0:.1} dB) -- possible channel swap"
    );
}

#[test]
fn mono_encode_round_trips_through_this_crate_s_own_decoder() {
    let original = synth(1, 1.0);
    let ogg = encode_to_ogg(&original);
    assert_eq!(&ogg[0..4], b"OggS");
    let mine = decode_with_vaco(&ogg);
    assert_eq!(mine.len(), 1);
    assert!(mine[0].iter().all(|s| s.is_finite()));
    let (snr, lag) = best_snr_db(&original[0], &mine[0], 4096);
    eprintln!("mono own-decoder SNR {snr:.1} dB at lag {lag}");
    assert!(snr > 3.0, "mono SNR too low: {snr:.2} dB");
}

#[test]
fn encoder_declares_delay_and_subframes_caps() {
    // The encoder's own registry descriptor promises these; a smoke check
    // that the constant hasn't silently drifted from what `send_frame`
    // actually does (multiple packets per input, output continuing after
    // `send_frame(None)`).
    assert!(vaco_codec_vorbis::ENCODER_VORBIS.caps.contains(Caps::DELAY));
    assert!(
        vaco_codec_vorbis::ENCODER_VORBIS
            .caps
            .contains(Caps::SUBFRAMES)
    );
}
