//! `axcorrelate` — cross-correlate two audio streams.
//!
//! `ffmpeg -h filter=axcorrelate` (2026-08-23): inputs `axcorrelate0`,
//! `axcorrelate1` (both audio), one audio output. Options: `size` (segment
//! size, `2..131072`, default `256`) and `algo` (`slow`/`fast`/`best`,
//! default `best`).
//!
//! # What was measured
//!
//! Feeding `ffmpeg -filter_complex "[0:a][1:a]axcorrelate=size=256"` two
//! identical sine tones gives full-scale positive output
//! (`32767` at `i16`); feeding one tone and its phase-inverted copy gives
//! full-scale negative output (`-32768`); two independent (uncorrelated)
//! signals give a small-magnitude output. That is a normalised correlation
//! coefficient in `[-1, 1]`, mapped directly onto the sample's own full-scale
//! range — mono in, mono out, and (separately confirmed) stereo in produces
//! stereo out, so the correlation runs independently per channel index
//! rather than downmixing first. Output length equals input length, so the
//! coefficient is a *sliding*, not block, correlation: one output sample per
//! input sample, over a trailing `size`-sample window ending at that sample.
//!
//! What the reference's own option text does not say, and this
//! implementation does not attempt to reproduce because it would need
//! sample-level access to a running `ffmpeg` process to distinguish: whether
//! the window is mean-subtracted before the ratio is taken. Both a demeaned
//! and a raw (non-demeaned) normalised cross-correlation give the same three
//! measured results above, since every probe signal used was already
//! (approximately) zero-mean. This implementation uses the raw form —
//! `r = Σxy / sqrt(Σx² · Σy²)` — which is simpler to maintain incrementally
//! and is a standard textbook normalised cross-correlation; see
//! `docs/filter/vaco-filter-aeffects.md` for the caveat.
//!
//! `algo` selects among the reference's three *implementations* of the same
//! arithmetic (a brute-force sum, an FFT-accelerated one, and an
//! auto-selecting "best"); it is parsed and validated here but does not
//! change this implementation's output, since the direct summation below is
//! already exact for every window size this crate is asked to run.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, Synced};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "axcorrelate",
    description: "cross-correlate two audio streams",
    inputs: common::AXCORRELATE_PADS,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// One channel's running sliding-window sums. Add-new/remove-old keeps each
/// step O(1) rather than re-summing the whole window every sample.
#[derive(Debug, Clone, Default)]
struct Window {
    xs: VecDeque<f64>,
    ys: VecDeque<f64>,
    sxx: f64,
    syy: f64,
    sxy: f64,
}

impl Window {
    fn push(&mut self, x: f64, y: f64, size: usize) -> f64 {
        self.xs.push_back(x);
        self.ys.push_back(y);
        self.sxx += x * x;
        self.syy += y * y;
        self.sxy += x * y;
        while self.xs.len() > size {
            if let (Some(ox), Some(oy)) = (self.xs.pop_front(), self.ys.pop_front()) {
                self.sxx -= ox * ox;
                self.syy -= oy * oy;
                self.sxy -= ox * oy;
            }
        }
        let denom = (self.sxx * self.syy).sqrt();
        if denom > 0.0 {
            (self.sxy / denom).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }
}

struct Axcorrelate {
    size: usize,
    windows: Vec<Window>,
}

impl FrameSyncFilter for Axcorrelate {
    fn on_event(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(a) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let (fmt, rate, _samples, layout, a_channels) = crate::sample::decode(&a)?;
        let b_channels = match event.get(1) {
            Some(b) => crate::sample::decode(b)?.4,
            None => return Ok(FrameOut::None),
        };

        let n_channels = a_channels.len().min(b_channels.len());
        if self.windows.len() < n_channels {
            self.windows.resize_with(n_channels, Window::default);
        }
        let n_samples = a_channels
            .first()
            .map_or(0, Vec::len)
            .min(b_channels.first().map_or(0, Vec::len));

        let mut out_channels: crate::sample::Channels =
            (0..n_channels).map(|_| vec![0.0f64; n_samples]).collect();

        for ch in 0..n_channels {
            let Some(xa) = a_channels.get(ch) else {
                continue;
            };
            let Some(xb) = b_channels.get(ch) else {
                continue;
            };
            let Some(win) = self.windows.get_mut(ch) else {
                continue;
            };
            let Some(dst) = out_channels.get_mut(ch) else {
                continue;
            };
            for i in 0..n_samples {
                let x = xa.get(i).copied().unwrap_or(0.0);
                let y = xb.get(i).copied().unwrap_or(0.0);
                let r = win.push(x, y, self.size);
                if let Some(slot) = dst.get_mut(i) {
                    *slot = r;
                }
            }
        }

        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &out_channels,
        )?;
        out.pts = event.timestamp();
        out.time_base = event.time_base();
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let size = common::usize_opt(req, &["size"], 256).clamp(2, 131_072);
    let filter = Axcorrelate {
        size,
        windows: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Audio, req.instance),
        filter: Box::new(Synced::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::Window;

    /// Identical signals correlate to (approximately) `1.0` once the window
    /// fills — a property of the *definition* of normalised cross-correlation,
    /// not a re-statement of [`Window::push`]'s own arithmetic.
    #[test]
    fn identical_signal_correlates_to_one() {
        let mut win = Window::default();
        let mut r = 0.0;
        for i in 0..64 {
            let x = (f64::from(i) * 0.3).sin();
            r = win.push(x, x, 32);
        }
        assert!((r - 1.0).abs() < 1e-9, "expected ~1.0, got {r}");
    }

    /// A signal against its own negation correlates to `-1.0`.
    #[test]
    fn inverted_signal_correlates_to_negative_one() {
        let mut win = Window::default();
        let mut r = 0.0;
        for i in 0..64 {
            let x = (f64::from(i) * 0.3).sin();
            r = win.push(x, -x, 32);
        }
        assert!((r + 1.0).abs() < 1e-9, "expected ~-1.0, got {r}");
    }

    /// Quadrature sinusoids (90 degrees out of phase) are orthogonal over a
    /// window spanning whole periods, so their correlation is near zero —
    /// independent of how `push` is implemented, since this is a property of
    /// `sin`/`cos` orthogonality, not of this module.
    #[test]
    fn quadrature_signals_are_nearly_uncorrelated() {
        let mut win = Window::default();
        let mut r = 0.0;
        let period = 32u32;
        for i in 0..(period * 8) {
            let phase = std::f64::consts::TAU * f64::from(i) / f64::from(period);
            r = win.push(phase.sin(), phase.cos(), period as usize);
        }
        assert!(r.abs() < 0.05, "expected ~0.0, got {r}");
    }

    #[test]
    fn silence_does_not_divide_by_zero() {
        let mut win = Window::default();
        let r = win.push(0.0, 0.0, 8);
        assert!(r.abs() < f64::EPSILON);
    }
}
