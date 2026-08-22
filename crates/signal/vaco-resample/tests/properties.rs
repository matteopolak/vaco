//! Property tests. The invariants, rather than particular values.

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

use harness::{budget, run_f64, simple, spec};
use proptest::prelude::*;
use vaco_chlayout::ChannelLayout;
use vaco_resample::convert::elem;
use vaco_resample::mix::{MixLevels, build_matrix};
use vaco_resample::{MatrixEncoding, ResampleOptions, Resampler};
use vaco_sampfmt::SampleFmt;

const RATES: [u32; 12] = [
    8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000, 192_000,
];

fn layouts() -> Vec<(&'static str, ChannelLayout)> {
    ChannelLayout::standard().collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// The invariant plan 17 §B.11 calls the highest-value test in the crate.
    #[test]
    fn chunking_never_changes_the_output(
        ri in 0usize..RATES.len(),
        ro in 0usize..RATES.len(),
        chunks in prop::collection::vec(1usize..=200, 1..12),
        len in 50usize..900,
    ) {
        let (ir, or) = (RATES[ri], RATES[ro]);
        let x: Vec<f64> = (0..len)
            .map(|i| ((i as f64) * 0.017).sin() * 0.6 + ((i as f64) * 0.31).cos() * 0.3)
            .collect();
        let mut rs = simple(ir, or, ChannelLayout::MONO, ChannelLayout::MONO);
        let whole = run_f64(&mut rs, &x, 1, 1, x.len().max(1));

        // Feed the same stream with a varying chunk schedule.
        let mut rs = simple(ir, or, ChannelLayout::MONO, ChannelLayout::MONO);
        let mut got: Vec<f64> = Vec::new();
        let mut pos = 0usize;
        let mut ci = 0usize;
        let mut out = vec![0u8; 1 << 20];
        while pos < x.len() {
            let take = chunks[ci % chunks.len()].min(x.len() - pos);
            ci += 1;
            let bytes: Vec<u8> = x[pos..pos + take]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            let src = vaco_resample::AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
            let n = {
                let mut dst =
                    vaco_resample::AudioMut::packed(SampleFmt::F64, 1, &mut out).unwrap();
                rs.convert(Some(src), &mut dst).unwrap()
            };
            for c in out.as_chunks::<8>().0.iter().take(n) {
                got.push(f64::from_le_bytes(*c));
            }
            pos += take;
        }
        loop {
            let n = {
                let mut dst =
                    vaco_resample::AudioMut::packed(SampleFmt::F64, 1, &mut out).unwrap();
                rs.convert(None, &mut dst).unwrap()
            };
            if n == 0 { break; }
            for c in out.as_chunks::<8>().0.iter().take(n) {
                got.push(f64::from_le_bytes(*c));
            }
        }
        prop_assert_eq!(whole.len(), got.len());
        prop_assert_eq!(whole, got);
    }

    /// `out_samples` must be an upper bound, never an under-estimate: callers
    /// size buffers with it.
    #[test]
    fn out_samples_is_an_upper_bound(
        ri in 0usize..RATES.len(),
        ro in 0usize..RATES.len(),
        len in 1usize..500,
    ) {
        let (ir, or) = (RATES[ri], RATES[ro]);
        let mut rs = simple(ir, or, ChannelLayout::MONO, ChannelLayout::MONO);
        let bound = rs.out_samples(len);
        let x = vec![0.0_f64; len];
        let produced = run_f64(&mut rs, &x, 1, 1, len).len();
        prop_assert!(
            produced <= bound,
            "produced {} but out_samples promised at most {}",
            produced,
            bound
        );
    }

    /// A constant signal survives any rate conversion at the same level. This
    /// is what the mirrored edges and the phase-0 normalisation exist for; a
    /// zero-primed or badly normalised bank fails it immediately.
    #[test]
    fn dc_gain_is_unity(
        ri in 0usize..RATES.len(),
        ro in 0usize..RATES.len(),
        level in -1.0f64..1.0,
    ) {
        let (ir, or) = (RATES[ri], RATES[ro]);
        let mut rs = simple(ir, or, ChannelLayout::MONO, ChannelLayout::MONO);
        let y = run_f64(&mut rs, &vec![level; 800], 1, 1, 4096);
        for v in &y {
            prop_assert!(
                (v - level).abs() < 1e-4,
                "DC {} became {}",
                level,
                v
            );
        }
    }

    /// After normalisation for an integer output, no output row can overflow
    /// full scale for any full-scale input.
    #[test]
    fn integer_matrices_cannot_clip(
        a in 0usize..40,
        b in 0usize..40,
        clev in -4.0f32..4.0,
        slev in -4.0f32..4.0,
        lfe in -4.0f32..4.0,
    ) {
        let ls = layouts();
        let (Some((_, li)), Some((_, lo))) = (ls.get(a % ls.len()), ls.get(b % ls.len())) else {
            return Ok(());
        };
        let levels = MixLevels {
            center: clev,
            surround: slev,
            lfe,
            rematrix_volume: 1.0,
            rematrix_maxval: 0.0,
        };
        let m = build_matrix(li, lo, &levels, MatrixEncoding::None, true).unwrap();
        prop_assert!(
            m.peak() <= 1.0 + 1e-12,
            "integer matrix peak {} exceeds full scale",
            m.peak()
        );
    }

    /// Rematrixing a layout to itself is the identity, whatever the mix levels.
    #[test]
    fn layout_to_itself_is_the_identity(
        a in 0usize..40,
        clev in -4.0f32..4.0,
        slev in -4.0f32..4.0,
    ) {
        let ls = layouts();
        let Some((_, l)) = ls.get(a % ls.len()) else { return Ok(()) };
        let levels = MixLevels { center: clev, surround: slev, ..MixLevels::default() };
        let m = build_matrix(l, l, &levels, MatrixEncoding::None, false).unwrap();
        for o in 0..m.rows {
            for i in 0..m.cols {
                prop_assert_eq!(m.get(o, i), if o == i { 1.0 } else { 0.0 });
            }
        }
    }

    /// `f32 -> s16 -> f32` lands within half a quantisation step.
    #[test]
    fn float_to_s16_round_trip_is_within_half_an_lsb(x in -1.0f32..1.0) {
        let back = elem::i16_to_f32(elem::f32_to_i16(x));
        prop_assert!(
            (back - x).abs() <= 0.5 / 32768.0 + f32::EPSILON,
            "{} came back as {}",
            x,
            back
        );
    }

    /// Every element converter is total: no input panics, and integer results
    /// stay in range.
    #[test]
    fn element_converters_are_total(bits in any::<u64>()) {
        let f = f64::from_bits(bits);
        let g = f32::from_bits(bits as u32);
        let _ = elem::f64_to_i16(f);
        let _ = elem::f64_to_i32(f);
        let _ = elem::f64_to_i64(f);
        let _ = elem::f64_to_u8(f);
        let _ = elem::f32_to_i16(g);
        let _ = elem::f32_to_i32(g);
        let _ = elem::f32_to_i64(g);
        let _ = elem::f32_to_u8(g);
        let i = bits as i64;
        prop_assert!(i64::from(elem::i64_to_i32(i)) == i >> 32);
    }

    /// Constructing a resampler over arbitrary rates, formats and channel
    /// counts either succeeds or returns an error. It never panics.
    #[test]
    fn construction_never_panics(
        ir in 0u32..200_000,
        or in 0u32..200_000,
        fi in 0usize..12,
        fo in 0usize..12,
        ci in 0u32..40,
        co in 0u32..40,
        filter_size in -8i32..200,
        phase_shift in -4i32..30,
    ) {
        let fmts = SampleFmt::ALL;
        let (a, b) = (fmts[fi % 12], fmts[fo % 12]);
        let mut opts = ResampleOptions::default();
        opts.filter_size = filter_size;
        opts.phase_shift = phase_shift;
        let mut bud = budget();
        let li = ChannelLayout::default_for(ci)
            .unwrap_or_else(|| ChannelLayout::unspecified(ci.max(1)));
        let lo = ChannelLayout::default_for(co)
            .unwrap_or_else(|| ChannelLayout::unspecified(co.max(1)));
        let (Ok(si), Ok(so)) = (
            vaco_resample::AudioSpec::new(ir, a, li),
            vaco_resample::AudioSpec::new(or, b, lo),
        ) else {
            return Ok(());
        };
        let _ = Resampler::new(&si, &so, &opts, &mut bud);
    }
}

#[test]
fn every_layout_pair_builds_a_finite_matrix() {
    // 40 x 40 is small enough to be exhaustive, and exhaustive is better than
    // sampled for a table this size.
    let levels = MixLevels::default();
    for (na, la) in layouts() {
        for (nb, lb) in layouts() {
            for int_out in [false, true] {
                let m = build_matrix(&la, &lb, &levels, MatrixEncoding::None, int_out)
                    .unwrap_or_else(|e| panic!("{na} -> {nb}: {e}"));
                assert_eq!(m.rows, lb.channels as usize, "{na} -> {nb}");
                assert_eq!(m.cols, la.channels as usize, "{na} -> {nb}");
                assert!(
                    m.as_slice().iter().all(|v| v.is_finite()),
                    "{na} -> {nb}: non-finite coefficient"
                );
                if int_out {
                    assert!(m.peak() <= 1.0 + 1e-12, "{na} -> {nb}: peak {}", m.peak());
                }
            }
        }
    }
}

#[test]
fn every_format_pair_converts_without_panicking() {
    for a in SampleFmt::ALL {
        for b in SampleFmt::ALL {
            let mut bud = budget();
            let rs = Resampler::new(
                &spec(48000, a, ChannelLayout::STEREO),
                &spec(48000, b, ChannelLayout::STEREO),
                &ResampleOptions::default(),
                &mut bud,
            );
            assert!(rs.is_ok(), "{a} -> {b}");
        }
    }
}
