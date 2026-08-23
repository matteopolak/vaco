//! Shared option parsing for this crate's seven filters.
//!
//! As in `vaco-filter-audio-eq::common` and `vaco-filter-audio-dynamics::common`,
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
