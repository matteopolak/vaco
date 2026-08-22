//! The parser harness under arbitrary chunking.
//!
//! The harness owns reassembly, end-of-stream handling and byte accounting for
//! every parser in the project, so a hang or a panic here would be a hang or a
//! panic in all of them. A limit error is correct behaviour, not a finding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::mock::MockParser;
use vaco_codec_core::ParserDriver;
use vaco_core::Error;
use vaco_limits::Limits;

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    unit_len: u8,
    stalling: bool,
    over_consuming: bool,
    max_pending: u16,
    chunks: Vec<Vec<u8>>,
}

fuzz_target!(|input: Input| {
    if input.chunks.len() > 256 {
        return;
    }
    let mut parser = MockParser::new(usize::from(input.unit_len));
    if input.stalling {
        parser = parser.stalling();
    }
    if input.over_consuming {
        parser = parser.over_consuming();
    }
    let mut driver = ParserDriver::new(parser, Limits::tiny())
        .with_max_pending(usize::from(input.max_pending));

    for chunk in &input.chunks {
        // A refused push is correct behaviour under a tight budget.
        if driver.push(chunk).is_err() {
            break;
        }
        let mut steps = 0u32;
        loop {
            steps += 1;
            assert!(steps < 100_000, "the harness failed to terminate");
            match driver.next() {
                Ok(_) => {}
                Err(Error::NeedMoreInput) => break,
                Err(_) => break,
            }
        }
    }

    driver.finish();
    let mut steps = 0u32;
    loop {
        steps += 1;
        assert!(steps < 100_000, "the harness failed to terminate at end of stream");
        match driver.next() {
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // Eof is stable.
    assert!(driver.next().is_err());
});
