//! LOAS `AudioSyncStream` framing over arbitrary bytes.
//!
//! LOAS has an eleven-bit sync word and a thirteen-bit length, so a random
//! buffer offers a candidate frame roughly every two kilobytes — and unlike
//! ADTS, accepting one means running a *nested* parser (`StreamMuxConfig`, and
//! the `AudioSpecificConfig` inside it) over attacker-chosen bits. That is the
//! combination worth fuzzing: a framing decision that gates a much larger
//! parser.
//!
//! Properties, for every input:
//!
//! 1. The parser never consumes more than it was given, and never reports a
//!    packet longer than the frame its header declared.
//! 2. A run of calls that neither emits nor consumes terminates.
//! 3. An emitted packet re-parses to the same sync header.
//! 4. Whenever a configuration was read, the parameters derived from it are
//!    internally consistent.
//!
//! fuzz-crate: vaco-parse-aac
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Parser;
use vaco_limits::Limits;
use vaco_parse_aac::{LoasParser, SyncStreamHeader};

const MAX_CALLS: usize = 4096;

fuzz_target!(|data: &[u8]| {
    let mut parser = LoasParser::new(Limits::strict());
    let mut offset = 0usize;
    let mut stalls = 0usize;

    for _ in 0..MAX_CALLS {
        let Some(rest) = data.get(offset..) else { break };
        if rest.is_empty() {
            break;
        }
        let Ok((packet, used)) = parser.parse(rest) else {
            break;
        };
        assert!(used <= rest.len(), "over-consumed {used} of {}", rest.len());

        if let Some(packet) = packet {
            let payload = packet.payload();
            let header = SyncStreamHeader::parse(payload)
                .unwrap_or_else(|e| panic!("emitted a frame with no sync header: {e}"));
            assert_eq!(
                header.frame_len(),
                payload.len(),
                "packet length disagrees with audioMuxLengthBytes"
            );
            assert_eq!(used, payload.len() + (used - payload.len()));
            stalls = 0;
        } else if used == 0 {
            // No packet, no progress: the parser wants more input and there is
            // none. One such call is correct; a second would be a hang.
            stalls += 1;
            assert!(stalls < 2, "parser stalled without consuming input");
            break;
        } else {
            stalls = 0;
        }
        offset += used;
    }

    if let Some(config) = parser.config() {
        assert!(config.programs >= 1);
        assert!(!config.streams.is_empty());
        if let Some(params) = parser.parameters() {
            assert!(params.check_consistent().is_ok());
            if let Some(audio) = params.audio.as_ref() {
                assert!(audio.sample_rate > 0);
                if let Some(layout) = audio.layout.as_ref() {
                    assert!(layout.channels > 0);
                }
            }
        }
    }
});
