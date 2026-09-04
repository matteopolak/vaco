//! The binary boundary for MP4 Common Encryption.
//!
//! `vaco-demux-mp4` already proves its AES-CTR output packet-for-packet. This
//! test proves the user-facing `-decryption_key` option reaches that path and
//! uses ffmpeg only as a black-box fixture writer and packet-hash oracle.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::process::{Command, Stdio};

const KEY_HEX: &str = "00112233445566778899aabbccddeeff";
const KID_HEX: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
const KEY2_HEX: &str = "102132435465768798a9bacbdcedfe0f";
const KID2_HEX: &str = "ffeeddccbbaa99887766554433221100";
const KEY_DICTIONARY: &str = concat!(
    "0f1e2d3c4b5a69788796a5b4c3d2e1f0=00112233445566778899aabbccddeeff:",
    "ffeeddccbbaa99887766554433221100=102132435465768798a9bacbdcedfe0f"
);

fn ffmpeg(args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn vaco(args: &[&str]) -> (i32, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = vaco_cli::run(args, &mut stdout, &mut stderr);
    assert!(stdout.is_empty(), "media output must go to the named file");
    (code.code(), String::from_utf8_lossy(&stderr).into_owned())
}

fn packet_values(bytes: &[u8]) -> Vec<(String, String, String)> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty() && line.first() != Some(&b'#'))
        .filter_map(|line| {
            let text = core::str::from_utf8(line).ok()?;
            let mut fields = text.split(',').map(str::trim);
            let _stream = fields.next()?;
            let _dts = fields.next()?;
            let _pts = fields.next()?;
            Some((
                fields.next()?.to_owned(),
                fields.next()?.to_owned(),
                fields.next()?.to_owned(),
            ))
        })
        .collect()
}

#[test]
fn cli_decryption_key_reaches_mp4_and_matches_clear_packet_hashes() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }

    let dir = tempfile::tempdir().expect("temporary fixture directory");
    let source = dir.path().join("source.m4a");
    let clear = dir.path().join("clear.mp4");
    let encrypted = dir.path().join("encrypted.mp4");
    let vaco_clear_path = dir.path().join("vaco-clear.framemd5");
    let vaco_decrypted_path = dir.path().join("vaco-decrypted.framemd5");
    let (source_s, clear_s, encrypted_s, vaco_clear_s, vaco_decrypted_s) = (
        source.to_str().unwrap(),
        clear.to_str().unwrap(),
        encrypted.to_str().unwrap(),
        vaco_clear_path.to_str().unwrap(),
        vaco_decrypted_path.to_str().unwrap(),
    );

    let made = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=660:sample_rate=48000",
        "-t",
        "0.4",
        "-c:a",
        "aac",
        source_s,
    ])
    .and_then(|_| ffmpeg(&["-i", source_s, "-map", "0:a:0", "-c", "copy", clear_s]))
    .and_then(|_| {
        ffmpeg(&[
            "-i",
            source_s,
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            KEY_HEX,
            "-encryption_kid",
            KID_HEX,
            encrypted_s,
        ])
    });
    if made.is_none() {
        eprintln!("skipping: this ffmpeg cannot write cenc AAC");
        return;
    }

    let reference_clear = ffmpeg(&[
        "-i",
        clear_s,
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-bitexact",
        "-f",
        "framemd5",
        "-",
    ])
    .expect("ffmpeg must hash the clear fixture");
    let reference_decrypted = ffmpeg(&[
        "-decryption_key",
        KEY_HEX,
        "-i",
        encrypted_s,
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-bitexact",
        "-f",
        "framemd5",
        "-",
    ])
    .expect("ffmpeg must decrypt and hash its encrypted fixture");
    assert_eq!(reference_decrypted, reference_clear, "reference oracle");
    let reference_packets = packet_values(&reference_clear);
    assert_eq!(reference_packets.len(), 20, "reference packet count");

    let (clear_code, clear_err) = vaco(&[
        "-hide_banner",
        "-i",
        clear_s,
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-bitexact",
        "-f",
        "framemd5",
        vaco_clear_s,
    ]);
    assert_eq!(clear_code, 0, "clear input: {clear_err}");

    let (encrypted_code, encrypted_err) = vaco(&[
        "-hide_banner",
        "-decryption_key",
        KEY_HEX,
        "-i",
        encrypted_s,
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-bitexact",
        "-f",
        "framemd5",
        vaco_decrypted_s,
    ]);
    assert_eq!(encrypted_code, 0, "encrypted input: {encrypted_err}");
    let vaco_clear = std::fs::read(&vaco_clear_path).expect("vaco clear framemd5");
    let vaco_decrypted = std::fs::read(&vaco_decrypted_path).expect("vaco decrypted framemd5");
    assert_eq!(vaco_decrypted, vaco_clear, "vaco clear/decrypted packets");
    assert_eq!(
        packet_values(&vaco_decrypted),
        reference_packets,
        "packet duration, size and MD5 values"
    );
}

#[test]
fn cli_decryption_keys_selects_each_track_default_kid() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }

    let dir = tempfile::tempdir().expect("temporary fixture directory");
    let source = dir.path().join("source.m4a");
    let clear = dir.path().join("clear.mp4");
    let encrypted = [
        dir.path().join("encrypted-1.mp4"),
        dir.path().join("encrypted-2.mp4"),
    ];
    let vaco_decrypted = [
        dir.path().join("vaco-decrypted-1.framemd5"),
        dir.path().join("vaco-decrypted-2.framemd5"),
    ];
    let source_s = source.to_str().unwrap();
    let clear_s = clear.to_str().unwrap();

    let made = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=660:sample_rate=48000",
        "-t",
        "0.4",
        "-c:a",
        "aac",
        source_s,
    ])
    .and_then(|_| ffmpeg(&["-i", source_s, "-map", "0:a:0", "-c", "copy", clear_s]));
    if made.is_none() {
        eprintln!("skipping: this ffmpeg cannot write AAC MP4");
        return;
    }

    for (fixture, ((path, kid), key)) in encrypted
        .iter()
        .zip([KID_HEX, KID2_HEX])
        .zip([KEY_HEX, KEY2_HEX])
        .enumerate()
    {
        let path_s = path.to_str().unwrap();
        let mut args = vec![
            "-i",
            source_s,
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            key,
            "-encryption_kid",
            kid,
        ];
        if fixture == 1 {
            args.extend(["-movflags", "+empty_moov+frag_every_frame"]);
        }
        args.push(path_s);
        if ffmpeg(&args).is_none() {
            eprintln!("skipping: this ffmpeg cannot write cenc AAC");
            return;
        }
    }

    let reference_clear = ffmpeg(&[
        "-i",
        clear_s,
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-bitexact",
        "-f",
        "framemd5",
        "-",
    ])
    .expect("ffmpeg must hash the clear fixture");
    let reference_packets = packet_values(&reference_clear);
    assert_eq!(reference_packets.len(), 20, "reference packet count");

    for (encrypted_path, output_path) in encrypted.iter().zip(&vaco_decrypted) {
        let encrypted_s = encrypted_path.to_str().unwrap();
        let reference_decrypted = ffmpeg(&[
            "-decryption_keys",
            KEY_DICTIONARY,
            "-i",
            encrypted_s,
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-bitexact",
            "-f",
            "framemd5",
            "-",
        ])
        .expect("ffmpeg must select the matching KID and decrypt");
        assert_eq!(
            packet_values(&reference_decrypted),
            reference_packets,
            "reference dictionary oracle"
        );

        let (code, err) = vaco(&[
            "-hide_banner",
            "-decryption_keys",
            KEY_DICTIONARY,
            "-i",
            encrypted_s,
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-bitexact",
            "-f",
            "framemd5",
            output_path.to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "encrypted input: {err}");
        let vaco_packets = packet_values(&std::fs::read(output_path).expect("vaco framemd5"));
        assert_eq!(
            vaco_packets, reference_packets,
            "packet duration, size and MD5 values"
        );
    }
}
