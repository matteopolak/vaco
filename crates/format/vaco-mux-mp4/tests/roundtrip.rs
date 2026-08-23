//! Mux a small file with this crate, then read it back with
//! `vaco-demux-mp4` — the most direct way to verify the byte layout is
//! self-consistent, reusing that crate's own measured understanding of the
//! format rather than re-deriving a second, parallel one just for tests.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::SharedDynBuf;
use vaco_io::{MediaSink, MediaSource, MemorySource};
use vaco_limits::{Budget, Limits};
use vaco_mux_mp4::options::MovFlags;
use vaco_mux_mp4::{MovMuxer, MuxOptions};
use vaco_packet::Packet;

/// A minimal, structurally valid `AVCDecoderConfigurationRecord`: one SPS,
/// one PPS, 4-byte NAL length prefixes.
fn avc_extradata() -> Vec<u8> {
    vec![
        1, 0x42, 0x00, 0x0A, // version, profile, compat, level
        0xFF, // reserved(6) | lengthSizeMinusOne(2) = 4-byte lengths
        0xE1, // reserved(3) | numOfSequenceParameterSets(5) = 1
        0x00, 0x04, 0x67, 0x42, 0x00, 0x0A, // SPS length + bytes
        0x01, // numOfPictureParameterSets = 1
        0x00, 0x02, 0x68, 0xCE, // PPS length + bytes
    ]
}

fn h264_params() -> CodecParameters {
    let mut p = CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(CodecId::H264),
        extradata: Some(avc_extradata()),
        ..CodecParameters::default()
    };
    p.video = Some(VideoParameters {
        width: 64,
        height: 48,
        frame_rate: Rational::new(30, 1),
        ..VideoParameters::default()
    });
    p
}

fn nal_payload(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

/// The declared size of the box starting at `at` (its first four bytes).
fn box_len(bytes: &[u8], at: usize) -> usize {
    let word: [u8; 4] = bytes
        .get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .unwrap();
    u32::from_be_bytes(word) as usize
}

fn packet(stream: u32, dts: i64, is_key: bool, payload: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut p = Packet::from_slice(&mut budget, payload).unwrap();
    p.stream_index = stream;
    p.dts = Timestamp::new(dts);
    p.pts = p.dts;
    if is_key {
        p.flags |= vaco_packet::PacketFlags::KEY;
    }
    p
}

/// Drive `mux` directly through the `Muxer` trait, in the track's own time
/// base — bypassing `vaco_format_core::mux::MuxBuilder` so this test
/// exercises this crate's own chunking/table logic without also re-testing
/// the shared M1-M28 pipeline, which has its own test suite.
fn write_video_file(mux: &mut MovMuxer, sample_count: usize) {
    let idx = mux.add_stream(&h264_params()).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    for i in 0..sample_count {
        let dts = i as i64 * 100;
        let is_key = i % 5 == 0;
        let payload = nal_payload(&[0x65, i as u8, 0xAA, 0xBB, 0xCC]);
        mux.write_packet(&packet(idx, dts, is_key, &payload))
            .unwrap();
    }
    mux.write_trailer().unwrap();
}

#[test]
fn a_progressive_file_round_trips_every_sample() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MovMuxer::new(Box::new(sink.clone()) as Box<dyn MediaSink>).unwrap();
    write_video_file(&mut mux, 20);

    let bytes = sink.snapshot();
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let mut demux = vaco_demux_mp4::Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        vaco_demux_mp4::Mp4Options::default(),
    )
    .unwrap();
    assert_eq!(demux.streams().len(), 1);
    assert_eq!(demux.streams()[0].params.codec_id, Some(CodecId::H264));

    let mut count = 0;
    let mut last_dts = -1i64;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                let dts = p.dts.ticks().unwrap();
                assert!(dts > last_dts, "dts must strictly increase");
                last_dts = dts;
                assert_eq!(dts % 100, 0);
                count += 1;
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(count, 20);
}

#[test]
fn faststart_puts_moov_before_mdat() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let opts = MuxOptions {
        movflags: MovFlags::FASTSTART,
        ..MuxOptions::default()
    };
    let mut mux =
        MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
    write_video_file(&mut mux, 10);

    let bytes = sink.snapshot();
    // First box is `ftyp`; the second box's type tells us the layout.
    let ftyp_len = box_len(&bytes, 0);
    let second_kind = bytes.get(ftyp_len + 4..ftyp_len + 8).unwrap();
    assert_eq!(
        second_kind, b"moov",
        "faststart must place moov before mdat"
    );

    // And it must still demux to the same samples as the non-faststart file.
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let mut demux = vaco_demux_mp4::Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        vaco_demux_mp4::Mp4Options::default(),
    )
    .unwrap();
    let mut count = 0;
    while demux.read_packet().is_ok() {
        count += 1;
    }
    assert_eq!(count, 10);
}

#[test]
fn a_non_faststart_file_puts_mdat_before_moov() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MovMuxer::new(Box::new(sink.clone()) as Box<dyn MediaSink>).unwrap();
    write_video_file(&mut mux, 5);
    let bytes = sink.snapshot();
    let ftyp_len = box_len(&bytes, 0);
    let second_kind = bytes.get(ftyp_len + 4..ftyp_len + 8).unwrap();
    assert_eq!(second_kind, b"mdat");
}

#[test]
fn sync_samples_are_reported_correctly() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MovMuxer::new(Box::new(sink.clone()) as Box<dyn MediaSink>).unwrap();
    write_video_file(&mut mux, 11);
    let bytes = sink.snapshot();

    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let mut demux = vaco_demux_mp4::Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        vaco_demux_mp4::Mp4Options::default(),
    )
    .unwrap();
    let mut i = 0;
    while let Ok(p) = demux.read_packet() {
        assert_eq!(p.is_key(), i % 5 == 0, "sample {i}");
        i += 1;
    }
    assert_eq!(i, 11);
}

#[test]
fn a_fragmented_file_round_trips() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let opts = MuxOptions {
        movflags: MovFlags::FRAG_KEYFRAME,
        ..MuxOptions::default()
    };
    let mut mux =
        MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
    write_video_file(&mut mux, 17);

    let bytes = sink.snapshot();
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let mut demux = vaco_demux_mp4::Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        vaco_demux_mp4::Mp4Options::default(),
    )
    .unwrap();
    let mut count = 0;
    let mut last_dts = -1i64;
    while let Ok(p) = demux.read_packet() {
        let dts = p.dts.ticks().unwrap();
        assert!(dts >= last_dts);
        last_dts = dts;
        count += 1;
    }
    assert_eq!(count, 17);
}

fn open_demux(bytes: Vec<u8>) -> vaco_demux_mp4::Mp4Demuxer {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    vaco_demux_mp4::Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        vaco_demux_mp4::Mp4Options::default(),
    )
    .unwrap()
}

#[test]
fn separate_moof_still_demuxes_every_sample() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let opts = MuxOptions {
        movflags: MovFlags::FRAG_KEYFRAME | MovFlags::SEPARATE_MOOF | MovFlags::DEFAULT_BASE_MOOF,
        ..MuxOptions::default()
    };
    let mut mux =
        MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
    write_video_file(&mut mux, 13);
    let mut demux = open_demux(sink.snapshot());
    let mut count = 0;
    while demux.read_packet().is_ok() {
        count += 1;
    }
    assert_eq!(count, 13);
}

#[test]
fn dash_output_carries_a_sidx_and_still_demuxes() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let opts = MuxOptions {
        movflags: MovFlags::FRAG_KEYFRAME | MovFlags::DASH,
        ..MuxOptions::default()
    };
    let mut mux =
        MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
    write_video_file(&mut mux, 12);
    let bytes = sink.snapshot();

    // Walk top-level boxes looking for `sidx` between `moov` and the first `moof`.
    let mut pos = 0usize;
    let mut saw_sidx = false;
    while pos + 8 <= bytes.len() {
        let len = box_len(&bytes, pos);
        let kind = bytes.get(pos + 4..pos + 8).unwrap();
        if kind == b"sidx" {
            saw_sidx = true;
        }
        if len < 8 {
            break;
        }
        pos += len;
    }
    assert!(saw_sidx, "dash output must carry a sidx box");

    let mut demux = open_demux(bytes);
    let mut count = 0;
    while demux.read_packet().is_ok() {
        count += 1;
    }
    assert_eq!(count, 12);
}

#[test]
fn two_tracks_interleave_and_both_demux_back() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MovMuxer::new(Box::new(sink.clone()) as Box<dyn MediaSink>).unwrap();
    let video = mux.add_stream(&h264_params()).unwrap();
    let mut audio_params = CodecParameters {
        media_type: Some(MediaType::Audio),
        codec_id: Some(CodecId::Aac),
        extradata: Some(vec![0x12, 0x08]),
        ..CodecParameters::default()
    };
    audio_params.audio = Some(vaco_codec_core::AudioParameters {
        sample_rate: 48_000,
        ..vaco_codec_core::AudioParameters::default()
    });
    let audio = mux.add_stream(&audio_params).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    for i in 0..10 {
        let v_dts = i64::from(i) * 100;
        mux.write_packet(&packet(
            video,
            v_dts,
            i % 5 == 0,
            &nal_payload(&[0x65, 0xAA]),
        ))
        .unwrap();
        let a_dts = i64::from(i) * 48;
        mux.write_packet(&packet(audio, a_dts, true, &[0xAB, 0xCD, 0xEF]))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let mut demux = open_demux(sink.snapshot());
    assert_eq!(demux.streams().len(), 2);
    let mut counts = [0u32; 2];
    while let Ok(p) = demux.read_packet() {
        if let Some(slot) = counts.get_mut(p.stream_index as usize) {
            *slot += 1;
        }
    }
    assert_eq!(counts, [10, 10]);
}

proptest::proptest! {
    /// The part the brief calls out by name: faststart's offset fixup.
    /// Every sample's *payload bytes* must come back unchanged regardless of
    /// how many samples there are or how big they are — including sizes
    /// that push chunk offsets around the `stco`/`co64` boundary this test
    /// does not specifically target (that boundary is exercised directly in
    /// `vaco-format-isom::writer`'s own proptest instead; this one is about
    /// the muxer's fixed-point convergence, not the table width choice).
    #[test]
    fn faststart_offsets_are_exact_for_arbitrary_sample_shapes(
        sizes in proptest::collection::vec(1u32..2000, 1..60),
    ) {
        let sink = SharedDynBuf::with_limits(Limits::permissive());
        let opts = MuxOptions { movflags: MovFlags::FASTSTART, ..MuxOptions::default() };
        let mut mux = MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
        let idx = mux.add_stream(&h264_params()).unwrap();
        mux.init().unwrap();
        mux.write_header().unwrap();
        let payloads: Vec<Vec<u8>> = sizes
            .iter()
            .enumerate()
            .map(|(i, &n)| (0..n).map(|b| (b as usize + i) as u8).collect())
            .collect();
        for (i, payload) in payloads.iter().enumerate() {
            mux.write_packet(&packet(idx, i as i64 * 100, i % 4 == 0, payload)).unwrap();
        }
        mux.write_trailer().unwrap();

        let mut demux = open_demux(sink.snapshot());
        for want in &payloads {
            let p = demux.read_packet().unwrap();
            proptest::prop_assert_eq!(p.payload(), want.as_slice());
        }
        proptest::prop_assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
    }
}

#[test]
fn itunes_style_tags_round_trip() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let opts = MuxOptions {
        tags: vec![(*b"\xa9nam", "Hello Title".to_owned())],
        ..MuxOptions::default()
    };
    let mut mux =
        MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
    write_video_file(&mut mux, 3);
    let mut demux = open_demux(sink.snapshot());
    let title = demux
        .metadata()
        .iter()
        .find(|(k, _)| k == "title")
        .map(|(_, v)| v.clone());
    assert_eq!(title.as_deref(), Some("Hello Title"));
    let mut count = 0;
    while demux.read_packet().is_ok() {
        count += 1;
    }
    assert_eq!(count, 3);
}

#[test]
fn every_registered_brand_writes_its_measured_ftyp_bytes() {
    use vaco_mux_mp4::Brand;
    let cases: &[(Brand, &[u8], &[&[u8]])] = &[
        (Brand::Mp4, b"isom", &[b"isom", b"iso2", b"mp41"]),
        (Brand::Mov, b"qt  ", &[b"qt  "]),
        (Brand::Ipod, b"M4V ", &[b"M4V ", b"isom", b"iso2"]),
        (Brand::Ismv, b"isml", &[b"isml", b"piff"]),
        (Brand::F4v, b"f4v ", &[b"f4v ", b"isom", b"iso2", b"avc1"]),
        (Brand::Psp, b"MSNV", &[b"MSNV", b"isom", b"iso2"]),
        (Brand::ThreeGp, b"3gp4", &[b"3gp4", b"isom", b"iso2"]),
        (Brand::ThreeG2, b"3g2a", &[b"3g2a", b"isom", b"iso2"]),
    ];
    for (brand, major, compatible) in cases {
        let bytes = vaco_mux_mp4::brand::file_type_box(*brand);
        assert_eq!(bytes.get(4..8).unwrap(), b"ftyp");
        assert_eq!(bytes.get(8..12).unwrap(), *major, "{brand:?} major brand");
        let mut at = 16usize; // past major_brand + minor_version
        for want in *compatible {
            assert_eq!(
                bytes.get(at..at + 4).unwrap(),
                *want,
                "{brand:?} compatible brand at {at}"
            );
            at += 4;
        }
        assert_eq!(bytes.len(), at, "{brand:?} has no extra compatible brands");
    }
}

#[test]
fn a_non_mp4_brand_still_demuxes_as_a_normal_mp4_file() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let opts = MuxOptions {
        brand: vaco_mux_mp4::Brand::Ipod,
        ..MuxOptions::default()
    };
    let mut mux =
        MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
    write_video_file(&mut mux, 6);
    let mut demux = open_demux(sink.snapshot());
    let mut count = 0;
    while demux.read_packet().is_ok() {
        count += 1;
    }
    assert_eq!(count, 6);
}

#[test]
fn nero_chapters_round_trip() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let opts = MuxOptions {
        chapters: vec![
            vaco_mux_mp4::options::ChapterMark {
                start: Timestamp::new(0),
                time_base: Rational::new(1, 1),
                title: "Intro".to_owned(),
            },
            vaco_mux_mp4::options::ChapterMark {
                start: Timestamp::new(5),
                time_base: Rational::new(1, 1),
                title: "Chapter Two".to_owned(),
            },
        ],
        ..MuxOptions::default()
    };
    let mut mux =
        MovMuxer::with_options(Box::new(sink.clone()) as Box<dyn MediaSink>, opts).unwrap();
    write_video_file(&mut mux, 4);
    let demux = open_demux(sink.snapshot());
    let titles: Vec<&str> = demux
        .chapters()
        .iter()
        .flat_map(|c| c.metadata.iter())
        .filter(|(k, _)| k == "title")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(titles, vec!["Intro", "Chapter Two"]);
}

/// [`Muxer::set_metadata`] (M30, gap 1) is the path `vaco-cli`'s `-metadata`
/// actually drives — [`itunes_style_tags_round_trip`] and
/// [`nero_chapters_round_trip`] above exercise the same boxes through
/// [`MovMuxer::with_options`] instead, which is a different, lower-level
/// entry point. This is the "best test" the CL-16 brief asks for: set
/// metadata with the generic, container-agnostic
/// [`vaco_format_core::metadata::MuxMetadata`], mux, and read every bit of it
/// back through `vaco-demux-mp4`.
#[test]
fn set_metadata_round_trips_through_the_demuxer() {
    use vaco_format_core::Chapter;
    use vaco_format_core::metadata::{MuxAttachment, MuxMetadata};

    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MovMuxer::new(Box::new(sink.clone()) as Box<dyn MediaSink>).unwrap();
    let idx = mux.add_stream(&h264_params()).unwrap();
    assert_eq!(idx, 0);
    mux.init().unwrap();

    let mut meta = MuxMetadata {
        tags: vec![
            ("title".to_owned(), "Hello Title".to_owned()),
            ("artist".to_owned(), "Some Artist".to_owned()),
            // No `itunes_fourcc` mapping: dropped rather than guessed at.
            ("no_such_mapping".to_owned(), "ignored".to_owned()),
        ],
        chapters: vec![Chapter {
            id: 0,
            time_base: Rational::new(1, 1),
            start: Timestamp::new(0),
            end: Timestamp::new(5),
            metadata: vec![("title".to_owned(), "Intro".to_owned())],
        }],
        attachments: vec![MuxAttachment {
            filename: "cover.png".to_owned(),
            mime_type: "image/png".to_owned(),
            description: String::new(),
            data: vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4],
        }],
        ..MuxMetadata::default()
    };
    meta.stream_tags = vec![vec![("language".to_owned(), "eng".to_owned())]];
    mux.set_metadata(&meta).unwrap();

    mux.write_header().unwrap();
    for i in 0..2i64 {
        let payload = nal_payload(&[0x65, i as u8, 0xAA, 0xBB, 0xCC]);
        mux.write_packet(&packet(idx, i * 100, true, &payload))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let mut demux = open_demux(sink.snapshot());

    let title = demux
        .metadata()
        .iter()
        .find(|(k, _)| k == "title")
        .map(|(_, v)| v.clone());
    assert_eq!(title.as_deref(), Some("Hello Title"));
    let artist = demux
        .metadata()
        .iter()
        .find(|(k, _)| k == "artist")
        .map(|(_, v)| v.clone());
    assert_eq!(artist.as_deref(), Some("Some Artist"));
    assert!(
        demux.metadata().iter().all(|(k, _)| k != "no_such_mapping"),
        "an unmapped key must not silently invent an ilst atom"
    );

    let chapter_titles: Vec<&str> = demux
        .chapters()
        .iter()
        .flat_map(|c| c.metadata.iter())
        .filter(|(k, _)| k == "title")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(chapter_titles, vec!["Intro"]);

    let stream_language = demux.streams()[0]
        .metadata
        .iter()
        .find(|(k, _)| k == "language")
        .map(|(_, v)| v.clone());
    assert_eq!(stream_language.as_deref(), Some("eng"));

    // 2 video samples plus one packet for the `covr` cover image, which
    // `vaco-demux-mp4` exposes as its own `ATTACHED_PIC` stream (measured:
    // see that crate's `cover_stream`) rather than folding into `metadata`.
    let mut count = 0;
    while demux.read_packet().is_ok() {
        count += 1;
    }
    assert_eq!(count, 3);
}

/// `vaco-cli`'s scheduler drives a raw `dyn Muxer` (per
/// `planning/INTERFACE-GAPS.md`'s `MuxWork` note) and cannot guarantee
/// `set_metadata` runs after `add_stream`, so `MovMuxer` must not depend on
/// that order either — this is the MP4 analogue of the same test in
/// `vaco-mux-matroska`.
#[test]
fn set_metadata_before_add_stream_still_resolves_per_stream_language() {
    use vaco_format_core::metadata::MuxMetadata;

    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MovMuxer::new(Box::new(sink.clone()) as Box<dyn MediaSink>).unwrap();

    let meta = MuxMetadata {
        stream_tags: vec![vec![("language".to_owned(), "fra".to_owned())]],
        ..MuxMetadata::default()
    };
    mux.set_metadata(&meta).unwrap();

    let idx = mux.add_stream(&h264_params()).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(idx, 0, true, &nal_payload(&[0x65])))
        .unwrap();
    mux.write_trailer().unwrap();

    let demux = open_demux(sink.snapshot());
    let language = demux.streams()[0]
        .metadata
        .iter()
        .find(|(k, _)| k == "language")
        .map(|(_, v)| v.clone());
    assert_eq!(language.as_deref(), Some("fra"));
}
