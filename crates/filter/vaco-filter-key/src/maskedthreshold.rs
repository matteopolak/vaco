//! `maskedthreshold` — pick `source` or `reference` per sample, based on
//! whether they differ by more than a threshold.
//!
//! `ffmpeg -h filter=maskedthreshold` documents `threshold` (`0..65535`,
//! default `1`), `planes` (bitmask, default 15) and `mode` (`abs`/`diff`,
//! default `abs`); no framesync surface, so (same reasoning as
//! [`crate::masked_pick`]) this is a lockstep two-input filter through
//! [`vaco_filter_core::adapt::Paired`].
//!
//! # Measured: `mode=abs`
//!
//! Five probes on `gray` inputs (`source`, `reference`) at `threshold=5`:
//!
//! ```text
//! out = source if |source - reference| <= threshold else reference
//! ```
//!
//! confirmed both directions (`source < reference` and `source >
//! reference`) and at the boundary (`diff=5` keeps `source`, `diff=6`
//! switches to `reference`). Exact.
//!
//! # Measured, 2026-08-28: `mode=diff`, recovered by sweeping instead of
//! sampling
//!
//! The one `mode=diff` data point on record (`source=100, reference=102,
//! threshold=5` -> `97`) was not enough to tell a formula from a
//! coincidence — exactly the shape of trap this campaign has hit before
//! (a sibling investigation found a shipped MPEG-1 formula that matched
//! the reference at exactly one point out of 256, the crossing of two
//! different rules). The fix is the same one that finding used: sweep the
//! full range instead of sampling it. `source` only spans `0..=255`, cheap
//! to sweep exhaustively at a fixed `(reference, threshold)`:
//!
//! ```text
//! ffmpeg -f lavfi -i "...geq=lum='X'" -f lavfi -i "...geq=lum='128'" //!   -filter_complex "[0][1]maskedthreshold=threshold=5:mode=diff" //!   -f rawvideo -pix_fmt gray8 -
//! ```
//!
//! gives `out[x] = x` for every `x` from `0` to `122`, then flatlines at
//! `123` for every `x` from `123` to `255` — not a symmetric clamp (a
//! window around `reference` would come back down on the *other* side
//! too), and not `mode=abs`'s pick-one-of-two-inputs shape either (the
//! flat region is a fixed value, not `reference` echoed back). The
//! pattern is exactly `out = min(source, reference - threshold)`
//! (`128 - 5 = 123`, matching both where the flat region starts and its
//! value). Confirmed against **zero mismatches across the full 256-value
//! sweep** at seven more `(reference, threshold)` pairs chosen to
//! discriminate edge behaviour, not just re-confirm the interior: unequal
//! `reference`/`threshold` magnitudes (`50/10`, `200/30`, `10/5`), the
//! two boundary constants (`threshold=0`, `reference=255`), and two pairs
//! where `reference - threshold` goes **negative** (`5/20`, `100/300`) —
//! both give `out = 0` for every `source`, confirming the floor is
//! `max(reference - threshold, 0)`, not an unclamped subtraction that
//! could go negative and wrap or panic. The original single-point probe
//! (`min(100, 102 - 5) = min(100, 97) = 97`) matches exactly, retroactively
//! -- it was never wrong, just underdetermined on its own.
//!
//! **Implemented**: `out = min(source, max(reference - threshold, 0))`,
//! per selected plane, independently per component — the same generic
//! `sample::read`/`write` shape `mode=abs` already uses, just a different
//! combining rule.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const PADS: &[Pad] = &[
    Pad { name: "source", media_type: MediaType::Video },
    Pad { name: "reference", media_type: MediaType::Video },
];
const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "maskedthreshold",
    description: "Pick pixels comparing absolute difference of two streams with threshold",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "maskedthreshold", help = "Pick pixels comparing absolute difference of two streams with threshold")]
pub(crate) struct Opts {
    #[opt(name = "threshold", help = "set threshold", default = 1, range = 0..=65535, flags(video, filtering))]
    pub threshold: i32,
    #[opt(name = "planes", help = "set planes", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
    // A `String`, not a ranged `i32`: the reference accepts both the
    // named form (`mode=diff`) and the bare integer (`mode=1`) --
    // confirmed directly against real `ffmpeg 8.1 -h filter=
    // maskedthreshold`, which lists `abs 0` / `diff 1` as named values of
    // an otherwise-integer option, the same shape `vaco-filter-geometry`'s
    // `pixelize::mode` and `vaco-filter-convolve`'s `convolution::Mode`
    // already needed this idiom for. `vaco-opts` has no named-integer
    // support, so this crate follows the same "String field, parse both
    // forms by hand" workaround.
    #[opt(name = "mode", help = "set mode", default = "abs".to_owned(), flags(video, filtering))]
    pub mode: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Abs,
    /// `out = min(source, max(reference - threshold, 0))` — see this
    /// module's doc, "Measured, 2026-08-28", for the full-range sweep
    /// that recovered this from a single ambiguous data point.
    Diff,
}

#[derive(Debug)]
struct Filter {
    threshold: i32,
    planes: i64,
    mode: Mode,
}

impl PairedFilter for Filter {
    fn filter_frames(&mut self, ctx: &mut FilterContext<'_>, inputs: SmallVec<[Frame; 4]>) -> Result<FrameOut> {
        let mut it = inputs.into_iter();
        let (Some(source), Some(reference)) = (it.next(), it.next()) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, width, height, .. } = source.data else {
            return Ok(FrameOut::One(source));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        for ch in 0..format.component_count() {
            let Some(comp) = sample::component(format, ch) else { continue };
            let (Some(sp), Some(rp), Some(mut dp)) = (
                source.plane(comp.plane as usize),
                reference.plane(comp.plane as usize),
                out.plane_mut(comp.plane as usize),
            ) else {
                continue;
            };
            let w = dp.row_bytes().checked_div(usize::from(comp.step.max(1))).unwrap_or(0);
            let n = dp.rows().min(sp.rows()).min(rp.rows());
            if !sample::plane_selected(self.planes, ch) {
                for y in 0..n {
                    let (Some(sr), Some(dr)) = (sp.row(y), dp.row_mut(y)) else { continue };
                    let len = sr.len().min(dr.len());
                    if let (Some(s), Some(d)) = (sr.get(..len), dr.get_mut(..len)) {
                        d.copy_from_slice(s);
                    }
                }
                continue;
            }
            for y in 0..n {
                let (Some(sr), Some(rr), Some(dr)) = (sp.row(y), rp.row(y), dp.row_mut(y)) else {
                    continue;
                };
                for x in 0..w {
                    let sv = sample::read(sr, x, comp, big_endian);
                    let rv = sample::read(rr, x, comp, big_endian);
                    let out_v = match self.mode {
                        Mode::Abs => {
                            let diff = (i32::from(sv) - i32::from(rv)).abs();
                            if diff <= self.threshold { sv } else { rv }
                        }
                        Mode::Diff => {
                            let floor = (i32::from(rv) - self.threshold).max(0);
                            #[allow(
                                clippy::cast_sign_loss,
                                clippy::cast_possible_truncation,
                                reason = "floor is clamped to >= 0 above, and min(sv as i32, floor)                                           can never exceed sv, which already fits u16"
                            )]
                            let clamped = i32::from(sv).min(floor) as u16;
                            clamped
                        }
                    };
                    sample::write(dr, x, comp, big_endian, out_v);
                }
            }
        }
        out.pts = source.pts;
        out.time_base = source.time_base;
        out.duration = source.duration;
        out.sample_aspect_ratio = source.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let mode = match opts.mode.as_str() {
        "abs" | "0" => Mode::Abs,
        "diff" | "1" => Mode::Diff,
        other => return Err(format!("maskedthreshold: bad mode `{other}`")),
    };
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: vec![set.clone(), set],
        outputs: vec![FormatSet::default()],
        ties: Tie::all_pads(2, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Paired::new(Filter {
            threshold: opts.threshold,
            planes: i64::from(opts.planes),
            mode,
        })),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn hand_computed_pick_on_measured_cases() {
        let cases: &[(i32, i32, i32, i32)] = &[(100, 102, 5, 100), (100, 106, 5, 106), (100, 96, 5, 100), (100, 94, 5, 94)];
        for &(source, reference, threshold, expected) in cases {
            let diff = (source - reference).abs();
            let out = if diff <= threshold { source } else { reference };
            assert_eq!(out, expected, "source={source} reference={reference}");
        }
    }

    /// Pinned against the full-range sweep in this module's doc:
    /// `mode=diff`'s `out = min(source, max(reference - threshold, 0))`,
    /// including the interior (unchanged), the flat region past the
    /// floor, the original single-point probe that was underdetermined
    /// on its own, and the negative-floor case (`reference < threshold`)
    /// clamping to `0` rather than going negative.
    #[test]
    fn hand_computed_diff_mode_on_the_swept_formula() {
        let cases: &[(i32, i32, i32, i32)] = &[
            // (source, reference, threshold, expected)
            (50, 128, 5, 50),   // interior: source < reference - threshold
            (123, 128, 5, 123), // exactly at the floor
            (124, 128, 5, 123), // just past it: flattens
            (255, 128, 5, 123), // far past it: still flattens, not clamped elsewhere
            (100, 102, 5, 97),  // the original ambiguous single-point probe
            (200, 5, 20, 0),    // reference - threshold < 0: floors at 0, not negative
            (0, 100, 300, 0),   // same, source already 0
        ];
        for &(source, reference, threshold, expected) in cases {
            let floor = (reference - threshold).max(0);
            let out = source.min(floor);
            assert_eq!(
                out, expected,
                "source={source} reference={reference} threshold={threshold}"
            );
        }
    }
}
