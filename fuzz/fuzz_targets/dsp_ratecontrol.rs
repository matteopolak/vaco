//! Fuzzing `vaco-codec-dsp-ratecontrol` for panics and non-finite output
//! under arbitrary configuration and frame sequences.
//!
//! Every field of `RateControlConfig` is exactly the kind of externally
//! supplied value (CLI options, an eventual config file) that can arrive
//! as a `NaN`, an infinity, or an inverted `min_qscale > max_qscale` pair
//! — and `f64::clamp` panics on the latter two. `RateControlConfig::sanitized`
//! (called once, inside `RateController::new`) exists specifically to
//! repair this before anything downstream reads it; this target is the
//! adversarial check that it actually does, across the full space of
//! malformed configs, not just the couple of hand-picked cases a unit test
//! would think to construct.
//! fuzz-crate: vaco-codec-dsp-ratecontrol
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_dsp_ratecontrol::{FrameReport, RateControlConfig, RateController};
use vaco_core::Rational;

#[derive(Arbitrary, Debug)]
struct Input {
    mode: u8,
    target_bitrate_bps: u64,
    peak_bitrate_bps: u64,
    fps_num: i32,
    fps_den: i32,
    vbv_buffer_bits: u64,
    min_qscale: f64,
    max_qscale: f64,
    initial_qscale: f64,
    constant_qscale: f64,
    frames: Vec<(f64, u32, f64)>, // (complexity, bits, reported_qscale)
}

fuzz_target!(|input: Input| {
    let mut cfg = match input.mode % 3 {
        0 => RateControlConfig::cbr(
            input.target_bitrate_bps,
            Rational {
                num: input.fps_num,
                den: input.fps_den,
            },
        ),
        1 => RateControlConfig::vbr(
            input.target_bitrate_bps,
            input.peak_bitrate_bps,
            Rational {
                num: input.fps_num,
                den: input.fps_den,
            },
        ),
        _ => RateControlConfig::constant_quality(input.constant_qscale),
    };
    cfg.vbv_buffer_bits = input.vbv_buffer_bits;
    cfg.min_qscale = input.min_qscale;
    cfg.max_qscale = input.max_qscale;
    cfg.initial_qscale = input.initial_qscale;

    let mut rc = RateController::new(cfg);
    for (complexity, bits, reported_qscale) in input.frames.into_iter().take(512) {
        let qscale = rc.next_qscale(complexity);
        assert!(qscale.is_finite(), "next_qscale must never return non-finite: {qscale}");
        // A caller may report back a different qscale than the one it was
        // handed (it clamped further, or deviated for its own reasons);
        // the reported value is exactly as untrusted as `bits`.
        rc.report(FrameReport {
            bits: u64::from(bits),
            qscale: reported_qscale,
        });
        assert!(
            rc.buffer_fullness_bits().is_finite(),
            "buffer_fullness_bits must never go non-finite"
        );
    }
    let _ = rc.achieved_bitrate_bps();
});
