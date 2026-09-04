//! Common Encryption against the reference's own encryptor: `ffmpeg
//! -encryption_scheme cenc-aes-ctr` writes *subsample* encryption for H.264
//! (`senc` flags `0x2`, one `(clear, protected)` pair per NAL unit) and
//! full-sample encryption for AAC, both with 8-byte IVs. Decrypting with the
//! same key must give back exactly the packets of the clear file the
//! encrypted one was stream-copied from.
//!
//! Skipped rather than failed when `ffmpeg` is absent, the convention
//! `vaco-codec-flac`'s `ffmpeg_fixture.rs` follows.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::process::{Command, Stdio};

use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, MemorySource};

const KEY_HEX: &str = "00112233445566778899aabbccddeeff";
const KID_HEX: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";

fn ffmpeg(args: &[&str]) -> Option<()> {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(())
}

fn packets(bytes: Vec<u8>, key: Option<[u8; 16]>) -> Vec<(u32, Vec<u8>)> {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let mut demux = Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options {
            decryption_key: key,
            ..Mp4Options::default()
        },
    )
    .unwrap();
    let mut out = Vec::new();
    while let Ok(pkt) = demux.read_packet() {
        out.push((pkt.stream_index, pkt.payload().to_vec()));
    }
    out
}

#[test]
fn ffmpeg_cenc_aes_ctr_subsample_and_full_sample_decrypt_to_the_clear_packets() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vaco-cenc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clear = dir.join("clear.mp4");
    let enc = dir.join("enc.mp4");
    let (clear_s, enc_s) = (clear.to_str().unwrap(), enc.to_str().unwrap());

    // H.264 with B-frames (several NAL units per sample, so the subsample
    // table has more than one entry) plus AAC, then a stream copy into a
    // `cenc` file: the packets are the same bytes before and after.
    let encoded = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x48:rate=25",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "0.4",
        "-c:v",
        "libx264",
        "-g",
        "5",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        clear_s,
    ])
    .and_then(|()| {
        ffmpeg(&[
            "-i",
            clear_s,
            "-c",
            "copy",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            KEY_HEX,
            "-encryption_kid",
            KID_HEX,
            enc_s,
        ])
    });
    if encoded.is_none() {
        eprintln!("skipping: this ffmpeg cannot write cenc-aes-ctr with libx264/aac");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let clear_bytes = std::fs::read(&clear).unwrap();
    let enc_bytes = std::fs::read(&enc).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let mut key = [0u8; 16];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(&KEY_HEX[2 * i..2 * i + 2], 16).unwrap();
    }

    // Without a key the protected tracks are refused by name, not read.
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(enc_bytes.clone()));
    let mut refused = Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options::default(),
    )
    .unwrap();
    for s in refused.streams() {
        assert!(
            s.metadata
                .iter()
                .any(|(k, v)| k == "encryption_scheme" && v == "cenc"),
            "stream {} must report its scheme",
            s.index
        );
        assert!(
            s.metadata
                .iter()
                .any(|(k, v)| k == "encryption_key_id" && v == KID_HEX),
            "stream {} must report its key id",
            s.index
        );
    }
    assert!(refused.read_packet().is_err());

    let expected = packets(clear_bytes, None);
    let got = packets(enc_bytes, Some(key));
    assert!(
        expected.len() > 8,
        "fixture too small: {} packets",
        expected.len()
    );
    assert_eq!(got.len(), expected.len(), "packet count");
    for (i, (e, g)) in expected.iter().zip(&got).enumerate() {
        assert_eq!(e.0, g.0, "packet {i} stream");
        assert!(
            e.1 == g.1,
            "packet {i} (stream {}) differs after decryption: {} vs {} bytes",
            e.0,
            g.1.len(),
            e.1.len()
        );
    }
}
