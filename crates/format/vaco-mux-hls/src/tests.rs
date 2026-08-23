//! Unit tests over `HlsMuxer`'s pure rendering logic, using private-field
//! access (this module is `mod tests` inside `lib.rs`) so playlist text can
//! be checked without any real I/O.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use super::*;

fn bare_muxer(opts: HlsMuxOptions) -> HlsMuxer {
    HlsMuxer::new(
        "out/stream.m3u8".to_owned(),
        None,
        Box::new(vaco_format_adaptive::NoSegmentMuxers),
        opts,
    )
}

fn seg(uri: &str, secs: f64) -> WrittenSegment {
    WrittenSegment {
        uri: uri.to_owned(),
        duration: Duration::from_micros((secs * 1_000_000.0) as i64),
        byte_range: None,
        program_date_time: None,
    }
}

#[test]
fn renders_a_minimal_vod_playlist() {
    let mut m = bare_muxer(HlsMuxOptions {
        hls_playlist_type: HlsPlaylistType::Vod,
        ..HlsMuxOptions::default()
    });
    m.written.push_back(seg("stream0.ts", 2.0));
    m.written.push_back(seg("stream1.ts", 2.0));
    m.trailer_written = true;
    let text = m.render_media_playlist();
    assert!(text.starts_with("#EXTM3U\n"));
    assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"));
    assert!(text.contains("#EXT-X-TARGETDURATION:2\n"));
    assert!(text.contains("#EXTINF:2.000,\nstream0.ts\n"));
    assert!(text.contains("#EXTINF:2.000,\nstream1.ts\n"));
    assert!(text.ends_with("#EXT-X-ENDLIST\n"));
}

#[test]
fn byte_ranges_bump_the_version_and_are_written_before_extinf() {
    let mut m = bare_muxer(HlsMuxOptions::default());
    m.written.push_back(WrittenSegment {
        byte_range: Some(ByteRange {
            offset: 0,
            length: 1000,
        }),
        ..seg("stream.ts", 2.0)
    });
    let text = m.render_media_playlist();
    assert!(text.contains("#EXT-X-VERSION:4\n"));
    let br = text.find("#EXT-X-BYTERANGE:1000@0").unwrap();
    let extinf = text.find("#EXTINF:2.000,").unwrap();
    assert!(br < extinf, "BYTERANGE must precede its EXTINF");
}

#[test]
fn fmp4_segments_get_an_ext_x_map_and_version_7() {
    let mut m = bare_muxer(HlsMuxOptions {
        hls_segment_type: HlsSegmentType::Fmp4,
        ..HlsMuxOptions::default()
    });
    m.written.push_back(seg("stream0.m4s", 2.0));
    let text = m.render_media_playlist();
    assert!(text.contains("#EXT-X-VERSION:7\n"));
    assert!(text.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
}

#[test]
fn independent_segments_flag_is_honoured() {
    let mut m = bare_muxer(HlsMuxOptions {
        hls_flags: HlsFlags::INDEPENDENT_SEGMENTS,
        ..HlsMuxOptions::default()
    });
    m.written.push_back(seg("s.ts", 2.0));
    assert!(
        m.render_media_playlist()
            .contains("#EXT-X-INDEPENDENT-SEGMENTS\n")
    );
}

#[test]
fn trim_window_advances_the_media_sequence() {
    let mut m = bare_muxer(HlsMuxOptions {
        hls_list_size: 2,
        ..HlsMuxOptions::default()
    });
    for i in 0..5u64 {
        m.written.push_back(seg(&format!("s{i}.ts"), 2.0));
        m.trim_window();
    }
    assert_eq!(m.written.len(), 2);
    assert_eq!(m.media_sequence_base, 3);
    assert_eq!(m.written.front().unwrap().uri, "s3.ts");
}

#[test]
fn hls_list_size_zero_keeps_everything() {
    let mut m = bare_muxer(HlsMuxOptions {
        hls_list_size: 0,
        ..HlsMuxOptions::default()
    });
    for i in 0..10u64 {
        m.written.push_back(seg(&format!("s{i}.ts"), 2.0));
        m.trim_window();
    }
    assert_eq!(m.written.len(), 10);
    assert_eq!(m.media_sequence_base, 0);
}

#[test]
fn master_playlist_names_the_media_playlist() {
    let m = bare_muxer(HlsMuxOptions::default());
    let text = m.render_master_playlist("stream.m3u8");
    assert!(text.starts_with("#EXTM3U\n"));
    assert!(text.contains("#EXT-X-STREAM-INF:BANDWIDTH="));
    assert!(text.trim_end().ends_with("stream.m3u8"));
}

#[test]
fn add_stream_after_the_header_is_refused() {
    let mut m = bare_muxer(HlsMuxOptions::default());
    m.header_written = true;
    let err = m
        .add_stream(&CodecParameters::new(vaco_core::MediaType::Video))
        .unwrap_err();
    assert!(matches!(err, Error::InvalidData(_)));
}

#[test]
fn write_header_needs_at_least_one_stream() {
    let mut m = bare_muxer(HlsMuxOptions::default());
    let err = m.write_header().unwrap_err();
    assert!(matches!(err, Error::InvalidData(_)));
}
