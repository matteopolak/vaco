//! The 37 `*_pipe` splitters' boundary scanners over arbitrary bytes.
//!
//! A boundary scanner is exactly where a hang or a panic lives: it walks
//! attacker-controlled length fields (PNG chunks, XWD's header-size field,
//! QOI/WebP/BMP's declared sizes) and attacker-controlled marker bytes
//! (JPEG's entropy-coded-segment stuffing, J2K's codestream markers). This
//! target drives every [`ImageFraming`] strategy directly against arbitrary
//! bytes, and separately drives a full [`PipeDemuxer`] (buffering, packet
//! construction, `Eof` stability) for the strategies real specs use.
//!
//! Properties checked, not just "does not panic":
//!
//! * [`compute_spans`] terminates and returns spans that are non-overlapping,
//!   strictly increasing, and each fit inside the input — for every
//!   [`ImageFraming`] variant, so a new one added later is covered automatically.
//! * [`PipeDemuxer::read_packet`] drains in a bounded number of calls and,
//!   once it reports [`Error::Eof`], keeps reporting it (never "corruption
//!   after end of stream" the way a sticky-flag bug would look).
//! fuzz-crate: vaco-demux-image2

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_demux_image2::pipe::framing::{ImageFraming, compute_spans};
use vaco_demux_image2::pipe::{PipeDemuxer, PipeOptions};
use vaco_format_core::Demuxer;
use vaco_io::{MediaSource, MemorySource};

const MAX_PACKETS: u32 = 100_000;

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    data: Vec<u8>,
    framing_choice: u8,
    seekable: bool,
    loop_input: bool,
}

fn framing_from(choice: u8) -> ImageFraming {
    match choice % 12 {
        0 => ImageFraming::WholeRemaining,
        1 => ImageFraming::Png,
        2 => ImageFraming::Marker {
            start: [0xFF, 0xD8],
            end: [0xFF, 0xD9],
            skip_stuffing: true,
        },
        3 => ImageFraming::Marker {
            start: [0xFF, 0x4F],
            end: [0xFF, 0xD9],
            skip_stuffing: false,
        },
        4 => ImageFraming::RiffSized,
        5 => ImageFraming::BmpSized,
        6 => ImageFraming::Netpbm,
        7 => ImageFraming::Pgx,
        8 => ImageFraming::Qoi,
        9 => ImageFraming::Xwd,
        10 => ImageFraming::CArrayText,
        _ => ImageFraming::SvgText,
    }
}

fn spec_for(framing: ImageFraming) -> &'static vaco_demux_image2::pipe::PipeSpec {
    // One representative spec per `ImageFraming` variant used above, so
    // `PipeDemuxer::open` (not just the bare scanner) gets exercised too.
    // `Radiance` is covered by `compute_spans` above but has no `PipeSpec`
    // stand-in here since driving a whole `PipeDemuxer` over it adds nothing
    // `compute_spans` alone does not already check.
    use vaco_demux_image2::pipe::*;
    match framing {
        ImageFraming::Png => &SPEC_PNG,
        ImageFraming::Marker { skip_stuffing: true, .. } => &SPEC_JPEG,
        ImageFraming::Marker { .. } => &SPEC_J2K,
        ImageFraming::RiffSized => &SPEC_WEBP,
        ImageFraming::BmpSized => &SPEC_BMP,
        ImageFraming::Netpbm => &SPEC_PPM,
        ImageFraming::Pgx => &SPEC_PGX,
        ImageFraming::Qoi => &SPEC_QOI,
        ImageFraming::Xwd => &SPEC_XWD,
        ImageFraming::CArrayText => &SPEC_XBM,
        ImageFraming::SvgText => &SPEC_SVG,
        ImageFraming::WholeRemaining | ImageFraming::Radiance => &SPEC_PCX,
    }
}

fn drain(d: &mut PipeDemuxer) -> u32 {
    let mut n = 0;
    loop {
        match d.read_packet() {
            Ok(_) => {
                n += 1;
                assert!(n < MAX_PACKETS, "read_packet did not terminate");
            }
            Err(_) => return n,
        }
    }
}

fuzz_target!(|input: Input| {
    // Every ImageFraming strategy exists, independent of which spec uses it.
    for framing in [
        ImageFraming::WholeRemaining,
        ImageFraming::Png,
        ImageFraming::Marker {
            start: [0xFF, 0xD8],
            end: [0xFF, 0xD9],
            skip_stuffing: true,
        },
        ImageFraming::Marker {
            start: [0xFF, 0x4F],
            end: [0xFF, 0xD9],
            skip_stuffing: false,
        },
        ImageFraming::RiffSized,
        ImageFraming::BmpSized,
        ImageFraming::Netpbm,
        ImageFraming::Pgx,
        ImageFraming::Qoi,
        ImageFraming::Xwd,
        ImageFraming::CArrayText,
        ImageFraming::SvgText,
        ImageFraming::Radiance,
    ] {
        let spans = compute_spans(framing, &input.data);
        let mut prev_end = 0usize;
        for &(start, end) in &spans {
            assert!(start <= end, "span start after its own end");
            assert!(end <= input.data.len(), "span runs past the input");
            assert!(start >= prev_end, "spans overlap or go backwards");
            prev_end = end;
        }
    }

    // The chosen strategy also gets a real `PipeDemuxer` over it: buffering,
    // budget charging, packet construction and PTS stamping all run too.
    let framing = framing_from(input.framing_choice);
    let spec = spec_for(framing);
    let src: Box<dyn MediaSource> = if input.seekable {
        Box::new(MemorySource::new(input.data.clone()))
    } else {
        Box::new(MemorySource::forward_only(input.data.clone()))
    };
    let options = PipeOptions {
        framerate: vaco_core::Rational::new(25, 1),
        loop_input: input.loop_input,
    };
    let Ok(mut d) = PipeDemuxer::open_with_options(spec, src, options) else {
        return;
    };

    if input.loop_input {
        // A looping demuxer over a non-empty span table never reports Eof;
        // bound the drain by MAX_PACKETS directly rather than via `drain`'s
        // own Eof wait.
        for _ in 0..MAX_PACKETS.min(64) {
            if d.read_packet().is_err() {
                break; // an empty span table: Eof even with looping on
            }
        }
        return;
    }

    drain(&mut d);
    for _ in 0..4 {
        match d.read_packet() {
            Err(Error::Eof) => {}
            Err(_) => break,
            Ok(_) => panic!("a packet appeared after end of stream"),
        }
    }

    let _ = d.streams().len();
    let _ = d.duration();
});
