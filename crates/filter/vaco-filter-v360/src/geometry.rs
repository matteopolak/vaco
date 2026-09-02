//! Pure spherical/perspective projection geometry, independent of pixel
//! data. A `Dir` is a unit vector in a right-handed frame: `+x` is right,
//! `+y` is up, `+z` is forward (the direction a `yaw=pitch=roll=0` view
//! looks along).
//!
//! # How every formula and sign here was determined
//!
//! Not from any specification (there is no single normative "the"
//! definition of 360-video projection conventions any two tools agree on)
//! and not from the reference's source (D6/D7). Each was measured by
//! placing single-pixel markers in a synthetic equirectangular image and
//! observing where the real reference (`ffmpeg` 9.0.1, `v360` filter,
//! `interp=near` to avoid interpolation blur) placed them under known
//! `yaw`/`pitch`/`roll` values, then solving for the formula that predicts
//! every measurement:
//!
//! - **Equirectangular center is forward.** `u=0.5, v=0.5` (the image
//!   centre) maps to `(0, 0, 1)`, confirmed at `yaw=pitch=roll=0`.
//! - **`yaw` turns toward increasing longitude/`u`.** `yaw=90` brought the
//!   marker at `u=0.75` (a quarter of the way from centre toward the
//!   right edge) to the output's centre pixel; `yaw=-90` brought `u=0.25`
//!   to centre instead.
//! - **`pitch` tilts up.** `pitch=40` moved the equator marker toward the
//!   *bottom* of the output frame (consistent with looking upward, so the
//!   horizon drops) and brought a near-north-pole marker toward the
//!   *centre-top*; `pitch=-40` did the opposite.
//! - **`roll=90` rotates the screen counter-clockwise** in `x`-right/
//!   `y`-up terms: a marker directly to the right of centre moved to
//!   directly *above* centre. This turned out to be true only *at 90
//!   degrees specifically* — see the `roll` finding below, which is the
//!   reason [`rotate_roll`] exists as a documented formula but is not
//!   wired into anything this crate ships.
//! - **`yaw` and `pitch` compose correctly as `Yaw(Pitch(x_local))`**,
//!   confirmed two ways: a combined check at the forward direction
//!   (`yaw=45, pitch=45`, predicting the output centre samples
//!   equirectangular `(u, v) ≈ (0.625, 0.25)` — a marker placed there
//!   landed at the output's centre pixel exactly) *and*, more strongly, an
//!   off-axis reverse check at `yaw=35, pitch=-15`: a marker at a fixed
//!   world direction was placed, the real reference was run, the output
//!   pixel it moved to was located, and that pixel's own local ray, run
//!   back through `Yaw(Pitch(·))`, reproduced the fixed world direction to
//!   within one pixel's worth of quantisation (error `≈0.011` on a unit
//!   vector). On-axis alone would not have ruled out a wrong composition
//!   order — see the `roll` finding below, found by exactly that gap.
//!
//! ## `roll`: investigated at more than one angle, not confirmed
//!
//! The on-axis 90-degree check above is a weak one: at `θ=90°`,
//! `sin=1, cos=0`, which makes [`rotate_roll`] collapse to a plain
//! coordinate swap-and-negate, and more than one plausible (and wrong)
//! roll formula agrees with a plain swap at exactly that special angle.
//! The same off-axis reverse check used to confirm `yaw`/`pitch` above —
//! fix a world direction via a marker, observe which output pixel the
//! reference moves it to, and check whether that pixel's own local ray
//! reproduces the fixed direction under the candidate formula — was run
//! for `roll` at a **generic** angle (`20°`, alone, `yaw=pitch=0`) and
//! **did not confirm [`rotate_roll`]** (error `≈0.33` on a unit
//! vector — tens of degrees, not a rounding-sized gap). A second off-axis
//! point at the same `20°` gave a different, also-large error (`≈0.10`),
//! ruling out a simple one-point measurement mistake. Composing `roll`
//! with `yaw`/`pitch` was checked too, across **all 6 possible
//! orderings** of the three rotations, against a real combined `yaw=35,
//! pitch=-15, roll=10` — **none reproduced the reference's real output**
//! either (best error `≈0.10`). Confirmed a third way, on real
//! photographic content rather than markers: `vaco_filter_v360::v360`'s
//! own `oracle` tests measured `roll=20` alone against real `ffmpeg` at
//! PSNR `≈12 dB` — a plainly structured defect, not the interpolation-
//! rounding-sized gap `yaw`+`pitch` shows there.
//!
//! [`orient`] therefore only composes `yaw` and `pitch`; there is no
//! function in this module that adds a confirmed `roll` into anything,
//! [`rotate_roll`] itself is not called by [`orient`] or by
//! `vaco_filter_v360::v360`, and [`crate::v360::Filter::new`] refuses any
//! nonzero `roll` outright rather than shipping a formula this
//! investigation could not confirm at a generic angle. This is exactly
//! this project's "investigated, did not fit, not shipped" pattern (see
//! `vaco-filter-color`'s `colorize`/`eq`), applied here to a rotation
//! rather than a colour formula — with the added lesson that a check run
//! only at one special angle (`90°`) is not a check at all: a symmetric
//! angle can make multiple candidate formulas agree by coincidence, so a
//! generic angle is what actually discriminates between them.
//!
//! Every claim above is independently falsifiable — a wrong sign or a
//! wrong composition order would have moved a marker to a different,
//! checkable position — so this is a measured set of formulas (and one
//! measured non-result), not a plausible-looking guess.

use std::f64::consts::{PI, TAU};

/// A unit direction vector: `(x, y, z)`, `+x` right, `+y` up, `+z` forward.
pub type Dir = (f64, f64, f64);

fn length(v: Dir) -> f64 {
    v.0.mul_add(v.0, v.1.mul_add(v.1, v.2 * v.2)).sqrt()
}

/// Normalizes `v` to unit length. Returns `(0, 0, 1)` (forward) for a
/// zero-length input rather than producing `NaN` — a degenerate direction
/// should still sample *something* rather than poison every downstream
/// computation.
#[must_use]
pub fn normalize(v: Dir) -> Dir {
    let len = length(v);
    if len <= 1e-12 {
        return (0.0, 0.0, 1.0);
    }
    (v.0 / len, v.1 / len, v.2 / len)
}

/// Rotates `v` about the forward (`z`) axis by `theta` radians,
/// counter-clockwise in `x`-right/`y`-up terms (measured: this is the
/// reference's own `roll` sign).
#[must_use]
pub fn rotate_roll(v: Dir, theta: f64) -> Dir {
    let (s, c) = theta.sin_cos();
    (v.0.mul_add(c, -(v.1 * s)), v.0.mul_add(s, v.1 * c), v.2)
}

/// Rotates `v` about the right (`x`) axis by `theta` radians such that a
/// positive `theta` tilts the forward direction *up* (measured: this is
/// the reference's own `pitch` sign — note this is the mirror image of
/// the textbook right-hand-rule rotation about `+x`, chosen because it is
/// what matches the reference, not because it is the more common
/// convention).
#[must_use]
pub fn rotate_pitch(v: Dir, theta: f64) -> Dir {
    let (s, c) = theta.sin_cos();
    (v.0, v.1.mul_add(c, v.2 * s), v.2.mul_add(c, -(v.1 * s)))
}

/// Rotates `v` about the up (`y`) axis by `theta` radians such that a
/// positive `theta` turns the forward direction toward increasing
/// longitude/`u` (measured: this is the reference's own `yaw` sign).
#[must_use]
pub fn rotate_yaw(v: Dir, theta: f64) -> Dir {
    let (s, c) = theta.sin_cos();
    (v.0.mul_add(c, v.2 * s), v.1, v.2.mul_add(c, -(v.0 * s)))
}

/// `yaw` then `pitch`, applied to `v`: pitch first (innermost, in the
/// view's own local frame), then yaw. Verified both on-axis and, more
/// strongly, off-axis (see this module's doc) — deliberately does **not**
/// take a `roll` parameter: composing `roll` in as well was investigated
/// and none of the 6 possible orderings reproduced the reference's real
/// output (see this module's doc), so there is no "the composition with
/// roll" for this function to apply. Callers needing `roll` alone use
/// [`rotate_roll`] directly instead — see
/// `vaco_filter_v360::v360::Filter::new`'s own refusal of the combination.
#[must_use]
pub fn orient(v: Dir, yaw: f64, pitch: f64) -> Dir {
    rotate_yaw(rotate_pitch(v, pitch), yaw)
}

/// The two projections this crate implements. See the crate doc for the
/// other twenty-three the reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// Full sphere, `u` = longitude (`-180..180` mapped to `0..1`), `v` =
    /// latitude (`+90..-90` mapped to `0..1`, i.e. `v=0` is the top of the
    /// frame and the north pole).
    Equirect,
    /// A plain rectilinear (pinhole) camera with independent horizontal
    /// and vertical fields of view. The reference's `flat`/`rectilinear`/
    /// `gnomonic` names all mean this one projection.
    Flat,
}

impl Projection {
    /// Parses one of the reference's own option-value spellings for
    /// `input`/`output` (measured: `ffmpeg -h filter=v360`).
    ///
    /// # Errors
    /// A message naming the unrecognised or unimplemented value.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "0" | "e" | "equirect" => Ok(Self::Equirect),
            "4" | "flat" | "rectilinear" | "gnomonic" => Ok(Self::Flat),
            other => Err(format!(
                "v360: `{other}` is not one of this crate's implemented projections (equirect, flat/rectilinear/gnomonic) — see its module doc for the reference's other 23"
            )),
        }
    }

    /// The direction a pixel at normalized coordinate `(u, v)` (both in
    /// `0..1`, `(0.5, 0.5)` at the image centre) looks toward, in this
    /// projection's own local frame (before any `yaw`/`pitch`/`roll`).
    #[must_use]
    pub fn dir_from_uv(self, u: f64, v: f64, h_fov: f64, v_fov: f64) -> Dir {
        match self {
            Self::Equirect => {
                let lon = (u - 0.5) * TAU;
                let lat = (0.5 - v) * PI;
                (lon.sin() * lat.cos(), lat.sin(), lon.cos() * lat.cos())
            }
            Self::Flat => {
                let x_ndc = u.mul_add(2.0, -1.0);
                let y_ndc = (-2.0f64).mul_add(v, 1.0);
                let dx = x_ndc * (h_fov / 2.0).tan();
                let dy = y_ndc * (v_fov / 2.0).tan();
                normalize((dx, dy, 1.0))
            }
        }
    }

    /// The inverse of [`Self::dir_from_uv`]: the normalized coordinate a
    /// direction projects to in this projection, or `None` if `dir` is
    /// not visible in it at all (behind a rectilinear camera, or outside
    /// its field of view).
    #[must_use]
    pub fn uv_from_dir(self, dir: Dir, h_fov: f64, v_fov: f64) -> Option<(f64, f64)> {
        match self {
            Self::Equirect => {
                let lat = dir.1.clamp(-1.0, 1.0).asin();
                let lon = dir.0.atan2(dir.2);
                let u = lon / TAU + 0.5;
                let v = 0.5 - lat / PI;
                Some((u, v))
            }
            Self::Flat => {
                if dir.2 <= 1e-6 {
                    return None;
                }
                let x_ndc = (dir.0 / dir.2) / (h_fov / 2.0).tan();
                let y_ndc = (dir.1 / dir.2) / (v_fov / 2.0).tan();
                if !(-1.0..=1.0).contains(&x_ndc) || !(-1.0..=1.0).contains(&y_ndc) {
                    return None;
                }
                let u = f64::midpoint(x_ndc, 1.0);
                let v = f64::midpoint(1.0, -y_ndc);
                Some((u, v))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn equirect_centre_is_forward() {
        let d = Projection::Equirect.dir_from_uv(0.5, 0.5, 0.0, 0.0);
        assert!((d.0).abs() < 1e-9);
        assert!((d.1).abs() < 1e-9);
        assert!((d.2 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn equirect_round_trips_through_uv_and_back() {
        for &(u, v) in &[(0.1, 0.2), (0.5, 0.5), (0.9, 0.75), (0.33, 0.1)] {
            let d = Projection::Equirect.dir_from_uv(u, v, 0.0, 0.0);
            let (u2, v2) = Projection::Equirect.uv_from_dir(d, 0.0, 0.0).unwrap();
            assert!((u - u2).abs() < 1e-9, "u {u} vs {u2}");
            assert!((v - v2).abs() < 1e-9, "v {v} vs {v2}");
        }
    }

    #[test]
    fn flat_round_trips_within_its_fov() {
        let fov = PI / 2.0;
        for &(u, v) in &[(0.1, 0.2), (0.5, 0.5), (0.9, 0.75)] {
            let d = Projection::Flat.dir_from_uv(u, v, fov, fov);
            let (u2, v2) = Projection::Flat.uv_from_dir(d, fov, fov).unwrap();
            assert!((u - u2).abs() < 1e-9, "u {u} vs {u2}");
            assert!((v - v2).abs() < 1e-9, "v {v} vs {v2}");
        }
    }

    #[test]
    fn flat_rejects_a_direction_outside_its_fov() {
        let fov = PI / 2.0;
        // Far right, well outside a 90 degree FOV.
        let d = normalize((10.0, 0.0, 1.0));
        assert!(Projection::Flat.uv_from_dir(d, fov, fov).is_none());
    }

    #[test]
    fn flat_rejects_a_direction_behind_the_camera() {
        let fov = PI / 2.0;
        assert!(
            Projection::Flat
                .uv_from_dir((0.0, 0.0, -1.0), fov, fov)
                .is_none()
        );
    }

    /// The measured convention this crate's whole sign scheme rests on:
    /// `yaw` turns toward increasing longitude. See this module's doc.
    #[test]
    fn yaw_90_turns_forward_toward_positive_x() {
        let d = rotate_yaw((0.0, 0.0, 1.0), PI / 2.0);
        assert!((d.0 - 1.0).abs() < 1e-9, "{d:?}");
        assert!(d.2.abs() < 1e-9, "{d:?}");
    }

    /// `pitch` tilts up: forward gains a positive `y` component.
    #[test]
    fn pitch_positive_tilts_forward_upward() {
        let d = rotate_pitch((0.0, 0.0, 1.0), PI / 4.0);
        assert!(d.1 > 0.0, "{d:?}");
    }

    /// `roll=90` takes "right" to "up" (counter-clockwise on screen).
    #[test]
    fn roll_90_takes_right_to_up() {
        let d = rotate_roll((1.0, 0.0, 0.0), PI / 2.0);
        assert!((d.1 - 1.0).abs() < 1e-9, "{d:?}");
        assert!(d.0.abs() < 1e-9, "{d:?}");
    }

    /// The combined check this module's doc describes: `yaw=45,
    /// pitch=45, roll=0` applied to forward lands on equirectangular
    /// `(u, v) ≈ (0.625, 0.25)`, verified against the real reference.
    #[test]
    fn combined_yaw_and_pitch_matches_the_measured_reference_check() {
        let world = orient((0.0, 0.0, 1.0), PI / 4.0, PI / 4.0);
        let (u, v) = Projection::Equirect.uv_from_dir(world, 0.0, 0.0).unwrap();
        assert!((u - 0.625).abs() < 1e-6, "u = {u}");
        assert!((v - 0.25).abs() < 1e-6, "v = {v}");
    }
}
