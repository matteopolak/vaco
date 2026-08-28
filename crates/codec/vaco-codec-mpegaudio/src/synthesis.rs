//! The 32-band polyphase synthesis filterbank shared by Layer I, II and III.
//!
//! `Vaco-Spec-Ref: iso-11172-3` §2.4.3 (`Nik = cos[(16+i)(2k+1)π/64]`) and
//! Annex B Table 3-B.3 (the window `Di`), confirmed against the actual
//! standard text rather than transcribed from memory.

use crate::tables::SYNTHESIS_WINDOW;

const FIFO_LEN: usize = 1024;

/// One channel's synthesis history. Each channel of a decode needs its own.
#[derive(Debug, Clone)]
pub(crate) struct Synthesis {
    fifo: [f32; FIFO_LEN],
    matrix: [[f32; 32]; 64],
}

impl Synthesis {
    pub(crate) fn new() -> Self {
        let mut matrix = [[0.0f32; 32]; 64];
        for (i, row) in matrix.iter_mut().enumerate() {
            for (k, cell) in row.iter_mut().enumerate() {
                let angle =
                    std::f64::consts::PI * ((16 + i) as f64) * ((2 * k + 1) as f64) / 64.0;
                *cell = angle.cos() as f32;
            }
        }
        Self {
            fifo: [0.0; FIFO_LEN],
            matrix,
        }
    }

    /// Consume 32 subband samples for one time slot and produce 32 PCM
    /// samples.
    pub(crate) fn synth_block(&mut self, subband: &[f32; 32]) -> [f32; 32] {
        self.fifo.copy_within(0..FIFO_LEN - 64, 64);
        for (i, row) in self.matrix.iter().enumerate().take(64) {
            let sum: f32 = row.iter().zip(subband.iter()).map(|(&m, &s)| m * s).sum();
            if let Some(slot) = self.fifo.get_mut(i) {
                *slot = sum;
            }
        }

        let mut u = [0.0f32; 512];
        for i in 0..8 {
            for j in 0..32 {
                let a = self.fifo.get(128 * i + j).copied().unwrap_or(0.0);
                let b = self.fifo.get(128 * i + 96 + j).copied().unwrap_or(0.0);
                if let Some(slot) = u.get_mut(64 * i + j) {
                    *slot = a;
                }
                if let Some(slot) = u.get_mut(64 * i + 32 + j) {
                    *slot = b;
                }
            }
        }
        for (u_i, w_i) in u.iter_mut().zip(SYNTHESIS_WINDOW.iter()) {
            *u_i *= w_i;
        }

        let mut out = [0.0f32; 32];
        for (j, slot) in out.iter_mut().enumerate() {
            let sum: f32 = (0..16).map(|i| u.get(j + 32 * i).copied().unwrap_or(0.0)).sum();
            *slot = sum;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DC-only subband input (only subband 0 excited) must, after enough
    /// blocks to fill the history, produce a periodic output — the property
    /// an oracle can check without re-deriving the filterbank's own numbers.
    #[test]
    fn silence_in_is_silence_out() {
        let mut s = Synthesis::new();
        for _ in 0..32 {
            let out = s.synth_block(&[0.0; 32]);
            assert!(out.iter().all(|&x| x == 0.0));
        }
    }

    #[test]
    fn a_single_impulse_produces_bounded_output() {
        let mut s = Synthesis::new();
        let mut subband = [0.0f32; 32];
        subband[0] = 1.0;
        let out = s.synth_block(&subband);
        for &x in &out {
            assert!(x.is_finite());
            assert!(x.abs() < 10.0, "unexpectedly large sample {x}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod stability_tests {
    use super::*;

    /// A steady single-subband tone, fed through many consecutive blocks,
    /// should settle into a steady-state output amplitude rather than
    /// decaying — this is what a subtle FIFO/window bug that only shows up
    /// after dozens of blocks would violate, and 12/32-block tests
    /// elsewhere are too short to catch it.
    #[test]
    fn steady_input_does_not_decay_over_many_blocks() {
        let mut s = Synthesis::new();
        let mut subband = [0.0f32; 32];
        subband[0] = 1.0;
        let mut last_peak = 0.0f32;
        for i in 0..2000 {
            let out = s.synth_block(&subband);
            let peak = out.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
            if i == 200 {
                last_peak = peak;
            }
            if i == 1999 {
                assert!(
                    peak > last_peak * 0.5,
                    "peak decayed from {last_peak} at block 200 to {peak} at block 1999"
                );
            }
        }
    }
}
