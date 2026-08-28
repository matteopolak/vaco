//! `removegrain` — per-plane spatial rank-order clipping against a pixel's 8
//! neighbours, one of the `AviSynth` `RemoveGrain` plugin's family of modes
//! (a widely documented, publicly specified pixel operation — see the
//! `AviSynth` wiki's `RemoveGrain` page — not something read out of `FFmpeg`'s
//! implementation of it).
//!
//! # Options (`ffmpeg -h filter=removegrain`, probed 2026-08-23)
//!
//! `m0`/`m1`/`m2`/`m3`: mode per plane, `int` `0..=24`, default `0` (no
//! filtering) for every plane.
//!
//! # Algorithm, and where it is a documented simplification
//!
//! Mode `0` is exact: the plane passes through unmodified.
//!
//! For every other mode, this implementation collects the 8 immediate
//! neighbours of a pixel, sorts them, and clips the centre pixel to
//! `[sorted[mode-1], sorted[8-mode]]` for `mode` in `1..=7` — mode `1`
//! clips to the full neighbour range (mildest: only removes a value more
//! extreme than *every* neighbour), and mode `4` clips to the middle pair
//! (closest to a rank-order median, strongest of the seven). That is a
//! genuine, monotonically-ordered rank-clipping family and it is *inspired*
//! by `RemoveGrain`'s documented mode numbering, but it is not a transcription
//! of `AviSynth`'s specific per-mode formulas (several of which mix in
//! distance-to-centre weighting this implementation does not model).
//!
//! # Modes `8..=24`: rejected, not substituted
//!
//! `clip_pixel`'s internal `rank = mode.clamp(1, 7)` still runs mode `7`'s
//! clip for any mode above `7` — that was, until this pass, reachable from
//! `create` too: `m0=12` parsed fine and quietly ran mode `7`'s formula
//! with no error, a real `RemoveGrain` mode number, silently substituted.
//! `create` now rejects `m0`/`m1`/`m2`/`m3=8..=24` explicitly instead
//! (`ensure_mode_implemented`), before an `Instance` is ever built. The
//! internal fallback-to-7 clamp stays exactly as it was: transcribing the
//! seventeen real per-mode `AviSynth` formulas (several mix in distance-
//! to-centre weighting this rank-clip family does not model at all) is a
//! genuine implementation project, not something this pass attempts —
//! only the silent-vs-explicit question is what changed here. See
//! `docs/filter/vaco-filter-denoise.md`.
//!
//! # Independent oracle
//!
//! Not "does this match `AviSynth`'s mode 3 formula" (which would need to
//! read an implementation to check), but a property **every** member of the
//! rank-clip family must have by construction: the output can never fall
//! outside `[min(neighbours), max(neighbours)]`. `tests::clips_an_outlier_
//! back_into_its_neighbourhood_range` plants one arbitrarily extreme pixel
//! in an otherwise uniform 3x3 block and checks the output lands exactly on
//! the (single, shared) neighbour value — true for every mode `1..=24`
//! without depending on which specific pairing this file picked. The
//! flat-field invariant (mode leaves a constant plane untouched) is the
//! other: `sorted[k]` is the same value for every `k` when every neighbour
//! is equal, so every mode's clip is a no-op.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};

pub const DESC: FilterDesc = FilterDesc {
    name: "removegrain",
    description: "Remove grain.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

fn mode_opt(req: &Instantiate<'_>, key: &str, position: usize) -> u8 {
    let raw = req
        .named(key)
        .or_else(|| req.positional(position))
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(0);
    raw.clamp(0, 24) as u8
}

#[derive(Debug, Clone, Copy)]
struct Options {
    modes: [u8; 4],
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        Self {
            modes: [
                mode_opt(req, "m0", 0),
                mode_opt(req, "m1", 1),
                mode_opt(req, "m2", 2),
                mode_opt(req, "m3", 3),
            ],
        }
    }

    fn mode_for(self, plane: usize) -> u8 {
        self.modes.get(plane).copied().unwrap_or(0)
    }

    /// Rejects any declared `m0..=m3` in `8..=24` by name, instead of
    /// letting it through to silently run mode `7`'s clip. `0..=7` are
    /// this implementation's real (if simplified — see module doc) rank-
    /// clip family; `8..=24` are real reference mode numbers this crate
    /// has not transcribed.
    ///
    /// # Errors
    /// Names the option (`m0`..`m3`) and the exact mode number rejected.
    fn ensure_implemented(self) -> std::result::Result<(), String> {
        const NAMES: [&str; 4] = ["m0", "m1", "m2", "m3"];
        for (name, mode) in NAMES.iter().zip(self.modes) {
            if mode > 7 {
                return Err(format!(
                    "removegrain: {name}={mode} is not implemented (modes 8..=24 are real \
                     AviSynth RemoveGrain modes this crate has not transcribed — see this \
                     module's own doc)"
                ));
            }
        }
        Ok(())
    }
}

fn clip_pixel(buf: &PlaneBuf, x: usize, y: usize, mode: u8) -> f32 {
    let Some(center) = buf.get(x, y) else { return 0.0 };
    if mode == 0 {
        return center;
    }
    #[allow(clippy::cast_possible_wrap, reason = "x/y are plane coordinates, far below i64 overflow")]
    let (xi, yi) = (x as i64, y as i64);
    let mut nbrs = [0.0f32; 8];
    let mut i = 0;
    for dy in -1i64..=1 {
        for dx in -1i64..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            if let Some(slot) = nbrs.get_mut(i) {
                *slot = buf.get_clamped(xi + dx, yi + dy);
            }
            i += 1;
        }
    }
    nbrs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Modes 8..=24 fall back to mode 7's clip (see module doc).
    let rank = mode.clamp(1, 7);
    let lo_idx = usize::from(rank.saturating_sub(1));
    let hi_idx = 8usize.saturating_sub(usize::from(rank));
    let lo = nbrs.get(lo_idx).copied().unwrap_or(center);
    let hi = nbrs.get(hi_idx).copied().unwrap_or(center);
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    center.clamp(lo, hi)
}

fn process_plane(buf: &PlaneBuf, mode: u8) -> PlaneBuf {
    if mode == 0 {
        return buf.clone();
    }
    let mut out = PlaneBuf::zeroed(buf.width, buf.height, buf.max_val);
    for y in 0..buf.height {
        for x in 0..buf.width {
            out.set(x, y, clip_pixel(buf, x, y, mode));
        }
    }
    out
}

#[derive(Debug)]
struct RemoveGrain {
    opts: Options,
}

impl FrameFilter for RemoveGrain {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            #[allow(clippy::cast_possible_truncation, reason = "plane_count() is at most 4")]
            let plane_idx = p as u8;
            let Some((bytes, max_val)) = video::sample_layout(format, plane_idx) else {
                return Err(video::unsupported_format());
            };
            let (pw, ph) = video::plane_dims(format, width, height, plane_idx);
            let Some(src) = input.plane(p) else { continue };
            let read = PlaneBuf::read(src, pw, ph, bytes, max_val);
            let mode = self.opts.mode_for(p);
            let result = process_plane(&read, mode);
            if let Some(mut dst) = out.plane_mut(p) {
                result.write(&mut dst, bytes);
            }
        }
        video::copy_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Options::parse(req);
    opts.ensure_implemented()?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(RemoveGrain { opts }).with_timeline(Timeline::always())),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn mode_zero_is_exact_identity() {
        let mut buf = PlaneBuf::zeroed(5, 5, 255.0);
        for y in 0..5 {
            for x in 0..5 {
                buf.set(x, y, ((x * 5 + y) as f32) * 3.0);
            }
        }
        let out = process_plane(&buf, 0);
        assert_eq!(out.as_slice(), buf.as_slice());
    }

    #[test]
    fn a_flat_field_is_unchanged_by_every_mode() {
        let buf = PlaneBuf::zeroed(5, 5, 255.0).clone();
        let mut flat = buf;
        for y in 0..5 {
            for x in 0..5 {
                flat.set(x, y, 77.0);
            }
        }
        for mode in [1, 2, 4, 7, 12, 24] {
            let out = process_plane(&flat, mode);
            for v in out.as_slice() {
                assert!((v - 77.0).abs() < 1e-4, "mode {mode}: {v}");
            }
        }
    }

    #[test]
    fn clips_an_outlier_back_into_its_neighbourhood_range() {
        // A uniform field of 100.0 with one wildly different pixel at the
        // centre: every mode 1..=24 must clip it back to exactly 100.0,
        // since min == max == 100.0 for its neighbours.
        let mut buf = PlaneBuf::zeroed(5, 5, 255.0);
        for y in 0..5 {
            for x in 0..5 {
                buf.set(x, y, 100.0);
            }
        }
        buf.set(2, 2, 255.0);
        for mode in 1..=24u8 {
            let out = process_plane(&buf, mode);
            let v = out.get(2, 2).unwrap();
            assert!((v - 100.0).abs() < 1e-3, "mode {mode}: got {v}");
        }
    }

    /// `m0`..`m3=8..=24` used to parse fine and silently run mode `7`'s
    /// clip -- a real `RemoveGrain` mode number, accepted, wrong, no
    /// error. `create` now rejects each by name instead.
    #[test]
    fn modes_eight_through_twenty_four_are_a_named_error_not_a_silent_substitution() {
        for (key, mode) in [("m0", 8), ("m1", 12), ("m2", 24), ("m3", 9)] {
            let arg = vaco_filter_graph::ast::Arg {
                key: Some((*key).to_owned()),
                raw_value: mode.to_string(),
                span: vaco_filter_graph::span::Span::default(),
            };
            let arguments = [arg];
            let req = Instantiate {
                name: "removegrain",
                instance: "removegrain",
                args: Some(&format!("{key}={mode}")),
                arguments: &arguments,
            };
            match create(&req) {
                Ok(_) => panic!("{key}={mode} should be rejected, not silently substituted"),
                Err(err) => assert!(
                    err.contains("removegrain") && err.contains("not implemented"),
                    "{key}={mode}: unexpected error text: {err}"
                ),
            }
        }
    }

    /// Modes `0..=7` -- the family this crate actually implements -- still
    /// create fine; the fix rejects only the unimplemented range.
    #[test]
    fn modes_zero_through_seven_still_create() {
        for mode in 0..=7 {
            let req = Instantiate {
                name: "removegrain",
                instance: "removegrain",
                args: Some(&format!("m0={mode}")),
                arguments: &[],
            };
            assert!(create(&req).is_ok(), "m0={mode} should still create");
        }
    }
}
