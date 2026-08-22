//! The parser harness.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use vaco_codec_core::mock::MockParser;
use vaco_codec_core::{CodecId, CodecParameters, ParserDriver};
use vaco_core::Error;
use vaco_limits::Limits;

#[test]
fn units_are_consumed_across_chunk_boundaries() {
    let mut d = ParserDriver::new(MockParser::new(4), Limits::permissive());
    // Split a 12-byte stream into chunks that never align with a unit.
    d.push(&[0; 3]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
    d.push(&[0; 3]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
    d.push(&[0; 6]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
    assert_eq!(d.consumed(), 12);
    assert_eq!(d.parser().units(), 3);
    assert_eq!(d.pending(), 0);
}

#[test]
fn end_of_stream_is_signalled_once_and_then_reported() {
    let mut d = ParserDriver::new(MockParser::new(4), Limits::permissive());
    d.push(&[0; 8]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
    d.finish();
    assert!(matches!(d.next_unit(), Err(Error::Eof)));
    assert!(matches!(d.next_unit(), Err(Error::Eof)));
    // Bytes after end of stream are refused rather than silently dropped.
    assert!(matches!(d.push(&[0; 4]), Err(Error::Eof)));
}

#[test]
fn a_trailing_partial_unit_is_discarded_at_end_of_stream() {
    let mut d = ParserDriver::new(MockParser::new(4), Limits::permissive());
    d.push(&[0; 6]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
    assert_eq!(d.pending(), 2);
    d.finish();
    assert!(matches!(d.next_unit(), Err(Error::Eof)));
}

#[test]
fn a_parser_that_over_reports_consumption_is_caught() {
    let mut d = ParserDriver::new(MockParser::new(4).over_consuming(), Limits::permissive());
    d.push(&[0; 8]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::InvalidData(_))));
}

#[test]
fn a_stalled_parser_does_not_hang() {
    let mut d = ParserDriver::new(MockParser::new(4).stalling(), Limits::permissive());
    d.push(&[0; 64]).unwrap();
    // Feeding: a stall is legitimate, it just means "I need more bytes".
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
    d.finish();
    // At end of stream a stall cannot be waited out; the harness gives up
    // rather than spinning.
    assert!(matches!(d.next_unit(), Err(Error::Eof)));
}

#[test]
fn the_reassembly_buffer_is_capped() {
    let mut d = ParserDriver::new(MockParser::new(1024).stalling(), Limits::permissive())
        .with_max_pending(32);
    d.push(&[0; 32]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
    assert!(matches!(d.push(&[0; 1]), Err(Error::LimitExceeded { .. })));
}

#[test]
fn parameters_come_from_the_parser() {
    let params = CodecParameters::video().with_codec(CodecId::H264);
    let mut d = ParserDriver::new(
        MockParser::new(4).with_parameters(params),
        Limits::permissive(),
    );
    assert_eq!(d.parameters().and_then(|p| p.codec_id), Some(CodecId::H264));
    d.push(&[0; 4]).unwrap();
    let _ = d.next_unit();
    assert!(d.parameters().is_some());
}

#[test]
fn reset_clears_buffered_bytes_and_end_of_stream() {
    let mut d = ParserDriver::new(MockParser::new(4), Limits::permissive());
    d.push(&[0; 6]).unwrap();
    let _ = d.next_unit();
    d.finish();
    d.reset();
    assert_eq!(d.pending(), 0);
    d.push(&[0; 4]).unwrap();
    assert!(matches!(d.next_unit(), Err(Error::NeedMoreInput)));
}

/// Feeding a stream one byte at a time must not look like a stall.
///
/// Regression for a defect `vaco-parse-aac`'s fuzzer found: `next_unit` ticked
/// the progress guard whenever the parser could not yet form a unit, but the
/// caller *was* progressing — it was adding bytes between calls. Any parser
/// driven in chunks smaller than about 1/64 of a unit aborted with
/// `NoProgress`, which is every byte-stream parser in the project.
#[test]
fn byte_at_a_time_feeding_is_progress() {
    let mut d = ParserDriver::new(MockParser::new(200), Limits::permissive());
    for i in 0..200_u16 {
        d.push(&[7]).expect("push must not fail");
        match d.next_unit() {
            Ok(_) | Err(Error::NeedMoreInput) => {}
            Err(e) => panic!("byte {i} of 200 aborted the stream: {e}"),
        }
    }
}

/// The hang the guard exists to catch is still caught: a caller that spins on
/// `next_unit` without ever pushing re-parses the same bytes forever.
#[test]
fn spinning_without_pushing_still_aborts() {
    let mut d = ParserDriver::new(MockParser::new(200), Limits::permissive());
    d.push(&[1, 2, 3]).expect("push");
    for _ in 0..10_000 {
        match d.next_unit() {
            Err(Error::NeedMoreInput) => {}
            Err(_) => return, // the guard tripped, which is the point
            Ok(_) => panic!("a parser with too few bytes must not emit"),
        }
    }
    panic!("a caller spinning without pushing was never stopped");
}
