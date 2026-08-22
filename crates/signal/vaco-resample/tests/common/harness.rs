//! Shared helpers: drive a [`Resampler`] over `f64` mono/multichannel data.

#![allow(
    unreachable_pub,
    dead_code,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::unwrap_used,
    clippy::drop_non_drop,
    reason = "test support code; a panic here is a failing test, which is the point"
)]

use vaco_chlayout::ChannelLayout;
use vaco_limits::{Budget, Limits};
use vaco_resample::{AudioMut, AudioRef, AudioSpec, ResampleOptions, Resampler};
use vaco_sampfmt::SampleFmt;

pub fn budget() -> Budget {
    Budget::new(Limits::permissive())
}

pub fn spec(rate: u32, fmt: SampleFmt, layout: ChannelLayout) -> AudioSpec {
    AudioSpec::new(rate, fmt, layout).unwrap()
}

/// Convert interleaved `f64` through a resampler, feeding `chunk` samples at a
/// time and then flushing. Returns interleaved `f64`.
pub fn run_f64(
    rs: &mut Resampler,
    input: &[f64],
    in_channels: usize,
    out_channels: usize,
    chunk: usize,
) -> Vec<f64> {
    let frames = input.len() / in_channels;
    let mut out: Vec<f64> = Vec::new();
    let mut scratch = vec![0u8; 65536 * out_channels * 8];
    let mut pos = 0usize;
    let chunk = chunk.max(1);
    loop {
        let take = chunk.min(frames - pos);
        let bytes: Vec<u8> = input[pos * in_channels..(pos + take) * in_channels]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let src = AudioRef::packed(SampleFmt::F64, in_channels as u32, &bytes).unwrap();
        {
            let mut dst =
                AudioMut::packed(SampleFmt::F64, out_channels as u32, &mut scratch).unwrap();
            let n = rs.convert(Some(src), &mut dst).unwrap();
            drop(dst);
            push_f64(&mut out, &scratch, n * out_channels);
        }
        pos += take;
        if pos >= frames {
            break;
        }
    }
    // Drain, possibly over several calls.
    loop {
        let mut dst = AudioMut::packed(SampleFmt::F64, out_channels as u32, &mut scratch).unwrap();
        let n = rs.convert(None, &mut dst).unwrap();
        drop(dst);
        if n == 0 {
            break;
        }
        push_f64(&mut out, &scratch, n * out_channels);
    }
    out
}

fn push_f64(out: &mut Vec<f64>, bytes: &[u8], count: usize) {
    let (chunks, _) = bytes.as_chunks::<8>();
    for c in chunks.iter().take(count) {
        out.push(f64::from_le_bytes(*c));
    }
}

/// A simple, complete conversion with default options.
pub fn simple(
    in_rate: u32,
    out_rate: u32,
    layout_in: ChannelLayout,
    layout_out: ChannelLayout,
) -> Resampler {
    let mut b = budget();
    Resampler::new(
        &spec(in_rate, SampleFmt::F64, layout_in),
        &spec(out_rate, SampleFmt::F64, layout_out),
        &ResampleOptions::default(),
        &mut b,
    )
    .unwrap()
}

/// Signal-to-noise ratio of `got` against `want`, in dB. `f64::INFINITY` when
/// they are bit-identical.
pub fn snr(want: &[f64], got: &[f64]) -> f64 {
    let n = want.len().min(got.len());
    if n == 0 {
        return f64::NEG_INFINITY;
    }
    let mut sig = 0.0;
    let mut err = 0.0;
    for i in 0..n {
        sig += want[i] * want[i];
        let d = want[i] - got[i];
        err += d * d;
    }
    if err == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (sig / err).log10()
}

pub fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut m = 0.0_f64;
    for i in 0..n {
        m = m.max((a[i] - b[i]).abs());
    }
    m
}
