//! End-to-end demuxing, over files built by [`vaco_demux_matroska::synth`].
//!
//! Every expectation here that concerns a number `ffprobe` prints was checked
//! against `ffprobe 8.1` on the same bytes; the fixtures are reproduced by
//! `cargo run -p vaco-demux-matroska --example mkvgen -- <dir>`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use vaco_core::{Error, MediaType, Rational};
use vaco_demux_matroska::ebml::schema as el;
use vaco_demux_matroska::synth::{self, SegmentSize};
use vaco_demux_matroska::{MatroskaDemuxer, probe};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, MemorySource};
use vaco_packet::Packet;

// ------------------------------------------------------------------ helpers

fn open(bytes: Vec<u8>) -> Result<MatroskaDemuxer, Error> {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
}

fn open_pipe(bytes: Vec<u8>) -> Result<MatroskaDemuxer, Error> {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::forward_only(bytes));
    MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
}

/// Read every packet, with a hard cap so a non-terminating parse fails the test
/// instead of hanging the suite.
fn drain(d: &mut MatroskaDemuxer) -> Vec<Packet> {
    let mut out = Vec::new();
    for _ in 0..100_000 {
        match d.read_packet() {
            Ok(p) => out.push(p),
            Err(_) => return out,
        }
    }
    panic!("read_packet produced 100 000 packets without ending");
}

fn info(scale: u64) -> Vec<u8> {
    synth::uint(el::TIMESTAMPSCALE, scale)
}

/// A lacing-enabled PCM track with a 20 ms `DefaultDuration`.
fn audio_track() -> Vec<u8> {
    let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48000.0);
    audio.extend_from_slice(&synth::uint(el::CHANNELS, 2));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_PCM/INT/LIT"));
    body.extend_from_slice(&synth::uint(el::FLAGLACING, 1));
    body.extend_from_slice(&synth::uint(el::DEFAULTDURATION, 20_000_000));
    body.extend_from_slice(&synth::element(el::AUDIO, &audio));
    synth::element(el::TRACKENTRY, &body)
}

fn simple_block(track: u8, ts: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
    synth::element(
        el::SIMPLEBLOCK,
        &synth::block_body(track, ts, flags, payload),
    )
}

/// One audio track, one cluster, one block carrying `payload` with `flags`.
fn laced_file(flags: u8, payload: &[u8]) -> Vec<u8> {
    let cluster = synth::cluster(
        0,
        &[simple_block(1, 0, 0x80 | flags, payload)],
        SegmentSize::Known,
    );
    synth::file(
        "matroska",
        &info(1_000_000),
        &audio_track(),
        &[cluster],
        SegmentSize::Known,
    )
}

// -------------------------------------------------------------------- probe

#[test]
fn probing_matches_the_reference_scores() {
    let bytes = laced_file(0, b"frame");
    assert_eq!(probe::probe(&ProbeData::new(&bytes)), ProbeScore::MAX);
    assert_eq!(probe::doc_type(&bytes), Some("matroska"));
}

// ------------------------------------------------------------------- header

#[test]
fn the_time_base_comes_from_timestamp_scale() {
    // RFC 9559 section 5.1.2.9: nanoseconds per tick, default 1 000 000.
    let d = open(laced_file(0, b"frame")).unwrap();
    assert_eq!(d.timestamp_scale(), 1_000_000);
    assert_eq!(d.streams()[0].time_base.num, 1);
    assert_eq!(d.streams()[0].time_base.den, 1000);
}

/// The case every implementation that assumes milliseconds gets wrong.
/// Checked against `ffprobe`: `time_base=1/10000000`, `pts=10010000`.
#[test]
fn a_hundred_nanosecond_scale_gives_a_hundred_nanosecond_time_base() {
    let block = simple_block(1, 10_000, 0x80, &[0u8; 8]);
    let cluster = synth::cluster(10_000_000, &[block], SegmentSize::Known);
    let bytes = synth::file(
        "matroska",
        &info(100),
        &synth::video_track(1, "V_VP8", 160, 120),
        &[cluster],
        SegmentSize::Known,
    );
    let mut d = open(bytes).unwrap();
    assert_eq!(d.streams()[0].time_base.den, 10_000_000);
    let packets = drain(&mut d);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].pts.ticks(), Some(10_010_000));
}

#[test]
fn default_duration_keeps_a_nanosecond_clock_exact() {
    let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48_000.0);
    audio.extend_from_slice(&synth::uint(el::CHANNELS, 2));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_PCM/INT/LIT"));
    body.extend_from_slice(&synth::uint(el::DEFAULTDURATION, 26_122_448));
    body.extend_from_slice(&synth::element(el::AUDIO, &audio));
    let track = synth::element(el::TRACKENTRY, &body);
    let cluster = synth::cluster(
        0,
        &[simple_block(1, 0, 0x80, &[0u8; 8])],
        SegmentSize::Known,
    );
    let mut demux = open(synth::file(
        "matroska",
        &info(1),
        &track,
        &[cluster],
        SegmentSize::Known,
    ))
    .unwrap();

    assert_eq!(
        demux.streams()[0].time_base,
        Rational::new(1, 1_000_000_000)
    );
    let packet = demux.read_packet().unwrap();
    assert_eq!(packet.duration.as_ratio(), (1_632_653, 62_500_000));
}

#[test]
fn a_doc_type_we_do_not_read_is_refused() {
    let mut bytes = synth::ebml_header("dtshd");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &[]));
    assert!(matches!(open(bytes), Err(Error::Unsupported(_))));
}

#[test]
fn an_empty_or_non_ebml_stream_is_refused_without_panicking() {
    assert!(open(Vec::new()).is_err());
    assert!(open(b"RIFF\x00\x00\x00\x00WAVE".to_vec()).is_err());
}

#[test]
fn pixel_cropping_shrinks_the_reported_size() {
    // RFC 9559 section 15.1.
    let mut video = synth::uint(el::PIXELWIDTH, 320);
    video.extend_from_slice(&synth::uint(el::PIXELHEIGHT, 240));
    video.extend_from_slice(&synth::uint(el::PIXELCROPLEFT, 8));
    video.extend_from_slice(&synth::uint(el::PIXELCROPRIGHT, 8));
    video.extend_from_slice(&synth::uint(el::PIXELCROPTOP, 4));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 1));
    body.extend_from_slice(&synth::string(el::CODECID, "V_VP8"));
    body.extend_from_slice(&synth::element(el::VIDEO, &video));
    let track = synth::element(el::TRACKENTRY, &body);
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &track,
        &[],
        SegmentSize::Known,
    );
    let d = open(bytes).unwrap();
    let v = d.streams()[0].params.video.as_ref().unwrap();
    assert_eq!((v.coded_width, v.coded_height), (320, 240));
    assert_eq!((v.width, v.height), (304, 236));
    // With no DisplayWidth the schema default is the cropped size, so 1:1.
    assert_eq!(v.sample_aspect_ratio.num, 1);
    assert_eq!(v.sample_aspect_ratio.den, 1);
}

#[test]
fn a_track_whose_codec_has_no_codec_id_variant_still_becomes_a_stream() {
    // `A_MLP` (Meridian Lossless Packing), not `A_AC3`: `vaco-codec-core` has
    // no `CodecId::Mlp` variant, whereas `A_AC3` used to be the example here
    // and stopped being one the day finding 4
    // (`planning/CONFORMANCE-FINDINGS.md`) mapped it to `CodecId::Ac3` — a
    // test that failed *because a real gap closed*, exactly the anti-pattern
    // `planning/AGENT-CONSTRAINTS.md` "Never pin the absence of something the
    // project is building" warns about. This still asserts the behaviour
    // that matters (an unmappable codec is still reported as a stream, with
    // its media type), just with an example that is not the demuxer's own
    // codec table's job to keep current.
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_MLP"));
    let track = synth::element(el::TRACKENTRY, &body);
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &track,
        &[],
        SegmentSize::Known,
    );
    let d = open(bytes).unwrap();
    assert_eq!(d.streams().len(), 1);
    assert_eq!(d.streams()[0].media_type(), Some(MediaType::Audio));
    assert_eq!(d.streams()[0].params.codec_id, None);
}

// ------------------------------------------------------------------- lacing

/// All four modes, against the same three frames. Checked against `ffprobe`,
/// which reports pts 0/20/40 and sizes 80/50/100 for the two size-carrying
/// lacings and 80/80/80 for the fixed one.
#[test]
fn every_lacing_produces_the_same_timestamps() {
    let f1 = vec![0xA1; 80];
    let f2 = vec![0xB2; 50];
    let f3 = vec![0xC3; 100];
    let frames: [&[u8]; 3] = [&f1, &f2, &f3];

    for (name, flags, payload) in [
        ("xiph", 0x02u8, synth::xiph_lace(&frames)),
        ("ebml", 0x06, synth::ebml_lace(&frames)),
    ] {
        let mut d = open(laced_file(flags, &payload)).unwrap();
        let p = drain(&mut d);
        assert_eq!(p.len(), 3, "{name}");
        assert_eq!(
            p.iter().map(|p| p.pts.ticks()).collect::<Vec<_>>(),
            vec![Some(0), Some(20), Some(40)],
            "{name}"
        );
        assert_eq!(
            p.iter().map(|p| p.len).collect::<Vec<_>>(),
            vec![80, 50, 100],
            "{name}"
        );
        assert_eq!(p[0].payload()[0], 0xA1, "{name}");
        assert_eq!(p[1].payload()[0], 0xB2, "{name}");
        assert_eq!(p[2].payload()[0], 0xC3, "{name}");
        // Every frame of a lace reports the block's own byte position.
        assert_eq!(p[0].pos, p[2].pos, "{name}");
    }

    let same = vec![0x11; 80];
    let payload = synth::fixed_lace(&[&same, &same, &same]);
    let mut d = open(laced_file(0x04, &payload)).unwrap();
    let p = drain(&mut d);
    assert_eq!(p.len(), 3);
    assert!(p.iter().all(|p| p.len == 80));
    assert_eq!(p[2].pts.ticks(), Some(40));

    let mut d = open(laced_file(0x00, &f1)).unwrap();
    assert_eq!(drain(&mut d).len(), 1);
}

#[test]
fn a_lace_claiming_more_frames_than_it_carries_is_dropped_not_fatal() {
    // 0xFF declares 256 frames with no size octets behind it.
    let mut d = open(laced_file(0x02, &[0xFF])).unwrap();
    assert!(drain(&mut d).is_empty());
}

#[test]
fn a_fixed_lace_that_does_not_divide_is_dropped() {
    let mut payload = vec![0x02];
    payload.extend_from_slice(&[0u8; 5]);
    let mut d = open(laced_file(0x04, &payload)).unwrap();
    assert!(drain(&mut d).is_empty());
}

// ------------------------------------------------------------ unknown sizes

/// RFC 8794 section 6.2: nothing but the schema says where these end.
#[test]
fn an_unknown_size_segment_full_of_unknown_size_clusters_reads() {
    let mk = |ts: u64, fill: u8| {
        synth::cluster(
            ts,
            &[simple_block(1, 0, 0x80, &[fill; 64])],
            SegmentSize::Unknown,
        )
    };
    let bytes = synth::file(
        "webm",
        &info(1_000_000),
        &synth::video_track(1, "V_VP8", 160, 120),
        &[mk(0, 1), mk(100, 2), mk(200, 3)],
        SegmentSize::Unknown,
    );
    let mut d = open(bytes.clone()).unwrap();
    let p = drain(&mut d);
    assert_eq!(
        p.iter().map(|p| p.pts.ticks()).collect::<Vec<_>>(),
        vec![Some(0), Some(100), Some(200)]
    );

    // The same bytes on a pipe: no seeking anywhere in the path.
    let mut d = open_pipe(bytes).unwrap();
    assert_eq!(drain(&mut d).len(), 3);
}

#[test]
fn a_level_one_element_ends_an_unknown_size_cluster() {
    let mut segment = synth::element(el::INFO, &info(1_000_000));
    segment.extend_from_slice(&synth::element(
        el::TRACKS,
        &synth::video_track(1, "V_VP8", 16, 16),
    ));
    segment.extend_from_slice(&synth::cluster(
        0,
        &[simple_block(1, 0, 0x80, b"one")],
        SegmentSize::Unknown,
    ));
    // `Tags` is not a legal child of `Cluster`, so it closes it.
    segment.extend_from_slice(&synth::element(el::TAGS, &[]));
    segment.extend_from_slice(&synth::cluster(
        100,
        &[simple_block(1, 0, 0x80, b"two")],
        SegmentSize::Known,
    ));
    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));
    let mut d = open(bytes).unwrap();
    let p = drain(&mut d);
    assert_eq!(p.len(), 2);
    assert_eq!(p[1].pts.ticks(), Some(100));
}

#[test]
fn an_unknown_size_element_that_is_not_a_cluster_is_refused() {
    // RFC 8794 section 6.2 permits unknown sizes only where the schema says so.
    let mut segment = synth::element(el::INFO, &info(1_000_000));
    segment.extend_from_slice(&synth::element_unknown_size(el::TAGS, &[]));
    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));
    assert!(open(bytes).is_err());
}

// -------------------------------------------------------- content encodings

#[test]
fn header_stripping_prepends_its_settings_to_every_frame() {
    let mut comp = synth::uint(el::CONTENTCOMPALGO, 3);
    comp.extend_from_slice(&synth::element(el::CONTENTCOMPSETTINGS, &[0xDE, 0xAD]));
    let mut enc = synth::uint(el::CONTENTENCODINGORDER, 0);
    enc.extend_from_slice(&synth::element(el::CONTENTCOMPRESSION, &comp));
    let encodings = synth::element(
        el::CONTENTENCODINGS,
        &synth::element(el::CONTENTENCODING, &enc),
    );
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 1));
    body.extend_from_slice(&synth::string(el::CODECID, "V_VP8"));
    body.extend_from_slice(&encodings);
    let track = synth::element(el::TRACKENTRY, &body);
    let cluster = synth::cluster(
        0,
        &[simple_block(1, 0, 0x80, &[0x5A; 16])],
        SegmentSize::Known,
    );
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &track,
        &[cluster],
        SegmentSize::Known,
    );
    let mut d = open(bytes).unwrap();
    let p = drain(&mut d);
    assert_eq!(p.len(), 1);
    // ffprobe reports size 18 for these bytes.
    assert_eq!(p[0].len, 18);
    assert_eq!(&p[0].payload()[..2], &[0xDE, 0xAD]);
}

#[test]
fn a_content_encoding_we_cannot_undo_drops_the_track_rather_than_the_file() {
    // ContentCompAlgo 1 is bzlib, which we do not implement.
    let mut comp = synth::uint(el::CONTENTCOMPALGO, 1);
    comp.extend_from_slice(&synth::element(el::CONTENTCOMPSETTINGS, &[]));
    let mut enc = synth::uint(el::CONTENTENCODINGORDER, 0);
    enc.extend_from_slice(&synth::element(el::CONTENTCOMPRESSION, &comp));
    let encodings = synth::element(
        el::CONTENTENCODINGS,
        &synth::element(el::CONTENTENCODING, &enc),
    );
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 1));
    body.extend_from_slice(&synth::string(el::CODECID, "V_VP8"));
    body.extend_from_slice(&encodings);
    let track = synth::element(el::TRACKENTRY, &body);
    let cluster = synth::cluster(
        0,
        &[simple_block(1, 0, 0x80, &[0u8; 16])],
        SegmentSize::Known,
    );
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &track,
        &[cluster],
        SegmentSize::Known,
    );
    let mut d = open(bytes).unwrap();
    assert_eq!(d.streams().len(), 1);
    assert!(drain(&mut d).is_empty());
}

// ---------------------------------------------------------------- robustness

#[test]
fn a_block_naming_an_undeclared_track_is_dropped() {
    let cluster = synth::cluster(
        0,
        &[
            simple_block(9, 0, 0x80, b"ghost"),
            simple_block(1, 0, 0x80, b"real"),
        ],
        SegmentSize::Known,
    );
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &audio_track(),
        &[cluster],
        SegmentSize::Known,
    );
    let mut d = open(bytes).unwrap();
    let p = drain(&mut d);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].payload(), b"real");
}

#[test]
fn every_truncation_of_a_valid_file_terminates() {
    let bytes = {
        let cluster = synth::cluster(
            0,
            &[simple_block(1, 0, 0x80, &[7u8; 40])],
            SegmentSize::Known,
        );
        synth::file(
            "matroska",
            &info(1_000_000),
            &audio_track(),
            &[cluster],
            SegmentSize::Known,
        )
    };
    for n in 0..bytes.len() {
        if let Ok(mut d) = open(bytes[..n].to_vec()) {
            let _ = drain(&mut d);
        }
    }
}

#[test]
fn a_deeply_nested_master_element_is_bounded_not_recursive() {
    // 20 000 nested `SimpleTag`s, the shape `vaco-format-isom` tests its box
    // parser with. `SimpleTag` is one of the schema's two recursive elements, so
    // this is legal EBML and must cost bounded stack.
    let mut inner = synth::string(el::TAGNAME, "X");
    for _ in 0..20_000 {
        inner = synth::element(el::SIMPLETAG, &inner);
    }
    let tag = synth::element(el::TAG, &inner);
    let mut segment = synth::element(el::INFO, &info(1_000_000));
    segment.extend_from_slice(&synth::element(el::TRACKS, &audio_track()));
    segment.extend_from_slice(&synth::element(el::TAGS, &tag));
    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));
    let d = open(bytes).unwrap();
    assert_eq!(d.streams().len(), 1);
}

#[test]
fn a_declared_size_larger_than_the_file_is_refused_before_allocating() {
    let mut segment = synth::element(el::INFO, &info(1_000_000));
    // A `Tags` element claiming 2^48 octets in a file of a few hundred.
    segment.extend_from_slice(&synth::id_bytes(el::TAGS));
    segment.extend_from_slice(&synth::vint(1u64 << 48, 8));
    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));
    // Either refused outright or opened with no streams; never a large
    // allocation and never a panic.
    if let Ok(mut d) = open(bytes) {
        assert!(d.streams().is_empty());
        let _ = drain(&mut d);
    }
}

#[test]
fn reading_past_the_end_keeps_returning_eof() {
    let mut d = open(laced_file(0, b"one")).unwrap();
    assert_eq!(drain(&mut d).len(), 1);
    for _ in 0..8 {
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
}

// ------------------------------------------------------------------- seeking

#[test]
fn seeking_without_cues_lands_on_the_first_cluster() {
    use vaco_core::Timestamp;
    use vaco_format_core::seek::{SeekFlags, SeekTarget};

    let mut d = open(bytes_of_four_clusters()).unwrap();
    assert_eq!(drain(&mut d).len(), 4);
    d.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(200),
        },
        SeekFlags::empty(),
    )
    .unwrap();
    let after = drain(&mut d);
    // Every packet read so far is a keyframe, so the demuxer's own index has an
    // entry per cluster and the seek lands on the one holding 200.
    assert_eq!(after[0].pts.ticks(), Some(200));
    assert_eq!(after.len(), 2);

    // With the index cleared — a first pass that never reached the target — a
    // cue-less seek restarts from the first cluster rather than failing.
    let mut fresh = open(bytes_of_four_clusters()).unwrap();
    fresh
        .seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::new(200),
            },
            SeekFlags::empty(),
        )
        .unwrap();
    assert_eq!(drain(&mut fresh).len(), 4);
}

#[test]
fn a_pipe_cannot_seek() {
    use vaco_core::Timestamp;
    use vaco_format_core::seek::{SeekFlags, SeekTarget};

    let mut d = open_pipe(laced_file(0, b"one")).unwrap();
    assert!(matches!(
        d.seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::ZERO
            },
            SeekFlags::empty()
        ),
        Err(Error::NotSeekable)
    ));
}

/// Four one-block clusters at 0, 100, 200 and 300 ticks.
fn bytes_of_four_clusters() -> Vec<u8> {
    let clusters: Vec<_> = (0..4u64)
        .map(|i| {
            synth::cluster(
                i * 100,
                &[simple_block(1, 0, 0x80, &[i as u8; 32])],
                SegmentSize::Known,
            )
        })
        .collect();
    synth::file(
        "matroska",
        &info(1_000_000),
        &audio_track(),
        &clusters,
        SegmentSize::Known,
    )
}

// ------------------------------------------------------ recovery, from a fuzz finding

/// A corrupt level-1 element size must not cost the file its packets.
///
/// Found by `matroska_demux`: two mutated octets in a `Void` element's size
/// VINT made it claim 21 GB. The scan skipped by that, landed at end of input,
/// and never recorded where the first `Cluster` was — so a linear read produced
/// nothing while a cue-driven seek produced everything. The reference reads the
/// same bytes and reports the same 22 packets it reports for the clean file,
/// logging "exceeds containing master element".
///
/// Two rules together fix it, and both are asserted here: an element whose end
/// runs past its `Segment` is refused rather than skipped, and when the scan
/// stops early the first cluster is recovered from `Cues`.
/// Fixed-width so a position's own encoding never shifts the positions.
fn u64_element(id: u32, value: u64) -> Vec<u8> {
    synth::element(id, &value.to_be_bytes())
}

#[test]
fn a_corrupt_element_size_before_the_clusters_still_yields_every_packet() {
    let cluster = synth::cluster(
        0,
        &[
            simple_block(1, 0, 0x80, b"one"),
            simple_block(1, 10, 0x00, b"two"),
        ],
        SegmentSize::Known,
    );
    let info_el = synth::element(el::INFO, &info(1_000_000));
    let tracks = synth::element(el::TRACKS, &audio_track());
    // A `Void` whose declared size runs far past the whole file.
    let mut void = synth::id_bytes(el::VOID);
    void.extend_from_slice(&synth::vint(1u64 << 40, 8));
    void.extend_from_slice(&[0u8; 16]);

    // Build the SeekHead with placeholder positions so its length is known;
    // every position is a fixed eight octets, so filling them in shifts nothing.
    let seek_head = |info_at: u64, tracks_at: u64, cues_at: u64| {
        let mut body = Vec::new();
        for (id, at) in [
            (el::INFO, info_at),
            (el::TRACKS, tracks_at),
            (el::CUES, cues_at),
        ] {
            let mut seek = synth::element(el::SEEKID, &synth::id_bytes(id));
            seek.extend_from_slice(&u64_element(el::SEEKPOSITION, at));
            body.extend_from_slice(&synth::element(el::SEEK, &seek));
        }
        synth::element(el::SEEKHEAD, &body)
    };
    let head_len = seek_head(0, 0, 0).len() as u64;
    let void_at = head_len;
    let info_at = void_at + void.len() as u64;
    let tracks_at = info_at + info_el.len() as u64;
    let cluster_at = tracks_at + tracks.len() as u64;
    let cues_at = cluster_at + cluster.len() as u64;

    let mut cue_positions = synth::uint(el::CUETRACK, 1);
    cue_positions.extend_from_slice(&u64_element(el::CUECLUSTERPOSITION, cluster_at));
    let mut cue_point = synth::uint(el::CUETIME, 0);
    cue_point.extend_from_slice(&synth::element(el::CUETRACKPOSITIONS, &cue_positions));
    let cues = synth::element(el::CUES, &synth::element(el::CUEPOINT, &cue_point));

    let mut segment = seek_head(info_at, tracks_at, cues_at);
    assert_eq!(segment.len() as u64, head_len);
    segment.extend_from_slice(&void);
    segment.extend_from_slice(&info_el);
    segment.extend_from_slice(&tracks);
    segment.extend_from_slice(&cluster);
    segment.extend_from_slice(&cues);

    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));

    let mut d = open(bytes).unwrap();
    assert_eq!(d.streams().len(), 1, "the SeekHead recovery found Tracks");
    let p = drain(&mut d);
    assert_eq!(p.len(), 2, "the first cluster was recovered from Cues");
    assert_eq!(p[0].payload(), b"one");
    assert_eq!(p[1].pts.ticks(), Some(10));
}

// ------------------------------------------------- start_time, through Discovery

/// `Stream::start_time` is `Discovery`'s to fill, and the rule is
/// `first_pts + initial_padding` — not the first pts.
///
/// This crate leaves `start_time` at `NONE` on purpose: the value needs packets,
/// and `read_header` must not consume any (a pipe cannot give them back, which
/// is the whole reason `Discovery` buffers and replays). The test exists because
/// the interaction is the only thing that makes either half correct, and neither
/// crate can assert it alone.
///
/// Numbers checked against `ffprobe 8.1` on a libopus-in-Matroska file: the
/// first audio packet has `pts=-7` (ms) and the track declares
/// `initial_padding=312` samples at 48 kHz, yet the reference reports
/// `start_pts=0`.
#[test]
fn discovery_turns_a_codec_delay_into_a_zero_start_time() {
    use vaco_format_core::discovery::Discovery;

    let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48000.0);
    audio.extend_from_slice(&synth::uint(el::CHANNELS, 1));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_OPUS"));
    // The delay libopus declares at 48 kHz.
    body.extend_from_slice(&synth::uint(el::CODECDELAY, 6_500_000));
    body.extend_from_slice(&synth::element(el::AUDIO, &audio));
    let track = synth::element(el::TRACKENTRY, &body);

    let clusters: Vec<_> = (0..4u64)
        .map(|i| {
            synth::cluster(
                i * 20,
                &[simple_block(1, 0, 0x80, &[0x11; 32])],
                SegmentSize::Known,
            )
        })
        .collect();
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &track,
        &clusters,
        SegmentSize::Known,
    );

    let mut d = open(bytes).unwrap();
    // The demuxer's own two halves of the contract.
    assert_eq!(
        d.streams()[0]
            .params
            .audio
            .as_ref()
            .unwrap()
            .initial_padding,
        312,
        "CodecDelay in samples"
    );
    assert!(
        d.streams()[0].start_time.is_none(),
        "the demuxer must not guess a start_time; it needs packets"
    );
    assert_eq!(
        d.read_packet().unwrap().pts.ticks(),
        Some(-7),
        "the first packet really is before zero"
    );

    // And the same file through the pass that is supposed to fill it in.
    let mut d = open(bytes_with_delay()).unwrap();
    assert!(d.read_packet().is_ok());
    let inner = open(bytes_with_delay()).unwrap();
    let mut disc = Discovery::new(inner, vaco_demux_matroska::FLAGS, &FormatOptions::default());
    disc.run(&NoParsers).unwrap();
    assert_eq!(
        disc.streams()[0].start_time.ticks(),
        Some(0),
        "first_pts(-7) + initial_padding(312 samples = 7 ticks) is 0"
    );
    assert_eq!(
        disc.report().start_time.map(vaco_core::Duration::as_micros),
        Some(0),
        "and the container start_time follows the streams"
    );
}

// ------------------------------------------- chapters, tags, attachments

/// Nested `ChapterAtom`s are dropped, not flattened.
///
/// Measured against `ffprobe 8.1 -show_chapters` on a hand-built file with one
/// top-level `ChapterAtom` (`ChapterUID` 1) holding two nested ones (`UID` 2
/// and 3) as *children*, plus a second top-level atom (`UID` 4): the reference
/// prints only chapters 1 and 4. The nested pair is silently ignored rather
/// than flattened into the list, so this crate's existing behaviour — reading
/// only `EditionEntry`'s direct children — already matches; this test pins it
/// against a regression toward flattening.
#[test]
fn nested_chapter_atoms_are_ignored_like_the_reference() {
    fn display(s: &str) -> Vec<u8> {
        synth::element(el::CHAPTERDISPLAY, &synth::string(el::CHAPSTRING, s))
    }
    fn atom(uid: u64, start: u64, end: u64, title: &str, children: &[u8]) -> Vec<u8> {
        let mut body = synth::uint(el::CHAPTERUID, uid);
        body.extend_from_slice(&synth::uint(el::CHAPTERTIMESTART, start));
        body.extend_from_slice(&synth::uint(el::CHAPTERTIMEEND, end));
        body.extend_from_slice(&display(title));
        body.extend_from_slice(children);
        synth::element(el::CHAPTERATOM, &body)
    }
    let leaf_a = atom(2, 0, 2_000_000_000, "Part 1a", &[]);
    let leaf_b = atom(3, 2_000_000_000, 5_000_000_000, "Part 1b", &[]);
    let mut nested_children = leaf_a;
    nested_children.extend_from_slice(&leaf_b);
    let atom_1 = atom(1, 0, 5_000_000_000, "Part 1", &nested_children);
    let atom_2 = atom(4, 5_000_000_000, 10_000_000_000, "Part 2", &[]);
    let mut edition_body = atom_1;
    edition_body.extend_from_slice(&atom_2);
    let edition = synth::element(el::EDITIONENTRY, &edition_body);
    let chapters = synth::element(el::CHAPTERS, &edition);

    let mut segment = synth::element(el::INFO, &info(1_000_000));
    segment.extend_from_slice(&synth::element(el::TRACKS, &audio_track()));
    segment.extend_from_slice(&chapters);
    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));

    let d = open(bytes).unwrap();
    assert_eq!(d.chapters().len(), 2, "the nested pair must not appear");
    assert_eq!(d.chapters()[0].id, 1);
    assert_eq!(d.chapters()[1].id, 4);
}

/// `Tags ▸ Targets ▸ TagChapterUID` and `TagAttachmentUID` reach the chapter
/// and the attachment stream they name, matching `ffprobe 8.1`: a chapter tag
/// merges into that chapter's `tags`, and an attachment tag merges into that
/// attachment's stream metadata, alongside `filename`/`mimetype`.
#[test]
fn target_scoped_tags_reach_the_chapter_and_the_attachment_they_name() {
    fn display(s: &str) -> Vec<u8> {
        synth::element(el::CHAPTERDISPLAY, &synth::string(el::CHAPSTRING, s))
    }
    fn simple_tag(name: &str, value: &str) -> Vec<u8> {
        let mut body = synth::string(el::TAGNAME, name);
        body.extend_from_slice(&synth::string(el::TAGSTRING, value));
        synth::element(el::SIMPLETAG, &body)
    }
    let mut atom_body = synth::uint(el::CHAPTERUID, 1);
    atom_body.extend_from_slice(&synth::uint(el::CHAPTERTIMESTART, 0));
    atom_body.extend_from_slice(&synth::uint(el::CHAPTERTIMEEND, 5_000_000_000));
    atom_body.extend_from_slice(&display("Part 1"));
    let atom = synth::element(el::CHAPTERATOM, &atom_body);
    let edition = synth::element(el::EDITIONENTRY, &atom);
    let chapters = synth::element(el::CHAPTERS, &edition);

    let mut file_body = synth::uint(el::FILEUID, 555);
    file_body.extend_from_slice(&synth::string(el::FILENAME, "cover.jpg"));
    file_body.extend_from_slice(&synth::string(el::FILEMEDIATYPE, "image/jpeg"));
    let attached = synth::element(el::ATTACHEDFILE, &file_body);
    let attachments = synth::element(el::ATTACHMENTS, &attached);

    let mut tag_chapter = synth::element(el::TARGETS, &synth::uint(el::TAGCHAPTERUID, 1));
    tag_chapter.extend_from_slice(&simple_tag("COMMENT", "chapter comment"));
    let mut tag_attachment = synth::element(el::TARGETS, &synth::uint(el::TAGATTACHMENTUID, 555));
    tag_attachment.extend_from_slice(&simple_tag("DESCRIPTION", "attachment desc"));
    let mut tags_body = synth::element(el::TAG, &tag_chapter);
    tags_body.extend_from_slice(&synth::element(el::TAG, &tag_attachment));
    let tags = synth::element(el::TAGS, &tags_body);

    let mut segment = synth::element(el::INFO, &info(1_000_000));
    segment.extend_from_slice(&synth::element(el::TRACKS, &audio_track()));
    // `Tags` is written *before* `Chapters`/`Attachments` here on purpose:
    // RFC 9559 does not order them, and resolving a target before the thing it
    // names exists would silently drop the tag on a file written this way.
    segment.extend_from_slice(&tags);
    segment.extend_from_slice(&chapters);
    segment.extend_from_slice(&attachments);
    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));

    let d = open(bytes).unwrap();
    assert_eq!(d.chapters().len(), 1);
    assert!(
        d.chapters()[0]
            .metadata
            .iter()
            .any(|(k, v)| k == "COMMENT" && v == "chapter comment")
    );
    let attachment = d
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(MediaType::Attachment))
        .unwrap();
    assert!(
        attachment
            .metadata
            .iter()
            .any(|(k, v)| k == "DESCRIPTION" && v == "attachment desc")
    );
    assert!(
        attachment
            .metadata
            .iter()
            .any(|(k, v)| k == "filename" && v == "cover.jpg")
    );
}

/// A `Targets` naming no UID at all — only a `TargetTypeValue` — is
/// indistinguishable from an untargeted tag. Measured against `ffprobe 8.1`:
/// both land in the container's own tags.
#[test]
fn a_target_type_value_with_no_uid_is_still_container_metadata() {
    let mut tag_body = synth::element(el::TARGETS, &synth::uint(el::TARGETTYPEVALUE, 50));
    let mut simple = synth::string(el::TAGNAME, "ALBUM_ONLY_TAG");
    simple.extend_from_slice(&synth::string(el::TAGSTRING, "hello"));
    tag_body.extend_from_slice(&synth::element(el::SIMPLETAG, &simple));
    let tags = synth::element(el::TAGS, &synth::element(el::TAG, &tag_body));

    let mut segment = synth::element(el::INFO, &info(1_000_000));
    segment.extend_from_slice(&synth::element(el::TRACKS, &audio_track()));
    segment.extend_from_slice(&tags);
    let mut bytes = synth::ebml_header("matroska");
    bytes.extend_from_slice(&synth::element(el::SEGMENT, &segment));

    let d = open(bytes).unwrap();
    assert!(
        d.metadata()
            .iter()
            .any(|(k, v)| k == "ALBUM_ONLY_TAG" && v == "hello")
    );
}

/// `CodecDelay`'s leading `SkipSamples` is re-armed on every seek.
///
/// **Measured** against `ffmpeg 8.1`: `ffmpeg -v debug -ss <target> -i
/// opus.webm -f null -` logs `demuxer injecting skip 312 / discard 0` — the
/// track's own `CodecDelay` sample count — after a seek to 0.0s and again
/// after a seek to 2.0s, unchanged even with the file's `SeekPreRoll` patched
/// to zero. So the skip is re-applied on every discontinuity, not computed
/// from `SeekPreRoll`, and not a one-time event at open.
#[test]
fn a_seek_rearms_the_codec_delay_skip_on_the_next_packet() {
    use vaco_core::Timestamp;
    use vaco_format_core::seek::{SeekFlags, SeekTarget};
    use vaco_packet::{PacketSideData, PacketSideDataKind};

    let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48000.0);
    audio.extend_from_slice(&synth::uint(el::CHANNELS, 1));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_OPUS"));
    body.extend_from_slice(&synth::uint(el::CODECDELAY, 6_500_000));
    body.extend_from_slice(&synth::element(el::AUDIO, &audio));
    let track = synth::element(el::TRACKENTRY, &body);
    let clusters: Vec<_> = (0..4u64)
        .map(|i| {
            synth::cluster(
                i * 20,
                &[simple_block(1, 0, 0x80, &[0x11; 8])],
                SegmentSize::Known,
            )
        })
        .collect();
    let bytes = synth::file(
        "matroska",
        &info(1_000_000),
        &track,
        &clusters,
        SegmentSize::Known,
    );

    let mut d = open(bytes.clone()).unwrap();
    let first = d.read_packet().unwrap();
    assert!(
        matches!(
            first.side_data(PacketSideDataKind::SkipSamples),
            Some(PacketSideData::SkipSamples {
                start: 312,
                end: 0,
                skip_reason: 0,
                discard_reason: 0,
            })
        ),
        "the very first packet since open carries the skip"
    );
    let second = d.read_packet().unwrap();
    assert!(
        second.side_data(PacketSideDataKind::SkipSamples).is_none(),
        "and the next one does not repeat it"
    );
    // Drain the rest so every cluster is indexed; the seek below needs an
    // entry at or after its target for a forward, index-driven search to find.
    while d.read_packet().is_ok() {}

    d.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(40),
        },
        SeekFlags::empty(),
    )
    .unwrap();
    let after_seek = d.read_packet().unwrap();
    assert!(
        matches!(
            after_seek.side_data(PacketSideDataKind::SkipSamples),
            Some(PacketSideData::SkipSamples {
                start: 312,
                end: 0,
                skip_reason: 0,
                discard_reason: 0,
            })
        ),
        "a seek is a discontinuity too, and the reference re-injects the same skip"
    );
}

/// The fixture `discovery_turns_a_codec_delay_into_a_zero_start_time` uses.
fn bytes_with_delay() -> Vec<u8> {
    let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48000.0);
    audio.extend_from_slice(&synth::uint(el::CHANNELS, 1));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_OPUS"));
    body.extend_from_slice(&synth::uint(el::CODECDELAY, 6_500_000));
    body.extend_from_slice(&synth::element(el::AUDIO, &audio));
    let track = synth::element(el::TRACKENTRY, &body);
    let clusters: Vec<_> = (0..4u64)
        .map(|i| {
            synth::cluster(
                i * 20,
                &[simple_block(1, 0, 0x80, &[0x11; 32])],
                SegmentSize::Known,
            )
        })
        .collect();
    synth::file(
        "matroska",
        &info(1_000_000),
        &track,
        &clusters,
        SegmentSize::Known,
    )
}
