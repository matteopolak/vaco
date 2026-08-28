//! Both `vaco-bsf-subtitle` filters over an arbitrary packet.
//!
//! Unlike the rest of this filter family, `mov2textsub`/`text2movsub` are
//! real byte-level transforms (see each module's docs), so this is exactly
//! the "attacker bytes meet a length-prefixed format" case D6 exists for:
//! `mov2textsub` reads a length it does not control out of the first two
//! bytes and must never index past the end of what is actually there, and
//! `text2movsub` must refuse rather than wrap when the input is too long for
//! a `u16` prefix to describe. Round-tripping through both in either order
//! is asserted not to panic and not to hang; `mov2textsub` then
//! `text2movsub`'s output length is asserted to match the original
//! declared-and-bounded text length, which is the one property a truncating
//! bug in either direction would break.
//!
//! fuzz-crate: vaco-bsf-subtitle

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bsf_subtitle::{mov2textsub, text2movsub};
use vaco_codec_core::CodecParameters;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const MAX_PAYLOAD: usize = 4096;
const MAX_STEPS: u32 = 10_000;

fn drain(filter: &mut dyn vaco_codec_core::BitstreamFilter, name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut steps = 0u32;
    loop {
        steps += 1;
        assert!(steps < MAX_STEPS, "{name}: receive loop did not terminate");
        match filter.receive_packet() {
            Ok(p) => out.extend_from_slice(p.payload()),
            Err(_) => break,
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let Some((&control, rest)) = data.split_first() else {
        return;
    };
    let payload = rest.get(..rest.len().min(MAX_PAYLOAD)).unwrap_or(rest);
    let mut budget = Budget::new(Limits::permissive());
    let Ok(pkt) = Packet::from_slice(&mut budget, payload) else {
        return;
    };

    // `control & 1` picks which direction runs first, so the corpus explores
    // both orderings rather than only ever seeing `mov2textsub` first.
    if control & 1 == 0 {
        let Ok(mut f) = (mov2textsub::DESC.build)(&CodecParameters::default()) else {
            return;
        };
        if f.send_packet(Some(&pkt)).is_err() {
            return;
        }
        let text = drain(f.as_mut(), "mov2textsub");
        // Never longer than the input minus the two-byte prefix it read from
        // — the truncation-to-declared-length property, restated as an
        // invariant rather than one fixed sample.
        assert!(
            text.len() <= payload.len().saturating_sub(2),
            "mov2textsub produced more text than its input could carry"
        );

        let Ok(mut g) = (text2movsub::DESC.build)(&CodecParameters::default()) else {
            return;
        };
        let Ok(text_pkt) = Packet::from_slice(&mut budget, &text) else {
            return;
        };
        if g.send_packet(Some(&text_pkt)).is_err() {
            return;
        }
        let wrapped = drain(g.as_mut(), "text2movsub");
        assert_eq!(wrapped.len(), text.len() + 2, "text2movsub must add exactly a two-byte prefix");
    } else {
        let Ok(mut f) = (text2movsub::DESC.build)(&CodecParameters::default()) else {
            return;
        };
        // `text2movsub` refuses payloads over `u16::MAX` bytes rather than
        // silently truncating or wrapping (measured against the reference —
        // see the module docs), so a refusal here is a correct outcome, not
        // a fuzz target bug to chase.
        if f.send_packet(Some(&pkt)).is_err() {
            return;
        }
        let wrapped = drain(f.as_mut(), "text2movsub");
        assert_eq!(wrapped.len(), payload.len() + 2);

        let Ok(mut g) = (mov2textsub::DESC.build)(&CodecParameters::default()) else {
            return;
        };
        let Ok(wrapped_pkt) = Packet::from_slice(&mut budget, &wrapped) else {
            return;
        };
        if g.send_packet(Some(&wrapped_pkt)).is_err() {
            return;
        }
        let back = drain(g.as_mut(), "mov2textsub");
        assert_eq!(back, payload, "text2movsub then mov2textsub must round-trip");
    }
});
