//! Partitioned (uniform block, overlap-add) FIR convolution: the last of
//! plan `16-filters.md` §4.1's `vaco-filter-adsp` row that had no caller
//! yet ([`crate`]'s own doc names it explicitly). Intended for the FIR
//! reverb/HRTF-style filters plan `16-filters.md` names but has not built
//! yet (`headphone`'s own HRTF convolution, `afir` if it lands) — a long
//! impulse response processed in fixed-size blocks with the input-to-output
//! delay of exactly one block, not the whole filter length a single giant
//! FFT would cost.
//!
//! # Algorithm
//!
//! Standard "Uniform Partitioned Overlap-Add" (Gardner, *Efficient
//! Convolution Without Input-Output Delay*, AES 1995 — a published academic
//! method, not read from any reference implementation): split the impulse
//! response into `P` partitions of `block` samples each, FFT every
//! partition once at construction (size `2*block`, so a partition's linear
//! convolution with one input block fits with no time-domain aliasing).
//! Each new input block is FFT'd once and pushed onto a frequency-domain
//! delay line of the last `P` blocks; every partition's spectrum multiplies
//! the delay line's matching (correctly time-shifted) input spectrum, the
//! products sum, and one inverse FFT produces that block's output —
//! `O(P)` transform-domain multiplies and exactly 2 FFTs per block,
//! regardless of how long the filter is.

use std::collections::VecDeque;
use std::sync::Arc;

use vaco_tx::{Plan, Tx};

/// A streaming partitioned FIR convolver for one channel.
#[derive(Debug)]
pub struct PartitionedFir {
    block: usize,
    fft_len: usize,
    fwd: Tx<f64>,
    inv: Tx<f64>,
    /// One spectrum per impulse-response partition, oldest-tap-first.
    partitions: Vec<Vec<f64>>,
    /// Spectra of the last `partitions.len()` input blocks, most recent
    /// first — the "frequency-domain delay line".
    history: VecDeque<Vec<f64>>,
    /// The tail of the previous block's linear convolution, still to be
    /// added into this block's head (the "overlap" of overlap-add).
    overlap: Vec<f64>,
}

impl PartitionedFir {
    /// Build a convolver for `kernel` (the impulse response, any length),
    /// processing `block`-sample input blocks. `block` must be nonzero and
    /// `kernel` non-empty.
    #[must_use]
    pub fn new(kernel: &[f64], block: usize) -> Option<Self> {
        if block == 0 || kernel.is_empty() {
            return None;
        }
        let fft_len = block.saturating_mul(2);
        let plan_fwd: Arc<Plan<f64>> = Plan::fft(fft_len, false).ok()?;
        let plan_inv: Arc<Plan<f64>> = Plan::fft(fft_len, true).ok()?;
        let mut fwd = Tx::new(plan_fwd);
        let inv = Tx::new(plan_inv);

        let num_partitions = kernel.len().div_ceil(block);
        let mut partitions = Vec::new();
        for p in 0..num_partitions {
            let mut time = vec![0.0f64; fft_len.saturating_mul(2)];
            for i in 0..block {
                let Some(&tap) = kernel.get(p.saturating_mul(block).saturating_add(i)) else {
                    break;
                };
                if let Some(slot) = time.get_mut(2 * i) {
                    *slot = tap;
                }
            }
            let mut freq = vec![0.0f64; fft_len.saturating_mul(2)];
            fwd.execute(&mut freq, &time);
            partitions.push(freq);
        }

        let history = VecDeque::from(vec![vec![0.0f64; fft_len.saturating_mul(2)]; num_partitions]);
        Some(Self {
            block,
            fft_len,
            fwd,
            inv,
            partitions,
            history,
            overlap: vec![0.0f64; block],
        })
    }

    /// The block size this instance was built for.
    #[must_use]
    pub const fn block_size(&self) -> usize {
        self.block
    }

    /// Convolve one `block`-sample input block, returning that many output
    /// samples (the delayed, overlap-added result of the *whole* filter
    /// applied so far — a `block`-sample latency after the first call, not
    /// the full filter length).
    ///
    /// `input` shorter than `block` is zero-padded; longer is truncated —
    /// a caller with a partial final block should pad it itself if it wants
    /// the tail's own contribution reported explicitly, but truncating
    /// rather than panicking keeps every call infallible.
    #[must_use]
    pub fn process(&mut self, input: &[f64]) -> Vec<f64> {
        let mut time = vec![0.0f64; self.fft_len.saturating_mul(2)];
        for (i, slot) in time.iter_mut().step_by(2).take(self.block).enumerate() {
            *slot = input.get(i).copied().unwrap_or(0.0);
        }
        let mut freq = vec![0.0f64; self.fft_len.saturating_mul(2)];
        self.fwd.execute(&mut freq, &time);
        self.history.push_front(freq);
        self.history.truncate(self.partitions.len().max(1));

        let mut acc = vec![0.0f64; self.fft_len.saturating_mul(2)];
        for (p, h) in self.partitions.iter().enumerate() {
            let Some(x) = self.history.get(p) else { continue };
            for k in 0..self.fft_len {
                let (hr, hi) = (h.get(2 * k).copied().unwrap_or(0.0), h.get(2 * k + 1).copied().unwrap_or(0.0));
                let (xr, xi) = (x.get(2 * k).copied().unwrap_or(0.0), x.get(2 * k + 1).copied().unwrap_or(0.0));
                // Complex multiply-accumulate: (hr + i*hi) * (xr + i*xi).
                if let Some(re) = acc.get_mut(2 * k) {
                    *re += hr.mul_add(xr, -(hi * xi));
                }
                if let Some(im) = acc.get_mut(2 * k + 1) {
                    *im += hr.mul_add(xi, hi * xr);
                }
            }
        }

        let mut time_out = vec![0.0f64; self.fft_len.saturating_mul(2)];
        self.inv.execute(&mut time_out, &acc);
        #[allow(clippy::cast_precision_loss, reason = "fft_len is a block-derived count, far below 2^53")]
        let norm = 1.0 / self.fft_len as f64;

        let mut out = vec![0.0f64; self.block];
        for (i, slot) in out.iter_mut().enumerate() {
            let head = time_out.get(2 * i).copied().unwrap_or(0.0) * norm;
            *slot = head + self.overlap.get(i).copied().unwrap_or(0.0);
        }
        for i in 0..self.block {
            let tail = time_out.get(2 * (self.block + i)).copied().unwrap_or(0.0) * norm;
            if let Some(slot) = self.overlap.get_mut(i) {
                *slot = tail;
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Brute-force O(n*m) linear convolution, truncated to `signal.len()`
    /// samples — the independent oracle a partitioned-FFT implementation is
    /// checked against.
    fn brute_convolve(signal: &[f64], kernel: &[f64]) -> Vec<f64> {
        let mut out = vec![0.0; signal.len()];
        for (n, slot) in out.iter_mut().enumerate() {
            let mut sum = 0.0;
            for (k, &tap) in kernel.iter().enumerate() {
                if let Some(&s) = signal.get(n.wrapping_sub(k))
                    && k <= n
                {
                    sum += s * tap;
                }
            }
            *slot = sum;
        }
        out
    }

    fn run_blocks(fir: &mut PartitionedFir, signal: &[f64], block: usize) -> Vec<f64> {
        let mut out = Vec::new();
        for chunk in signal.chunks(block) {
            let mut padded = vec![0.0; block];
            padded[..chunk.len()].copy_from_slice(chunk);
            out.extend(fir.process(&padded));
        }
        out
    }

    #[test]
    fn a_unit_impulse_kernel_is_the_identity() {
        let mut fir = PartitionedFir::new(&[1.0], 4).unwrap();
        let signal = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let out = run_blocks(&mut fir, &signal, 4);
        for (a, b) in out.iter().zip(signal.iter()) {
            assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        }
    }

    #[test]
    fn a_two_tap_kernel_within_one_block_matches_brute_force() {
        let kernel = [0.5, 0.5];
        let signal = [1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0];
        let expected = brute_convolve(&signal, &kernel);
        let mut fir = PartitionedFir::new(&kernel, 4).unwrap();
        let out = run_blocks(&mut fir, &signal, 4);
        for (a, b) in out.iter().zip(expected.iter()) {
            assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        }
    }

    #[test]
    fn a_kernel_spanning_multiple_partitions_matches_brute_force_across_block_boundaries() {
        // Kernel longer than the block size, so this genuinely exercises
        // more than one partition and the frequency-domain delay line's
        // cross-block bookkeeping, not just a single block's own FFT.
        let block = 4;
        let kernel: Vec<f64> = (0..10).map(|i| 1.0 / f64::from(i + 1)).collect();
        let signal: Vec<f64> = (0..32).map(|i| f64::from((i * 7) % 5) - 2.0).collect();
        let expected = brute_convolve(&signal, &kernel);
        let mut fir = PartitionedFir::new(&kernel, block).unwrap();
        let out = run_blocks(&mut fir, &signal, block);
        for (i, (a, b)) in out.iter().zip(expected.iter()).enumerate() {
            assert!((a - b).abs() < 1e-6, "sample {i}: {a} vs {b}");
        }
    }

    #[test]
    fn degenerate_inputs_are_rejected_rather_than_panicking() {
        assert!(PartitionedFir::new(&[], 4).is_none());
        assert!(PartitionedFir::new(&[1.0], 0).is_none());
    }

    #[test]
    fn block_size_reports_what_the_instance_was_built_for() {
        let fir = PartitionedFir::new(&[1.0, 2.0], 8).unwrap();
        assert_eq!(fir.block_size(), 8);
    }
}
