//! ADTS framing against arbitrary bytes.
//!
//! ADTS is the one AAC transport that has to *find* its own frames, so it is
//! the one that can be made to loop, to resynchronise for ever, or to accept a
//! chance twelve-bit sync word and hand a nonsense length to whatever is
//! downstream. Real inputs are truncated recordings, satellite streams with bit
//! errors, and files that are not AAC at all.
//!
//! The properties asserted here hold for *every* input, not just well-formed
//! ones:
//!
//! 1. The parser never consumes more than it was given.
//! 2. It always makes progress: a call that emits no packet and consumes no
//!    bytes may not repeat for ever on a buffer that is not growing.
//! 3. An emitted packet is exactly the frame its header declared, and its
//!    header re-parses to the same values — which is what stops a resynchroniser
//!    from emitting a "frame" it never actually validated.
//! 4. Feeding the same bytes through `ParserDriver` one chunk at a time yields
//!    the same frames as feeding them whole. That is the reassembly bug the
//!    driver exists to prevent, checked from the parser's side.
//!
//! fuzz-crate: vaco-parse-aac
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::{Parser, ParserDriver};
use vaco_core::Error;
use vaco_limits::Limits;
use vaco_parse_aac::{AdtsHeader, AdtsParser};

/// A hostile input can make the parser scan byte by byte; cap the work so a
/// pathological case is a slow unit rather than a timeout with no stack.
const MAX_CALLS: usize = 4096;

fn drain_whole(data: &[u8]) -> Vec<Vec<u8>> {
    let mut parser = AdtsParser::new(Limits::strict());
    let mut out = Vec::new();
    let mut offset = 0usize;
    for _ in 0..MAX_CALLS {
        let Some(rest) = data.get(offset..) else { break };
        if rest.is_empty() {
            break;
        }
        let Ok((packet, used)) = parser.parse(rest) else {
            break;
        };
        assert!(used <= rest.len(), "parser over-consumed: {used} > {}", rest.len());
        if let Some(packet) = packet {
            let payload = packet.payload().to_vec();
            let header = AdtsHeader::parse(&payload)
                .unwrap_or_else(|e| panic!("emitted a packet whose header does not parse: {e}"));
            assert_eq!(
                usize::from(header.frame_length),
                payload.len(),
                "packet length disagrees with the header that produced it"
            );
            out.push(payload);
        } else if used == 0 {
            // No packet and no progress: more input is needed, and there is
            // none.
            break;
        }
        offset += used;
    }
    // End of stream, the same signal `ParserDriver::finish` sends: an empty
    // slice. A frame the parser deferred for want of a confirming sync word
    // comes out here, and forgetting this call is what made the first version
    // of this target disagree with the chunked run on every single-frame input.
    for _ in 0..MAX_CALLS {
        let Ok((packet, used)) = parser.parse(&[]) else { break };
        assert_eq!(used, 0, "the parser consumed bytes from an empty slice");
        match packet {
            Some(packet) => out.push(packet.payload().to_vec()),
            None => break,
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let whole = drain_whole(data);

    // The same bytes, delivered in awkward pieces. The chunk size is taken from
    // the data itself so the fuzzer can steer it.
    let chunk = usize::from(data.first().copied().unwrap_or(1)).max(1);
    let mut driver = ParserDriver::new(AdtsParser::new(Limits::strict()), Limits::permissive());
    let mut pieced: Vec<Vec<u8>> = Vec::new();
    let mut offset = 0usize;
    let mut calls = 0usize;
    while offset < data.len() && calls < MAX_CALLS {
        calls += 1;
        let end = offset.saturating_add(chunk).min(data.len());
        let Some(slice) = data.get(offset..end) else { break };
        if driver.push(slice).is_err() {
            // A reassembly cap or a budget is a clean refusal, not a bug — but
            // it means the two runs are no longer comparable.
            return;
        }
        offset = end;
        if !drain(&mut driver, &mut pieced) {
            return;
        }
    }
    driver.finish();
    if !drain(&mut driver, &mut pieced) {
        return;
    }

    if calls < MAX_CALLS {
        assert_eq!(
            whole, pieced,
            "framing changed when the same bytes arrived in {chunk}-byte chunks"
        );
    }

    // 5. `packet_duration` is total and bounded, on both of its paths. The
    //    in-band path walks ADTS headers inside the payload, which is the same
    //    attacker-controlled scan the framing does — a frame length of zero
    //    there would be an infinite loop rather than a wrong answer. The
    //    configured path must ignore the payload entirely, so the assertion is
    //    that the answer does not depend on it.
    let plain = AdtsParser::new(Limits::strict());
    if let Some(d) = plain.packet_duration(data) {
        assert!(d.num > 0 && d.den > 0, "not a duration: {d:?}");
        // At most one raw data block per 7-byte header, four blocks each,
        // 1024 samples a block.
        let cap = (data.len() / 7 + 1) * 4 * 1024;
        assert!(u64::from(d.num.unsigned_abs()) <= cap as u64, "{d:?} over {cap}");
    }

    let mut configured = AdtsParser::new(Limits::strict());
    if configured.set_extradata(data).is_ok() {
        let a = configured.packet_duration(data);
        let b = configured.packet_duration(&[]);
        assert_eq!(a, b, "a configured parser must not read the payload");
        if let Some(d) = a {
            assert!(d.num > 0 && d.den > 0, "not a duration: {d:?}");
        }
    }
});

/// Take everything the driver will give up, and say whether the two runs are
/// still comparable.
///
/// `NeedMoreInput` and `Eof` are the normal ways out. Anything else is the
/// *driver* declining to continue rather than a framing decision by the parser,
/// so the two runs are no longer comparable.
///
/// This used to be reachable through no fault of the parser: `ParserDriver`
/// counted every `NeedMoreInput` as a stall and gave up after 64, so a small
/// enough chunk size stopped the stream whatever the parser did. That is fixed
/// — `push` now resets the guard when it adds bytes — so a driver refusal here
/// is once again a real finding.
fn drain(driver: &mut ParserDriver<AdtsParser>, out: &mut Vec<Vec<u8>>) -> bool {
    loop {
        match driver.next_unit() {
            Ok(packet) => out.push(packet.payload().to_vec()),
            Err(Error::NeedMoreInput | Error::Eof) => return true,
            Err(_) => return false,
        }
        if out.len() > MAX_CALLS {
            return false;
        }
    }
}
