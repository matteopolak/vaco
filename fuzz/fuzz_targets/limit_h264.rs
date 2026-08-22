//! H.264 parsing under a deliberately absurd budget.
//!
//! Plan 13 §2.2.2: *"`limit_*` fuzz targets run every component under
//! `Limits::strict()` with a deliberately tiny budget and assert that every
//! failure is a clean `Error::LimitExceeded` — never a panic, never an abort,
//! never success with a 900 MB buffer."*
//!
//! This is the same input space as `parse_h264`, driven with
//! [`Limits::tiny`] — 64 KiB total, 16 KiB per allocation, 65536 units of fuel.
//! Almost everything is refused, and the point is that refusal is *clean*: a
//! typed error out of the front door, with no allocation and no panic behind it.
//!
//! The two failure modes it exists to catch are a parser that allocates before
//! checking, and a parser that treats a budget error as unreachable.
//! fuzz-crate: vaco-parse-h264
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bitstream::annexb;
use vaco_codec_core::Parser;
use vaco_core::Error;
use vaco_format_nalu::{Framing, LengthSize, units};
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{
    AvcDecoderConfigurationRecord, H264Parser, ParameterSets, Pps, Sps, sei,
};

/// Errors a bounded parser is allowed to produce. Anything else means a
/// budget failure was mistaken for a data failure or vice versa.
fn is_clean(e: &Error) -> bool {
    matches!(
        e,
        Error::LimitExceeded { .. }
            | Error::InvalidData(_)
            | Error::UnexpectedEof
            | Error::Unsupported(_)
            | Error::NeedMoreInput
            | Error::Eof
    )
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::tiny());
    let mut sets = ParameterSets::new();
    let mut scratch = Vec::new();

    for nal in units(data, Framing::AnnexB).take(1024) {
        let rbsp = annexb::to_rbsp(nal.data, &mut scratch);
        if let Err(e) = Sps::parse(rbsp, &mut budget) {
            assert!(is_clean(&e), "unexpected error from Sps::parse: {e:?}");
        }
        let active = sets.active().cloned();
        if let Err(e) = Pps::parse(rbsp, active.as_ref(), &mut budget) {
            assert!(is_clean(&e), "unexpected error from Pps::parse: {e:?}");
        }
        if let Err(e) = sei::parse(rbsp, active.as_ref(), &mut budget) {
            assert!(is_clean(&e), "unexpected error from sei::parse: {e:?}");
        }
        let _ = sets.add_sps(rbsp, &mut budget);
        let _ = sets.add_pps(rbsp, &mut budget);

        // The budget is tiny, so it is quickly spent. Refuel between units so
        // the later ones are exercised too rather than all failing identically
        // on an exhausted counter.
        budget.refuel();
    }

    if let Err(e) = AvcDecoderConfigurationRecord::parse(data, &mut Budget::new(Limits::tiny())) {
        assert!(is_clean(&e), "unexpected error from avcC parse: {e:?}");
    }

    // The streaming parser, whose access-unit buffer is the other thing a
    // hostile stream can grow without bound.
    let mut parser = H264Parser::new(Limits::tiny()).with_max_access_unit(4096);
    'outer: for chunk in data.chunks(64) {
        let mut rest = chunk;
        while !rest.is_empty() {
            match parser.parse(rest) {
                Ok((unit, used)) => {
                    assert!(used <= rest.len());
                    assert!(
                        used == rest.len() || (used == 0 && unit.is_some()),
                        "a call must consume everything or hand back a queued unit"
                    );
                    rest = &rest[used..];
                }
                Err(e) => {
                    assert!(is_clean(&e), "unexpected error from H264Parser: {e:?}");
                    break 'outer;
                }
            }
        }
    }

    let mut parser = H264Parser::new(Limits::tiny());
    for size in [LengthSize::ONE, LengthSize::FOUR] {
        if let Err(e) = parser.push_access_unit(data, Framing::LengthPrefixed(size)) {
            assert!(is_clean(&e), "unexpected error from push_access_unit: {e:?}");
        }
    }
});
