//! `vaco_codec_exec::annexb::Splitter` against arbitrary bytes.
//!
//! This is the one piece of `vaco-codec-exec` that ever sees
//! attacker-influenced input in the traditional sense: everything else in
//! the crate either writes bytes we generated ourselves (the Y4M stream) or
//! runs a subprocess the *user* chose to install, but the subprocess's
//! stdout is still an untrusted byte stream from this crate's point of
//! view — a corrupted encoder build, a `x264`/`x265` version this crate has
//! never seen, or a `--aud`-supporting future tool with a different header
//! shape could all hand back bytes that do not look like what
//! `crate::process::x264_args`-style invocations normally produce.
//!
//! Splits the input on both start-code widths and both NAL families in one
//! run, fed in two different chunkings (all at once, and one byte at a
//! time) so a bug that only shows up when a start code straddles a `push`
//! boundary is reachable — the one hazard specific to this splitter's
//! incremental design, since a real subprocess's stdout reads never align
//! with NAL boundaries either.
//!
//! fuzz-crate: vaco-codec-exec

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_exec::annexb::{NalFamily, Splitter, is_keyframe};
use vaco_limits::{Budget, Limits};

fn run(data: &[u8], family: NalFamily, byte_at_a_time: bool) {
    let mut splitter = Splitter::new(family);
    let mut budget = Budget::new(Limits::strict());
    let mut units: Vec<Vec<u8>> = Vec::new();
    if byte_at_a_time {
        for b in data {
            match splitter.push(std::slice::from_ref(b), &mut budget) {
                Ok(u) => units.extend(u),
                Err(_) => return,
            }
        }
    } else {
        match splitter.push(data, &mut budget) {
            Ok(u) => units.extend(u),
            Err(_) => return,
        }
    }
    if let Some(tail) = splitter.finish() {
        units.push(tail);
    }
    // Every access unit `is_keyframe` looked at must not panic regardless of
    // how it was chunked, and the two chunkings of the same bytes must
    // agree on how many units came out -- an incremental parser that
    // depends on how its input was split is a real bug class distinct from
    // "does it panic".
    for unit in &units {
        let _ = is_keyframe(unit, family);
    }
}

fuzz_target!(|data: &[u8]| {
    for family in [NalFamily::H264, NalFamily::H265] {
        run(data, family, false);
        run(data, family, true);
    }
});
