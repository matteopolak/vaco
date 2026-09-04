//! Common Encryption against the reference's own encryptor: `ffmpeg
//! -encryption_scheme cenc-aes-ctr` writes *subsample* encryption for H.264
//! (`senc` flags `0x2`, one `(clear, protected)` pair per NAL unit) and
//! full-sample encryption for AAC, both with 8-byte IVs. Decrypting with the
//! same key must give back exactly the packets of the clear file the
//! encrypted one was stream-copied from.
//!
//! Skipped rather than failed when `ffmpeg` is absent, the convention
//! `vaco-codec-flac`'s `ffmpeg_fixture.rs` follows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::trivially_copy_pass_by_ref
)]

use std::ops::Range;
use std::path::Path;
use std::process::{Command, Stdio};

use vaco_core::{Error, Timestamp};
use vaco_demux_mp4::{DecryptionKey, Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, SeekFlags, SeekTarget};
use vaco_format_isom::{BoxIter, IsoFile, fourcc::boxes};
use vaco_io::{MediaSource, MemorySource};

const KEY_HEX: &str = "00112233445566778899aabbccddeeff";
const KID_HEX: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
const KEY2_HEX: &str = "102132435465768798a9bacbdcedfe0f";
const KID2_HEX: &str = "ffeeddccbbaa99887766554433221100";
const KEY_BINDING: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0=00112233445566778899aabbccddeeff";
const KEY2_BINDING: &str = "ffeeddccbbaa99887766554433221100=102132435465768798a9bacbdcedfe0f";

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

fn ffmpeg_output(args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn hex16(value: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn key_dictionary() -> Vec<DecryptionKey> {
    vec![
        DecryptionKey {
            kid: hex16(KID_HEX),
            key: hex16(KEY_HEX),
        },
        DecryptionKey {
            kid: hex16(KID2_HEX),
            key: hex16(KEY2_HEX),
        },
    ]
}

fn open_with_keys(bytes: Vec<u8>, keys: Vec<DecryptionKey>) -> Mp4Demuxer {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options {
            decryption_keys: keys,
            ..Mp4Options::default()
        },
    )
    .unwrap()
}

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len().saturating_add(8)).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn fullbx(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![version, 0, 0, 0];
    payload.extend_from_slice(body);
    bx(kind, &payload)
}

fn seig_entry(kid: [u8; 16], pattern: u8, protected: bool, iv_size: u8) -> Vec<u8> {
    let mut out = vec![0, pattern, u8::from(protected), iv_size];
    out.extend_from_slice(&kid);
    out
}

fn seig_boxes(entry: &[u8], runs: &[(u32, u32)], group_version: u8) -> Vec<u8> {
    let mut sgpd = Vec::new();
    sgpd.extend_from_slice(b"seig");
    if group_version >= 1 {
        sgpd.extend_from_slice(&u32::try_from(entry.len()).unwrap().to_be_bytes());
    }
    if group_version >= 2 {
        sgpd.extend_from_slice(&0u32.to_be_bytes());
    }
    sgpd.extend_from_slice(&1u32.to_be_bytes());
    sgpd.extend_from_slice(entry);

    let mut sbgp = Vec::new();
    sbgp.extend_from_slice(b"seig");
    sbgp.extend_from_slice(&u32::try_from(runs.len()).unwrap().to_be_bytes());
    for &(count, index) in runs {
        sbgp.extend_from_slice(&count.to_be_bytes());
        sbgp.extend_from_slice(&index.to_be_bytes());
    }

    let mut out = fullbx(b"sgpd", group_version, &sgpd);
    out.extend_from_slice(&fullbx(b"sbgp", 0, &sbgp));
    out
}

fn first_path(bytes: &[u8], kinds: &[&[u8; 4]]) -> Vec<(usize, usize)> {
    let mut children = BoxIter::new(bytes, 0);
    let mut out = Vec::new();
    for kind in kinds {
        let found = children
            .find_map(|candidate| {
                let candidate = candidate.ok()?;
                (candidate.kind().as_bytes() == **kind).then_some(candidate)
            })
            .expect("missing box in fixture path");
        out.push((
            usize::try_from(found.offset).unwrap(),
            usize::try_from(found.header.size).unwrap(),
        ));
        children = found.children();
    }
    out
}

fn seig_group_path(
    bytes: &[u8],
    container_kinds: &[&[u8; 4]],
    group_kind: &[u8; 4],
) -> Vec<(usize, usize)> {
    let mut children = BoxIter::new(bytes, 0);
    let mut out = Vec::new();
    for kind in container_kinds {
        let found = children
            .find_map(|candidate| {
                let candidate = candidate.ok()?;
                (candidate.kind().as_bytes() == **kind).then_some(candidate)
            })
            .expect("missing container in fixture path");
        out.push((
            usize::try_from(found.offset).unwrap(),
            usize::try_from(found.header.size).unwrap(),
        ));
        children = found.children();
    }
    let found = children
        .find_map(|candidate| {
            let candidate = candidate.ok()?;
            (candidate.kind().as_bytes() == *group_kind
                && candidate.full().ok()?.body.get(..4) == Some(b"seig"))
            .then_some(candidate)
        })
        .expect("missing seig group box in fixture path");
    out.push((
        usize::try_from(found.offset).unwrap(),
        usize::try_from(found.header.size).unwrap(),
    ));
    out
}

fn insert_at_container_end(bytes: &mut Vec<u8>, path: &[(usize, usize)], extra: &[u8]) {
    let &(start, size) = path.last().unwrap();
    let at = start.checked_add(size).unwrap();
    let add = u32::try_from(extra.len()).unwrap();
    for &(box_start, old_size) in path {
        assert_ne!(
            u32::from_be_bytes(bytes[box_start..box_start + 4].try_into().unwrap()),
            1
        );
        bytes[box_start..box_start + 4].copy_from_slice(
            &u32::try_from(old_size)
                .unwrap()
                .saturating_add(add)
                .to_be_bytes(),
        );
    }
    bytes.splice(at..at, extra.iter().copied());
}

fn sample_ranges(bytes: &[u8]) -> Vec<Range<usize>> {
    let file = IsoFile::parse(bytes, 0).unwrap();
    let track = &file.movie.unwrap().tracks[0];
    track
        .sample_table
        .cursor_at(0)
        .map(|sample| {
            let start = usize::try_from(sample.offset).unwrap();
            start..start.checked_add(sample.size as usize).unwrap()
        })
        .collect()
}

fn progressive_rotation(mut first: Vec<u8>, second: &[u8]) -> Vec<u8> {
    let first_ranges = sample_ranges(&first);
    let second_ranges = sample_ranges(second);
    assert_eq!(first_ranges.len(), second_ranges.len());
    assert!(first_ranges.len() > 8);
    let split = first_ranges.len().checked_div(2).unwrap();
    for (dst, src) in first_ranges[split..].iter().zip(&second_ranges[split..]) {
        assert_eq!(dst.len(), src.len());
        first[dst.clone()].copy_from_slice(&second[src.clone()]);
    }
    let first_senc = first_path(
        &first,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"senc"],
    );
    let second_senc = first_path(
        second,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"senc"],
    );
    let (first_senc, first_senc_size) = *first_senc.last().unwrap();
    let (second_senc, second_senc_size) = *second_senc.last().unwrap();
    assert_eq!(first_senc_size, second_senc_size);
    assert_eq!(first_senc_size, 16 + first_ranges.len() * 8);
    let records = split * 8;
    first[first_senc + 16 + records..first_senc + first_senc_size]
        .copy_from_slice(&second[second_senc + 16 + records..second_senc + second_senc_size]);

    let top: Vec<_> = BoxIter::new(&first, 0).filter_map(Result::ok).collect();
    let moov = top.iter().find(|b| b.kind() == boxes::MOOV).unwrap();
    let mdat = top.iter().find(|b| b.kind() == boxes::MDAT).unwrap();
    assert!(
        moov.offset > mdat.offset,
        "inserting into trailing moov must not move media"
    );
    let path = first_path(&first, &[b"moov", b"trak", b"mdia", b"minf", b"stbl"]);
    let runs = [
        (u32::try_from(split).unwrap(), 0),
        (u32::try_from(first_ranges.len() - split).unwrap(), 1),
    ];
    let groups = seig_boxes(&seig_entry(hex16(KID2_HEX), 0, true, 8), &runs, 1);
    insert_at_container_end(&mut first, &path, &groups);
    first
}

fn rotate_moof(mut moof: Vec<u8>) -> Vec<u8> {
    let traf_path = first_path(&moof, &[b"moof", b"traf"]);
    let trun_path = first_path(&moof, &[b"moof", b"traf", b"trun"]);
    let (trun, _) = *trun_path.last().unwrap();
    let flags = u32::from_be_bytes([0, moof[trun + 9], moof[trun + 10], moof[trun + 11]]);
    assert_ne!(flags & 1, 0, "fixture trun must carry data_offset");
    let count = u32::from_be_bytes(moof[trun + 12..trun + 16].try_into().unwrap());
    let groups = seig_boxes(
        &seig_entry(hex16(KID2_HEX), 0, true, 8),
        &[(count, 0x1_0001)],
        1,
    );
    let data_at = trun + 16;
    let old = i32::from_be_bytes(moof[data_at..data_at + 4].try_into().unwrap());
    let add = i32::try_from(groups.len()).unwrap();
    moof[data_at..data_at + 4].copy_from_slice(&old.checked_add(add).unwrap().to_be_bytes());
    insert_at_container_end(&mut moof, &traf_path, &groups);
    moof
}

fn fragmented_rotation(first: &[u8], second: &[u8]) -> Vec<u8> {
    let a: Vec<_> = BoxIter::new(first, 0).filter_map(Result::ok).collect();
    let b: Vec<_> = BoxIter::new(second, 0).filter_map(Result::ok).collect();
    assert_eq!(a.len(), b.len());
    let mut out = Vec::new();
    let mut fragment = 0usize;
    let mut use_second = false;
    for (one, two) in a.iter().zip(&b) {
        assert_eq!(one.kind(), two.kind());
        assert_eq!(one.header.size, two.header.size);
        let start1 = usize::try_from(one.offset).unwrap();
        let end1 = start1 + usize::try_from(one.header.size).unwrap();
        let start2 = usize::try_from(two.offset).unwrap();
        let end2 = start2 + usize::try_from(two.header.size).unwrap();
        match one.kind() {
            boxes::MOOF => {
                use_second = fragment % 2 == 1;
                fragment += 1;
                let moof = if use_second {
                    second[start2..end2].to_vec()
                } else {
                    first[start1..end1].to_vec()
                };
                if use_second {
                    out.extend_from_slice(&rotate_moof(moof));
                } else {
                    out.extend_from_slice(&moof);
                }
            }
            boxes::MDAT if use_second => out.extend_from_slice(&second[start2..end2]),
            boxes::MFRA => {}
            _ => out.extend_from_slice(&first[start1..end1]),
        }
    }
    assert!(fragment > 8);
    out
}

fn ffmpeg_packet_hashes(path: &Path, keys: Option<&str>) -> Vec<u8> {
    let path = path.to_str().unwrap();
    let mut args = Vec::new();
    if let Some(keys) = keys {
        args.extend(["-decryption_keys", keys]);
    }
    args.extend([
        "-i",
        path,
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-bitexact",
        "-f",
        "framemd5",
        "-",
    ]);
    ffmpeg_output(&args).expect("ffmpeg framemd5")
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

type ExactPacket = (u32, Option<i64>, Option<i64>, usize, Vec<u8>);

fn open(bytes: Vec<u8>, key: Option<[u8; 16]>) -> Mp4Demuxer {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        Mp4Options {
            decryption_key: key,
            ..Mp4Options::default()
        },
    )
    .unwrap()
}

fn exact_packets(demux: &mut Mp4Demuxer) -> Result<Vec<ExactPacket>, Error> {
    let mut out = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(packet) => out.push((
                packet.stream_index,
                packet.pts.ticks(),
                packet.dts.ticks(),
                packet.payload().len(),
                packet.payload().to_vec(),
            )),
            Err(Error::Eof) => return Ok(out),
            Err(err) => return Err(err),
        }
    }
}

fn nested_senc_boxes(bytes: &[u8]) -> Vec<(usize, u32)> {
    BoxIter::new(bytes, 0)
        .filter_map(Result::ok)
        .filter(|b| b.kind() == boxes::MOOF)
        .flat_map(|moof| moof.children().filter_map(Result::ok))
        .filter(|b| b.kind() == boxes::TRAF)
        .flat_map(|traf| traf.children().filter_map(Result::ok))
        .filter(|b| b.kind() == boxes::SENC)
        .filter_map(|b| {
            Some((
                usize::try_from(b.offset).ok()?,
                u32::try_from(b.header.size).ok()?,
            ))
        })
        .collect()
}

#[test]
fn fragmented_cenc_aac_decrypts_to_clear_packets_across_fragments_and_seek() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vaco-fragmented-cenc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.m4a");
    let clear = dir.join("clear.mp4");
    let encrypted = dir.join("encrypted.mp4");
    let (source_s, clear_s, encrypted_s) = (
        source.to_str().unwrap(),
        clear.to_str().unwrap(),
        encrypted.to_str().unwrap(),
    );

    // Encode AAC once, then make clear and encrypted fragmented stream copies
    // so the expected packet bytes come from the same encoded samples.
    let encoded = ffmpeg(&[
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
    .and_then(|()| {
        ffmpeg(&[
            "-i",
            source_s,
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-movflags",
            "+empty_moov+frag_every_frame",
            clear_s,
        ])
    })
    .and_then(|()| {
        ffmpeg(&[
            "-i",
            source_s,
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-movflags",
            "+empty_moov+frag_every_frame",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            KEY_HEX,
            "-encryption_kid",
            KID_HEX,
            encrypted_s,
        ])
    });
    if encoded.is_none() {
        eprintln!("skipping: this ffmpeg cannot write fragmented cenc AAC");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let clear_bytes = std::fs::read(&clear).unwrap();
    let encrypted_bytes = std::fs::read(&encrypted).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let moof_count = BoxIter::new(&encrypted_bytes, 0)
        .filter_map(Result::ok)
        .filter(|b| b.kind() == boxes::MOOF)
        .count();
    assert!(moof_count > 2, "fixture has only {moof_count} fragments");
    let senc_boxes = nested_senc_boxes(&encrypted_bytes);
    assert_eq!(senc_boxes.len(), moof_count, "one senc per audio traf");

    let mut key = [0u8; 16];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&KEY_HEX[2 * i..2 * i + 2], 16).unwrap();
    }
    let mut clear_demux = open(clear_bytes, None);
    let mut encrypted_demux = open(encrypted_bytes.clone(), Some(key));
    assert!(
        encrypted_demux.streams()[0]
            .metadata
            .iter()
            .any(|(name, value)| name == "encryption_scheme" && value == "cenc")
    );
    assert!(
        encrypted_demux.streams()[0]
            .metadata
            .iter()
            .any(|(name, value)| name == "encryption_key_id" && value == KID_HEX)
    );
    let expected = exact_packets(&mut clear_demux).unwrap();
    let got =
        exact_packets(&mut encrypted_demux).expect("fragment-local senc must decrypt every packet");
    assert!(
        expected.len() > 8,
        "fixture has only {} packets",
        expected.len()
    );
    assert_eq!(got, expected, "packet fields and payloads");

    let target = expected[expected.len() >> 1]
        .2
        .expect("AAC packet has a decode timestamp");
    for demux in [&mut clear_demux, &mut encrypted_demux] {
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(target),
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
    }
    let expected_after_seek = clear_demux.read_packet().unwrap();
    let got_after_seek = encrypted_demux.read_packet().unwrap();
    assert_eq!(
        got_after_seek.stream_index,
        expected_after_seek.stream_index
    );
    assert_eq!(got_after_seek.pts, expected_after_seek.pts);
    assert_eq!(got_after_seek.dts, expected_after_seek.dts);
    assert_eq!(got_after_seek.payload(), expected_after_seek.payload());

    let (first_senc, first_senc_size) = senc_boxes[0];
    let mut missing_senc = encrypted_bytes.clone();
    missing_senc[first_senc + 4..first_senc + 8].copy_from_slice(b"free");
    let missing_err = open(missing_senc, Some(key))
        .read_packet()
        .expect_err("a protected fragment without senc must be refused");
    assert!(missing_err.to_string().contains("senc"), "{missing_err}");

    let mut truncated_senc = encrypted_bytes;
    truncated_senc[first_senc..first_senc + 4]
        .copy_from_slice(&first_senc_size.saturating_sub(1).to_be_bytes());
    let truncated_err = open(truncated_senc, Some(key))
        .read_packet()
        .expect_err("a protected fragment with a truncated senc must be refused");
    assert!(
        truncated_err.to_string().contains("senc"),
        "{truncated_err}"
    );
}

#[test]
fn seig_selects_rotated_keys_in_progressive_and_fragmented_cenc() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vaco-seig-cenc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.m4a");
    let clear = [dir.join("clear.mp4"), dir.join("clear-fragmented.mp4")];
    let first = [dir.join("first.mp4"), dir.join("first-fragmented.mp4")];
    let second = [dir.join("second.mp4"), dir.join("second-fragmented.mp4")];
    let rotated = [dir.join("rotated.mp4"), dir.join("rotated-fragmented.mp4")];
    let source_s = source.to_str().unwrap();
    if ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=733:sample_rate=48000",
        "-t",
        "0.4",
        "-c:a",
        "aac",
        source_s,
    ])
    .is_none()
    {
        eprintln!("skipping: this ffmpeg cannot write AAC MP4");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    for layout in 0..2 {
        let clear_s = clear[layout].to_str().unwrap();
        let first_s = first[layout].to_str().unwrap();
        let second_s = second[layout].to_str().unwrap();
        let mut common = vec!["-i", source_s, "-map", "0:a:0", "-c", "copy"];
        if layout == 1 {
            common.extend([
                "-movflags",
                "+empty_moov+frag_every_frame+default_base_moof",
            ]);
        }
        let mut clear_args = common.clone();
        clear_args.push(clear_s);
        let mut first_args = common.clone();
        first_args.extend([
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            KEY_HEX,
            "-encryption_kid",
            KID_HEX,
            first_s,
        ]);
        let mut second_args = common;
        second_args.extend([
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            KEY2_HEX,
            "-encryption_kid",
            KID2_HEX,
            second_s,
        ]);
        if ffmpeg(&clear_args)
            .and_then(|()| ffmpeg(&first_args))
            .and_then(|()| ffmpeg(&second_args))
            .is_none()
        {
            eprintln!("skipping: this ffmpeg cannot write cenc AAC");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let oracle_clear = ffmpeg_packet_hashes(&clear[layout], None);
        assert_eq!(
            ffmpeg_packet_hashes(&first[layout], Some(KEY_BINDING)),
            oracle_clear,
            "ffmpeg layout {layout} first-key source"
        );
        assert_eq!(
            ffmpeg_packet_hashes(&second[layout], Some(KEY2_BINDING)),
            oracle_clear,
            "ffmpeg layout {layout} second-key source"
        );
    }

    let first_progressive = std::fs::read(&first[0]).unwrap();
    let second_progressive = std::fs::read(&second[0]).unwrap();
    let first_fragmented = std::fs::read(&first[1]).unwrap();
    let second_fragmented = std::fs::read(&second[1]).unwrap();
    let fixtures = [
        progressive_rotation(first_progressive, &second_progressive),
        fragmented_rotation(&first_fragmented, &second_fragmented),
    ];

    for layout in 0..2 {
        std::fs::write(&rotated[layout], &fixtures[layout]).unwrap();
        let clear_bytes = std::fs::read(&clear[layout]).unwrap();
        let mut expected = open(clear_bytes, None);
        let mut got = open_with_keys(fixtures[layout].clone(), key_dictionary());
        assert!(
            got.streams()[0]
                .metadata
                .iter()
                .any(|(name, value)| name == "encryption_scheme" && value == "cenc")
        );
        assert_eq!(
            got.streams()[0]
                .metadata
                .iter()
                .filter(|(name, _)| name == "encryption_key_id")
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            vec![KID_HEX],
            "layout {layout} reports the track default while seig selects samples"
        );
        let expected_packets = exact_packets(&mut expected).unwrap();
        let got_packets = exact_packets(&mut got).expect("seig must select each sample's KID");
        assert_eq!(got_packets, expected_packets, "layout {layout} packets");
        assert_eq!(got_packets.len(), 20, "layout {layout} packet count");

        if layout == 1 {
            let target = expected_packets[expected_packets
                .len()
                .checked_mul(3)
                .and_then(|index| index.checked_div(4))
                .unwrap()]
            .2
            .expect("AAC packet DTS");
            for demux in [&mut expected, &mut got] {
                demux
                    .seek(
                        SeekTarget::Timestamp {
                            stream_index: 0,
                            ts: Timestamp::new(target),
                        },
                        SeekFlags::BACKWARD,
                    )
                    .unwrap();
            }
            let expected_after_seek = expected.read_packet().unwrap();
            let got_after_seek = got.read_packet().unwrap();
            assert_eq!(got_after_seek.pts, expected_after_seek.pts);
            assert_eq!(got_after_seek.dts, expected_after_seek.dts);
            assert_eq!(got_after_seek.payload(), expected_after_seek.payload());
        }
    }

    let missing_key = open_with_keys(fixtures[0].clone(), vec![key_dictionary()[0]]);
    let mut missing_key = missing_key;
    let err =
        exact_packets(&mut missing_key).expect_err("mapped seig KID without a key must refuse");
    assert!(err.to_string().contains("seig"), "{err}");
    assert!(err.to_string().contains("key"), "{err}");

    for (name, mutate) in [("pattern", (25usize, 0x19u8)), ("clear", (26, 0))] {
        let mut invalid = fixtures[0].clone();
        let sgpd = seig_group_path(
            &invalid,
            &[b"moov", b"trak", b"mdia", b"minf", b"stbl"],
            b"sgpd",
        )
        .last()
        .unwrap()
        .0;
        invalid[sgpd + mutate.0] = mutate.1;
        let mut demux = open_with_keys(invalid, key_dictionary());
        let err = exact_packets(&mut demux).expect_err("unsupported seig entry must refuse");
        assert!(err.to_string().contains("seig"), "{name}: {err}");
        assert!(err.to_string().contains(name), "{name}: {err}");
    }

    let mut constant_iv = fixtures[0].clone();
    let sgpd_path = seig_group_path(
        &constant_iv,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl"],
        b"sgpd",
    );
    let sgpd = sgpd_path.last().unwrap().0;
    insert_at_container_end(
        &mut constant_iv,
        &sgpd_path,
        &[
            16, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc,
            0xcc, 0xcc,
        ],
    );
    constant_iv[sgpd + 16..sgpd + 20].copy_from_slice(&37u32.to_be_bytes());
    constant_iv[sgpd + 27] = 0;
    let mut constant_iv = open_with_keys(constant_iv, key_dictionary());
    let err = exact_packets(&mut constant_iv).expect_err("constant-IV seig must refuse");
    assert!(err.to_string().contains("seig"), "{err}");
    assert!(err.to_string().contains("constant IV"), "{err}");

    for version in [0u8, 2] {
        let mut unsupported = fixtures[0].clone();
        let sgpd = seig_group_path(
            &unsupported,
            &[b"moov", b"trak", b"mdia", b"minf", b"stbl"],
            b"sgpd",
        )
        .last()
        .unwrap()
        .0;
        unsupported[sgpd + 8] = version;
        let mut demux = open_with_keys(unsupported, key_dictionary());
        let err = exact_packets(&mut demux).expect_err("unsupported sgpd version must refuse");
        assert!(err.to_string().contains("seig"), "v{version}: {err}");
        assert!(err.to_string().contains("version 1"), "v{version}: {err}");
    }

    let mut out_of_range = fixtures[0].clone();
    let sbgp = seig_group_path(
        &out_of_range,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl"],
        b"sbgp",
    )
    .last()
    .unwrap()
    .0;
    out_of_range[sbgp + 32..sbgp + 36].copy_from_slice(&2u32.to_be_bytes());
    let mut out_of_range = open_with_keys(out_of_range, key_dictionary());
    let err = exact_packets(&mut out_of_range).expect_err("missing seig description must refuse");
    assert!(err.to_string().contains("seig"), "{err}");
    assert!(err.to_string().contains("missing"), "{err}");

    let mut duplicate = fixtures[0].clone();
    let stbl_path = first_path(&duplicate, &[b"moov", b"trak", b"mdia", b"minf", b"stbl"]);
    let sbgp_path = seig_group_path(
        &duplicate,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl"],
        b"sbgp",
    );
    let (sbgp, size) = *sbgp_path.last().unwrap();
    let extra = duplicate[sbgp..sbgp + size].to_vec();
    insert_at_container_end(&mut duplicate, &stbl_path, &extra);
    let mut duplicate = open_with_keys(duplicate, key_dictionary());
    let err = exact_packets(&mut duplicate).expect_err("duplicate seig mapping must refuse");
    assert!(err.to_string().contains("duplicate"), "{err}");
    assert!(err.to_string().contains("seig"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
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

#[test]
fn a_key_does_not_send_other_cenc_schemes_through_the_ctr_decryptor() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vaco-cbcs-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let enc = dir.join("enc.mp4");
    let Some(enc_s) = enc.to_str() else {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

    let encoded = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x48:rate=25",
        "-t",
        "0.2",
        "-c:v",
        "libx264",
        "-encryption_scheme",
        "cenc-aes-ctr",
        "-encryption_key",
        KEY_HEX,
        "-encryption_kid",
        KID_HEX,
        enc_s,
    ]);
    if encoded.is_none() {
        eprintln!("skipping: this ffmpeg cannot write cenc-aes-ctr with libx264");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let bytes = std::fs::read(&enc).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    // Keep ffmpeg's real encrypted sample tables and change only `schm`'s
    // scheme_type. That is enough to prove the demuxer selects its crypto by
    // the declared scheme rather than by the mere presence of a key + `senc`.
    let schm = bytes
        .windows(4)
        .position(|window| window == b"schm")
        .expect("encrypted fixture has a schm box");
    let scheme = schm + 8;
    assert_eq!(&bytes[scheme..scheme + 4], b"cenc");
    let mut key = [0u8; 16];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&KEY_HEX[2 * i..2 * i + 2], 16).unwrap();
    }
    for other in [*b"cens", *b"cbc1", *b"cbcs"] {
        let mut changed = bytes.clone();
        changed[scheme..scheme + 4].copy_from_slice(&other);
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(changed));
        let mut demux = Mp4Demuxer::open(
            src,
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options {
                decryption_key: Some(key),
                ..Mp4Options::default()
            },
        )
        .unwrap();
        let name = String::from_utf8_lossy(&other);
        assert!(
            demux.streams()[0]
                .metadata
                .iter()
                .any(|(k, v)| k == "encryption_scheme" && v == name.as_ref())
        );
        let err = demux
            .read_packet()
            .expect_err("non-cenc schemes must not use the AES-CTR path");
        assert!(matches!(err, vaco_core::Error::Unsupported(_)));
        assert!(err.to_string().contains(name.as_ref()), "{name}: {err}");
    }
}
