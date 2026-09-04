//! Differential test for QOA against `ffmpeg`'s independent `qoa` decoder —
//! this crate's only test file for QOA before this one (`src/qoa.rs`'s own
//! `mod tests`, and `src/lib.rs`'s `QoaEncoder`/`QoaDecoder` tests) was
//! encoder round-tripped through decoder, both this crate's own code. That
//! is exactly the shape `CLAUDE.md` names as insufficient: "a self-round-trip
//! test proves the two halves agree, which is not the claim."
//!
//! # Why not a real third-party `.qoa` fixture
//!
//! `ffmpeg 9.0.1` on this machine has a `qoa` **decoder** but no `qoa`
//! **encoder** (`ffmpeg -encoders` lists none), and this is an offline
//! environment with no way to fetch one of qoaformat.org's own reference
//! files. So the strongest oracle available here is: encode with this
//! crate's own [`vaco_codec_simple_audio::QoaEncoder`] (spec-conformant by
//! construction — see `qoa.rs`'s module doc — but this crate's own code),
//! wrap the result in the minimal real QOA file header ffmpeg's demuxer
//! needs, and decode that **one shared set of bytes** with two genuinely
//! independent implementations: `ffmpeg`'s `qoa` decoder (a project this
//! crate shares no code with) and this crate's own [`QoaDecoder`]. Both
//! reading real QOA-framed bytes and agreeing with each other **and** with
//! the original signal is a real cross-implementation check, not a
//! same-code round trip — the same pattern `vaco-codec-alac`'s
//! `tests/oracle_alac_crate.rs` already uses for exactly this reason
//! (independent oracle decoder, own encoder, no independent encoder
//! available).
//!
//! # Why not a sine wave
//!
//! The source signal below sums four incommensurate sine components; see
//! `vaco-codec-adpcm/tests/oracle_ffmpeg.rs`'s module doc for why a single
//! periodic tone is the wrong choice for a test like this.
//!
//! QOA is lossy (an LMS predictor with a coarse quantiser), so this test
//! checks SNR against the source, not bit-exactness there — but the decoder
//! cross-check (`ffmpeg` vs this crate, same bytes) is bit-exact: QOA decode
//! is fully-specified integer arithmetic with no free rounding choice, so
//! two correct decoders reading the same bytes must produce identical
//! samples.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration test code over trusted, self-generated fixture data"
)]

use std::io::Write as _;
use std::process::Command;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::SendReceive;
use vaco_codec_simple_audio::{QoaDecoder, QoaEncoder};
use vaco_core::Timestamp;
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

/// Four incommensurate tones (not a pure sine — see module doc), one QOA
/// frame's worth of mono samples at 22050 Hz.
fn source_samples(n: usize, sample_rate: u32) -> Vec<i16> {
    (0..n)
        .map(|i| {
            let t = f64::from(i as u32) / f64::from(sample_rate);
            let v = 0.35 * (2.0 * std::f64::consts::PI * 437.0 * t).sin()
                + 0.25 * (2.0 * std::f64::consts::PI * 1289.0 * t).sin()
                + 0.15 * (2.0 * std::f64::consts::PI * 2777.0 * t).sin()
                + 0.10 * (2.0 * std::f64::consts::PI * 5431.0 * t).sin();
            (v * f64::from(i16::MAX)) as i16
        })
        .collect()
}

fn frame_i16(frame: &Frame) -> Vec<i16> {
    let FrameData::Audio { planes, .. } = &frame.data else {
        panic!("expected an audio frame");
    };
    planes[0]
        .data
        .as_slice()
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn ffmpegs_independent_qoa_decoder_agrees_with_ours_on_the_same_real_bytes() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available on this machine; skipping oracle test");
        return;
    }

    let sample_rate = 22_050u32;
    let n = 3000usize; // well under one QOA frame's 5120-sample cap
    let source = source_samples(n, sample_rate);

    // 1. Encode with this crate's own encoder.
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(
        &mut budget,
        SampleFmt::S16,
        ChannelLayout::MONO,
        n as u32,
        sample_rate,
    )
    .expect("alloc_audio");
    {
        let mut plane = frame.plane_mut(0).expect("plane 0");
        let row = plane.row_mut(0).expect("row 0");
        for (i, &s) in source.iter().enumerate() {
            row[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
        }
    }
    frame.pts = Timestamp::new(0);

    let mut enc = QoaEncoder::new(Limits::permissive());
    enc.send(Some(&frame)).expect("send_frame");
    let packet = enc.receive().expect("receive_packet");
    let frame_bytes = packet.payload().to_vec();

    // 2. Wrap in the real QOA file header (qoaformat.org spec §"Header":
    // magic "qoaf" + total samples per channel, big-endian) so ffmpeg's
    // demuxer accepts it. Not this crate's concern at the codec level (see
    // `qoa.rs`'s "What is not covered") but 8 bytes to add here.
    let mut qoa_file = Vec::new();
    qoa_file.extend_from_slice(b"qoaf");
    qoa_file.extend_from_slice(&(n as u32).to_be_bytes());
    qoa_file.extend_from_slice(&frame_bytes);

    let dir = std::env::temp_dir().join(format!("vaco-qoa-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir scratch dir");
    let qoa_path = dir.join("fixture.qoa");
    std::fs::File::create(&qoa_path)
        .and_then(|mut f| f.write_all(&qoa_file))
        .expect("write qoa file");

    // 3. Decode with ffmpeg (independent implementation).
    let ref_path = dir.join("ref.raw");
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(&qoa_path)
        .args([
            "-acodec",
            "pcm_s16le",
            "-f",
            "s16le",
            "-fflags",
            "+bitexact",
        ])
        .arg(&ref_path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed to decode our own qoa file");
    let ref_bytes = std::fs::read(&ref_path).expect("read ffmpeg output");
    let ffmpeg_pcm: Vec<i16> = ref_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    // 4. Decode the identical frame bytes with this crate's own decoder.
    let mut budget2 = Budget::new(Limits::permissive());
    let our_packet = Packet::from_slice(&mut budget2, &frame_bytes).expect("packet from_slice");
    let mut dec = QoaDecoder::new(Limits::permissive());
    dec.send(Some(&our_packet)).expect("send_packet");
    let decoded = dec.receive().expect("receive_frame");
    let our_pcm = frame_i16(&decoded);

    // The sample-count check CLAUDE.md's own postmortems name explicitly.
    assert_eq!(
        our_pcm.len(),
        n,
        "this crate's own decoder must report the real sample count"
    );
    assert_eq!(
        ffmpeg_pcm.len(),
        n,
        "ffmpeg's independent decode of our own real QOA bytes must report \
         the same real sample count"
    );

    // Two independent, fully-specified integer decoders reading the same
    // real QOA bytes must agree bit-for-bit.
    assert_eq!(
        our_pcm, ffmpeg_pcm,
        "this crate's QOA decoder must reproduce ffmpeg's independent decode \
         of the same real bitstream bit-for-bit"
    );

    // And both must be a real (lossy but faithful) reconstruction of the
    // source, not merely self-consistent garbage.
    let mut sum_sq_err = 0f64;
    let mut sum_sq_sig = 0f64;
    for (&a, &b) in source.iter().zip(our_pcm.iter()) {
        let e = f64::from(a) - f64::from(b);
        sum_sq_err += e * e;
        sum_sq_sig += f64::from(a) * f64::from(a);
    }
    let snr = 10.0 * (sum_sq_sig / sum_sq_err.max(1.0)).log10();
    assert!(snr > 20.0, "SNR against the real source too low: {snr} dB");
}
