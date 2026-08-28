//! H.261 and (baseline plus H.263+ Annexes D/K/T) H.263 video decode
//! against arbitrary bytes: the bit-level (H.261) and byte-level (H.263)
//! start-code scanners, picture/GOB/macroblock-layer VLC decode
//! (`MBA`/`MTYPE`/`MVD`/`CBP` for H.261; `MCBPC`/`CBPY`/`MVD` for H.263),
//! `TCOEFF`/`TCOEF` coefficient decode including both formats' escape
//! paths (and Annex T's `EXTENDED-ESCAPE`), motion compensation, and the
//! loop filter — the whole `send_packet`/`receive_frame` pipeline for
//! both `H261Decoder` and `H263Decoder`, run one packet at a time so the
//! fuzzer never has to synthesise a container to reach any of it.
//!
//! No structural change was needed to reach the annex work's new code:
//! `PLUSPTYPE` (bits 6-8 of `PTYPE` equal to `"111"`, a 1-in-8 byte
//! pattern) and everything behind it — the extended header's own field
//! cascade, Annex K's slice layer (including the stuffing-aware
//! start-code check any misaligned `"00 00 1xxxxxxx"` exercises), Annex
//! D's two `UMV` reconstruction paths, and Annex T's variable-length
//! `DQUANT`/`EXTENDED-LEVEL` — are all just more of the same arbitrary
//! bitstream this target already mutates; a coverage-guided run finds its
//! own way in.
//!
//! Both decoders are exercised from the same arbitrary input, since
//! neither one's start-code pattern can be confused with the other's
//! picture data, so there is no risk of one call's mutation hiding a bug
//! only reachable by the other.
//!
//! Two packets are sent per input (like `mpeg12_decode`'s target): the
//! first exercises decode from a cold start (no reference picture yet —
//! the `sample_mc`/`None` fallback path in both `h261.rs` and `h263.rs`),
//! the second exercises the persistent single-reference state a P-picture
//! macroblock reads from.
//!
//! fuzz-crate: vaco-codec-h263

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_h263::{H261Decoder, H263Decoder};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn drive(decoder: &mut dyn Decoder, packet: &Packet) {
    if decoder.send_packet(Some(packet)).is_err() {
        return;
    }
    while let Ok(frame) = decoder.receive_frame() {
        // Every plane the decoder claims to have written must actually be
        // addressable — an out-of-bounds `Plane::row` here is exactly the
        // kind of allocation/geometry bug a fuzzer is well-placed to find
        // fast, distinct from the pixel-accuracy differential suite.
        for idx in 0..3 {
            if let Some(plane) = frame.plane(idx) {
                let _ = plane.row(0);
            }
        }
    }
    if decoder.send_packet(Some(packet)).is_err() {
        return;
    }
    while decoder.receive_frame().is_ok() {}
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };

    let mut h261 = H261Decoder::new(Limits::strict());
    drive(&mut h261, &packet);

    let mut h263 = H263Decoder::new(Limits::strict());
    drive(&mut h263, &packet);
});
