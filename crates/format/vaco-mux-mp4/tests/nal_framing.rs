//! `avcC`/`hvcC` and the samples beside them must describe the same framing.
//!
//! Every H.264 file this crate wrote for an encoded stream was malformed for
//! months while every muxer test passed: the sample entry advertised
//! `is_avc=true, nal_length_size=4` and the `mdat` held Annex-B start codes.
//! A test that only checks a file was written, or that a demuxer can find the
//! samples again, cannot see that — so these assert the container's own bytes.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_format_core::Muxer;
use vaco_io::{MediaSink, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_mp4::MovMuxer;
use vaco_packet::Packet;

/// A real `libx264` High-profile SPS/PPS pair (the same bytes
/// `vaco-format-nalu`'s own `avcC` test measured against `ffmpeg 9.0.1`).
const SPS: [u8; 25] = [
    0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41, 0x41, 0xfb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10,
    0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x42, 0x99, 0x60,
];
const PPS: [u8; 6] = [0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];

fn annexb(units: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for u in units {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(u);
    }
    out
}

fn h264_params(extradata: Vec<u8>) -> CodecParameters {
    let mut p = CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(CodecId::H264),
        extradata: Some(extradata),
        ..CodecParameters::default()
    };
    p.video = Some(VideoParameters {
        width: 64,
        height: 48,
        frame_rate: Rational::new(25, 1),
        ..VideoParameters::default()
    });
    p
}

fn packet(stream: u32, dts: i64, payload: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut p = Packet::from_slice(&mut budget, payload).unwrap();
    p.stream_index = stream;
    p.dts = Timestamp::new(dts);
    p.pts = p.dts;
    p.flags |= vaco_packet::PacketFlags::KEY;
    p
}

/// The `avcC` payload, walked out of the file by finding the box tag and
/// reading the length word in front of it.
fn box_payload(file: &[u8], tag: [u8; 4]) -> Vec<u8> {
    let at = file
        .windows(4)
        .position(|w| w == tag)
        .unwrap_or_else(|| panic!("no {} box", String::from_utf8_lossy(&tag)));
    let len = u32::from_be_bytes(file[at - 4..at].try_into().unwrap()) as usize;
    file[at + 4..at - 4 + len].to_vec()
}

fn mdat_payload(file: &[u8]) -> Vec<u8> {
    let at = file.windows(4).position(|w| w == b"mdat").unwrap();
    let len = u32::from_be_bytes(file[at - 4..at].try_into().unwrap()) as usize;
    file[at + 4..at - 4 + len].to_vec()
}

fn mux_one(params: &CodecParameters, payload: &[u8]) -> Vec<u8> {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MovMuxer::new(Box::new(sink.clone()) as Box<dyn MediaSink>).unwrap();
    let idx = mux.add_stream(params).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(idx, 0, payload)).unwrap();
    mux.write_trailer().unwrap();
    sink.snapshot()
}

/// The bug this file exists for. An Annex-B stream — what every encoder in
/// this workspace produces, and what a copy from MPEG-TS or raw Annex B
/// carries — must come out as a real `avcC` beside length-prefixed samples,
/// not as start codes in both places.
#[test]
fn an_annexb_source_is_written_as_a_real_record_and_length_prefixed_samples() {
    let extradata = annexb(&[&SPS, &PPS]);
    let sample = annexb(&[&SPS, &PPS, &[0x65, 0x11, 0x22, 0x33]]);
    let file = mux_one(&h264_params(extradata), &sample);

    let avcc = box_payload(&file, *b"avcC");
    assert_eq!(
        avcc.first(),
        Some(&1),
        "avcC must open with configurationVersion = 1, not a start code"
    );
    assert_eq!(
        avcc,
        vaco_format_nalu::build_h264_avcc(&[&SPS], &[&PPS]).unwrap(),
        "and must be the record derived from this stream's own parameter sets"
    );
    assert_eq!(
        avcc.get(4).map(|b| (b & 3) + 1),
        Some(4),
        "lengthSizeMinusOne says four-byte prefixes"
    );

    // Every sample must now parse as `length, bytes` runs that land exactly
    // on the end of the payload — the check that fails on an Annex-B `mdat`,
    // where `00 00 00 01` reads as a one-byte NAL followed by nonsense.
    let mdat = mdat_payload(&file);
    let mut at = 0usize;
    let mut lengths = Vec::new();
    while at + 4 <= mdat.len() {
        let len = u32::from_be_bytes(mdat[at..at + 4].try_into().unwrap()) as usize;
        lengths.push(len);
        at += 4 + len;
    }
    assert_eq!(
        at,
        mdat.len(),
        "length prefixes must tile the sample exactly"
    );
    assert_eq!(lengths, vec![SPS.len(), PPS.len(), 4]);
}

/// The other direction must not regress: a stream that already carried a
/// real record has length-prefixed samples already, and neither may be
/// touched.
#[test]
fn a_record_shaped_source_is_passed_through_untouched() {
    let record = vaco_format_nalu::build_h264_avcc(&[&SPS], &[&PPS]).unwrap();
    let mut sample = Vec::new();
    sample.extend_from_slice(&4u32.to_be_bytes());
    sample.extend_from_slice(&[0x65, 0x11, 0x22, 0x33]);
    let file = mux_one(&h264_params(record.clone()), &sample);

    assert_eq!(box_payload(&file, *b"avcC"), record);
    assert_eq!(mdat_payload(&file), sample);
}
