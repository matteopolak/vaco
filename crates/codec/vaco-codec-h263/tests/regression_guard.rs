//! Byte-exact pins of this crate's own current output, for every mode
//! that decodes today — H.261, baseline H.263, H.263+'s Annex D (UMV)
//! combined with Annex K (Slice Structured, which `ffmpeg`'s own h263p
//! encoder always couples `-umv` with — see `docs/codec/vaco-codec-h263.md`),
//! and Annex J (Deblocking Filter).
//!
//! This is a regression guard, not a correctness check: it does not
//! compare against `ffmpeg` (this crate's own measured baseline against
//! real `ffmpeg` output is ~99.2% exact, not 100% — see "Measured
//! accuracy" in the docs above; a real second source of ±1 differences
//! would make these hashes the wrong tool for that job). Its only job is
//! to make a change to the shared macroblock decode loop fail loudly if
//! it changes *this crate's own* output for a mode nothing was supposed
//! to touch — written specifically before restructuring that loop for
//! Annex F's OBMC. `Vaco-Provenance: n/a` — no spec content here, just a
//! recorded fact about this crate's own prior behaviour.
//!
//! Each fixture is a real `ffmpeg 8.1 -bitexact`-encoded QCIF elementary
//! stream (5 frames, mixed I/P, `testsrc`), extracted from its container
//! with no re-encoding. Regenerating a fixture or its pinned hash is
//! only ever correct as a *deliberate* update alongside a change that is
//! known to alter output for that mode — never to make a red test green
//! without first understanding why it turned red.

use vaco_codec_core::Decoder;
use vaco_codec_h263::{H261Decoder, H263Decoder};
use vaco_hash::HashAlgo;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn decode_all(decoder: &mut dyn Decoder, packet: &Packet) -> Vec<u8> {
    let mut out = Vec::new();
    let drain = |decoder: &mut dyn Decoder, out: &mut Vec<u8>| {
        while let Ok(frame) = decoder.receive_frame() {
            for plane_idx in 0..3 {
                let Some(plane) = frame.plane(plane_idx) else {
                    continue;
                };
                for row in plane.rows_iter() {
                    out.extend_from_slice(row);
                }
            }
        }
    };
    while let Err(vaco_core::Error::OutputPending) = decoder.send_packet(Some(packet)) {
        drain(decoder, &mut out);
    }
    drain(decoder, &mut out);
    let _ = decoder.send_packet(None);
    drain(decoder, &mut out);
    out
}

fn assert_pinned(codec: &str, fixture_bytes: &[u8], expected_sha256: &str) -> Result<(), String> {
    let limits = Limits::permissive();
    let mut budget = Budget::new(limits.clone());
    let Ok(packet) = Packet::from_slice(&mut budget, fixture_bytes) else {
        return Err("fixture too large for the test budget".to_owned());
    };
    let mut decoder: Box<dyn Decoder> = match codec {
        "h261" => Box::new(H261Decoder::new(limits)),
        "h263" => Box::new(H263Decoder::new(limits)),
        other => return Err(format!("unknown codec {other}")),
    };
    let out = decode_all(&mut *decoder, &packet);
    let Some(got) = HashAlgo::Sha256.digest_hex(&out) else {
        return Err("sha256 is always computable".to_owned());
    };
    if got == expected_sha256 {
        Ok(())
    } else {
        Err(format!(
            "{codec} regression fixture decoded to a different byte-exact output than the pinned \
             expectation ({} output bytes, got sha256 {got}, want {expected_sha256}) -- if this \
             change is a deliberate, understood output change for this mode, regenerate the pin; \
             if not, the macroblock decode loop just regressed a currently-working mode",
            out.len()
        ))
    }
}

#[test]
fn h261_mixed_i_p_is_byte_stable() -> Result<(), String> {
    assert_pinned(
        "h261",
        include_bytes!("fixtures/regression_h261_mixed.261"),
        "5936408a3a5b2a9e0c17c1e3620992e033b2a2218dce4ac65562109b1b502e34",
    )
}

#[test]
fn h263_baseline_mixed_i_p_is_byte_stable() -> Result<(), String> {
    assert_pinned(
        "h263",
        include_bytes!("fixtures/regression_h263_baseline.263"),
        "4d02bb31a18e1d36cddb5f1eb4bd1d6803a5a14c2535512027d471df395e75d2",
    )
}

#[test]
fn h263_plus_umv_and_slice_structured_is_byte_stable() -> Result<(), String> {
    // ffmpeg's own h263p encoder always couples `-umv` with Slice
    // Structured mode (see docs/codec/vaco-codec-h263.md's Annex D/K
    // finding) — this fixture exercises decode_slice_rect's own
    // rectangular-slice scan, the one macroblock-loop variant besides
    // decode_gob's plain raster order.
    assert_pinned(
        "h263",
        include_bytes!("fixtures/regression_h263p_umv_slices.263"),
        "bbc11f32dafcc511b39f5b17a3784a10a99ba71bbf5831693e4fb3c5b4326c15",
    )
}

#[test]
fn h263_plus_annex_j_deblocking_is_byte_stable() -> Result<(), String> {
    // Exercises finish_picture's whole-picture deblocking pass, the one
    // existing precedent for a post-macroblock-loop full-frame pass.
    assert_pinned(
        "h263",
        include_bytes!("fixtures/regression_h263p_annexj.263"),
        "f1d4da9105664f2abf40f4a6411e25aaeb517fd98f90d28778e17cbd6a91a449",
    )
}
