//! Streaming state: chunk invariance, delay, drain and reset.
//!
//! Chunk invariance is the highest-value test in the crate (plan 17 §B.11): a
//! resampler is stateful, and almost every state bug shows up as "the same
//! stream fed in different-sized pieces produces different output".

#![allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    clippy::cast_possible_wrap,
    clippy::drop_non_drop,
    clippy::field_reassign_with_default,
    clippy::redundant_closure_for_method_calls,
    clippy::collapsible_if,
    reason = "test and benchmark code; a panic here is a failing test, which is the point"
)]

#[path = "common/harness.rs"]
mod harness;

use harness::{max_abs_diff, run_f64, simple, spec};
use vaco_chlayout::ChannelLayout;
use vaco_limits::{Budget, Limits};
use vaco_resample::{AudioMut, AudioRef, DitherMethod, ResampleOptions, Resampler};
use vaco_sampfmt::SampleFmt;

fn signal(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let t = i as f64;
            (t * 0.031).sin() * 0.4 + (t * 0.211).sin() * 0.3 + (t * 0.0007).sin() * 0.2
        })
        .collect()
}

#[test]
fn chunking_does_not_change_the_output() {
    let x = signal(2000);
    for (ir, or) in [
        (44100u32, 48000u32),
        (48000, 44100),
        (48000, 96000),
        (96000, 48000),
        (8000, 44100),
        (192000, 48000),
    ] {
        let mut rs = simple(ir, or, ChannelLayout::MONO, ChannelLayout::MONO);
        let whole = run_f64(&mut rs, &x, 1, 1, x.len());
        for chunk in [1usize, 2, 7, 33, 256, 1024, 65536] {
            let mut rs = simple(ir, or, ChannelLayout::MONO, ChannelLayout::MONO);
            let piecewise = run_f64(&mut rs, &x, 1, 1, chunk);
            assert_eq!(
                whole.len(),
                piecewise.len(),
                "{ir} -> {or} chunk {chunk}: length"
            );
            assert_eq!(
                whole, piecewise,
                "{ir} -> {or} chunk {chunk}: output must be byte-identical"
            );
        }
    }
}

#[test]
fn chunking_does_not_change_a_downmix_or_a_dither() {
    let x: Vec<f64> = signal(6 * 900);
    let l51 = ChannelLayout::from_name("5.1").unwrap();
    let mut opts = ResampleOptions::default();
    opts.dither_method = DitherMethod::Triangular;
    let make = || {
        let mut b = Budget::new(Limits::permissive());
        Resampler::new(
            &spec(48000, SampleFmt::F64, l51.clone()),
            &spec(44100, SampleFmt::F64, ChannelLayout::STEREO),
            &opts,
            &mut b,
        )
        .unwrap()
    };
    let mut rs = make();
    let whole = run_f64(&mut rs, &x, 6, 2, 4096);
    for chunk in [1usize, 3, 64, 513] {
        let mut rs = make();
        let piecewise = run_f64(&mut rs, &x, 6, 2, chunk);
        assert_eq!(whole, piecewise, "chunk {chunk}");
    }
}

#[test]
fn dither_is_a_pure_function_of_position() {
    let x = signal(500);
    let mut opts = ResampleOptions::default();
    opts.dither_method = DitherMethod::TriangularHighpass;
    let make = |o: &ResampleOptions| {
        let mut b = Budget::new(Limits::permissive());
        Resampler::new(
            &spec(48000, SampleFmt::F64, ChannelLayout::MONO),
            &spec(48000, SampleFmt::S16, ChannelLayout::MONO),
            o,
            &mut b,
        )
        .unwrap()
    };
    let mut a = make(&opts);
    let mut b = make(&opts);
    let mut out_a = vec![0u8; 4096];
    let mut out_b = vec![0u8; 4096];
    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    let n_a = {
        let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
        let mut dst = AudioMut::packed(SampleFmt::S16, 1, &mut out_a).unwrap();
        a.convert(Some(src), &mut dst).unwrap()
    };
    let n_b = {
        // Same stream, two calls.
        let (h, t) = bytes.split_at(200 * 8);
        let mut written = 0;
        for part in [h, t] {
            let src = AudioRef::packed(SampleFmt::F64, 1, part).unwrap();
            let mut dst = AudioMut::packed(SampleFmt::S16, 1, &mut out_b[written * 2..]).unwrap();
            written += b.convert(Some(src), &mut dst).unwrap();
        }
        written
    };
    assert_eq!(n_a, n_b);
    assert_eq!(out_a[..n_a * 2], out_b[..n_b * 2]);

    // A different seed must actually change the noise.
    let mut o2 = opts;
    o2.dither_seed = 12345;
    let mut c = make(&o2);
    let mut out_c = vec![0u8; 4096];
    let n_c = {
        let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
        let mut dst = AudioMut::packed(SampleFmt::S16, 1, &mut out_c).unwrap();
        c.convert(Some(src), &mut dst).unwrap()
    };
    assert_eq!(n_a, n_c);
    assert_ne!(out_a[..n_a * 2], out_c[..n_c * 2], "seed must matter");
}

#[test]
fn a_small_output_buffer_is_drained_over_several_calls() {
    let x = signal(1000);
    let mut rs = simple(44100, 48000, ChannelLayout::MONO, ChannelLayout::MONO);
    let want = {
        let mut rs2 = simple(44100, 48000, ChannelLayout::MONO, ChannelLayout::MONO);
        run_f64(&mut rs2, &x, 1, 1, 1024)
    };
    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut got: Vec<f64> = Vec::new();
    let mut small = vec![0u8; 7 * 8];
    let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
    let mut first = Some(src);
    loop {
        let n = {
            let mut dst = AudioMut::packed(SampleFmt::F64, 1, &mut small).unwrap();
            rs.convert(first.take(), &mut dst).unwrap()
        };
        if n == 0 {
            break;
        }
        for c in small.as_chunks::<8>().0.iter().take(n) {
            got.push(f64::from_le_bytes(*c));
        }
    }
    assert_eq!(got.len(), want.len());
    assert_eq!(got, want);
}

#[test]
fn reset_starts_a_new_stream() {
    let x = signal(400);
    let mut rs = simple(44100, 48000, ChannelLayout::MONO, ChannelLayout::MONO);
    let a = run_f64(&mut rs, &x, 1, 1, 128);
    rs.reset();
    let b = run_f64(&mut rs, &x, 1, 1, 128);
    assert_eq!(a, b, "reset must return to the initial state exactly");
}

#[test]
fn constant_input_gives_constant_output() {
    // The reference mirrors at both stream ends rather than zero-priming, so a
    // DC input has no fade-in and no fade-out. This is the test that catches a
    // zero-primed implementation.
    for (ir, or) in [(48000u32, 96000u32), (44100, 48000), (48000, 44100)] {
        let x = vec![0.75_f64; 600];
        let mut rs = simple(ir, or, ChannelLayout::MONO, ChannelLayout::MONO);
        let y = run_f64(&mut rs, &x, 1, 1, 4096);
        let worst = y.iter().map(|v| (v - 0.75).abs()).fold(0.0_f64, f64::max);
        assert!(
            worst < 2e-5,
            "{ir} -> {or}: DC gain deviates by {worst:e} (zero-priming would show a fade-in)"
        );
    }
}

#[test]
fn same_rate_and_layout_is_the_identity() {
    let x = signal(300);
    let mut rs = simple(48000, 48000, ChannelLayout::STEREO, ChannelLayout::STEREO);
    assert_eq!(rs.internal(), "none", "no work means the direct path");
    let y = run_f64(&mut rs, &x, 2, 2, 64);
    assert_eq!(max_abs_diff(&x, &y), 0.0);
    assert_eq!(y.len(), x.len());
}

#[test]
fn delay_is_reported_and_shrinks_on_drain() {
    let x = signal(4000);
    let mut rs = simple(44100, 48000, ChannelLayout::MONO, ChannelLayout::MONO);
    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut out = vec![0u8; 8 * 8192];
    let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
    {
        let mut dst = AudioMut::packed(SampleFmt::F64, 1, &mut out).unwrap();
        rs.convert(Some(src), &mut dst).unwrap();
    }
    let held = rs.delay(44100);
    assert!(held > 0, "the filter must hold its group delay");
    assert!(held < 100, "delay {held} is implausibly large");
    loop {
        let mut dst = AudioMut::packed(SampleFmt::F64, 1, &mut out).unwrap();
        if rs.convert(None, &mut dst).unwrap() == 0 {
            break;
        }
    }
    assert_eq!(rs.delay(44100), 0, "a fully drained filter holds nothing");
}

#[test]
fn zero_length_and_single_sample_calls_are_safe() {
    let mut rs = simple(44100, 48000, ChannelLayout::MONO, ChannelLayout::MONO);
    let mut out = vec![0u8; 1024];
    for n in [0usize, 1, 0, 1, 2, 0] {
        let bytes = vec![0u8; n * 8];
        let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
        let mut dst = AudioMut::packed(SampleFmt::F64, 1, &mut out).unwrap();
        rs.convert(Some(src), &mut dst).unwrap();
    }
    let mut dst = AudioMut::packed(SampleFmt::F64, 1, &mut out).unwrap();
    rs.convert(None, &mut dst).unwrap();
}

#[test]
fn a_stream_shorter_than_the_filter_still_drains() {
    for n in 1usize..40 {
        let mut rs = simple(44100, 48000, ChannelLayout::MONO, ChannelLayout::MONO);
        let x = signal(n);
        let y = run_f64(&mut rs, &x, 1, 1, 4096);
        let want = (n as u64 * 160).div_ceil(147) as usize;
        assert_eq!(y.len(), want, "{n} input samples");
        assert!(y.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn mismatched_buffers_are_rejected_not_ignored() {
    let mut rs = simple(48000, 48000, ChannelLayout::STEREO, ChannelLayout::MONO);
    let bytes = vec![0u8; 16];
    let mut out = vec![0u8; 16];
    // Wrong input format.
    let src = AudioRef::packed(SampleFmt::F32, 2, &bytes).unwrap();
    let mut dst = AudioMut::packed(SampleFmt::F64, 1, &mut out).unwrap();
    assert!(rs.convert(Some(src), &mut dst).is_err());
}

/// A degenerate configuration must be refused at construction, not attempted.
///
/// `taps = ceil(filter_size / factor)` and `factor` shrinks with both the
/// downsampling ratio and `cutoff`, so the two multiply. The fuzzer found this
/// as a slow unit rather than a crash: a bank that fits the allocation cap can
/// still take a fifth of a second to *fill*, because every coefficient is a
/// Bessel series. Both a tap cap and a fuel charge now stand in front of it.
#[test]
fn a_degenerate_filter_is_refused_rather_than_attempted() {
    let mut opts = ResampleOptions::default();
    opts.filter_size = 65536;
    opts.cutoff = 0.004;
    let mut b = Budget::new(Limits::strict());
    let err = Resampler::new(
        &spec(192_000, SampleFmt::F32, ChannelLayout::MONO),
        &spec(8000, SampleFmt::F32, ChannelLayout::MONO),
        &opts,
        &mut b,
    );
    assert!(err.is_err(), "an eight-million-tap filter must be refused");

    // The same shape under a permissive budget is still refused by the tap cap.
    let mut b = Budget::new(Limits::permissive());
    assert!(
        Resampler::new(
            &spec(192_000, SampleFmt::F32, ChannelLayout::MONO),
            &spec(8000, SampleFmt::F32, ChannelLayout::MONO),
            &opts,
            &mut b,
        )
        .is_err()
    );

    // And an ordinary configuration is not caught by either guard.
    let mut b = Budget::new(Limits::strict());
    assert!(
        Resampler::new(
            &spec(44100, SampleFmt::F32, ChannelLayout::STEREO),
            &spec(48000, SampleFmt::S16, ChannelLayout::STEREO),
            &ResampleOptions::default(),
            &mut b,
        )
        .is_ok()
    );
}

/// Up-mixing puts the rematrix stage *after* the resampler, which is the other
/// half of `Pipeline::stage`. It is easy for one branch to work and the other
/// to rot, so both orderings are exercised end to end.
#[test]
fn upmix_after_resampling_is_chunk_invariant_and_correctly_placed() {
    let x = signal(700);
    let l51 = ChannelLayout::from_name("5.1").unwrap();
    let make = || {
        let mut b = Budget::new(Limits::permissive());
        Resampler::new(
            &spec(44100, SampleFmt::F64, ChannelLayout::MONO),
            &spec(48000, SampleFmt::F64, l51.clone()),
            &ResampleOptions::default(),
            &mut b,
        )
        .unwrap()
    };
    let mut rs = make();
    let whole = run_f64(&mut rs, &x, 1, 6, 4096);
    assert_eq!(whole.len() % 6, 0);
    assert_eq!(whole.len() / 6, (700 * 160usize).div_ceil(147));
    for chunk in [1usize, 5, 128] {
        let mut rs = make();
        assert_eq!(whole, run_f64(&mut rs, &x, 1, 6, chunk), "chunk {chunk}");
    }
    // MEASURED: `mono -> 5.1` is a direct copy into FC at gain 1.0 and silence
    // everywhere else. Mono's one channel *is* `FrontCenter`, so it finds a home
    // by name and the mono upmix rule never fires. `mono -> stereo` is the case
    // where it does, because stereo has no FC.
    let mono_only = run_f64(&mut make(), &x, 1, 6, 4096);
    for (i, frame) in mono_only.chunks_exact(6).enumerate().skip(40).take(20) {
        assert_eq!(frame[0], 0.0, "frame {i}: FL is not synthesised");
        assert_eq!(frame[1], 0.0, "frame {i}: FR is not synthesised");
        assert_eq!(frame[3], 0.0, "frame {i}: LFE is not synthesised");
        assert_eq!(frame[4], 0.0, "frame {i}: BL is not synthesised");
        assert!(frame[2].abs() > 0.0, "frame {i}: FC carries the signal");
    }
}

/// A rate ratio no real conversion uses is refused, because the cost is in the
/// output count and nothing else bounds it.
///
/// The fuzzer found `8 Hz -> 335872 Hz` at `filter_size = 8192`: eight input
/// samples, 335 872 output samples, each an 8192-tap convolution, 47.7 seconds.
/// The coefficient bank is 32 KB and the phase count is 1, so neither the
/// allocation budget nor the tap cap sees anything unusual.
#[test]
fn an_absurd_rate_ratio_is_refused() {
    let mut opts = ResampleOptions::default();
    opts.filter_size = 8192;
    opts.exact_rational = false;
    opts.phase_shift = 0;
    let mut b = Budget::new(Limits::permissive());
    let err = Resampler::new(
        &spec(8, SampleFmt::F32, ChannelLayout::MONO),
        &spec(335_872, SampleFmt::F32, ChannelLayout::MONO),
        &opts,
        &mut b,
    );
    assert!(err.is_err(), "a 41984:1 ratio must be refused");

    // The widest ratio the permissive limits admit at all still works.
    let mut b = Budget::new(Limits::permissive());
    assert!(
        Resampler::new(
            &spec(8000, SampleFmt::F32, ChannelLayout::MONO),
            &spec(2_822_400, SampleFmt::F32, ChannelLayout::MONO),
            &ResampleOptions::default(),
            &mut b,
        )
        .is_ok(),
        "8 kHz -> 2822.4 kHz is 352.8:1 and is a real conversion"
    );
}
