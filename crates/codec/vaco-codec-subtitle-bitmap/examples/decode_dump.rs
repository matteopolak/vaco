//! Decode a hand-built fixture for one of the three formats and print each
//! resulting rect's geometry and raw palette-index pixels as plain text, one
//! rect per `RECT`/index-line pair — for diffing against a reference
//! decoder's own output (see `tests/fixtures/compare.py`).
//!
//! Usage:
//! - `cargo run -p vaco-codec-subtitle-bitmap --example decode_dump -- dvb <file>`
//! - `cargo run -p vaco-codec-subtitle-bitmap --example decode_dump -- pgs <file>`
//! - `cargo run -p vaco-codec-subtitle-bitmap --example decode_dump -- vobsub <spu-file> <palette-hex-csv>`
//!   (`<palette-hex-csv>` is up to 16 `rrggbb` triples, comma-separated, the
//!   same grammar `vaco-subtitle-bitmap::vobsub::idx::parse_palette` reads.)
//!
//! This bypasses any registry entirely — there is none for these formats
//! (see the crate's top-level doc comment) — and drives each decoder
//! directly, which is also how this crate's own fixtures were verified.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::disallowed_methods,
    clippy::panic,
    reason = "throwaway CLI tool, not part of the crate's own budget-guarded decode path"
)]

use vaco_codec_subtitle_bitmap::{dvb, pgs::PgsDecoder, vobsub};
use vaco_format_subtitle_bitmap::{IndexedBitmap, Palette, Rgba};
use vaco_limits::Limits;

fn print_rect(rect: &IndexedBitmap) {
    let r = rect.rect();
    println!("RECT {} {} {} {}", r.x, r.y, r.width, r.height);
    let line: Vec<String> = rect.indices().iter().map(u8::to_string).collect();
    println!("{}", line.join(" "));
}

fn parse_palette_csv(s: &str) -> Palette {
    let mut entries = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.len() != 6 {
            continue;
        }
        let r = u8::from_str_radix(&part[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&part[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&part[4..6], 16).unwrap_or(0);
        entries.push(Rgba::new(r, g, b, 0xFF));
    }
    Palette::new(entries).expect("palette")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).expect("usage: decode_dump <dvb|pgs|vobsub> ...");
    let limits = Limits::permissive();

    match mode.as_str() {
        "dvb" => {
            let path = args.get(2).expect("usage: decode_dump dvb <file>");
            let data = std::fs::read(path).expect("read fixture");
            let event = dvb::decode_display_set(&data, &limits).expect("decode");
            println!("EVENTS 1");
            for rect in &event.rects {
                print_rect(rect);
            }
        }
        "pgs" => {
            let path = args.get(2).expect("usage: decode_dump pgs <file>");
            let data = std::fs::read(path).expect("read fixture");
            let mut dec = PgsDecoder::new();
            let mut events = Vec::new();
            let mut pos = 0usize;
            while let Some(header) =
                vaco_subtitle_bitmap::sup::parse_header(data.get(pos..).unwrap_or(&[]))
            {
                let total = vaco_subtitle_bitmap::sup::HEADER_LEN + usize::from(header.size);
                let Some(record) = data.get(pos..pos.saturating_add(total)) else {
                    break;
                };
                if let Some(event) = dec.push_segment(record, &limits).expect("decode segment") {
                    events.push(event);
                }
                pos = pos.saturating_add(total);
            }
            println!("EVENTS {}", events.len());
            for event in &events {
                println!("FORCED {}", event.forced);
                for rect in &event.rects {
                    print_rect(rect);
                }
            }
        }
        "vobsub" => {
            let path = args.get(2).expect("usage: decode_dump vobsub <file> <palette>");
            let palette_csv = args.get(3).expect("usage: decode_dump vobsub <file> <palette>");
            let data = std::fs::read(path).expect("read fixture");
            let palette = parse_palette_csv(palette_csv);
            let event = vobsub::decode_spu(&data, &palette, &limits).expect("decode");
            println!("EVENTS 1");
            println!("FORCED {}", event.forced);
            for rect in &event.rects {
                print_rect(rect);
            }
        }
        other => panic!("unknown mode {other}"),
    }
}
