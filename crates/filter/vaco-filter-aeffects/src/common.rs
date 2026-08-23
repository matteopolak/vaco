//! Shared option parsing for this crate's seven filters.
//!
//! As in `vaco-filter-aeq::common` and `vaco-filter-audio-dynamics::common`,
//! options are read straight off [`Instantiate::named`] rather than through a
//! strict `vaco_opts::Options`-derived parser, so a filtergraph string setting
//! an option this crate does not implement is silently accepted rather than
//! rejected — matching the established precedent in both sibling crates.

use vaco_core::MediaType;
use vaco_filter_core::Pad;

use vaco_filter_graph::registry::Instantiate;

pub(crate) const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// `axcorrelate`'s two named input pads, matching `ffmpeg -h filter=axcorrelate`
/// (`axcorrelate0`, `axcorrelate1`) rather than the generic `main`/`sidechain`
/// naming `vaco-filter-audio-dynamics` uses for its own dual-input filters.
pub(crate) const AXCORRELATE_PADS: &[Pad] = &[
    Pad {
        name: "axcorrelate0",
        media_type: MediaType::Audio,
    },
    Pad {
        name: "axcorrelate1",
        media_type: MediaType::Audio,
    },
];

pub(crate) fn f64_opt(req: &Instantiate<'_>, keys: &[&str], default: f64) -> f64 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(f) = v.trim().parse::<f64>()
        {
            return f;
        }
    }
    default
}

pub(crate) fn bool_opt(req: &Instantiate<'_>, keys: &[&str], default: bool) -> bool {
    for k in keys {
        if let Some(v) = req.named(k) {
            let v = v.trim();
            if v.eq_ignore_ascii_case("true") || v == "1" {
                return true;
            }
            if v.eq_ignore_ascii_case("false") || v == "0" {
                return false;
            }
        }
    }
    default
}

pub(crate) fn usize_opt(req: &Instantiate<'_>, keys: &[&str], default: usize) -> usize {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(n) = v.trim().parse::<usize>()
        {
            return n;
        }
    }
    default
}

/// A linearly-interpolated delay line: writes one sample per call and reads
/// back a value from `delay_samples` (which may be fractional) earlier,
/// interpolating between the two nearest whole-sample history entries.
///
/// Shared by `chorus`, `flanger` and `vibrato` — every LFO-modulated delay
/// filter in this crate needs exactly this building block, so it lives here
/// once (D19) rather than being re-derived per filter.
pub(crate) struct InterpDelay {
    hist: std::collections::VecDeque<f64>,
    max_len: usize,
}

impl InterpDelay {
    pub(crate) fn new(max_len_samples: usize) -> Self {
        let max_len = max_len_samples.max(1);
        let mut hist = std::collections::VecDeque::new();
        hist.resize(max_len, 0.0);
        Self { hist, max_len }
    }

    /// Push `x` into the line and return the interpolated value
    /// `delay_samples` behind the sample just pushed (`0.0` returns `x`
    /// itself).
    pub(crate) fn process(&mut self, x: f64, delay_samples: f64) -> f64 {
        self.hist.push_back(x);
        if self.hist.len() > self.max_len {
            self.hist.pop_front();
        }
        let len = self.hist.len();
        if len == 0 {
            return x;
        }
        let max_delay = (len - 1) as f64;
        let d = delay_samples.clamp(0.0, max_delay);
        let read_pos = max_delay - d; // 0 = oldest, max_delay = newest (just pushed)
        let i0 = read_pos.floor().max(0.0) as usize;
        let frac = read_pos - (i0 as f64);
        let i1 = (i0 + 1).min(len.saturating_sub(1));
        let s0 = self.hist.get(i0).copied().unwrap_or(0.0);
        let s1 = self.hist.get(i1).copied().unwrap_or(0.0);
        s0 + (s1 - s0) * frac
    }

    pub(crate) fn flush(&mut self) {
        self.hist.clear();
        self.hist.resize(self.max_len, 0.0);
    }
}
