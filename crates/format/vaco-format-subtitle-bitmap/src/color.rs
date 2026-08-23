//! DVB's `Y Cr Cb T` colour space, and its conversion to RGBA.
//!
//! EN 300 743 §10 states CLUT entries as `Y Cr Cb T` (luma, two chroma
//! components, and a linear transparency), not RGB. The conversion below is
//! ITU-R BT.601's — a public colour-space formula referenced by the standard
//! itself, not an implementation detail of any particular decoder — so
//! parsing a CLUT segment all the way to usable colours stays container-layer
//! work rather than something borrowed from a decoder (D6/D7: this is
//! transcribed from the public BT.601 coefficients, not from any decoder's
//! source).

use crate::palette::Rgba;

/// Convert one `Y Cr Cb` triple (8 bits each, full-range BT.601) plus an
/// 8-bit transparency `t` to RGBA.
///
/// Integer BT.601: `R = Y + 1.402(Cr-128)`, `G = Y - 0.344(Cb-128) -
/// 0.714(Cr-128)`, `B = Y + 1.772(Cb-128)`, with the coefficients scaled by
/// 1024 and applied with a shift rather than a division (`clippy::integer_division`
/// is denied workspace-wide; 1024 is exactly `1 << 10`, so a shift is both the
/// permitted spelling and the faster one). Out-of-range results saturate,
/// same as any colour-space conversion at the gamut edge.
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "y/cb/cr/t and r/g/b are EN 300 743 §10's own field names for this exact conversion"
)]
pub fn ycbcrt_to_rgba(y: u8, cb: u8, cr: u8, t: u8) -> Rgba {
    let yi = i32::from(y);
    let cbi = i32::from(cb) - 128;
    let cri = i32::from(cr) - 128;

    let r = yi + ((1436 * cri) >> 10);
    let g = yi - ((352 * cbi) >> 10) - ((731 * cri) >> 10);
    let b = yi + ((1815 * cbi) >> 10);

    Rgba::new(clamp_u8(r), clamp_u8(g), clamp_u8(b), t)
}

fn clamp_u8(v: i32) -> u8 {
    let clamped = v.clamp(0, i32::from(u8::MAX));
    u8::try_from(clamped).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_range_black_is_black() {
        let rgba = ycbcrt_to_rgba(0, 128, 128, 255);
        assert_eq!((rgba.r, rgba.g, rgba.b), (0, 0, 0));
    }

    #[test]
    fn full_range_white_is_white() {
        let rgba = ycbcrt_to_rgba(255, 128, 128, 255);
        assert_eq!((rgba.r, rgba.g, rgba.b), (255, 255, 255));
    }

    #[test]
    fn transparency_passes_through_unchanged() {
        let rgba = ycbcrt_to_rgba(128, 128, 128, 42);
        assert_eq!(rgba.a, 42);
    }

    #[test]
    fn never_panics_across_the_full_input_grid() {
        // Cheap exhaustive-ish sweep rather than a fuzz target of its own:
        // every input is a `u8`, so this *is* the full domain restricted to a
        // coarse stride, and the function has no error path to hit.
        for y in (0..=255u8).step_by(17) {
            for cb in (0..=255u8).step_by(17) {
                for cr in (0..=255u8).step_by(17) {
                    let _ = ycbcrt_to_rgba(y, cb, cr, 255);
                }
            }
        }
    }
}
