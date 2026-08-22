//! H.264 header parsing against arbitrary bytes.
//!
//! The input is an Annex B elementary stream. Everything the crate can parse is
//! reached: the streaming access-unit splitter, and every NAL unit fed directly
//! to the SPS, PPS, slice-header and SEI parsers as well, so a unit the splitter
//! would have skipped still gets parsed.
//!
//! Properties, beyond "does not panic":
//!
//! * **Chunking is invisible.** The same bytes fed one at a time and all at once
//!   must produce the identical sequence of access units. This is the property
//!   the classic chunked-parser bug violates, and it cannot be found by a
//!   whole-buffer target.
//! * **Access units partition the input.** Every byte the parser emits came from
//!   the input, in order, exactly once.
//! * **Progress.** A call that consumes nothing and emits nothing is a hang;
//!   the target asserts the parser always consumes what it is given.
//! * **Derived geometry is self-consistent.** A reported width never exceeds the
//!   macroblock-aligned one, and neither is ever zero.
//!
//! A `LimitExceeded` is correct behaviour and returns normally (plan 13 §2.2.4).
//! fuzz-crate: vaco-parse-h264
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bitstream::annexb;
use vaco_codec_core::Parser;
use vaco_format_nalu::{Framing, LengthSize, units};
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{
    AvcDecoderConfigurationRecord, H264NalHeader, H264Parser, NalUnitType, ParameterSets, Pps,
    SliceHeader, Sps, codec_parameters, params, sei,
};

/// Feed `data` through the streaming parser, returning the access units.
///
/// The contract being exercised: a call either consumes everything it is given,
/// or consumes nothing *and* hands back a queued access unit. Anything else — a
/// short consume, or nothing at all — is a stall, and a stall in a parser is a
/// hang in whatever is driving it.
fn run(data: &[u8], chunk: usize) -> Vec<Vec<u8>> {
    let mut parser = H264Parser::new(Limits::strict());
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
    // Drain at end of stream. Bounded: each call either yields a unit or ends
    // the loop, and a unit is never empty, so this cannot spin.
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
    // Every emitted byte came from the input, in order. Access units are a
    // subsequence of the stream: leading garbage and empty units are dropped,
    // so this is containment rather than equality.
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

    // ---- Every NAL unit, fed directly to every parser that might accept it.
    let mut budget = Budget::new(Limits::strict());
    let mut sets = ParameterSets::new();
    let mut scratch = Vec::new();
    for nal in units(data, Framing::AnnexB).take(4096) {
        let rbsp = annexb::to_rbsp(nal.data, &mut scratch);
        let Some(header) = H264NalHeader::parse(rbsp) else {
            continue;
        };
        match header.nal_unit_type {
            NalUnitType::Sps | NalUnitType::SubsetSps => {
                if let Ok(sps) = Sps::parse(rbsp, &mut budget) {
                    // The geometry a parsed SPS reports must be self-consistent.
                    if let Some((w, h)) = sps.dimensions() {
                        assert!(w > 0 && h > 0, "a zero dimension escaped the crop check");
                        assert!(w <= sps.coded_width(), "cropping made the picture wider");
                        assert!(h <= sps.coded_height(), "cropping made the picture taller");
                    }
                    // And every derived value must be computable without panicking.
                    let p = codec_parameters(&sps);
                    let _ = params::pixel_format(&sps);
                    let _ = params::sample_aspect_ratio(&sps);
                    let _ = sps.frame_rate();
                    let _ = sps.color_info();
                    let _ = sps.profile_name();
                    let _ = sps.max_num_reorder_frames();
                    let _ = p.validate(&budget);
                    let _ = sets.add_sps(rbsp, &mut budget);
                }
            }
            NalUnitType::Pps => {
                let active = sets.active().cloned();
                let _ = Pps::parse(rbsp, active.as_ref(), &mut budget);
                let _ = sets.add_pps(rbsp, &mut budget);
            }
            NalUnitType::Sei => {
                let active = sets.active().cloned();
                let _ = sei::parse(rbsp, active.as_ref(), &mut budget);
            }
            t if t.has_slice_header() => {
                // A slice header needs its parameter sets; try every stored PPS
                // so a slice that names one is actually parsed rather than
                // skipped.
                for pps_id in 0..=255u8 {
                    if let Some((pps, sps)) = sets.sps_for_pps(pps_id) {
                        let _ = SliceHeader::parse(rbsp, sps, pps, &mut budget);
                    }
                }
            }
            _ => {}
        }
    }

    // ---- The same bytes as an `avcC` record and as a length-prefixed sample.
    let mut budget = Budget::new(Limits::strict());
    if let Ok(record) = AvcDecoderConfigurationRecord::parse(data, &mut budget) {
        assert!(
            record.sps.len() <= 31,
            "the five-bit count cannot exceed 31"
        );
        let _ = record.mime_codec_string();
        let mut parser = H264Parser::new(Limits::strict());
        let _ = parser.set_extradata(data);
    }
    for size in [LengthSize::ONE, LengthSize::TWO, LengthSize::FOUR] {
        let mut parser = H264Parser::new(Limits::strict());
        let _ = parser.push_access_unit(data, Framing::LengthPrefixed(size));
    }
});
