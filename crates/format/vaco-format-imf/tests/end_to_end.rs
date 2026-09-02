//! End-to-end: a real OP-Atom MXF track file (built with this workspace's
//! own `vaco-mux-mxf::MUXER_OPATOM`, already measured against `ffmpeg` in
//! that crate's own test suite) plus hand-built CPL/ASSETMAP XML, opened
//! through the full [`vaco_format_imf::ImfDemuxer`] `open` + `bind_url`
//! path exactly the way `vaco-cli`'s own input resolution calls it.
//!
//! No IMF reference implementation exists on this machine (`ffmpeg 8.1`
//! here has no `imf` demuxer — confirmed via `ffmpeg -demuxers`), so this
//! is a self-consistency check, not a byte-for-byte comparison against a
//! measured reference: it proves the CPL's own edit-decision-list
//! (`EntryPoint`/`SourceDuration`/`RepeatCount`, across two `Segment`s of
//! the *same* virtual track) is honoured when reading real clip-wrapped
//! essence back, using the exact frame values placed at each edit-unit
//! index as the check.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    reason = "test code"
)]

use std::fs;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_format_core::{Demuxer, Muxer, discovery::NoParsers};
use vaco_format_imf::ImfDemuxer;
use vaco_io::{MemorySource, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_mxf::MUXER_OPATOM;
use vaco_packet::{Packet, PacketFlags};

fn video_params() -> CodecParameters {
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::Mpeg2video);
    if let Some(v) = p.video.as_mut() {
        v.width = 16;
        v.height = 16;
        v.frame_rate = Rational::new(25, 1);
    }
    p
}

fn packet(pts: i64, byte: u8) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, &[byte; 8]).unwrap();
    pkt.stream_index = 0;
    pkt.pts = Timestamp::new(pts);
    pkt.dts = Timestamp::new(pts);
    if pts == 0 {
        pkt.flags |= PacketFlags::KEY;
    }
    pkt
}

/// Build a 6-frame OP-Atom MXF file whose frame `i`'s payload is 8 bytes of
/// value `i` — a marker that survives clip-wrapped read-back unmodified,
/// making a wrong edit-unit index immediately visible.
fn build_track_file() -> Vec<u8> {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = (MUXER_OPATOM.open)(Box::new(sink.clone())).unwrap();
    mux.add_stream(&video_params()).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    for i in 0..6i64 {
        mux.write_packet(&packet(i, i as u8)).unwrap();
    }
    mux.write_trailer().unwrap();
    sink.snapshot()
}

const CPL_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CompositionPlaylist xmlns="http://www.smpte-ra.org/schemas/2067-3/2016">
  <Id>urn:uuid:11111111-1111-1111-1111-111111111111</Id>
  <ContentTitleText>End To End Test</ContentTitleText>
  <EditRate>25 1</EditRate>
  <SegmentList>
    <Segment>
      <Id>urn:uuid:22222222-2222-2222-2222-222222222222</Id>
      <SequenceList>
        <MainImageSequence>
          <Id>urn:uuid:33333333-3333-3333-3333-333333333333</Id>
          <TrackId>urn:uuid:44444444-4444-4444-4444-444444444444</TrackId>
          <ResourceList>
            <Resource>
              <Id>urn:uuid:55555555-5555-5555-5555-555555555555</Id>
              <TrackFileId>urn:uuid:cccccccc-2222-2222-2222-222222222222</TrackFileId>
              <IntrinsicDuration>6</IntrinsicDuration>
              <EntryPoint>1</EntryPoint>
              <SourceDuration>2</SourceDuration>
            </Resource>
          </ResourceList>
        </MainImageSequence>
      </SequenceList>
    </Segment>
    <Segment>
      <Id>urn:uuid:66666666-6666-6666-6666-666666666666</Id>
      <SequenceList>
        <MainImageSequence>
          <Id>urn:uuid:77777777-7777-7777-7777-777777777777</Id>
          <TrackId>urn:uuid:44444444-4444-4444-4444-444444444444</TrackId>
          <ResourceList>
            <Resource>
              <Id>urn:uuid:88888888-8888-8888-8888-888888888888</Id>
              <TrackFileId>urn:uuid:cccccccc-2222-2222-2222-222222222222</TrackFileId>
              <IntrinsicDuration>6</IntrinsicDuration>
              <EntryPoint>4</EntryPoint>
              <SourceDuration>2</SourceDuration>
              <RepeatCount>2</RepeatCount>
            </Resource>
          </ResourceList>
        </MainImageSequence>
      </SequenceList>
    </Segment>
  </SegmentList>
</CompositionPlaylist>"#;

const ASSETMAP_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<AssetMap xmlns="http://www.smpte-ra.org/schemas/429-9/2007/AM">
  <Id>urn:uuid:aaaaaaaa-0000-0000-0000-000000000000</Id>
  <AssetList>
    <Asset>
      <Id>urn:uuid:cccccccc-2222-2222-2222-222222222222</Id>
      <ChunkList>
        <Chunk><Path>video_track.mxf</Path></Chunk>
      </ChunkList>
    </Asset>
  </AssetList>
</AssetMap>"#;

#[test]
fn a_composition_with_repeated_and_offset_resources_stitches_the_right_frames() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("video_track.mxf"), build_track_file()).unwrap();
    fs::write(dir.path().join("ASSETMAP.xml"), ASSETMAP_XML).unwrap();
    let cpl_path = dir.path().join("CPL_test.xml");
    fs::write(&cpl_path, CPL_XML).unwrap();

    let src = Box::new(MemorySource::new(CPL_XML.as_bytes().to_vec()));
    let mut demux = ImfDemuxer::open(src, &NoParsers).unwrap();
    // Before `bind_url`, real codec parameters are not yet known -- see
    // `demux.rs`'s own module docs for why this is not a bug.
    assert_eq!(demux.streams().len(), 1);
    assert_eq!(demux.streams()[0].media_type(), Some(MediaType::Video));

    demux.bind_url(cpl_path.to_str().unwrap()).unwrap();
    assert_eq!(
        demux.streams()[0].params.codec_id,
        Some(CodecId::Mpeg2video),
        "bind_url should have filled in real codec parameters from the OP-Atom essence"
    );

    // EntryPoint=1,SourceDuration=2 (frames 1,2) then
    // EntryPoint=4,SourceDuration=2,RepeatCount=2 (frames 4,5,4,5).
    let expected = [1u8, 2, 4, 5, 4, 5];
    for (n, &want) in expected.iter().enumerate() {
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.stream_index, 0);
        assert_eq!(
            pkt.pts,
            Timestamp::new(n as i64),
            "composition-timeline pts for edit unit {n}"
        );
        assert_eq!(
            pkt.payload(),
            &[want; 8],
            "edit unit {n} did not stitch the expected frame"
        );
    }
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn bind_url_refuses_a_remote_looking_cpl_path() {
    let src = Box::new(MemorySource::new(CPL_XML.as_bytes().to_vec()));
    let mut demux = ImfDemuxer::open(src, &NoParsers).unwrap();
    let err = demux
        .bind_url("https://example.com/CPL_test.xml")
        .unwrap_err();
    assert!(matches!(err, vaco_core::Error::Unsupported(_)));
}
