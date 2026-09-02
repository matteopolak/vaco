//! Noise-shaping dither: distinctness, actual application, no DC bias,
//! chunk-invariance in the correct (stateful) sense, reset, and — the
//! measurement plan 17 §B.6 calls for since these curves are ours, not the
//! reference's — a real spectral comparison against a psychoacoustic
//! weighting.
//!
//! See `src/dither/noise_shape.rs` for the generation method and the
//! summary numbers this file's `perceptually_weighted_noise_is_lower_than_tpdf`
//! reproduces as a regression (the *sign* of the improvement, not the exact
//! dB, which is a property of the design method).

#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::excessive_precision,
    clippy::many_single_char_names,
    clippy::panic,
    clippy::field_reassign_with_default,
    clippy::integer_division,
    reason = "test code; a panic here is a failing test, which is the point"
)]

#[path = "common/harness.rs"]
mod harness;

use harness::{budget, spec};
use vaco_chlayout::ChannelLayout;
use vaco_resample::{AudioMut, AudioRef, DitherMethod, ResampleOptions, Resampler};
use vaco_sampfmt::SampleFmt;

const ALL_NS: [&str; 7] = [
    "lipshitz",
    "f_weighted",
    "modified_e_weighted",
    "improved_e_weighted",
    "shibata",
    "low_shibata",
    "high_shibata",
];

fn run_s16(opts: &ResampleOptions, x: &[f64]) -> Vec<i16> {
    let mut b = budget();
    let rs_spec_in = spec(48000, SampleFmt::F64, ChannelLayout::MONO);
    let rs_spec_out = spec(48000, SampleFmt::S16, ChannelLayout::MONO);
    let mut rs = Resampler::new(&rs_spec_in, &rs_spec_out, opts, &mut b).unwrap();
    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
    let mut out = vec![0u8; x.len() * 2 + 64];
    let mut dst = AudioMut::packed(SampleFmt::S16, 1, &mut out).unwrap();
    let n = rs.convert(Some(src), &mut dst).unwrap();
    out[..n * 2]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn signal(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let t = i as f64;
            (t * 0.031).sin() * 0.05 + (t * 0.211).sin() * 0.03 + (t * 0.0007).sin() * 0.02
        })
        .collect()
}

fn opts_with(method: &str) -> ResampleOptions {
    let mut o = ResampleOptions::default();
    o.dither_method = DitherMethod::from_name(method).unwrap();
    o
}

// ── distinctness ─────────────────────────────────────────────────────────

#[test]
fn every_noise_shaping_name_produces_a_distinct_nonempty_curve() {
    let mut seen: Vec<Vec<f64>> = Vec::new();
    for name in ALL_NS {
        let m = DitherMethod::from_name(name).unwrap();
        assert!(
            m.is_noise_shaping(),
            "{name} should resolve to a noise-shaping method"
        );
        let c = m
            .noise_shape_coeffs()
            .unwrap_or_else(|| panic!("{name} has no coefficients"))
            .to_vec();
        assert!(!c.is_empty(), "{name} has an empty curve");
        assert!(
            c.iter().any(|&v| v != 0.0),
            "{name} is all-zero, i.e. not shaping anything"
        );
        assert!(!seen.contains(&c), "{name} duplicates an earlier curve");
        seen.push(c);
    }
}

#[test]
fn shibata_family_is_a_single_curve_scaled_by_strength() {
    let low = DitherMethod::from_name("low_shibata")
        .unwrap()
        .noise_shape_coeffs()
        .unwrap();
    let mid = DitherMethod::from_name("shibata")
        .unwrap()
        .noise_shape_coeffs()
        .unwrap();
    let high = DitherMethod::from_name("high_shibata")
        .unwrap()
        .noise_shape_coeffs()
        .unwrap();
    assert_eq!(low.len(), mid.len());
    assert_eq!(high.len(), mid.len());
    for i in 0..mid.len() {
        assert!(
            (low[i] - 0.5 * mid[i]).abs() < 1e-9,
            "low_shibata is not 0.5x shibata at tap {i}"
        );
        assert!(
            (high[i] - 1.5 * mid[i]).abs() < 1e-9,
            "high_shibata is not 1.5x shibata at tap {i}"
        );
    }
}

// ── it is actually applied ──────────────────────────────────────────────

#[test]
fn noise_shaping_dither_changes_the_output_versus_none() {
    let x = signal(4000);
    let none = run_s16(&ResampleOptions::default(), &x);
    for name in ALL_NS {
        let shaped = run_s16(&opts_with(name), &x);
        assert_eq!(shaped.len(), none.len());
        assert_ne!(
            shaped, none,
            "{name} produced byte-identical output to dither_method=none"
        );
    }
}

#[test]
fn noise_shaping_dither_differs_from_plain_tpdf() {
    // Not just "different from none" (any dither clears that bar) but
    // actually shaped, i.e. different from unshaped triangular dither too.
    let x = signal(4000);
    let tpdf = run_s16(&opts_with("triangular"), &x);
    for name in ALL_NS {
        let shaped = run_s16(&opts_with(name), &x);
        assert_ne!(
            shaped, tpdf,
            "{name} is byte-identical to plain triangular dither"
        );
    }
}

// ── no DC bias ───────────────────────────────────────────────────────────

#[test]
fn noise_shaping_has_no_dc_bias() {
    // A long, DC-free signal, quantised both with and without dither at a
    // deliberately shallow bit depth (output_sample_bits=6, one step =
    // 2^(15-5) = 1024 s16 LSBs) so any systematic shift *introduced by
    // dithering* would be easy to see relative to the undithered rounding,
    // not hidden in it.
    let x = signal(60_000);
    let mut base_opts = ResampleOptions::default();
    base_opts.output_sample_bits = 6;
    let none = run_s16(&base_opts, &x);
    for name in ALL_NS {
        let mut o = opts_with(name);
        o.output_sample_bits = 6;
        let shaped = run_s16(&o, &x);
        let n = shaped.len().min(none.len());
        let mean_shift: f64 = (0..n)
            .map(|i| f64::from(shaped[i]) - f64::from(none[i]))
            .sum::<f64>()
            / n as f64;
        // A step is 1024 LSBs; a bias worth flagging would be a sizeable
        // fraction of that. This small is rounding noise, not bias.
        assert!(
            mean_shift.abs() < 20.0,
            "{name}: dithering shifts the mean by {mean_shift} LSBs relative to no dither, \
             suggesting a DC bias (one step at this depth is 1024 LSBs)"
        );
    }
}

// ── chunk invariance, in the sense a stateful filter can promise ──────────

#[test]
fn noise_shaping_is_chunk_invariant() {
    let x = signal(3000);
    for name in ALL_NS {
        let opts = opts_with(name);
        let mut b1 = budget();
        let spec_in = spec(48000, SampleFmt::F64, ChannelLayout::MONO);
        let spec_out = spec(48000, SampleFmt::S16, ChannelLayout::MONO);
        let mut whole = Resampler::new(&spec_in, &spec_out, &opts, &mut b1).unwrap();
        let mut piecewise = Resampler::new(&spec_in, &spec_out, &opts, &mut b1).unwrap();

        let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut out_whole = vec![0u8; x.len() * 2 + 64];
        {
            let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
            let mut dst = AudioMut::packed(SampleFmt::S16, 1, &mut out_whole).unwrap();
            whole.convert(Some(src), &mut dst).unwrap();
        }

        let mut out_piece = vec![0u8; x.len() * 2 + 64];
        let mut written = 0usize;
        for chunk in bytes.chunks(8 * 37) {
            let src = AudioRef::packed(SampleFmt::F64, 1, chunk).unwrap();
            let mut dst =
                AudioMut::packed(SampleFmt::S16, 1, &mut out_piece[written * 2..]).unwrap();
            written += piecewise.convert(Some(src), &mut dst).unwrap();
        }
        assert_eq!(
            out_whole[..x.len() * 2],
            out_piece[..written * 2],
            "{name}: chunking changed the output"
        );
    }
}

#[test]
fn noise_shaping_reset_reproduces_a_fresh_stream() {
    let x = signal(2000);
    let opts = opts_with("shibata");
    let mut b = budget();
    let spec_in = spec(48000, SampleFmt::F64, ChannelLayout::MONO);
    let spec_out = spec(48000, SampleFmt::S16, ChannelLayout::MONO);
    let mut rs = Resampler::new(&spec_in, &spec_out, &opts, &mut b).unwrap();
    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();

    let mut out1 = vec![0u8; x.len() * 2 + 64];
    {
        let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
        let mut dst = AudioMut::packed(SampleFmt::S16, 1, &mut out1).unwrap();
        rs.convert(Some(src), &mut dst).unwrap();
    }
    rs.reset();
    let mut out2 = vec![0u8; x.len() * 2 + 64];
    {
        let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
        let mut dst = AudioMut::packed(SampleFmt::S16, 1, &mut out2).unwrap();
        rs.convert(Some(src), &mut dst).unwrap();
    }
    assert_eq!(
        out1, out2,
        "reset did not clear the noise-shaping error history"
    );
}

// ── the spectral measurement plan 17 §B.6 calls for ─────────────────────

fn terhardt_ath_db(f_khz: f64) -> f64 {
    3.64 * f_khz.powf(-0.8) - 6.5 * (-0.6 * (f_khz - 3.3).powi(2)).exp() + 0.001 * f_khz.powi(4)
}

/// A direct O(N^2) power spectrum. `n` is kept modest (a few thousand) so
/// this stays a fraction of a second; it is a test, not a hot path.
fn power_spectrum(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let mut out = vec![0.0; n / 2];
    for (k, bin) in out.iter_mut().enumerate() {
        let mut re = 0.0;
        let mut im = 0.0;
        for (t, &xt) in x.iter().enumerate() {
            let ang = -2.0 * std::f64::consts::PI * (k as f64) * (t as f64) / n as f64;
            re += xt * ang.cos();
            im += xt * ang.sin();
        }
        *bin = re * re + im * im;
    }
    out
}

fn perceptually_weighted_power(spectrum: &[f64], fs: f64) -> f64 {
    let n2 = spectrum.len();
    let mut total = 0.0;
    for (k, &p) in spectrum.iter().enumerate() {
        let f_khz = (k as f64 * fs / (2.0 * n2 as f64)) / 1000.0;
        let ath = 10f64.powf(terhardt_ath_db(f_khz.max(0.02)) / 10.0);
        total += p / ath;
    }
    total
}

#[test]
fn perceptually_weighted_noise_is_lower_than_tpdf() {
    // Silence in: the s16 output *is* the dither/quantisation noise, with
    // nothing else in it. output_sample_bits=8 keeps the noise amplitude
    // large enough that a 4096-sample window resolves its spectral shape
    // cleanly.
    let n = 4096;
    let silence = vec![0.0_f64; n];
    let mut tpdf_opts = opts_with("triangular");
    tpdf_opts.output_sample_bits = 8;
    let tpdf = run_s16(&tpdf_opts, &silence);
    let tpdf_f64: Vec<f64> = tpdf.iter().map(|&v| f64::from(v)).collect();
    let tpdf_power = perceptually_weighted_power(&power_spectrum(&tpdf_f64), 48000.0);

    for name in ALL_NS {
        let mut o = opts_with(name);
        o.output_sample_bits = 8;
        let shaped = run_s16(&o, &silence);
        let shaped_f64: Vec<f64> = shaped.iter().map(|&v| f64::from(v)).collect();
        let shaped_power = perceptually_weighted_power(&power_spectrum(&shaped_f64), 48000.0);
        assert!(
            shaped_power < tpdf_power,
            "{name}: perceptually-weighted noise power {shaped_power:.1} is not below \
             plain TPDF's {tpdf_power:.1}"
        );
    }
}
