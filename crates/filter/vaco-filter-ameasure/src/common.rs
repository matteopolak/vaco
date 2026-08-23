//! Shared option parsing and pad descriptors.
//!
//! Mirrors `vaco-filter-audio-dynamics::common` exactly: options are read
//! straight off [`Instantiate::named`] rather than through a strict
//! `vaco_opts::Options`-derived parser, so a filtergraph string setting an
//! option this crate does not implement is silently accepted rather than
//! rejected — the same documented convention every sibling audio crate
//! uses.

use vaco_core::MediaType;
use vaco_filter_core::Pad;

use vaco_filter_graph::registry::Instantiate;

/// The single named audio pad every 1-in/1-out filter in this crate uses.
pub(crate) const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// `apsnr`/`asdr`/`asisdr` all name their two inputs `input0`/`input1`
/// (`ffmpeg -h filter=apsnr`, 2026-08-23) — distinct from `axcorrelate`'s
/// own `axcorrelate0`/`axcorrelate1`, so not folded into one constant.
pub(crate) const INPUT01_PADS: &[Pad] = &[
    Pad {
        name: "input0",
        media_type: MediaType::Audio,
    },
    Pad {
        name: "input1",
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

/// Linear amplitude to dB, clamped rather than producing `-inf` for silence —
/// same convention and same floor as
/// `vaco-filter-audio-dynamics::common::db`.
pub(crate) fn db(linear: f64) -> f64 {
    if linear.is_finite() && linear > 1e-12 {
        20.0 * linear.abs().log10()
    } else {
        -240.0
    }
}

/// Cumulative pairwise statistics between a reference signal and a signal
/// under test — the one accumulator `apsnr`, `asdr` and `asisdr` are each a
/// different closed-form reduction of. Shared because it is pure
/// bookkeeping (D19: one definition per concept); the three metrics it
/// feeds are still three distinct, independently-checked formulas.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PairStats {
    pub(crate) sum_ref_sq: f64,
    pub(crate) sum_est_sq: f64,
    pub(crate) sum_diff_sq: f64,
    pub(crate) sum_cross: f64,
    pub(crate) count: u64,
}

impl PairStats {
    pub(crate) fn observe(&mut self, reference: f64, estimate: f64) {
        let diff = reference - estimate;
        self.sum_ref_sq += reference * reference;
        self.sum_est_sq += estimate * estimate;
        self.sum_diff_sq += diff * diff;
        self.sum_cross += reference * estimate;
        self.count += 1;
    }

    /// `10*log10(peak^2 / MSE)`, `peak == 1.0` for the normalized `[-1, 1]`
    /// domain every filter in this crate decodes samples into.
    pub(crate) fn psnr_db(&self) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let mse = self.sum_diff_sq / self.count as f64;
        if mse <= 0.0 {
            return Some(f64::INFINITY);
        }
        Some(10.0 * (1.0 / mse).log10())
    }

    /// `10*log10(||reference||^2 / ||reference - estimate||^2)`.
    pub(crate) fn sdr_db(&self) -> Option<f64> {
        if self.count == 0 || self.sum_ref_sq <= 0.0 {
            return None;
        }
        if self.sum_diff_sq <= 0.0 {
            return Some(f64::INFINITY);
        }
        Some(10.0 * (self.sum_ref_sq / self.sum_diff_sq).log10())
    }

    /// Scale-invariant SDR (Le Roux et al. 2019): project the estimate onto
    /// the reference's scale first, so a pure gain change scores infinite
    /// rather than being penalised as distortion.
    pub(crate) fn si_sdr_db(&self) -> Option<f64> {
        if self.count == 0 || self.sum_ref_sq <= 0.0 {
            return None;
        }
        let alpha = self.sum_cross / self.sum_ref_sq;
        let target_energy = alpha * alpha * self.sum_ref_sq;
        let noise_energy =
            self.sum_est_sq - 2.0 * alpha * self.sum_cross + alpha * alpha * self.sum_ref_sq;
        if noise_energy <= 0.0 {
            return Some(f64::INFINITY);
        }
        if target_energy <= 0.0 {
            return None;
        }
        Some(10.0 * (target_energy / noise_energy).log10())
    }
}
