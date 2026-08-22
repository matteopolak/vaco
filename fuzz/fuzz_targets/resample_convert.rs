//! The whole resampler driven by an arbitrary configuration and an arbitrary
//! chunk schedule.
//!
//! Nothing here parses a bitstream, but every input is still attacker-chosen in
//! practice: rates and channel counts come out of a container header, the option
//! values come off a command line, and the sample bytes are decoder output. The
//! bug classes reachable in safe Rust are a panic, an unbounded allocation, an
//! arithmetic overflow (this profile turns the checks on) and non-termination —
//! and a resampler has a specific way to hit all four, because its buffer sizes
//! and loop trip counts are derived from a *ratio* of two attacker-chosen
//! integers.
//!
//! Degenerate rates are the interesting region: `1 Hz -> 2^31-1 Hz` asks for a
//! bank with two billion phases, and `filter_size` interacts with the
//! downsampling stretch (`taps = ceil(filter_size / factor)`) so a tiny cutoff
//! and a large ratio multiply into an enormous filter. Both must be refused by
//! the budget rather than attempted.
//!
//! Beyond "does not crash", one real invariant is asserted: `out_samples` is a
//! bound callers size buffers with, so producing more than it promised is a
//! heap overflow in any caller that trusted it.
//!
//! # Diagnosing a slow unit
//!
//! `cargo fuzz` exits 0 on a slow unit, so the artifact on disk is the only
//! evidence it happened. Both slow units this target found were real
//! denial-of-service surfaces. `RESAMPLE_FUZZ_DEBUG=1` prints the decoded
//! `Config` for every input, which is how to turn an artifact hash back into
//! the configuration that caused it:
//!
//! ```text
//! RESAMPLE_FUZZ_DEBUG=1 cargo +nightly fuzz run resample_convert --features resample \
//!     fuzz/artifacts/resample_convert/slow-unit-<hash>
//! ```
//! fuzz-crate: vaco-resample
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_chlayout::ChannelLayout;
use vaco_limits::{Budget, Limits};
use vaco_resample::{AudioMut, AudioRef, AudioSpec, ResampleOptions, Resampler};
use vaco_sampfmt::SampleFmt;

/// Bound the work per case. The point is to reach many configurations, not to
/// resample a long stream in any one of them.
const MAX_FRAMES: usize = 4096;
const MAX_CHUNKS: usize = 24;

#[derive(Arbitrary, Debug)]
struct Config {
    in_rate: u32,
    out_rate: u32,
    in_fmt: u8,
    out_fmt: u8,
    in_channels: u8,
    out_channels: u8,
    filter_size: i16,
    phase_shift: i8,
    kaiser_beta: u8,
    cutoff: u8,
    filter_type: u8,
    dither: u8,
    exact_rational: bool,
    linear_interp: bool,
    planar_in: bool,
    /// Chunk sizes, cycled; an empty schedule means one call with everything.
    chunks: Vec<u8>,
    samples: Vec<u8>,
}

fn fmt_of(i: u8) -> SampleFmt {
    let all = SampleFmt::ALL;
    all[(i as usize) % all.len()]
}

fn layout_of(n: u8) -> ChannelLayout {
    let n = u32::from(n % 33);
    ChannelLayout::default_for(n).unwrap_or_else(|| ChannelLayout::unspecified(n.max(1)))
}

fuzz_target!(|cfg: Config| {
    if std::env::var_os("RESAMPLE_FUZZ_DEBUG").is_some() { eprintln!("CFG {cfg:?}"); }
    let mut opts = ResampleOptions::default();
    opts.filter_size = i32::from(cfg.filter_size);
    opts.phase_shift = i32::from(cfg.phase_shift);
    opts.kaiser_beta = 2.0 + f64::from(cfg.kaiser_beta) * (14.0 / 255.0);
    opts.cutoff = f64::from(cfg.cutoff) / 255.0;
    opts.exact_rational = cfg.exact_rational;
    opts.linear_interp = cfg.linear_interp;
    opts.filter_type = match cfg.filter_type % 3 {
        0 => vaco_resample::FilterType::Kaiser,
        1 => vaco_resample::FilterType::BlackmanNuttall,
        _ => vaco_resample::FilterType::Cubic,
    };
    opts.dither_method = match cfg.dither % 4 {
        0 => vaco_resample::DitherMethod::None,
        1 => vaco_resample::DitherMethod::Rectangular,
        2 => vaco_resample::DitherMethod::Triangular,
        _ => vaco_resample::DitherMethod::TriangularHighpass,
    };

    let in_fmt = fmt_of(cfg.in_fmt);
    let out_fmt = fmt_of(cfg.out_fmt);
    let in_layout = layout_of(cfg.in_channels);
    let out_layout = layout_of(cfg.out_channels);
    let in_ch = in_layout.channels as usize;
    let out_ch = out_layout.channels as usize;

    let (Ok(si), Ok(so)) = (
        AudioSpec::new(cfg.in_rate, in_fmt, in_layout),
        AudioSpec::new(cfg.out_rate, out_fmt, out_layout),
    ) else {
        return;
    };

    // A tight budget is the point: a degenerate ratio must be refused here, not
    // absorbed by an allocation nobody sized.
    let mut budget = Budget::new(Limits::strict());
    let Ok(mut rs) = Resampler::new(&si, &so, &opts, &mut budget) else {
        return;
    };

    let bytes_in = in_fmt.bytes_per_sample();
    let bytes_out = out_fmt.bytes_per_sample();
    let frame_in = bytes_in * if in_fmt.is_planar() { 1 } else { in_ch };
    let frames = (cfg.samples.len() / frame_in.max(1)).min(MAX_FRAMES);
    if frames == 0 {
        return;
    }

    // Source planes: one interleaved block, or one block per channel.
    let plane_len = frames * bytes_in;
    let src_store: Vec<Vec<u8>> = if in_fmt.is_planar() && cfg.planar_in {
        (0..in_ch)
            .map(|c| {
                let start = (c * plane_len) % cfg.samples.len().max(1);
                let mut v = vec![0u8; plane_len];
                for (i, b) in v.iter_mut().enumerate() {
                    *b = cfg.samples[(start + i) % cfg.samples.len()];
                }
                v
            })
            .collect()
    } else if in_fmt.is_planar() {
        // The format is planar but the config asked for packed; skip rather
        // than lying to the constructor, which would only exercise its check.
        return;
    } else {
        vec![cfg.samples[..frames * frame_in].to_vec()]
    };

    let mut out_store: Vec<Vec<u8>> = if out_fmt.is_planar() {
        (0..out_ch).map(|_| vec![0u8; 8192 * bytes_out]).collect()
    } else {
        vec![vec![0u8; 8192 * bytes_out * out_ch]]
    };
    let out_capacity = if out_fmt.is_planar() { 8192 } else { 8192 };

    let mut pos = 0usize;
    let mut ci = 0usize;
    let mut guard = 0u32;
    while pos < frames {
        guard += 1;
        assert!(guard < 100_000, "convert loop failed to make progress");
        let take = if cfg.chunks.is_empty() {
            frames - pos
        } else {
            let c = usize::from(cfg.chunks[ci % cfg.chunks.len().min(MAX_CHUNKS)]);
            ci += 1;
            c.max(1).min(frames - pos)
        };
        let promised = rs.out_samples(take);
        // `frame_in` is the per-plane stride of one frame: `bytes_in` for a
        // planar plane, `bytes_in * channels` for the interleaved block. Using
        // it on BOTH ends matters — an earlier version multiplied only the end
        // by the channel count, which fed five frames while telling
        // `out_samples` it was feeding one, and then blamed the crate for the
        // difference. The harness between you and the answer has opinions.
        let refs: Vec<&[u8]> = src_store
            .iter()
            .map(|p| &p[pos * frame_in..(pos + take) * frame_in])
            .collect();
        let src = if in_fmt.is_planar() {
            AudioRef::planar(in_fmt, &refs)
        } else {
            AudioRef::packed(in_fmt, in_ch as u32, refs[0])
        };
        let Ok(src) = src else { return };
        let n = if out_fmt.is_planar() {
            let mut split: Vec<&mut [u8]> = out_store.iter_mut().map(Vec::as_mut_slice).collect();
            let Ok(mut dst) = AudioMut::planar(out_fmt, &mut split) else {
                return;
            };
            rs.convert(Some(src), &mut dst)
        } else {
            let Ok(mut dst) = AudioMut::packed(out_fmt, out_ch as u32, &mut out_store[0]) else {
                return;
            };
            rs.convert(Some(src), &mut dst)
        };
        let Ok(n) = n else { return };
        assert!(n <= out_capacity, "wrote {n} samples into {out_capacity}");
        assert!(
            n <= promised,
            "produced {n} samples but out_samples promised at most {promised}"
        );
        pos += take;
    }

    // Drain. This must terminate.
    for _ in 0..10_000 {
        let n = if out_fmt.is_planar() {
            let mut split: Vec<&mut [u8]> = out_store.iter_mut().map(Vec::as_mut_slice).collect();
            let Ok(mut dst) = AudioMut::planar(out_fmt, &mut split) else {
                return;
            };
            rs.convert(None, &mut dst)
        } else {
            let Ok(mut dst) = AudioMut::packed(out_fmt, out_ch as u32, &mut out_store[0]) else {
                return;
            };
            rs.convert(None, &mut dst)
        };
        match n {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
    panic!("drain did not terminate");
});
