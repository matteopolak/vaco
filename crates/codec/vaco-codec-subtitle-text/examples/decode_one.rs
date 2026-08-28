//! Decode one subtitle packet payload to ASS dialogue text.
//!
//! The driver for this crate's differential comparison against the reference
//! binary (`tests/differential.sh`): reads one payload from a file, writes the
//! decoded ASS line to stdout, so a shell loop can diff it against
//! `ffmpeg -f ass -`'s own output for the same cue.
//!
//! Usage: `decode_one <subrip|ass|webvtt|mov_text|text|ttml> <payload-file>`

use std::io::Write as _;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(codec), Some(path)) = (args.next(), args.next()) else {
        eprintln!("usage: decode_one <codec> <payload-file>");
        return std::process::ExitCode::from(2);
    };
    let codec = match codec.as_str() {
        "subrip" | "srt" => vaco_codec_subtitle_text::TextCodec::SubRip,
        "ass" | "ssa" => vaco_codec_subtitle_text::TextCodec::Ass,
        "webvtt" | "vtt" => vaco_codec_subtitle_text::TextCodec::WebVtt,
        "mov_text" => vaco_codec_subtitle_text::TextCodec::MovText,
        "text" => vaco_codec_subtitle_text::TextCodec::Text,
        "ttml" => vaco_codec_subtitle_text::TextCodec::Ttml,
        other => {
            eprintln!("unknown codec `{other}`");
            return std::process::ExitCode::from(2);
        }
    };
    let Ok(payload) = std::fs::read(&path) else {
        eprintln!("cannot read {path}");
        return std::process::ExitCode::from(2);
    };
    if let Some(ass) = vaco_codec_subtitle_text::decode(codec, &payload) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(ass.as_bytes());
        let _ = out.write_all(b"\n");
    }
    std::process::ExitCode::SUCCESS
}
