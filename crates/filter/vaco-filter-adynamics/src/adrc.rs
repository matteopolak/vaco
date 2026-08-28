//! `adrc` — Audio Spectral Dynamic Range Controller.
//!
//! `ffmpeg -h filter=adrc` (2026-08-27): `transfer` (an expression string,
//! default `"p"`), `attack` (1 to 1000 ms, default 50), `release` (5 to 2000
//! ms, default 100), `channels` (default `"all"`). The reference's own name
//! for the filter ("Spectral" Dynamic Range Controller) and the shape of its
//! only real parameter (an arbitrary transfer *expression*, not a
//! threshold/ratio pair) both point at a per-FFT-bin implementation: an STFT
//! forward transform, the expression evaluated per bin against that bin's
//! magnitude, an inverse transform. That is a real subsystem (window choice,
//! hop size, overlap-add reconstruction, and a way to evaluate an arbitrary
//! `vaco-expr` string per bin per frame without becoming the slowest filter
//! in the crate) that this pass does not build.
//!
//! # What is measured, and what follows from it
//!
//! Fed a 1 kHz tone through the reference at its own default (`transfer=p`)
//! and diffed sample-for-sample against the unfiltered input: after an
//! initial ~15-sample settle (consistent with an STFT's own analysis
//! latency, which this implementation does not reproduce), the two agree to
//! within `1e-9` for the rest of the signal. So `transfer=p` — literally
//! "pass" — **is** measured to be the identity, and this implementation
//! special-cases it as one rather than guessing at a curve that would be
//! wrong.
//!
//! For any other `transfer` value, the reference's expression grammar (what
//! variable names a per-bin transfer function receives, what a bin's
//! magnitude is measured in) is not documented in `-h filter=adrc`'s output,
//! and guessing one would be exactly the "plausible invention" this
//! project's own standing rule warns against. Rather than leave the option
//! silently inert, a non-`"p"` `transfer` here runs a broadband time-domain
//! compressor (this crate's own [`crate::common::Dynamics`] engine, the same
//! gain-computer `acompressor` uses) driven by `attack`/`release` — a real,
//! working dynamics processor in the right family, explicitly **not** a
//! reproduction of the reference's per-bin spectral algorithm. `channels`
//! (a per-channel enable list) is accepted but not applied: every channel is
//! processed identically.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Dynamics};
use crate::engine::{Curve, Detection, Link, Mode};

pub const DESC: FilterDesc = FilterDesc {
    name: "adrc",
    description: "Audio Spectral Dynamic Range Controller",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone)]
enum Body {
    /// Measured: `transfer=p` is the identity (see module doc).
    Identity,
    /// Structural fallback for any other `transfer` value — see module doc
    /// for exactly what this is and is not.
    Compressor(Dynamics),
}

#[derive(Debug, Clone)]
struct Adrc(Body);

impl FrameFilter for Adrc {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Body::Compressor(d) = &mut self.0 {
            d.configure(ctx)?;
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        match &mut self.0 {
            Body::Identity => Ok(FrameOut::One(input)),
            Body::Compressor(d) => d.filter_frame(ctx, input),
        }
    }

    fn flush_state(&mut self) {
        if let Body::Compressor(d) = &mut self.0 {
            d.flush_state();
        }
    }
}

/// `transfer` unset, or explicitly `"p"` ("pass") once trimmed, is the
/// measured-identity case; anything else falls back to the structural
/// compressor body. Free of `Instantiate` so it is directly testable.
fn is_identity_transfer(transfer: Option<&str>) -> bool {
    transfer.is_none_or(|t| t.trim() == "p")
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let transfer = req.named("transfer");
    let is_identity = is_identity_transfer(transfer.as_deref());
    let attack = common::f64_opt(req, &["attack"], 50.0);
    let release = common::f64_opt(req, &["release"], 100.0);
    let body = if is_identity {
        Body::Identity
    } else {
        let curve = Curve {
            threshold_db: -18.0,
            ratio: 2.0,
            knee_db: 6.0,
            mode: Mode::Downward,
        };
        Body::Compressor(Dynamics::new(
            1.0,
            curve,
            attack,
            release,
            1.0,
            1.0,
            Link::Average,
            Detection::Rms,
            1.0,
            1.0,
        ))
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(Adrc(body)).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default (`transfer` unset, or explicitly `"p"`, with or without
    /// surrounding whitespace) must resolve to the measured-identity body;
    /// anything else must not.
    #[test]
    fn default_and_explicit_p_are_both_identity() {
        assert!(is_identity_transfer(None));
        assert!(is_identity_transfer(Some("p")));
        assert!(is_identity_transfer(Some("  p  ")));
        assert!(!is_identity_transfer(Some("2*p")));
        assert!(!is_identity_transfer(Some("")));
    }
}
