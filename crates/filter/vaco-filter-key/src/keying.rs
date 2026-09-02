//! Shared distance/ramp math for the RGB "key by colour distance" family
//! (`colorkey`, `colorhold`; the YUV/HSV/luma variants named by this
//! crate's row — `chromakey`, `chromahold`, `hsvkey`, `hsvhold`,
//! `lumakey` — are not implemented yet, see `lib.rs`'s scope note for
//! why).
//!
//! # Measured: the distance metric
//!
//! `colorkey=color=black:similarity=S:blend=0` on pure red (`0xff0000`)
//! against a `similarity` sweep pinpoints the transparent/opaque boundary
//! between `similarity=0.57735` (still opaque, i.e. distance > similarity)
//! and `similarity=0.5774` (transparent) — matching `1/sqrt(3) =
//! 0.5773502691896258` to five decimal places. That is exactly the
//! normalised Euclidean distance between `(1,0,0)` and `(0,0,0)` in
//! `[0,1]^3`, divided by `sqrt(3)`:
//!
//! ```text
//! distance = sqrt(sum((p_i - k_i)^2 for i in 0..3)) / sqrt(3)
//! ```
//!
//! (each `p_i`, `k_i` normalised to `[0, 1]` by its own component's max
//! value; the key colour is always 8-bit, so its own normalisation is by
//! 255 regardless of the frame's bit depth).
//!
//! # Measured: the blend ramp, and where it stops being exact
//!
//! With `blend > 0`, five points on `colorkey=color=black:similarity=0.2:
//! blend=0.2` applied to `0xRR0000` (`RR` swept) all matched
//! `alpha = floor(clamp((distance - similarity) / blend, 0, 1) * 255)`
//! exactly — including the truncation rule this crate's sibling
//! `vaco-filter-lut` measured independently for a completely different
//! filter family. `blend = 0` is a hard step (division by zero is not
//! evaluated; [`ramp`] special-cases it).
//!
//! Confirmed **not** exact for `colorhold`, which reuses the identical
//! ramp to blend a pixel toward its own grayscale average: two of the
//! four interior probe points came out one ULP off (`34` vs measured
//! `35`, `52` vs measured `51`) while the endpoints matched exactly. This
//! crate ships the measured formula as understood — right at every
//! extreme and off by one in the interior — rather than pretend it is
//! bit-exact; see `colorhold.rs`'s own doc and test for the specific
//! mismatch.

use vaco_core::parse::Rgba;

/// Normalised Euclidean distance between two `[0, 1]`-normalised RGB
/// points, divided by `sqrt(3)` — see this module's doc for the
/// measurement pinning down both the metric and the `sqrt(3)` divisor.
#[must_use]
pub(crate) fn rgb_distance(p: [f64; 3], k: [f64; 3]) -> f64 {
    let sum_sq: f64 = p
        .iter()
        .zip(k.iter())
        .map(|(pi, ki)| (pi - ki).powi(2))
        .sum();
    sum_sq.sqrt() / 3f64.sqrt()
}

/// The key colour, normalised to `[0, 1]` per channel (always 8-bit,
/// regardless of the frame's own bit depth — the `color` option is
/// always an 8-bit spec).
#[must_use]
pub(crate) fn key_rgb(c: Rgba) -> [f64; 3] {
    [
        f64::from(c.r) / 255.0,
        f64::from(c.g) / 255.0,
        f64::from(c.b) / 255.0,
    ]
}

/// `clamp((distance - similarity) / blend, 0, 1)`, with `blend <= 0`
/// treated as a hard step rather than a division by zero — measured in
/// this module's doc (`blend=0`'s transitions are exact steps at
/// `distance == similarity`).
#[must_use]
pub(crate) fn ramp(distance: f64, similarity: f64, blend: f64) -> f64 {
    if blend <= 0.0 {
        if distance > similarity { 1.0 } else { 0.0 }
    } else {
        ((distance - similarity) / blend).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_red_against_black_key_matches_one_over_sqrt_three() {
        let d = rgb_distance([1.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        assert!((d - 1.0 / 3f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "ramp's hard-step branch returns the literals 0.0/1.0 exactly, never a computed value"
    )]
    fn ramp_is_a_hard_step_at_zero_blend() {
        assert_eq!(ramp(0.19, 0.2, 0.0), 0.0);
        assert_eq!(ramp(0.21, 0.2, 0.0), 1.0);
    }

    #[test]
    fn ramp_matches_the_five_measured_points() {
        // Measured: ffmpeg 8.1, colorkey=color=black:similarity=0.2:
        // blend=0.2 on 0xRR0000 (this module's doc).
        let cases: &[(u8, u16)] = &[
            (0x64, 33),
            (0x85, 128),
            (0x96, 178),
            (0xaa, 235),
            (0xb1, 255),
        ];
        for &(rr, expected_alpha) in cases {
            let d = rgb_distance([f64::from(rr) / 255.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
            let frac = ramp(d, 0.2, 0.2);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "frac in [0, 1], product in [0, 255]"
            )]
            let alpha = (frac * 255.0) as u16;
            assert_eq!(alpha, expected_alpha, "rr=0x{rr:02x}");
        }
    }
}
