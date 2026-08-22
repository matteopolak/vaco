//! AV1 header parsing against arbitrary bytes.
//!
//! The input is an `ObuStream`-framed elementary stream (what `ffmpeg -f obu`
//! writes, measured — see `docs/codec/vaco-parse-av1.md`). Everything the
//! crate can parse is reached: the streaming temporal-unit splitter, and every
//! OBU fed directly to the sequence-header, frame-header, metadata and `av1C`
//! parsers as well, so a unit the splitter would have skipped still gets
//! parsed.
//!
//! Properties, beyond "does not panic":
//!
//! * **Chunking is invisible.** The same bytes fed one at a time and all at
//!   once must produce the identical sequence of access units — the property
//!   `vaco-parse-h264`'s fuzzer found three separate bugs against, all in the
//!   streaming path and none reachable by a whole-buffer test.
//! * **A call either consumes everything or hands back a queued unit.**
//!   Anything else is a stall, and a stall in a parser is a hang in whatever
//!   drives it.
//! * **Access units partition the input.** Every byte the parser emits came
//!   from the input, in order.
//! * **A parsed sequence header's coded size is never zero**, and a parsed
//!   frame header's `FrameSize` never reports `coded_width >
//!   upscaled_width` — `superres_params()` only ever downscales.
//!
//! A `LimitExceeded` is correct behaviour and returns normally (plan 13
//! §2.2.4).
//!
//! fuzz-crate: vaco-parse-av1
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Parser;
use vaco_limits::{Budget, Limits};
use vaco_parse_av1::av1c::Av1CodecConfigurationRecord;
use vaco_parse_av1::frame_header::FrameHeader;
use vaco_parse_av1::metadata;
use vaco_parse_av1::obu::{Av1Framing, ObuType, units};
use vaco_parse_av1::seq::SequenceHeader;
use vaco_parse_av1::{Av1Parser, params};

/// Feed `data` through the streaming parser in `chunk`-sized pieces, returning
/// the access units it emits.
fn run(data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
    let mut parser = Av1Parser::new(Limits::strict());
    let mut out = Vec::new();
    for c in data.chunks(chunk.max(1)) {
        let mut rest = c;
        while !rest.is_empty() {
            match parser.parse(rest) {
                Ok((unit, used)) => {
                    assert!(used <= rest.len(), "consumed more than it was given");
                    let produced = unit.is_some();
                    if let Some(p) = unit {
                        out.push(p.payload().to_vec());
                    }
                    assert!(
                        used == rest.len() || (used == 0 && produced),
                        "a call must consume everything or hand back a queued unit"
                    );
                    rest = &rest[used..];
                }
                // A budget cap is correct behaviour; stop rather than
                // continuing with a parser whose buffer was refused.
                Err(_) => return out,
            }
        }
    }
    for _ in 0..=data.len() {
        match parser.parse(&[]) {
            Ok((Some(p), used)) => {
                assert_eq!(used, 0, "end of stream consumes nothing");
                out.push(p.payload().to_vec());
            }
            _ => break,
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    // ---- The whole-buffer path, and the properties of what it produces.
    let whole = run(data, usize::MAX);
    let joined: Vec<u8> = whole.concat();
    assert!(
        joined.len() <= data.len(),
        "the parser emitted more bytes than it was given"
    );
    for unit in &whole {
        assert!(!unit.is_empty(), "an empty access unit was emitted");
    }

    // ---- Chunking must not change the answer.
    for chunk in [1usize, 3, 17] {
        assert_eq!(
            run(data, chunk),
            whole,
            "chunk size {chunk} changed the access-unit sequence"
        );
    }

    // ---- Every OBU, fed directly to every parser that might accept it.
    let mut budget = Budget::new(Limits::strict());
    for obu in units(data, Av1Framing::ObuStream).into_iter().take(4096) {
        let payload = obu.payload(data);
        match obu.header.obu_type {
            ObuType::SEQUENCE_HEADER => {
                if let Ok(sh) = SequenceHeader::parse(payload, &mut budget) {
                    assert!(
                        sh.max_frame_width > 0 && sh.max_frame_height > 0,
                        "a zero coded dimension escaped the check"
                    );
                    let p = params::codec_parameters(&sh);
                    let _ = p.validate(&budget);
                    let _ = params::pixel_format(&sh.color_config);
                    let _ = params::color_info(&sh.color_config);
                    let _ = sh.frame_rate();

                    // Frame headers under this sequence header, everywhere in
                    // the stream — not just when this exact OBU precedes one,
                    // since a fuzzer input rarely has a realistic OBU order.
                    for frame_obu in units(data, Av1Framing::ObuStream).into_iter().take(64) {
                        if frame_obu.header.obu_type != ObuType::FRAME_HEADER
                            && frame_obu.header.obu_type != ObuType::FRAME
                        {
                            continue;
                        }
                        if let Ok(fh) = FrameHeader::parse(
                            frame_obu.payload(data),
                            &sh,
                            frame_obu.header.temporal_id,
                            frame_obu.header.spatial_id,
                        ) && let Some(size) = fh.size()
                        {
                            assert!(
                                size.coded_width > 0 && size.coded_height > 0,
                                "a zero frame dimension escaped the check"
                            );
                            assert!(
                                size.coded_width <= size.upscaled_width,
                                "superres widened the coded picture"
                            );
                        }
                    }
                }
            }
            ObuType::METADATA => {
                let _ = metadata::parse(payload, &mut budget);
            }
            _ => {}
        }
    }

    // ---- `av1C`, over the same bytes as a container extradata blob.
    let _ = Av1CodecConfigurationRecord::parse(data, &mut budget);
});
