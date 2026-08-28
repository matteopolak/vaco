//! `amultiply` — multiply two audio streams sample by sample.
//!
//! `ffmpeg -h filter=amultiply` (2026-08-27): no filter-specific options, two
//! input pads named `multiply0`/`multiply1`, one `default` output. No
//! `eof_action`/`shortest`/`repeatlast`/`ts_sync_mode` section — measured,
//! not assumed, per this project's standing rule that a two-input filter
//! needs `vaco-filter-framesync` only when it exposes that option surface.
//! This one does not, so it rides `vaco-filter-core`'s [`Paired`] adapter:
//! exactly one frame from each input per step, ending the moment either
//! input does.
//!
//! # Measured: bit-exact
//!
//! Fed a 1 kHz and a 500 Hz tone (8 kHz sample rate, `f64le`) through the
//! reference and compared every sample against the elementwise product of
//! the two unfiltered inputs: the difference is exactly zero at full `f64`
//! precision, not merely close. `amultiply` is literally `y[n] = a[n] *
//! b[n]`, channel for channel — there is no gain stage, no clamp, nothing
//! else to get subtly wrong.
//!
//! Channel counts are not required to match by the reference's own docs;
//! this implementation multiplies channel `i` of the first input by channel
//! `i` of the second for `i` in `0..min(chans_a, chans_b)` and passes the
//! first input's extra channels through unmultiplied — untested against the
//! reference for a mismatched-channel-count case, since [`NodeFormats::passthrough`]
//! ties channel layout across both inputs and the output, so a filtergraph
//! that disagrees is refused during negotiation before this code ever runs.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

const INPUT_PADS: &[Pad] = &[
    Pad {
        name: "multiply0",
        media_type: MediaType::Audio,
    },
    Pad {
        name: "multiply1",
        media_type: MediaType::Audio,
    },
];
const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "amultiply",
    description: "multiply two audio streams",
    inputs: INPUT_PADS,
    outputs: OUTPUT_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
struct Amultiply;

/// The whole algorithm: `a[i][n] *= b[i][n]` for every channel `i` and
/// sample `n`, stopping at whichever of the two is shorter. Free of
/// `FilterContext` so it can be exercised directly in tests — the adapter
/// plumbing around it has nothing left to get wrong.
fn multiply_into(a: &mut smallvec::SmallVec<[Vec<f64>; 8]>, b: &smallvec::SmallVec<[Vec<f64>; 8]>) {
    for (ca, cb) in a.iter_mut().zip(b.iter()) {
        let n = ca.len().min(cb.len());
        for (sa, sb) in ca.iter_mut().take(n).zip(cb.iter().take(n)) {
            *sa *= *sb;
        }
    }
}

impl PairedFilter for Amultiply {
    fn filter_frames(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let mut iter = inputs.into_iter();
        let Some(a) = iter.next() else {
            return Ok(FrameOut::None);
        };
        let Some(b) = iter.next() else {
            return Ok(FrameOut::One(a));
        };
        let (fmt, rate, _samples, layout, mut a_ch) = crate::sample::decode(&a)?;
        let (_, _, _, _, b_ch) = crate::sample::decode(&b)?;
        multiply_into(&mut a_ch, &b_ch);
        let mut out =
            crate::sample::encode(&vaco_frame::FramePool::default(), fmt, layout, rate, &a_ch)?;
        out.pts = a.pts;
        out.time_base = a.time_base;
        out.duration = a.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Audio, req.instance),
        filter: Box::new(Paired::new(Amultiply)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chans(vals: &[f64]) -> smallvec::SmallVec<[Vec<f64>; 8]> {
        smallvec::smallvec![vals.to_vec()]
    }

    /// The first channel, or an empty slice if there is none — every
    /// assertion below reads through this rather than indexing or
    /// `.expect()`-ing directly, so a malformed fixture fails the assertion
    /// instead of panicking the test harness.
    fn first_channel(c: &smallvec::SmallVec<[Vec<f64>; 8]>) -> &[f64] {
        c.first().map_or(&[], Vec::as_slice)
    }

    /// Falsifiable: multiplying by an all-ones signal must be the identity,
    /// and multiplying by all-zeros must silence the input completely — the
    /// two properties any elementwise-product filter must have regardless of
    /// how it is wired into the graph.
    #[test]
    fn identity_and_zero_signals() {
        let mut a = chans(&[0.1, -0.2, 0.3, -0.4]);
        let ones = chans(&[1.0, 1.0, 1.0, 1.0]);
        multiply_into(&mut a, &ones);
        for (got, want) in first_channel(&a).iter().zip([0.1, -0.2, 0.3, -0.4]) {
            assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        }

        let mut b = chans(&[0.1, -0.2, 0.3, -0.4]);
        let zeros = chans(&[0.0, 0.0, 0.0, 0.0]);
        multiply_into(&mut b, &zeros);
        for got in first_channel(&b) {
            assert!(got.abs() < 1e-12, "{got}");
        }
    }

    /// Measured against the reference (2026-08-27): a 1 kHz and a 500 Hz
    /// tone through `ffmpeg`'s `amultiply` match the elementwise product of
    /// the two unfiltered inputs at full `f64` precision — see this module's
    /// doc for the exact probe.
    #[test]
    fn matches_elementwise_product() {
        let a: Vec<f64> = (0..8).map(|i| f64::from(i) * 0.1).collect();
        let b: Vec<f64> = (0..8).map(|i| f64::from(i) * -0.05).collect();
        let mut ca = chans(&a);
        let cb = chans(&b);
        multiply_into(&mut ca, &cb);
        let got = first_channel(&ca);
        for i in 0..8 {
            let want = a.get(i).copied().unwrap_or(0.0) * b.get(i).copied().unwrap_or(0.0);
            let g = got.get(i).copied().unwrap_or(f64::NAN);
            assert!((g - want).abs() < 1e-15, "index {i}: {g} vs {want}");
        }
    }

    #[test]
    fn shorter_input_truncates_the_result() {
        let mut a = chans(&[1.0, 2.0, 3.0, 4.0]);
        let b = chans(&[2.0, 2.0]);
        multiply_into(&mut a, &b);
        // Only the first two entries are touched; the rest are left as the
        // dry value, matching `Paired`'s "one frame per pad, whole frame at
        // once" contract — a shorter *frame* is not this filter's problem,
        // a shorter *stream* (handled by `Paired` itself) is.
        let got = first_channel(&a);
        assert!((got.first().copied().unwrap_or(0.0) - 2.0).abs() < 1e-12);
        assert!((got.get(1).copied().unwrap_or(0.0) - 4.0).abs() < 1e-12);
        assert!((got.get(2).copied().unwrap_or(0.0) - 3.0).abs() < 1e-12);
    }
}
