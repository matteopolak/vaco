//! Decode a raw H.261 or (baseline) H.263 elementary stream to raw
//! `yuv420p`, for comparison against `ffmpeg -f rawvideo -pix_fmt
//! yuv420p -`. Same purpose and pattern as `vaco-codec-mpeg12`'s own
//! `examples/decode_dump.rs`.
//!
//! Unlike that harness, this one does not split the input into one packet
//! per access unit first: both `H261Decoder::send_packet` and
//! `H263Decoder::send_packet` already walk every start code inside
//! whatever byte range they are handed (`Caps::SUBFRAMES` is set on both
//! for exactly this reason), so handing over the entire file as a single
//! packet decodes every picture in it just as well — `pts`/`duration`
//! only matter for a real playback pipeline, not for comparing raw pixels.
//!
//! Usage: `cargo run --example decode_dump -- h261|h263 input output.yuv`

use std::env;
use std::fs;
use std::io::Write;

use vaco_codec_core::Decoder;
use vaco_codec_h263::{H261Decoder, H263Decoder};
use vaco_core::Error;
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn main() {
    let args: Vec<String> = env::args().collect();
    let (Some(codec), Some(input_path), Some(output_path)) =
        (args.get(1), args.get(2), args.get(3))
    else {
        eprintln!("usage: decode_dump <h261|h263> <input> <output.yuv>");
        std::process::exit(2);
    };

    let Ok(data) = fs::read(input_path) else {
        eprintln!("could not read {input_path}");
        std::process::exit(1);
    };
    let Ok(mut out) = fs::File::create(output_path) else {
        eprintln!("could not create {output_path}");
        std::process::exit(1);
    };

    let limits = Limits::permissive();
    let mut budget = Budget::new(limits.clone());
    let Ok(packet) = Packet::from_slice(&mut budget, &data) else {
        eprintln!("could not build a packet from {input_path}");
        std::process::exit(1);
    };

    let mut decoder: Box<dyn Decoder> = match codec.as_str() {
        "h261" => Box::new(H261Decoder::new(limits)),
        "h263" => Box::new(H263Decoder::new(limits)),
        other => {
            eprintln!("unknown codec {other:?}: expected h261 or h263");
            std::process::exit(2);
        }
    };

    let mut frame_count = 0u64;
    loop {
        match decoder.send_packet(Some(&packet)) {
            Ok(()) => break,
            Err(Error::OutputPending) => drain(&mut *decoder, &mut out, &mut frame_count),
            Err(e) => {
                eprintln!("send_packet error: {e}");
                break;
            }
        }
    }
    drain(&mut *decoder, &mut out, &mut frame_count);
    let _ = decoder.send_packet(None);
    drain(&mut *decoder, &mut out, &mut frame_count);
    eprintln!("decoded {frame_count} frame(s)");
}

fn drain(decoder: &mut dyn Decoder, out: &mut fs::File, frame_count: &mut u64) {
    while let Ok(frame) = decoder.receive_frame() {
        write_yuv420p(out, &frame);
        *frame_count += 1;
    }
}

fn write_yuv420p(out: &mut fs::File, frame: &Frame) {
    for plane_idx in 0..3 {
        let Some(plane) = frame.plane(plane_idx) else {
            continue;
        };
        for row in plane.rows_iter() {
            let _ = out.write_all(row);
        }
    }
}
