//! Decode a raw ADTS `.aac` file frame-by-frame and dump interleaved
//! `f32le` PCM to stdout — a throwaway verification tool, mirroring
//! `vaco-codec-mpegaudio`'s own `examples/decode_dump.rs`.
//!
//! Usage: `decode_dump <file.aac>`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::disallowed_methods,
    clippy::indexing_slicing,
    reason = "throwaway CLI tool, not part of the crate's own budget-guarded decode path"
)]

use std::io::Write;

use vaco_codec_aac::AacDecoder;
use vaco_codec_core::Decoder;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_aac::AdtsHeader;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: decode_dump <file.aac>");
    let data = std::fs::read(&path).expect("read input");
    let mut dec = AacDecoder::new(Limits::strict());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut pos = 0usize;
    while pos + 7 <= data.len() {
        if !AdtsHeader::looks_like_sync(&data[pos..]) {
            pos += 1;
            continue;
        }
        let Ok(header) = AdtsHeader::parse(&data[pos..]) else {
            pos += 1;
            continue;
        };
        let frame_len = usize::from(header.frame_length);
        if frame_len < header.header_len() || pos + frame_len > data.len() {
            pos += 1;
            continue;
        }
        let frame_bytes = &data[pos..pos + frame_len];
        let mut budget = Budget::new(Limits::strict());
        if let Ok(packet) = Packet::from_slice(&mut budget, frame_bytes) {
            match dec.send_packet(Some(&packet)) {
                Ok(()) => {
                    while let Ok(frame) = dec.receive_frame() {
                        if let FrameData::Audio {
                            samples, planes, ..
                        } = &frame.data
                        {
                            let channels = planes.len();
                            let mut interleaved = vec![0.0f32; *samples as usize * channels];
                            for ch in 0..channels {
                                let Some(plane) = frame.plane(ch) else {
                                    continue;
                                };
                                let Some(row) = plane.row(0) else { continue };
                                for (i, chunk) in row.chunks_exact(4).enumerate() {
                                    if let Some(v) = interleaved.get_mut(i * channels + ch) {
                                        *v = f32::from_le_bytes(chunk.try_into().unwrap_or([0; 4]));
                                    }
                                }
                            }
                            let bytes: Vec<u8> =
                                interleaved.iter().flat_map(|v| v.to_le_bytes()).collect();
                            let _ = out.write_all(&bytes);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("decode error at frame offset {pos}: {e}");
                }
            }
        }
        pos += frame_len;
    }
}
