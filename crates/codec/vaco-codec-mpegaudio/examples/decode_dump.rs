//! Decode an MPEG-1/2/2.5 audio elementary stream and dump interleaved
//! `s16le` PCM to stdout — the same format `ffmpeg -f s16le -` produces, so
//! the two can be diffed directly for conformance verification.
//!
//! Usage: `cargo run -p vaco-codec-mpegaudio --example decode_dump -- <file>`
//!
//! This bypasses `vaco-cli` entirely (there is currently no registry path
//! from the CLI to a leaf decoder, tracked separately) and drives the
//! demuxer and decoder crates directly, which is also how this crate's own
//! conformance measurements against `ffmpeg` were produced.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::disallowed_methods,
    reason = "throwaway CLI tool, not part of the crate's own budget-guarded decode path"
)]

use std::io::Write;

use vaco_codec_core::Decoder;
use vaco_format_core::Demuxer;
use vaco_frame::FrameData;
use vaco_io::MemorySource;
use vaco_limits::Limits;

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_dump <file>");
    let data = std::fs::read(&path).expect("read input file");
    let src = Box::new(MemorySource::new(data));
    let mut demuxer = vaco_demux_mpegaudio::MpegAudioDemuxer::open(
        src,
        &vaco_format_core::FormatOptions::default(),
    )
    .expect("open mpeg audio stream");

    let mut decoder = vaco_codec_mpegaudio::MpegAudioDecoder::new(Limits::permissive());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    loop {
        let packet = match demuxer.read_packet() {
            Ok(p) => Some(p),
            Err(vaco_core::Error::Eof) => None,
            Err(e) => {
                eprintln!("demux error: {e:?}");
                break;
            }
        };
        let at_eof = packet.is_none();
        if let Err(e) = decoder.send_packet(packet.as_ref()) {
            eprintln!("decode error: {e:?}");
            if at_eof {
                break;
            }
            continue;
        }
        loop {
            match decoder.receive_frame() {
                Ok(frame) => write_frame(&mut out, &frame),
                Err(vaco_core::Error::NeedMoreInput) => break,
                Err(e) => {
                    eprintln!("receive_frame error: {e:?}");
                    break;
                }
            }
        }
        if at_eof {
            break;
        }
    }
}

fn write_frame(out: &mut impl Write, frame: &vaco_frame::Frame) {
    let FrameData::Audio {
        samples, layout, ..
    } = &frame.data
    else {
        return;
    };
    let channels = layout.channels.max(1) as usize;
    let mut planes = Vec::with_capacity(channels);
    for ch in 0..channels {
        let Some(p) = frame.plane(ch) else { continue };
        planes.push(p);
    }
    let mut buf = Vec::with_capacity(*samples as usize * channels * 2);
    for i in 0..*samples as usize {
        for plane in &planes {
            let row = plane.as_slice();
            let off = i * 4;
            let Some(bytes) = row.get(off..off + 4) else {
                buf.extend_from_slice(&[0, 0]);
                continue;
            };
            let mut arr = [0u8; 4];
            arr.copy_from_slice(bytes);
            let f = f32::from_le_bytes(arr);
            let clamped = f.clamp(-1.0, 1.0);
            let s16 = (clamped * f32::from(i16::MAX)) as i16;
            buf.extend_from_slice(&s16.to_le_bytes());
        }
    }
    let _ = out.write_all(&buf);
}
