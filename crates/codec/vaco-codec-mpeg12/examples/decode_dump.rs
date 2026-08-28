//! Decode a raw MPEG-1/2 elementary stream to raw `yuv420p`, for comparison
//! against `ffmpeg -f rawvideo -pix_fmt yuv420p -`.
//!
//! No CLI path exists yet to select this decoder by name from a demuxed
//! stream — this crate's own registration is new in the same change as
//! this example — so this is how its accuracy numbers in
//! `docs/codec/vaco-codec-mpeg12.md` were produced, the same pattern
//! `vaco-codec-mpegaudio`'s `examples/decode_dump.rs` uses.
//!
//! # Why this splits access units itself instead of using
//! `vaco-parse-mpegvideo`
//!
//! That crate's own packetiser hit a real bug while building this harness:
//! its end-of-stream flush (`Parser::parse(&[])`) reported a ~1 GiB
//! `max_alloc_total` budget exceeded on a five-kilobyte fixture. This crate
//! does not own that one, so this harness reimplements the same "picture,
//! sequence header or GOP header starts a new access unit" rule directly —
//! a dozen lines, using only `vaco-bitstream`'s start-code scanner this
//! crate already depends on — rather than block this crate's own
//! verification on someone else's bug. One packet per access unit also
//! matches how a real demuxer feeds a decoder, which is the shape this
//! decoder is written to expect even though it can tolerate more than one
//! picture per packet if handed one.
//!
//! Usage: `cargo run --example decode_dump -- input.m1v|m2v output.yuv`

use std::env;
use std::fs;
use std::io::Write;

use vaco_bitstream::annexb;
use vaco_codec_core::Decoder;
use vaco_codec_mpeg12::Mpeg12Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const SEQUENCE_HEADER: u8 = 0xB3;
const GROUP_START: u8 = 0xB8;
const PICTURE_START: u8 = 0x00;

/// Every offset in `data` where a new access unit begins: a
/// `picture_start_code`, or a `sequence_header_code`/`group_start_code`
/// immediately preceding one (they describe the picture that follows them,
/// not the one before — same rule `vaco-parse-mpegvideo`'s module docs
/// derive from a real `ffprobe -f mpegvideo` packet boundary).
fn access_unit_starts(data: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut pos = 0usize;
    let mut pending_header: Option<usize> = None;
    while let Some(sc) = annexb::find_start_code(data, pos) {
        let Some(&code) = data.get(sc + 3) else {
            break;
        };
        match code {
            PICTURE_START => {
                starts.push(pending_header.take().unwrap_or(sc));
            }
            SEQUENCE_HEADER | GROUP_START if pending_header.is_none() => {
                pending_header = Some(sc);
            }
            _ => {}
        }
        pos = sc + 4;
    }
    starts
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let (Some(input_path), Some(output_path)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: decode_dump <input.m1v|m2v> <output.yuv>");
        std::process::exit(2);
    };

    let Ok(data) = fs::read(input_path) else {
        eprintln!("could not read {input_path}");
        std::process::exit(1);
    };

    let limits = Limits::permissive();
    let mut budget = Budget::new(limits.clone());
    let mut decoder = Mpeg12Decoder::new(limits);
    let Ok(mut out) = fs::File::create(output_path) else {
        eprintln!("could not create {output_path}");
        std::process::exit(1);
    };

    let starts = access_unit_starts(&data);
    let mut frame_count = 0u64;
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(data.len());
        let Some(unit) = data.get(start..end) else {
            continue;
        };
        let Ok(packet) = Packet::from_slice(&mut budget, unit) else {
            eprintln!("could not build a packet for access unit {i}");
            continue;
        };
        loop {
            match decoder.send_packet(Some(&packet)) {
                Ok(()) => break,
                Err(vaco_core::Error::OutputPending) => {
                    drain(&mut decoder, &mut out, &mut frame_count);
                }
                Err(e) => {
                    eprintln!("send_packet error on access unit {i}: {e}");
                    break;
                }
            }
        }
        drain(&mut decoder, &mut out, &mut frame_count);
    }
    let _ = decoder.send_packet(None);
    drain(&mut decoder, &mut out, &mut frame_count);
    eprintln!(
        "decoded {frame_count} frame(s) from {} access unit(s), {} unsupported picture(s)",
        starts.len(),
        decoder.unsupported_pictures()
    );
}

fn drain(decoder: &mut Mpeg12Decoder, out: &mut fs::File, frame_count: &mut u64) {
    while let Ok(frame) = decoder.receive_frame() {
        write_yuv420p(out, &frame);
        *frame_count += 1;
    }
}

fn write_yuv420p(out: &mut fs::File, frame: &vaco_frame::Frame) {
    for plane_idx in 0..3 {
        let Some(plane) = frame.plane(plane_idx) else {
            continue;
        };
        for row in plane.rows_iter() {
            let _ = out.write_all(row);
        }
    }
}
