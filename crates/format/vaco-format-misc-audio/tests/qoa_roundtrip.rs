//! End-to-end regression for the `.qoa` file demuxer: encode a real tone
//! with this tree's own QOA encoder (`vaco-codec-simple-audio`), wrap the
//! resulting frames in the file-level header this crate's `qoa` module
//! parses, open that byte buffer with [`QoaDemuxer`], and decode every
//! packet it hands back with the same crate's own decoder.
//!
//! This is the gap that made QOA decode registered but unreachable: the
//! decoder never parsed file framing (by design -- see `vaco_codec_simple_
//! audio::qoa`'s module doc) and, before this demuxer existed, nothing else
//! did either, so no real `.qoa` file could ever be opened from the CLI.
//! Encode-then-demux-then-decode, checked against the original samples, is
//! the strongest test available in this crate without a network fixture:
//! it proves the file header and per-frame `fsize` framing this module adds
//! line up exactly with what a real encoder emits and a real decoder
//! expects, on both sides of the boundary this module owns.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    reason = "test code"
)]

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Decoder;
use vaco_codec_simple_audio::{QOA_DECODER, QOA_ENCODER};
use vaco_format_core::Demuxer;
use vaco_format_misc_audio::qoa::QoaDemuxer;
use vaco_frame::{Frame, FrameData};
use vaco_io::MemorySource;
use vaco_limits::{Budget, Limits};

const SAMPLE_RATE: u32 = 44_100;

fn synth_i16(seconds: f64) -> Vec<i16> {
    let n = (f64::from(SAMPLE_RATE) * seconds) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(SAMPLE_RATE);
            (0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() * f64::from(i16::MAX)) as i16
        })
        .collect()
}

/// Encode `samples` (mono) to QOA packets, then wrap them in a `.qoa` file:
/// the 8-byte `magic + total_samples` header this crate's demuxer expects,
/// per <https://qoaformat.org/qoa-specification.pdf>.
fn build_qoa_file(samples: &[i16]) -> Vec<u8> {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(&mut budget, vaco_sampfmt::SampleFmt::S16, ChannelLayout::MONO, samples.len() as u32, SAMPLE_RATE).unwrap();
    {
        let mut plane = frame.plane_mut(0).unwrap();
        let row = plane.row_mut(0).unwrap();
        for (dst, &s) in row.chunks_exact_mut(2).zip(samples.iter()) {
            dst.copy_from_slice(&s.to_le_bytes());
        }
    }
    frame.pts = vaco_core::Timestamp::new(0);

    let mut enc = (QOA_ENCODER.make)(Limits::permissive());
    enc.send_frame(Some(&frame)).unwrap();
    enc.send_frame(None).unwrap();
    let mut wire_frames: Vec<u8> = Vec::new();
    while let Ok(packet) = enc.receive_packet() {
        wire_frames.extend_from_slice(packet.payload());
    }
    assert!(!wire_frames.is_empty(), "encoder produced no QOA frames");

    let mut file = Vec::new();
    file.extend_from_slice(b"qoaf");
    file.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    file.extend_from_slice(&wire_frames);
    file
}

#[test]
fn a_real_encoded_tone_survives_demux_and_decode() {
    let original = synth_i16(0.5);
    let file_bytes = build_qoa_file(&original);

    let mut demux = QoaDemuxer::open(Box::new(MemorySource::new(file_bytes))).unwrap();
    let audio = demux.streams()[0].params.audio.clone().unwrap();
    assert_eq!(audio.sample_rate, SAMPLE_RATE);
    assert_eq!(audio.layout, Some(ChannelLayout::MONO));

    let mut dec = (QOA_DECODER.make)(Limits::permissive());
    let mut decoded: Vec<i16> = Vec::new();
    let drain = |dec: &mut dyn Decoder, decoded: &mut Vec<i16>| loop {
        match dec.receive_frame() {
            Ok(frame) => {
                let FrameData::Audio { samples, .. } = &frame.data else {
                    continue;
                };
                let n = *samples as usize;
                let plane = frame.plane(0).unwrap();
                let row = plane.row(0).unwrap();
                for chunk in row.chunks_exact(2).take(n) {
                    decoded.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                }
            }
            Err(vaco_core::Error::NeedMoreInput | vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected decode error: {e:?}"),
        }
    };
    while let Ok(packet) = demux.read_packet() {
        dec.send_packet(Some(&packet)).unwrap();
        drain(dec.as_mut(), &mut decoded);
    }
    dec.send_packet(None).unwrap();
    drain(dec.as_mut(), &mut decoded);

    assert!(
        decoded.len() >= original.len() - original.len() / 10,
        "decoded far fewer samples than encoded: {} vs {}",
        decoded.len(),
        original.len()
    );

    // QOA is lossy (a sign-sign LMS predictor), so this checks correlation
    // against the original tone rather than sample equality.
    let n = decoded.len().min(original.len());
    let mut dot = 0f64;
    let mut na = 0f64;
    let mut nb = 0f64;
    for i in 0..n {
        let a = f64::from(original[i]);
        let b = f64::from(decoded[i]);
        dot += a * b;
        na += a * a;
        nb += b * b;
    }
    let corr = dot / (na.sqrt() * nb.sqrt()).max(1e-9);
    assert!(corr > 0.9, "decoded tone poorly correlated with the original: {corr:.4}");
}
